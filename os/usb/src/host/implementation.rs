// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{
    ArchiveEventSubscriptionHandler, BlockingScalarHandler, DeferredLendMut, DeferredLendMutHandler,
    MessageId as _, ScalarHandler,
};
use usb::{host::messages::*, UsbError};
use utralib::{HW_UHPHS_EHCI_BASE, HW_UHPHS_EHCI_MEM};
use xous::{arch::irq::IrqNumber, keyos::PAGE_SIZE, MemoryRange};
use xous_ticktimer::{Ticktimer, TicktimerCallback};

use super::messages::*;
use crate::PowerManagerApi;

#[derive(server::Server)]
#[name = "os/usb"]
pub struct UsbHostServer {
    ticktimer: Ticktimer,
    ehci_memory: MemoryRange,
    qh_backing: MemoryRange,
    qtd_backing: MemoryRange,
    ehci: ehci::Controller<EhciMessageContext>,
    event_subscribers: Vec<server::ArchiveEventSubscriber<UsbEvent>>,
    next_device_handle: usize,
    devices: Vec<Device>,
    statistics: Statistics,
    power_manager: PowerManagerApi,
    work_callback: Option<TicktimerCallback>,
    enabled: bool,
}

#[derive(Debug, Default, Clone, server::Permissions)]
#[server_name = "os/usb"]
#[all_permissions]
struct InternalPermissions;

#[derive(Debug, Default)]
struct Statistics {
    last_print: u64,
    work_called: usize,
    transfers: usize,
}

enum EhciMessageContext {
    BulkIn(DeferredLendMut<BulkIn>),
    BulkOut(DeferredLendMut<BulkOut>),
    Control(ControlContext),
}

/// A control transfer in flight. `data` is where a read lands, and stays empty for a
/// write; the buffers the controller works from belong to the driver's uncached pool.
struct ControlContext {
    response: server::ArchiveResponse<Result<Vec<u8>, UsbError>>,
    data: Vec<u8>,
}

impl ehci::TransferContext for EhciMessageContext {
    fn data_buffer(&mut self) -> &mut [u8] {
        match self {
            EhciMessageContext::BulkIn(b) => {
                let body = b.body_mut();
                &mut body.buffer.as_slice_mut()[..body.length]
            }
            EhciMessageContext::BulkOut(b) => {
                let body = b.body_mut();
                &mut body.buffer.as_slice_mut()[..body.length]
            }
            EhciMessageContext::Control(c) => &mut c.data,
        }
    }
}

impl EhciMessageContext {
    fn respond(self, r: Result<usize, UsbError>) {
        match self {
            EhciMessageContext::BulkIn(mut s) => {
                if let Some(data) = r.ok().and_then(|len| s.body().buffer.subrange(0, len)) {
                    xous::flush_cache(data, xous::CacheOperation::Invalidate).ok();
                }
                s.set_response(r)
            }
            EhciMessageContext::BulkOut(mut s) => s.set_response(r),
            EhciMessageContext::Control(c) => {
                let ControlContext { response, data, .. } = c;
                let answer = r.map(|len| data[..len.min(data.len())].to_vec());
                if let Err(e) = response.respond(answer) {
                    log::warn!("Could not answer a control transfer: {e:?}");
                }
            }
        }
    }
}

#[derive(Debug)]
struct Device {
    handle: usize,
    claimed: Option<xous::PID>,
    address: u8,
    descriptors: ehci::descriptors::DescriptorSet,
}

struct InterruptContext<'a> {
    conn: server::CheckedConn<InternalPermissions>,
    ehci: &'a ehci::Controller<EhciMessageContext>,
}

impl server::Server for UsbHostServer {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        log::debug!("Claiming UHPHS IRQ");
        let int_ctx = Box::into_raw(Box::new(InterruptContext {
            conn: server::CheckedConn::default(),
            ehci: &self.ehci,
        }));
        xous::claim_interrupt(IrqNumber::Uhphs, ehci_irq_handler, int_ctx as *mut usize)
            .expect("Could not claim UHPHS interrupt");
        xous::register_system_event_handler(
            xous::SystemEvent::Disconnected,
            context.sid(),
            SubscriberDisconnected::ID,
        )
        .unwrap();
        self.work_callback =
            Some(TicktimerCallback::new(context.sid()).expect("Could not connect to ticktimer"));
    }
}

impl UsbHostServer {
    const BUFFER_POOL_SIZE: usize = 8;
    const QH_POOL_SIZE: usize = 16;
    const QTD_POOL_SIZE: usize = 128;
    pub const WORK_PERIOD_MS: usize = 100;
}

impl UsbHostServer {
    pub fn new() -> Self {
        // Unfortunately just having a register definition in the .svd file is not enough,
        // we need a memoryRegion declaration too for map_memory to work, because that's
        // what causes the MEMx tags to be put into the kernel arguments.
        // This assert is here to make sure both are present and correct in `utralib`
        #[allow(clippy::assertions_on_constants)]
        const _: () = assert!(
            HW_UHPHS_EHCI_BASE == HW_UHPHS_EHCI_MEM,
            "EHCI register area must coincide with EHCI custom memory"
        );

        let power_manager = PowerManagerApi::default();
        power_manager
            .enable_peripheral(atsama5d27::pmc::PeripheralId::Uhphs)
            .expect("Could not enable Usb host clock");

        let ehci_memory = xous::map_memory(
            xous::MemoryAddress::new(HW_UHPHS_EHCI_BASE),
            None,
            PAGE_SIZE,
            xous::MemoryFlags::W | xous::MemoryFlags::DEV | xous::MemoryFlags::NO_CACHE,
        )
        .expect("Could not map HW memory");
        let mut qh_backing = xous::map_memory(
            None,
            None,
            (core::mem::size_of::<ehci::QueueHeadPoolElement>() * Self::QH_POOL_SIZE)
                .next_multiple_of(PAGE_SIZE),
            xous::MemoryFlags::W
                | xous::MemoryFlags::DEV
                | xous::MemoryFlags::NO_CACHE
                | xous::MemoryFlags::POPULATE,
        )
        .expect("Could not map QH pool memory");

        let mut qtd_backing = xous::map_memory(
            None,
            None,
            (core::mem::size_of::<ehci::QtdPoolElement>() * Self::QTD_POOL_SIZE).next_multiple_of(PAGE_SIZE),
            xous::MemoryFlags::W
                | xous::MemoryFlags::DEV
                | xous::MemoryFlags::NO_CACHE
                | xous::MemoryFlags::POPULATE,
        )
        .expect("Could not map QTD pool memory");

        let mut buffer_backing = xous::map_memory(
            None,
            None,
            (core::mem::size_of::<ehci::BufferPoolElement>() * Self::BUFFER_POOL_SIZE)
                .next_multiple_of(PAGE_SIZE),
            xous::MemoryFlags::W | xous::MemoryFlags::NO_CACHE | xous::MemoryFlags::POPULATE,
        )
        .expect("Could not map QTD pool memory");

        let ehci = ehci::Controller::new(
            ehci_memory.as_ptr() as usize,
            unsafe { ehci::QueueHeadPool::new(qh_backing.as_slice_mut().as_mut_ptr_range(), virt_to_phys) },
            unsafe { ehci::QtdPool::new(qtd_backing.as_slice_mut().as_mut_ptr_range(), virt_to_phys) },
            unsafe { ehci::BufferPool::new(buffer_backing.as_slice_mut().as_mut_ptr_range(), virt_to_phys) },
            virt_to_phys,
        )
        .expect("Could not instantiate EHCI controller");

        power_manager
            .disable_peripheral(atsama5d27::pmc::PeripheralId::Uhphs)
            .expect("Could not disable Usb host clock");
        Self {
            ticktimer: Ticktimer::default(),
            ehci_memory,
            ehci,
            qh_backing,
            qtd_backing,
            event_subscribers: Vec::new(),
            next_device_handle: 0,
            devices: Vec::new(),
            statistics: Default::default(),
            power_manager,
            work_callback: None,
            enabled: false,
        }
    }

    fn work(&mut self) {
        if !self.enabled {
            return;
        }
        let tick_count = self.ticktimer.elapsed_ms();
        if tick_count > self.statistics.last_print + 1000 {
            log::trace!("Statistics: {:?}", self.statistics);
            self.statistics = Default::default();
            self.statistics.last_print = tick_count;
        };
        self.statistics.work_called += 1;
        if let Err(e) = self.ehci.work(
            tick_count as usize,
            &mut EhciEventHandler {
                devices: &mut self.devices,
                next_device_handle: &mut self.next_device_handle,
                event_subscribers: &mut self.event_subscribers,
            },
        ) {
            log::warn!("Error during ehci work(): {e:?}");
        }
        self.work_callback.as_ref().unwrap().request(Self::WORK_PERIOD_MS, DoWork::ID, 0);
    }

    fn enable(&mut self) -> Result<(), UsbError> {
        if self.enabled {
            return Ok(());
        }
        self.power_manager.enable_peripheral(atsama5d27::pmc::PeripheralId::Uhphs)?;
        self.enabled = true;
        self.ehci.enable()?;
        self.ehci.enable_interrupts();
        self.work();
        Ok(())
    }

    fn disable(&mut self) -> Result<(), UsbError> {
        if !self.enabled {
            return Ok(());
        }
        self.work_callback.as_ref().unwrap().cancel(DoWork::ID);
        self.ehci.disable(&mut EhciEventHandler {
            devices: &mut self.devices,
            next_device_handle: &mut self.next_device_handle,
            event_subscribers: &mut self.event_subscribers,
        })?;
        self.power_manager.disable_peripheral(atsama5d27::pmc::PeripheralId::Uhphs)?;
        self.enabled = false;
        Ok(())
    }
}

impl UsbHostServer {
    /// Check a bulk transfer's device and buffer, and ready the buffer for the controller. Returns the
    /// device address and endpoint to schedule it on.
    fn prepare_bulk(&mut self, context: &EhciMessageContext) -> Result<(u8, u8), UsbError> {
        let (pid, handle, endpoint, buffer, length) = match context {
            EhciMessageContext::BulkIn(m) => {
                let b = m.body();
                (m.pid(), b.handle, b.endpoint, b.buffer, b.length)
            }
            EhciMessageContext::BulkOut(m) => {
                let b = m.body();
                (m.pid(), b.handle, b.endpoint, b.buffer, b.length)
            }
            EhciMessageContext::Control(_) => return Err(UsbError::InvalidParameter),
        };
        let device = self.devices.iter().find(|d| d.handle == handle).ok_or(UsbError::NotFound)?;
        if device.claimed != Some(pid) {
            return Err(UsbError::NotClaimed);
        }
        if length > buffer.len() {
            return Err(UsbError::InvalidParameter);
        }
        if let Some(data) = buffer.subrange(0, length) {
            xous::flush_cache(data, xous::CacheOperation::CleanAndInvalidate).ok();
        }
        self.statistics.transfers += 1;
        Ok((device.address, endpoint))
    }
}

impl DeferredLendMutHandler<BulkIn> for UsbHostServer {
    fn handle(&mut self, msg: DeferredLendMut<BulkIn>, _context: &mut server::ServerContext<Self>) {
        let context = EhciMessageContext::BulkIn(msg);
        match self.prepare_bulk(&context) {
            Ok((address, endpoint)) => {
                if let Err((context, err)) = self.ehci.schedule_bulk_in(address, endpoint, context) {
                    context.respond(Err(err.into()));
                }
            }
            Err(e) => context.respond(Err(e)),
        }
    }

    fn default_response() -> <BulkIn as server::LendMut>::Response { Err(UsbError::Other) }
}

impl DeferredLendMutHandler<BulkOut> for UsbHostServer {
    fn handle(&mut self, msg: DeferredLendMut<BulkOut>, _context: &mut server::ServerContext<Self>) {
        let context = EhciMessageContext::BulkOut(msg);
        match self.prepare_bulk(&context) {
            Ok((address, endpoint)) => {
                if let Err((context, err)) = self.ehci.schedule_bulk_out(address, endpoint, context) {
                    context.respond(Err(err.into()));
                }
            }
            Err(e) => context.respond(Err(e)),
        }
    }

    fn default_response() -> <BulkIn as server::LendMut>::Response { Err(UsbError::Other) }
}

impl server::BlockingArchiveAsyncHandler<ControlTransfer> for UsbHostServer {
    fn handle(
        &mut self,
        request: server::ArchiveRequest<ControlTransfer>,
        _context: &mut server::ServerContext<Self>,
    ) {
        let server::ArchiveRequest { message, response } = request;
        let Some(device) = self.devices.iter().find(|d| d.handle == message.handle) else {
            response.respond(Err(UsbError::NotFound)).ok();
            return;
        };
        if !device.claimed.map(|pid| pid == response.pid()).unwrap_or(false) {
            response.respond(Err(UsbError::NotClaimed)).ok();
            return;
        }

        let device_to_host = message.request_type & 0x80 != 0;
        let unused_field_set = if device_to_host { !message.data.is_empty() } else { message.read_len != 0 };
        if unused_field_set {
            response.respond(Err(UsbError::InvalidParameter)).ok();
            return;
        }
        let length = if device_to_host { message.read_len } else { message.data.len() };
        if length > ehci::MAX_CONTROL_DATA_LEN {
            response.respond(Err(UsbError::DataTooLarge)).ok();
            return;
        }
        let length = length as u16;
        let request = ehci::ControlRequest {
            typ: message.request_type,
            request: message.request,
            value: message.value,
            index: message.index,
            length,
        };

        self.statistics.transfers += 1;

        let address = device.address;
        let data = if device_to_host { vec![0; length as usize] } else { Vec::new() };
        let context = EhciMessageContext::Control(ControlContext { response, data });
        let scheduled = self.ehci.schedule_control(address, request, &message.data, context);
        if let Err((ctx, err)) = scheduled {
            ctx.respond(Err(err.into()));
        }
    }

    fn default_response() -> <ControlTransfer as server::BlockingArchive>::Response { Err(UsbError::Other) }
}

struct EhciEventHandler<'a> {
    next_device_handle: &'a mut usize,
    event_subscribers: &'a mut Vec<server::ArchiveEventSubscriber<UsbEvent>>,
    devices: &'a mut Vec<Device>,
}

impl server::ScalarHandler<DoWork> for UsbHostServer {
    fn handle(&mut self, _msg: DoWork, sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        if sender != xous::current_pid().unwrap()
            && Some(sender) != self.work_callback.as_ref().map(|tt| tt.pid())
        {
            log::error!("DoWork received from the outside: PID={}", sender);
            return;
        }
        self.work();
    }
}

impl<'a> ehci::EventHandler<EhciMessageContext> for EhciEventHandler<'a> {
    fn device_connected(
        &mut self,
        _controller: &mut ehci::Controller<EhciMessageContext>,
        address: u8,
        descriptors: ehci::descriptors::DescriptorSet,
    ) {
        log::info!("Device connected and configured: {descriptors:?}");
        let handle = *self.next_device_handle;
        *self.next_device_handle += 1;
        self.devices.push(Device { handle, claimed: None, address, descriptors: descriptors.clone() });
        self.send_event(&UsbEvent::Connect { handle, descriptors });
    }

    fn device_disconnected(&mut self, _controller: &mut ehci::Controller<EhciMessageContext>, address: u8) {
        log::info!("Device disconnected");
        if let Some(index) = self.devices.iter().position(|d| d.address == address) {
            self.send_event(&UsbEvent::Disconnect { handle: self.devices[index].handle });
            self.devices.remove(index);
        } else {
            log::debug!("No device entry found for address {address} (may have been rejected)");
        }
    }

    fn transfer_result(
        &mut self,
        _controller: &mut ehci::Controller<EhciMessageContext>,
        result: Result<usize, ehci::EhciError>,
        context: EhciMessageContext,
    ) {
        context.respond(result.map_err(From::from));
    }
}

impl<'a> EhciEventHandler<'a> {
    fn send_event(&mut self, event: &UsbEvent) {
        // Send event to each subscriber, keep only those who were actually willing to receive it
        // (e.g. weren't stopped, disconnected, etc.)
        self.event_subscribers.retain(|subscriber| subscriber.send(event).is_ok())
    }
}

impl ArchiveEventSubscriptionHandler<Subscribe> for UsbHostServer {
    fn handle(
        &mut self,
        _msg: Subscribe,
        subscriber: server::ArchiveEventSubscriber<UsbEvent>,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        for device in &self.devices {
            subscriber
                .send(&UsbEvent::Connect { handle: device.handle, descriptors: device.descriptors.clone() })
                .ok();
        }
        self.event_subscribers.push(subscriber);
        Ok(())
    }
}

impl BlockingScalarHandler<Claim> for UsbHostServer {
    fn handle(
        &mut self,
        msg: Claim,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <Claim as server::BlockingScalar>::Response {
        let device = self.devices.iter_mut().find(|d| d.handle == msg.0).ok_or(UsbError::NotFound)?;
        device.claimed = Some(sender);
        Ok(())
    }
}

impl BlockingScalarHandler<OpenEndpoint> for UsbHostServer {
    fn handle(
        &mut self,
        msg: OpenEndpoint,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <OpenEndpoint as server::BlockingScalar>::Response {
        let device = self.devices.iter_mut().find(|d| d.handle == msg.handle).ok_or(UsbError::NotFound)?;
        if device.claimed != Some(sender) {
            return Err(UsbError::NotClaimed);
        }
        self.ehci.open_endpoint(device.address, msg.endpoint, msg.max_packet_length, msg.direction)?;
        Ok(())
    }
}

impl ScalarHandler<SetEnabled> for UsbHostServer {
    fn handle(&mut self, msg: SetEnabled, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        if msg.0 {
            if let Err(e) = self.enable() {
                log::warn!("Error during ehci host enable: {e:?}");
            }
        } else if let Err(e) = self.disable() {
            log::warn!("Error during ehci host disable: {e:?}");
        }
    }
}

impl ScalarHandler<ResetHostController> for UsbHostServer {
    fn handle(
        &mut self,
        _msg: ResetHostController,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if !self.enabled {
            return;
        }
        if let Err(e) = self.disable() {
            log::warn!("Error disabling the ehci host for a reset: {e:?}");
        }
        if let Err(e) = self.enable() {
            log::warn!("Error re-enabling the ehci host after a reset: {e:?}");
        }
    }
}

impl server::BlockingScalarHandler<IsConnected> for UsbHostServer {
    fn handle(
        &mut self,
        _msg: IsConnected,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> bool {
        !self.devices.is_empty()
    }
}

impl server::ScalarHandler<SubscriberDisconnected> for UsbHostServer {
    fn handle(
        &mut self,
        msg: SubscriberDisconnected,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.event_subscribers.retain(|s| s.cid() != msg.0);
    }
}

impl Drop for UsbHostServer {
    fn drop(&mut self) {
        // TODO: halt EHCI
        xous::unmap_memory(self.ehci_memory).ok();
        xous::unmap_memory(self.qh_backing).ok();
        xous::unmap_memory(self.qtd_backing).ok();
    }
}

fn virt_to_phys(p: *const u8) -> usize { xous::virt_to_phys(p as usize).unwrap() }

fn ehci_irq_handler(_irq_no: usize, arg: *mut usize) {
    let context = unsafe { &*(arg as *const InterruptContext) };
    context.ehci.acknowledge_interrupts();
    context.conn.send_scalar_nowait(DoWork).ok();
}
