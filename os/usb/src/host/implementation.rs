// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::{Duration, Instant};

use server::{
    ArchiveEventSubscriptionHandler, BlockingScalarHandler, DeferredLendMut, DeferredLendMutHandler,
    MessageId as _, ScalarHandler,
};
use usb::{host::messages::*, UsbError};
use utralib::{HW_UHPHS_EHCI_BASE, HW_UHPHS_EHCI_MEM};
use xous::{arch::irq::IrqNumber, keyos::PAGE_SIZE, MemoryRange};
use xous_ticktimer::{Ticktimer, TicktimerCallback};

use super::messages::*;
use crate::{PowerManagerApi, PowerManagerExtApi};

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
    power_manager_ext: PowerManagerExtApi,
    work_callback: Option<TicktimerCallback>,
    enabled: bool,
    otg_watch_since: Option<Instant>,
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
        }
    }
}

impl EhciMessageContext {
    fn respond(self, r: Result<usize, UsbError>) {
        match self {
            EhciMessageContext::BulkIn(mut s) => s.set_response(r),
            EhciMessageContext::BulkOut(mut s) => s.set_response(r),
        }
    }
}

#[derive(Debug)]
struct Device {
    handle: usize,
    claimed: Option<xous::PID>,
    address: u8,
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
    const DEVICE_CONNECTION_TIMEOUT: Duration = Duration::from_millis(300);
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
            power_manager_ext: Default::default(),
            work_callback: None,
            enabled: false,
            otg_watch_since: None,
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
                power_manager_ext: &self.power_manager_ext,
                otg_watch_since: &mut self.otg_watch_since,
            },
        ) {
            log::warn!("Error during ehci work(): {e:?}");
        }
        if self.otg_watch_since.is_some_and(|since| since.elapsed() >= Self::DEVICE_CONNECTION_TIMEOUT) {
            log::info!("No device after host-mode activation, backing off OTG");
            self.power_manager_ext.set_otg_priority(power_manager::OtgPriority::Never).ok();
            self.otg_watch_since = None;
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
            power_manager_ext: &self.power_manager_ext,
            otg_watch_since: &mut self.otg_watch_since,
        })?;
        self.power_manager.disable_peripheral(atsama5d27::pmc::PeripheralId::Uhphs)?;
        self.enabled = false;
        Ok(())
    }
}

impl DeferredLendMutHandler<BulkIn> for UsbHostServer {
    fn handle(&mut self, mut msg: DeferredLendMut<BulkIn>, _context: &mut server::ServerContext<Self>) {
        let Some(device) = self.devices.iter().find(|d| d.handle == msg.body().handle) else {
            msg.set_response(Err(UsbError::NotFound));
            return;
        };
        if !device.claimed.map(|pid| pid == msg.pid()).unwrap_or(false) {
            msg.set_response(Err(UsbError::NotClaimed));
            return;
        }
        if msg.body().length > 0 {
            let Some(buffer) = msg.body().buffer.subrange(0, msg.body().length) else {
                msg.set_response(Err(UsbError::InvalidParameter));
                return;
            };
            xous::flush_cache(buffer, xous::CacheOperation::Invalidate).ok();
        }

        self.statistics.transfers += 1;

        if let Err((ctx, err)) =
            self.ehci.schedule_bulk_in(device.address, msg.body().endpoint, EhciMessageContext::BulkIn(msg))
        {
            ctx.respond(Err(err.into()));
        }
    }

    fn default_response() -> <BulkIn as server::LendMut>::Response { Err(UsbError::Other) }
}

impl DeferredLendMutHandler<BulkOut> for UsbHostServer {
    fn handle(&mut self, mut msg: DeferredLendMut<BulkOut>, _context: &mut server::ServerContext<Self>) {
        let Some(device) = self.devices.iter().find(|d| d.handle == msg.body().handle) else {
            msg.set_response(Err(UsbError::NotFound));
            return;
        };
        if !device.claimed.map(|pid| pid == msg.pid()).unwrap_or(false) {
            msg.set_response(Err(UsbError::NotClaimed));
            return;
        }
        if msg.body().length > 0 {
            let Some(buffer) = msg.body().buffer.subrange(0, msg.body().length) else {
                msg.set_response(Err(UsbError::InvalidParameter));
                return;
            };
            xous::flush_cache(buffer, xous::CacheOperation::Clean).ok();
        }

        self.statistics.transfers += 1;

        if let Err((ctx, err)) =
            self.ehci.schedule_bulk_out(device.address, msg.body().endpoint, EhciMessageContext::BulkOut(msg))
        {
            ctx.respond(Err(err.into()));
        }
    }

    fn default_response() -> <BulkIn as server::LendMut>::Response { Err(UsbError::Other) }
}

struct EhciEventHandler<'a> {
    next_device_handle: &'a mut usize,
    event_subscribers: &'a mut Vec<server::ArchiveEventSubscriber<UsbEvent>>,
    devices: &'a mut Vec<Device>,
    power_manager_ext: &'a PowerManagerExtApi,
    otg_watch_since: &'a mut Option<Instant>,
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
        // Reject self-powered devices; only accept bus-powered devices
        const SELF_POWERED_BIT: u8 = 1 << 6; // D6: self-powered

        let is_self_powered =
            descriptors.configurations().any(|config| (config.attributes & SELF_POWERED_BIT) != 0);

        if is_self_powered {
            log::info!("Device is self-powered, disabling OTG and not connecting: {descriptors:?}");
            self.power_manager_ext.set_otg_priority(power_manager::OtgPriority::Never).ok();
            return;
        }

        log::info!("Device connected and configured: {descriptors:?}");
        let handle = *self.next_device_handle;
        *self.next_device_handle += 1;
        self.devices.push(Device { handle, claimed: None, address });
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

    fn port_connected(&mut self, _controller: &mut ehci::Controller<EhciMessageContext>) {
        *self.otg_watch_since = None;
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

impl ScalarHandler<HostOtgMode> for UsbHostServer {
    fn handle(&mut self, msg: HostOtgMode, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        if msg.0 {
            self.otg_watch_since = Some(Instant::now());
        } else {
            self.otg_watch_since = None;
        }
    }
}

impl server::BlockingScalarHandler<IsEnabled> for UsbHostServer {
    fn handle(
        &mut self,
        _msg: IsEnabled,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> bool {
        self.enabled
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
