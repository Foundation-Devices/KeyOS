// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(keyos)]

use atsama5d27::{
    dma::{
        DescriptorControl, DmaChannel, DmaPeripheralTransferConfig, DmaTransferDirection,
        ExecuteTransferLlParams, View0Descriptor, Xdmac, XdmacChannel, DMA_CHANNELS, MASK_DISABLE_INTERRUPT,
        MASK_LINKED_LIST_INTERRUPT,
    },
    pmc::PeripheralId,
};
use dma::{error::DmaError, messages::*};
use server::{
    BlockingArchiveHandler, BlockingScalarAsyncHandler, BlockingScalarHandler, BlockingScalarRequest,
    CheckedPermissions, MessageAllowed, MessageId as _, ScalarEventSubscriber,
    ScalarEventSubscriptionHandler, ScalarHandler, Server,
};
use utralib::{HW_CSR1_MEM, HW_CSR3_MEM, HW_CSR3_MEM_LEN, HW_XDMAC0_BASE};
use xous::{arch::irq::IrqNumber, keyos::PAGE_SIZE, MemoryRange};

power_manager::use_api!();

#[derive(Debug, server::Message)]
pub struct TransferCompleteMsg(pub usize);

#[derive(Debug, server::Message)]
pub(crate) struct SubscriberDisconnected(pub xous::CID);

#[derive(server::Server)]
#[name = "os/dma"]
struct DmaServer {
    xdmac_mem: MemoryRange,
    channels: [Channel; DMA_CHANNELS],
    power_manager: PowerManagerApi,
    enabled: bool,
}

struct Channel {
    channel: XdmacChannel,
    owner: Option<xous::PID>,
    is_running: bool,
    last_transferred: usize,
    waiters: Vec<BlockingScalarRequest<WaitTransferMsg>>,
    config: DmaPeripheralTransferConfig,
    peripheral_phys_addr: usize,
    descriptors: MemoryRange,
    descriptors_phys_addr: usize,
    subscriber: Option<ScalarEventSubscriber<TransferComplete>>,
}

#[derive(Default, Clone)]
struct InterruptConnection;

impl CheckedPermissions for InterruptConnection {
    const NAME: &str = "os/dma";
}

impl MessageAllowed<TransferCompleteMsg> for InterruptConnection {}

struct InterruptContext {
    conn: server::CheckedConn<InterruptConnection>,
    xdmac: Xdmac,
}

impl Server for DmaServer {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        let int_ctx = Box::into_raw(Box::new(InterruptContext {
            conn: Default::default(),
            xdmac: Xdmac::with_alt_base_addr(self.xdmac_mem.as_ptr() as usize),
        }));
        xous::claim_interrupt(IrqNumber::Xdmac0, dma_irq_handler, int_ctx as *mut usize)
            .expect("Could not claim Xdmac0 interrupt");
        self.power_manager.enable_peripheral(PeripheralId::Xdmac0).expect("Could not enable Xdmac0 clock");
        for channel in &mut self.channels {
            channel.channel.set_interrupt(true);
            channel.channel.set_li_interrupt(true);
            channel.channel.set_di_interrupt(true);
        }
        self.power_manager.disable_peripheral(PeripheralId::Xdmac0).expect("Could not disable Xdmac0 clock");
        xous::register_system_event_handler(
            xous::SystemEvent::Disconnected,
            context.sid(),
            SubscriberDisconnected::ID,
        )
        .expect("Could not register Disconnected event handler");
    }
}

impl DmaServer {
    fn new() -> Self {
        let xdmac_mem = xous::map_memory(
            xous::MemoryAddress::new(HW_XDMAC0_BASE),
            None,
            0x2000,
            xous::MemoryFlags::W | xous::MemoryFlags::DEV,
        )
        .expect("Could not map Xdmac0");

        let xdmac = Xdmac::with_alt_base_addr(xdmac_mem.as_ptr() as usize);
        let channels = core::array::from_fn(|i| {
            let (descriptors, descriptors_phys_addr) = Self::allocate_descriptor_storage(0x1000).unwrap();
            Channel {
                channel: xdmac.channel(DmaChannel::from_usize(i).unwrap()),
                owner: None,
                peripheral_phys_addr: 0,
                is_running: false,
                last_transferred: 0,
                waiters: Default::default(),
                config: Default::default(),
                descriptors,
                descriptors_phys_addr,
                subscriber: None,
            }
        });
        Self { xdmac_mem, channels, power_manager: PowerManagerApi::default(), enabled: false }
    }

    fn allocate_descriptor_storage(size: usize) -> Result<(MemoryRange, usize), xous::Error> {
        let result = xous::map_memory(
            None,
            None,
            size,
            xous::MemoryFlags::W | xous::MemoryFlags::POPULATE | xous::MemoryFlags::PLAINTEXT,
        )?;
        Ok((result, xous::virt_to_phys(result.as_ptr() as usize)?))
    }

    fn enable_hw(&mut self) {
        if self.enabled {
            return;
        }
        self.power_manager.enable_peripheral(PeripheralId::Xdmac0).expect("Could not enable XDMAC clock");
        self.enabled = true;
    }

    fn disable_hw_if_not_needed(&mut self) {
        if !self.enabled || self.channels.iter().any(|c| c.is_running) {
            return;
        }
        self.power_manager.disable_peripheral(PeripheralId::Xdmac0).expect("Could not disable XDMAC clock");
        self.enabled = false;
    }
}

impl Channel {
    fn transferred_bytes(&self) -> usize {
        let descs = (self.channel.last_descriptor() as usize - self.descriptors_phys_addr)
            / core::mem::size_of::<View0Descriptor>();
        let transferred_data_units = self
            .descriptors
            .as_slice::<View0Descriptor>()
            .iter()
            .take(descs)
            .map(|c| c.control.ublen())
            .sum::<u32>()
            - self.channel.remaining_data_size();
        transferred_data_units as usize * self.config.data_width.byte_len()
    }

    fn clear(&mut self) {
        self.owner = None;
        self.subscriber = None;
        if self.is_running {
            self.channel.disable();
            self.is_running = false;
        }
        self.last_transferred = 0;
        self.waiters.clear();
    }
}

impl BlockingArchiveHandler<PeripheralTransferMsg> for DmaServer {
    fn handle(
        &mut self,
        msg: PeripheralTransferMsg,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, DmaError> {
        let free_channel_index =
            self.channels.iter().position(|c| c.owner.is_none()).ok_or(DmaError::NoFreeChannels)?;
        let phys_addr = xous::virt_to_phys_pid(sender, msg.address)?;
        if !(HW_CSR1_MEM..=HW_CSR3_MEM + HW_CSR3_MEM_LEN).contains(&phys_addr) {
            return Err(DmaError::InvalidAddress);
        }
        self.enable_hw();
        self.channels[free_channel_index].channel.configure_peripheral_transfer(msg.config.clone());
        log::trace!("Transfer on CH{free_channel_index} set up at {phys_addr:08x}: {:?}", msg.config);
        self.channels[free_channel_index].owner = Some(sender);
        self.channels[free_channel_index].peripheral_phys_addr = phys_addr;
        self.channels[free_channel_index].config = msg.config;
        self.disable_hw_if_not_needed();
        Ok(free_channel_index)
    }
}

impl BlockingScalarHandler<ExecuteTransferMsg> for DmaServer {
    fn handle(
        &mut self,
        msg: ExecuteTransferMsg,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), DmaError> {
        let Some(channel) = self.channels.get_mut(msg.transfer_id) else {
            return Err(DmaError::InvalidParameter);
        };
        if channel.owner != Some(sender) {
            return Err(DmaError::InvalidParameter);
        }
        if channel.is_running {
            return Err(DmaError::AlreadyRunning);
        }
        if msg.buf.len() % channel.config.data_width.byte_len() != 0
            || msg.buf.as_ptr() as usize % channel.config.data_width.byte_len() != 0
        {
            return Err(DmaError::InvalidAlignment);
        }

        let buf_ptr = msg.buf.as_ptr() as usize;
        let buf_end = buf_ptr + msg.buf.len();
        let aligned_start = buf_ptr & !(PAGE_SIZE - 1);
        let aligned_end = buf_end.next_multiple_of(PAGE_SIZE);
        let bytes_per_data = channel.config.data_width.byte_len();

        let pages = (aligned_end - aligned_start) / PAGE_SIZE;
        let descriptor_storage_size =
            (pages * core::mem::size_of::<View0Descriptor>()).next_multiple_of(PAGE_SIZE);
        if channel.descriptors.len() < descriptor_storage_size {
            let (descriptors, descriptors_phys_addr) =
                Self::allocate_descriptor_storage(descriptor_storage_size)?;
            xous::unmap_memory(channel.descriptors).unwrap();
            channel.descriptors = descriptors;
            channel.descriptors_phys_addr = descriptors_phys_addr;
        }

        for page in 0..pages {
            let start = usize::max(buf_ptr, aligned_start + page * PAGE_SIZE);
            let end = usize::min(buf_end, aligned_start + (page + 1) * PAGE_SIZE);

            let desc = &mut channel.descriptors.as_slice_mut::<View0Descriptor>()[page];
            desc.control = DescriptorControl(0);
            desc.control.set_ublen(((end - start) / bytes_per_data) as u32);
            if page < pages - 1 {
                match channel.config.direction {
                    DmaTransferDirection::PeripheralToMemory => {
                        desc.control.set_next_destination_update(true)
                    }
                    DmaTransferDirection::MemoryToPeripheral => desc.control.set_next_source_update(true),
                }
                desc.control.set_next_descriptor_enable(true);
            }
            desc.next_descriptor =
                (channel.descriptors_phys_addr + (page + 1) * core::mem::size_of::<View0Descriptor>()) as u32;
            desc.address = xous::virt_to_phys_pid(msg.pid, start)? as u32;
        }
        xous::flush_cache(
            channel.descriptors.subrange(0, core::mem::size_of::<View0Descriptor>() * pages).unwrap(),
            xous::CacheOperation::Clean,
        )
        .ok();

        log::trace!("Executing transfer on CH{}, length={}", msg.transfer_id, msg.buf.len());
        channel.last_transferred = 0;
        channel.is_running = true;

        let mut execute_params = ExecuteTransferLlParams {
            first_descriptor: channel.descriptors_phys_addr as u32,
            first_descriptor_type: 0,
            ..Default::default()
        };

        match channel.config.direction {
            DmaTransferDirection::PeripheralToMemory => {
                execute_params.src = channel.peripheral_phys_addr as u32;
                execute_params.dst_from_descriptor = true;
            }
            DmaTransferDirection::MemoryToPeripheral => {
                execute_params.src_from_descriptor = true;
                execute_params.dst = channel.peripheral_phys_addr as u32;
            }
        }

        self.enable_hw();
        self.channels[msg.transfer_id].channel.execute_transfer_ll(execute_params);
        Ok(())
    }
}

impl BlockingScalarAsyncHandler<WaitTransferMsg> for DmaServer {
    fn handle(
        &mut self,
        request: BlockingScalarRequest<WaitTransferMsg>,
        _context: &mut server::ServerContext<Self>,
    ) {
        let ch = request.message.0;
        let Some(channel) = self.channels.get_mut(ch) else {
            request.response.respond(Err(DmaError::InvalidParameter)).ok();
            return;
        };
        if channel.owner != Some(request.response.pid()) {
            request.response.respond(Err(DmaError::InvalidParameter)).ok();
            return;
        }
        if channel.is_running {
            log::trace!("Waiting on CH{ch}");
            channel.waiters.push(request);
        } else {
            log::trace!("Not waiting on CH{ch}");
            request.response.respond(Ok(channel.last_transferred)).ok();
        }
    }

    fn default_response() -> <WaitTransferMsg as server::BlockingScalar>::Response {
        Err(DmaError::UnknownError)
    }
}

impl ScalarHandler<TransferCompleteMsg> for DmaServer {
    fn handle(
        &mut self,
        msg: TransferCompleteMsg,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if sender != xous::current_pid().unwrap() {
            return;
        }
        log::trace!("Transfer complete on CH{}", msg.0);
        let channel = &mut self.channels[msg.0];
        channel.last_transferred = channel.transferred_bytes();
        channel.is_running = false;
        for waiter in core::mem::take(&mut channel.waiters) {
            log::trace!("Returning waiter: {waiter:?}");
            waiter.response.respond(Ok(channel.last_transferred)).ok();
        }
        if let Some(sub) = &channel.subscriber {
            let event =
                TransferComplete { transfer_id: msg.0 as u32, bytes: channel.last_transferred as u32 };
            if let Err(e) = sub.send(&event) {
                log::warn!("TransferComplete on CH{} send failed: {e:?}", msg.0);
            }
        }
        self.disable_hw_if_not_needed();
    }
}

impl ScalarEventSubscriptionHandler<SubscribeTransferComplete> for DmaServer {
    fn handle(
        &mut self,
        msg: SubscribeTransferComplete,
        subscriber: ScalarEventSubscriber<TransferComplete>,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), DmaError> {
        let SubscribeTransferComplete(ch) = msg;
        let channel = self.channels.get_mut(ch).ok_or(DmaError::InvalidParameter)?;
        if channel.owner != Some(subscriber.pid()) {
            return Err(DmaError::InvalidParameter);
        }
        channel.subscriber = Some(subscriber);
        Ok(())
    }
}

impl ScalarHandler<SubscriberDisconnected> for DmaServer {
    fn handle(
        &mut self,
        _msg: SubscriberDisconnected,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        for channel in &mut self.channels {
            if channel.owner == Some(sender) {
                channel.clear();
            }
        }
        self.disable_hw_if_not_needed();
    }
}

impl ScalarHandler<StopTransferMsg> for DmaServer {
    fn handle(
        &mut self,
        msg: StopTransferMsg,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let Some(channel) = self.channels.get_mut(msg.0) else {
            return;
        };
        if channel.owner != Some(sender) || !channel.is_running {
            return;
        }
        log::trace!("Stopping transfer on CH{}", msg.0);
        // This also flushes the FIFO for P2M transfers.
        channel.channel.disable();
    }
}

impl ScalarHandler<DropTransferMsg> for DmaServer {
    fn handle(
        &mut self,
        msg: DropTransferMsg,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        log::trace!("Dropping channel CH{}", msg.0);
        let Some(channel) = self.channels.get_mut(msg.0) else {
            return;
        };
        if channel.owner != Some(sender) {
            return;
        }
        channel.clear();
        self.disable_hw_if_not_needed();
    }
}

impl BlockingScalarHandler<FlushTransferMsg> for DmaServer {
    fn handle(
        &mut self,
        msg: FlushTransferMsg,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, DmaError> {
        let Some(channel) = self.channels.get_mut(msg.0) else {
            return Err(DmaError::InvalidParameter);
        };
        if channel.owner != Some(sender) {
            return Err(DmaError::InvalidParameter);
        }
        log::trace!("Flushing transfer on CH{}", msg.0);
        if channel.is_running {
            channel.channel.software_flush();
            Ok(channel.transferred_bytes())
        } else {
            Ok(channel.last_transferred)
        }
    }
}

pub fn start_server() { server::listen(DmaServer::new()) }

fn dma_irq_handler(_irq_no: usize, arg: *mut usize) {
    let context = unsafe { &*(arg as *const InterruptContext) };
    for i in 0..DMA_CHANNELS {
        let channel = context.xdmac.channel(DmaChannel::from_usize(i).unwrap());
        let interrupt_status = channel.interrupt_status();
        if interrupt_status & (MASK_LINKED_LIST_INTERRUPT | MASK_DISABLE_INTERRUPT) != 0 {
            context.conn.send_scalar_nowait(TransferCompleteMsg(i)).ok();
        }
    }
}
