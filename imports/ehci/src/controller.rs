extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use log::{debug, info, warn};

use crate::descriptors::{DescriptorSet, DescriptorType};
use crate::pool::PoolElementHandle;
use crate::queue::EndpointDirection;
use crate::registers::{CapabilityRegisters, OperationalRegisters};
use crate::transfer::{ControlRequest, Transfer};
use crate::util::VolatileCellHelper;
use crate::{error::EhciError, queue::AsyncQueue};
use crate::{BufferPool, QtdPool, QueueHeadPool, TransferContext};

/// Main USB Controller struct. Instantiate to access all functionality.
pub struct Controller<CT> {
    opregs: *mut OperationalRegisters,
    async_queue: Box<AsyncQueue<BuiltinTransfer<CT>>>,
    ports: Vec<PortStatus>,
    qtd_pool: QtdPool,
    buffer_pool: BufferPool,
    virt_to_phys: fn(*const u8) -> usize,
    enabled: bool,
}

/// User-defined callbacks for the events that happen during USB processing.
pub trait EventHandler<CT> {
    /// A device has been connected, set up, and ready to use.
    fn device_connected(&mut self, controller: &mut Controller<CT>, address: u8, descriptors: DescriptorSet);

    /// A device was disconnected or deconfigured (including reset)
    fn device_disconnected(&mut self, controller: &mut Controller<CT>, address: u8);

    /// A transfer scheduled by the [`Controller::schedule_transfer`] method has
    /// either succeeded or not.
    ///
    /// This event is always sent exactly once for each schedule_transfer call, even
    /// if the device is disconnected.
    fn transfer_result(
        &mut self,
        controller: &mut Controller<CT>,
        result: Result<usize, EhciError>,
        context: CT,
    );

    /// Fires when a port's `connected` bit goes high, before reset and
    /// enumeration. Indicates a peripheral has electrically attached.
    fn port_connected(&mut self, _controller: &mut Controller<CT>) {}
}

enum BuiltinTransfer<CT> {
    SetAddress {
        port_number: usize,
        setup_buffer: PoolElementHandle<[u8; 0x40]>,
    },
    SetConfiguration {
        port_number: usize,
        setup_buffer: PoolElementHandle<[u8; 0x40]>,
    },
    GetDescriptor {
        port_number: usize,
        descriptor_type: u8,
        setup_buffer: PoolElementHandle<[u8; 0x40]>,
        data_buffer: PoolElementHandle<[u8; 0x40]>,
    },
    User {
        context: CT,
    },
}

impl<CT> BuiltinTransfer<CT> {
    pub(crate) fn into_user(self) -> CT {
        match self {
            BuiltinTransfer::User { context } => context,
            _ => panic!(),
        }
    }
}

impl<CT: TransferContext> TransferContext for BuiltinTransfer<CT> {
    fn data_buffer(&mut self) -> &mut [u8] {
        match self {
            BuiltinTransfer::SetAddress { .. } => &mut [],
            BuiltinTransfer::SetConfiguration { .. } => &mut [],
            BuiltinTransfer::GetDescriptor { data_buffer, .. } => &mut **data_buffer,
            BuiltinTransfer::User { context } => context.data_buffer(),
        }
    }

    fn setup_buffer(&self) -> &[u8] {
        match self {
            BuiltinTransfer::SetAddress { setup_buffer, .. } => {
                &setup_buffer[..core::mem::size_of::<ControlRequest>()]
            }
            BuiltinTransfer::SetConfiguration { setup_buffer, .. } => {
                &setup_buffer[..core::mem::size_of::<ControlRequest>()]
            }
            BuiltinTransfer::GetDescriptor { setup_buffer, .. } => {
                &setup_buffer[..core::mem::size_of::<ControlRequest>()]
            }
            BuiltinTransfer::User { context } => context.setup_buffer(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum PortStatus {
    #[default]
    Disconnected,
    Resetting {
        since: usize,
    },
    WaitingForEnabled {
        since: usize,
    },
    WaitingForSetAddress {
        since: usize,
    },
    WaitingForDeviceDescriptor {
        since: usize,
    },
    WaitingForConfigDescriptor {
        descriptor: Vec<u8>,
        since: usize,
    },
    WaitingForSetConfiguration {
        descriptor: DescriptorSet,
        since: usize,
    },
    Configured {
        descriptor: DescriptorSet,
    },
}

impl<CT: TransferContext> Controller<CT> {
    /// Initialize internal data. The controller has to be powered on by this point.
    /// It is assumed that the controller was fully reset before calling this function.
    /// Both pools need to be in memory that is marked as uncached and ordered, as it is
    /// used by the host controller and the CPU in tandem.
    ///
    /// `qh_pool` is used to initialize queue heads. One QH is used per open endpoint.
    /// There is one open endpoint by default, one per device, and more for each manually
    /// opened one.
    ///
    /// `qtd_pool` is used for in-flight transfers. 3 are needed for each control transfer,
    /// and 1 for all other types.
    ///
    /// `uncached_buffer` is a buffer in uncached memory used for internal control transfer
    /// data.
    pub fn new(
        base: usize,
        qh_pool: QueueHeadPool,
        qtd_pool: QtdPool,
        buffer_pool: BufferPool,
        virt_to_phys: fn(*const u8) -> usize,
    ) -> Result<Self, EhciError> {
        let caps = base as *const CapabilityRegisters;
        let caps_len = unsafe { (*caps).caplength.get() };
        if caps_len < 12 {
            log::error!("EHCI caps length was invalid");
            return Err(EhciError::InvalidCapsLen);
        }
        let opregs = (base + caps_len as usize) as *mut OperationalRegisters;
        let port_number = unsafe { (*caps).structural_params.get().n_ports() } as usize;
        let async_queue = Box::new(AsyncQueue::new(qh_pool)?);

        Ok(Self {
            opregs,
            ports: vec![Default::default(); port_number],
            async_queue,
            qtd_pool,
            buffer_pool,
            virt_to_phys,
            enabled: false,
        })
    }

    /// Should be called on every USB interrupt, and periodically at least every 100ms
    /// tick_count should be a monotonic clock in ms, used for coarse delays
    pub fn work(&mut self, tick_count: usize, handler: &mut impl EventHandler<CT>) -> Result<(), EhciError> {
        if !self.enabled {
            return Err(EhciError::ControllerDisabled);
        }
        self.update_ports(tick_count, handler)?;
        let finished_transfers = self.async_queue.work();
        for ft in finished_transfers {
            // We throw away error results here, because we want to handle all transactions
            // even if one fails. If there are any errors in the "handle" functions, we are
            // going to time out as if the device didn't answer anyway.
            match ft.context {
                BuiltinTransfer::SetAddress { port_number, .. } => {
                    if let Err(e) = self.handle_set_address_result(port_number, tick_count, ft.result.is_ok())
                    {
                        warn!("Error processing set address on {port_number}: {e:?}");
                    }
                }
                BuiltinTransfer::GetDescriptor { port_number, descriptor_type, data_buffer, .. } => {
                    if let Err(e) = self.handle_get_descriptor_result(
                        port_number,
                        descriptor_type,
                        tick_count,
                        ft.result,
                        data_buffer,
                    ) {
                        warn!(
                            "Error processing descriptor result ({descriptor_type:?}) on {port_number}: {e:?}"
                        );
                    }
                }
                BuiltinTransfer::SetConfiguration { port_number, .. } => {
                    self.handle_set_configuration_result(port_number, ft.result.is_ok(), handler);
                }
                BuiltinTransfer::User { context } => {
                    handler.transfer_result(self, ft.result, context);
                }
            };
        }

        Ok(())
    }

    /// Schedule a transfer. Result only indicates that the scheduling itself
    /// was successful, the transfer result will be returned via the EventHandler
    ///
    /// The address has to be a connected device, and the transfer must point to
    /// an endpoint that is already opened. (See [`Controller::open_endpoint`])
    fn schedule_transfer(
        &mut self,
        address: u8,
        transfer: Transfer<BuiltinTransfer<CT>>,
    ) -> Result<(), (Transfer<BuiltinTransfer<CT>>, EhciError)> {
        let port_number = match self.address_to_port(address) {
            Ok(n) => n,
            Err(e) => return Err((transfer, e)),
        };
        if !self.ports[port_number].is_configured() {
            return Err((transfer, EhciError::Disconnected));
        }
        self.async_queue.schedule_transfer(address, transfer)
    }

    /// Schedule a Bulk In transfer. Result only indicates that the scheduling itself
    /// was successful, the transfer result will be returned via the EventHandler
    ///
    /// The address has to be a connected device, and the transfer must point to
    /// an endpoint that is already opened. (See [`Controller::open_endpoint`])
    ///
    /// The buffer in the context should either be uncached, or invalidated before
    /// calling this function.
    pub fn schedule_bulk_in(
        &mut self,
        address: u8,
        endpoint: u8,
        context: CT,
    ) -> Result<(), (CT, EhciError)> {
        if !self.enabled {
            return Err((context, EhciError::ControllerDisabled));
        }
        let transfer = Transfer::new_bulk_in(
            &mut self.qtd_pool,
            endpoint,
            BuiltinTransfer::User { context },
            self.virt_to_phys,
        )
        .map_err(|(ctx, e)| (ctx.into_user(), e))?;
        self.schedule_transfer(address, transfer)
            .map_err(|(transfer, e)| (transfer.take_context().into_user(), e))
    }

    /// Schedule a Bulk Out transfer. Result only indicates that the scheduling itself
    /// was successful, the transfer result will be returned via the EventHandler
    ///
    /// The address has to be a connected device, and the transfer must point to
    /// an endpoint that is already opened. (See [`Controller::open_endpoint`])
    ///
    /// The buffer in the context should either be uncached, or cleaned before
    /// calling this function.
    pub fn schedule_bulk_out(
        &mut self,
        address: u8,
        endpoint: u8,
        context: CT,
    ) -> Result<(), (CT, EhciError)> {
        if !self.enabled {
            return Err((context, EhciError::ControllerDisabled));
        }
        let transfer = Transfer::new_bulk_out(
            &mut self.qtd_pool,
            endpoint,
            BuiltinTransfer::User { context },
            self.virt_to_phys,
        )
        .map_err(|(ctx, e)| (ctx.into_user(), e))?;
        self.schedule_transfer(address, transfer)
            .map_err(|(transfer, e)| (transfer.take_context().into_user(), e))
    }

    /// Open a specific USB endpoint. Should be called before scheduling any transfers.
    /// Initializes transfer queues.
    pub fn open_endpoint(
        &mut self,
        address: u8,
        endpoint: u8,
        max_packet_length: u16,
        direction: EndpointDirection,
    ) -> Result<(), EhciError> {
        if !self.enabled {
            return Err(EhciError::ControllerDisabled);
        }
        let port_number = self.address_to_port(address)?;
        if !self.ports[port_number].is_configured() {
            return Err(EhciError::Disconnected);
        }
        self.async_queue.open_endpoint(address, endpoint, max_packet_length, direction)
    }

    /// Enable the controller.
    pub fn enable(&mut self) -> Result<(), EhciError> {
        if self.enabled {
            return Ok(());
        }
        unsafe {
            // Halt the controller to make the changes
            (*self.opregs).cmd.change(|o| {
                o.set_run(false);
            });
            while !(*self.opregs).status.get().halted() {}

            // Reset controller
            (*self.opregs).cmd.change(|o| o.set_host_controller_reset(true));
            while (*self.opregs).cmd.get().host_controller_reset() {}

            // Set up basic registers
            (*self.opregs).async_list.set(self.async_queue.head());
            (*self.opregs).cmd.change(|o| {
                o.set_run(true);
                o.set_async_schedule_enable(true);
            });
            // Take the ports over from the OHCI controller
            (*self.opregs).config.set(1);

            for port in &mut (*self.opregs).ports {
                port.change(|o| o.set_port_power(true));
            }
        };
        self.enabled = true;
        Ok(())
    }

    /// Disable the controller. Disconnects devices and returns all unprocessed messages with a failure.
    pub fn disable(&mut self, handler: &mut impl EventHandler<CT>) -> Result<(), EhciError> {
        if !self.enabled {
            return Ok(());
        }
        unsafe {
            (*self.opregs).cmd.change(|o| {
                o.set_run(false);
            });
            while !(*self.opregs).status.get().halted() {}
        }
        self.enabled = false;
        for transfer_result in self.async_queue.flush() {
            if let BuiltinTransfer::User { context } = transfer_result.context {
                handler.transfer_result(self, transfer_result.result, context);
            }
        }
        let mut disconnects = Vec::new();
        for (port_number, port) in self.ports.iter_mut().enumerate() {
            if matches!(port, PortStatus::Configured { .. }) {
                disconnects.push(port_number);
            }
            *port = PortStatus::Disconnected;
        }
        for port_number in disconnects {
            handler.device_disconnected(self, Self::port_to_address(port_number))
        }
        Ok(())
    }

    fn update_ports(
        &mut self,
        tick_count: usize,
        handler: &mut impl EventHandler<CT>,
    ) -> Result<(), EhciError> {
        let mut disconnects = Vec::new();
        let mut connects = Vec::new();
        let mut any_port_connected = false;
        for (port_number, port) in self.ports.iter_mut().enumerate() {
            let mut port_status = unsafe { (*self.opregs).ports[port_number].get() };
            match *port {
                PortStatus::Disconnected => {
                    // Make sure all endpoints of this address is closed, as we are
                    // not configured.
                    self.async_queue.close_endpoint(Self::port_to_address(port_number), None, None);
                    if port_status.connected() {
                        info!("Resetting port {port_number}");
                        port_status.set_reset(true);
                        unsafe { (*self.opregs).ports[port_number].set(port_status) };
                        *port = PortStatus::Resetting { since: tick_count };
                        any_port_connected = true;
                    }
                }
                PortStatus::Resetting { since } => {
                    // XXX: We should check line status, because low speed devices
                    //      need to be handed over, but we don't support those anyway.
                    if tick_count.wrapping_sub(since) >= 300 {
                        debug!("Enabling port {port_number}");
                        port_status.set_reset(false);
                        unsafe { (*self.opregs).ports[port_number].set(port_status) };
                        *port = PortStatus::WaitingForEnabled { since: tick_count };
                    }
                }
                PortStatus::WaitingForEnabled { since } => {
                    if port_status.enabled() {
                        connects.push(port_number);
                    } else if tick_count.wrapping_sub(since) >= 1000 {
                        debug!("Timed out waiting for port {port_number} to become Enabled");
                        debug!("Status reg: {:?}", unsafe { (*self.opregs).status.get() });
                        debug!("Port reg: {port_status:?}");
                        *port = PortStatus::Disconnected;
                    }
                }
                PortStatus::WaitingForSetAddress { since }
                | PortStatus::WaitingForSetConfiguration { since, .. }
                | PortStatus::WaitingForDeviceDescriptor { since, .. }
                | PortStatus::WaitingForConfigDescriptor { since, .. } => {
                    if !port_status.connected() {
                        info!("Port {port_number} disconnected while in state {port:?}");
                        *port = PortStatus::Disconnected;
                    } else if tick_count.wrapping_sub(since) >= 1000 {
                        debug!("Timed out waiting for Set Address result on port {port_number}");
                        *port = PortStatus::Disconnected;
                    }
                }
                PortStatus::Configured { .. } => {
                    if !port_status.connected() {
                        info!("Port {port_number} disconnected");
                        *port = PortStatus::Disconnected;
                        disconnects.push(port_number);
                    }
                }
            }
        }
        for port_number in disconnects {
            handler.device_disconnected(self, Self::port_to_address(port_number))
        }
        for port_number in connects {
            debug!("Setting address of port {port_number}");
            let transfer = self.set_address_transfer(port_number)?;
            self.async_queue.schedule_transfer(0, transfer).map_err(|(_, e)| e)?;
            self.ports[port_number] = PortStatus::WaitingForSetAddress { since: tick_count };
        }
        if any_port_connected {
            handler.port_connected(self);
        }
        Ok(())
    }

    fn handle_set_address_result(
        &mut self,
        port_number: usize,
        tick_count: usize,
        success: bool,
    ) -> Result<(), EhciError> {
        let PortStatus::WaitingForSetAddress { since: _ } = self.ports[port_number] else {
            debug!("Spurious SetAddressResult ({port_number})");
            return Ok(());
        };

        if !success {
            warn!("Set Address unsuccesful on port {port_number}");
            self.ports[port_number] = PortStatus::Disconnected;
            return Err(EhciError::SetupUnsuccessful);
        }

        debug!("Getting device descriptor on port {port_number}");
        self.async_queue.open_endpoint(
            Self::port_to_address(port_number),
            0,
            0x40,
            EndpointDirection::Out,
        )?;
        let transfer = self.get_descriptor_transfer(port_number, DescriptorType::Device as u8)?;
        self.async_queue
            .schedule_transfer(Self::port_to_address(port_number), transfer)
            .map_err(|(_, e)| e)?;
        self.ports[port_number] = PortStatus::WaitingForDeviceDescriptor { since: tick_count };
        Ok(())
    }

    fn handle_get_descriptor_result(
        &mut self,
        port_number: usize,
        descriptor_type: u8,
        tick_count: usize,
        result: Result<usize, EhciError>,
        data_buffer: PoolElementHandle<[u8; 0x40]>,
    ) -> Result<(), EhciError> {
        let mut previous_data = match &self.ports[port_number] {
            PortStatus::WaitingForDeviceDescriptor { .. } => Vec::new(),
            PortStatus::WaitingForConfigDescriptor { descriptor, .. } => descriptor.clone(),
            _ => {
                debug!("Spurious GetDescriptorResult ({port_number})");
                return Ok(());
            }
        };
        let Ok(data_len) = result else {
            warn!("Get Descriptor({descriptor_type}) unsuccessful on port {port_number}");
            self.ports[port_number] = PortStatus::Disconnected;
            return Err(EhciError::SetupUnsuccessful);
        };
        if descriptor_type == DescriptorType::Device as u8 {
            debug!("Getting configuration descriptor on port {port_number}");
            let transfer = self.get_descriptor_transfer(port_number, DescriptorType::Config as u8)?;
            self.async_queue
                .schedule_transfer(Self::port_to_address(port_number), transfer)
                .map_err(|(_, e)| e)?;
            self.ports[port_number] = PortStatus::WaitingForConfigDescriptor {
                descriptor: data_buffer[..data_len].into(),
                since: tick_count,
            };
            Ok(())
        } else {
            debug!("Configuring port {port_number}");
            let transfer = self.set_configuration_transfer(port_number)?;
            self.async_queue
                .schedule_transfer(Self::port_to_address(port_number), transfer)
                .map_err(|(_, e)| e)?;
            previous_data.extend_from_slice(&data_buffer[..data_len]);
            if let Ok(descriptor) = DescriptorSet::new(previous_data) {
                self.ports[port_number] =
                    PortStatus::WaitingForSetConfiguration { since: tick_count, descriptor };
                Ok(())
            } else {
                warn!("Invalid descriptor on port {port_number}");
                self.ports[port_number] = PortStatus::Disconnected;
                Err(EhciError::DescriptorError)
            }
        }
    }

    fn handle_set_configuration_result(
        &mut self,
        port_number: usize,
        success: bool,
        handler: &mut impl EventHandler<CT>,
    ) {
        let PortStatus::WaitingForSetConfiguration { descriptor, .. } = &self.ports[port_number] else {
            debug!("Spurious SetConfigurationResult ({port_number})");
            return;
        };
        if success {
            info!("Port {port_number} configured successfully");
            debug!("Descriptor: {descriptor:#?}");
            let descriptor = descriptor.clone();
            self.ports[port_number] = PortStatus::Configured { descriptor: descriptor.clone() };
            handler.device_connected(self, Self::port_to_address(port_number), descriptor)
        } else {
            warn!("Set Configuration unsuccesful on port {port_number}");
            self.ports[port_number] = PortStatus::Disconnected;
        }
    }

    fn set_address_transfer(
        &mut self,
        port_number: usize,
    ) -> Result<Transfer<BuiltinTransfer<CT>>, EhciError> {
        let setup_buffer = self.buffer_pool.alloc(
            ControlRequest {
                typ: 0,
                request: 5, /* SET_ADDRESS */
                value: Self::port_to_address(port_number) as u16,
                index: 0,
                length: 0,
            }
            .into_tmp_buffer(),
        )?;
        Transfer::new_control_write(
            &mut self.qtd_pool,
            BuiltinTransfer::SetAddress { port_number, setup_buffer },
            self.virt_to_phys,
        )
        .map_err(|(_, e)| e)
    }

    fn set_configuration_transfer(
        &mut self,
        port_number: usize,
    ) -> Result<Transfer<BuiltinTransfer<CT>>, EhciError> {
        let setup_buffer = self.buffer_pool.alloc(
            ControlRequest {
                typ: 0,
                request: 9, /* SET_CONFIGURATION */
                // XXX: We always select the first configuration, which might not
                //      be appropriate for every device.
                value: 1,
                index: 0,
                length: 0,
            }
            .into_tmp_buffer(),
        )?;
        Transfer::new_control_write(
            &mut self.qtd_pool,
            BuiltinTransfer::SetConfiguration { port_number, setup_buffer },
            self.virt_to_phys,
        )
        .map_err(|(_, e)| e)
    }

    fn get_descriptor_transfer(
        &mut self,
        port_number: usize,
        descriptor_type: u8,
    ) -> Result<Transfer<BuiltinTransfer<CT>>, EhciError> {
        let setup_buffer = self.buffer_pool.alloc(
            ControlRequest {
                typ: 0x80,
                request: 6, /* GET_DESCRIPTOR */
                value: (descriptor_type as u16) << 8,
                index: 0,
                length: 0x40,
            }
            .into_tmp_buffer(),
        )?;
        Transfer::new_control_read(
            &mut self.qtd_pool,
            BuiltinTransfer::GetDescriptor {
                port_number,
                descriptor_type,
                setup_buffer,
                data_buffer: self.buffer_pool.alloc([0; 0x40])?,
            },
            self.virt_to_phys,
        )
        .map_err(|(_, e)| e)
    }

    // XXX: We set the address to the port number, because we
    //      only support one device per port right now. This will
    //      make the address unique, but will need some rework
    //      if we want to support HUBs.
    fn port_to_address(port: usize) -> u8 { port as u8 + 1 }

    fn address_to_port(&self, address: u8) -> Result<usize, EhciError> {
        if address < 1 || address > self.ports.len() as u8 {
            Err(EhciError::InvalidAddress)
        } else {
            Ok(address as usize - 1)
        }
    }

    /// Enable interrupts.
    pub fn enable_interrupts(&self) {
        unsafe {
            (*self.opregs).interrupt_enable.change(|ie| {
                ie.set_interrupt(true);
                ie.set_error_interrupt(true);
                ie.set_port_change(true);
            })
        };
    }

    /// Clear all triggered interrupts so the interrupt signal can go back to low.
    pub fn acknowledge_interrupts(&self) {
        // Acknowledge interrupts by getting and setting the same flags
        // (writing 1 to an interrupt status flag resets it)
        unsafe { (*self.opregs).status.change(|_| {}) };
    }
}

impl<CT> core::fmt::Debug for Controller<CT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Controller")
            .field("cmd", unsafe { &(*self.opregs).cmd.get() })
            .field("status", unsafe { &(*self.opregs).status.get() })
            .field("async_queue_ptr", unsafe { &(*self.opregs).async_list.get() })
            .field("ports", &self.ports)
            .finish()
    }
}

unsafe impl<CT> Send for Controller<CT> {}

impl PortStatus {
    fn is_configured(&self) -> bool { matches!(self, PortStatus::Configured { .. }) }
}
