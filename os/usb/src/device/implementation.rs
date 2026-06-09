// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{BTreeMap, VecDeque};

use atsama5d27::{
    pmc::PeripheralId,
    udphs::{
        DmaControl, EndpointConfiguration, EndpointControl, EndpointDirection, EndpointStatus, UsbDevice,
    },
};
use server::{
    try_send_blocking_archive, BlockingArchiveHandler, BlockingScalarAsyncHandler, BlockingScalarHandler,
    BlockingScalarRequest, DeferredLendMut, DeferredLendMutHandler, MoveHandler, ScalarHandler,
};
use usb::{
    device::{messages::*, SetupPacket, BLD_DEV_VERSION, MAJ_DEV_VERSION, MIN_DEV_VERSION},
    UsbError,
};
use utralib::{HW_UDPHS_BASE, HW_UDPHS_RAM_MEM, HW_UDPHS_RAM_MEM_LEN};
use xous::arch::irq::IrqNumber;

use super::messages::*;
use crate::{PowerManagerApi, PowerManagerExtApi};

#[derive(server::Server)]
#[name = "os/usbdev"]
pub struct UsbDeviceServer {
    power_manager: PowerManagerApi,
    power_manager_ext: PowerManagerExtApi,
    hw: UsbDevice,
    pending_address: Option<u8>,
    otg_device_connected: bool,
    vbus_has_power: bool,
    is_configured: bool,
    should_be_enabled: bool,
    enabled: bool,
    interfaces: BTreeMap<u8, RegisteredInterface>,
    capabilities: Vec<RegisteredCapability>,
    setup_responders: Vec<xous::CID>,
    config_descriptor: Vec<u8>,
    bos_descriptor: Vec<u8>,
    remaining_setup_tx_data: Vec<u8>,
    end_setup_tx_with_short_packet: bool,
    endpoints: BTreeMap<u8, RuntimeEndpointData>,
    connection_waiters: Vec<BlockingScalarRequest<WaitForConnection>>,
    custom_vid: Option<u16>,
    custom_pid: Option<u16>,
}

#[derive(Debug, Default, Clone, server::Permissions)]
#[server_name = "os/usbdev"]
#[all_permissions]
struct InternalPermissions;

struct RegisteredInterface {
    descriptors: Vec<u8>,
}

struct RegisteredCapability {
    descriptors: Vec<u8>,
}

struct InterruptContext {
    conn: server::CheckedConn<InternalPermissions>,
    hw: UsbDevice,
}

/// State for a multi-chunk DMA or FIFO write in progress.
struct OngoingWrite {
    msg: DeferredLendMut<WriteEndpoint>,
    /// Bytes sent so far (multi-chunk DMA writes).
    offset: u32,
    /// Total bytes to send.
    total: u32,
    /// Send a ZLP after max_packet_len-aligned transfers.
    zlp: bool,
    /// Waiting for the FIFO-mode ZLP to complete via TxCompleteInterrupt.
    pending_zlp: bool,
}

struct RuntimeEndpointData {
    properties: EndpointProperties,
    use_dma: bool,
    ongoing_read: Option<DeferredLendMut<ReadEndpoint>>,
    ongoing_write: Option<OngoingWrite>,
    pending_rx: VecDeque<Vec<u8>>,
}

const MAX_PENDING_RX_PACKETS: usize = 8;
const EPT0_MAX_PACKET_SIZE: usize = 0x40;

const MANUFACTURER: &str = "Foundation Devices, Inc.";
const PRODUCT: &str = "Passport Prime";

#[rustfmt::skip]
const DEVICE_DESCRIPTOR: [u8; 0x12] = [
    0x12, // bLength
    0x01, // bDescriptorType: Device
    0x10, 0x02, // bcdUSB: 2.1 for BOS support
    0xef, 0x02, 0x01, // bDeviceClass, bDeviceSubClass and bDeviceProtocol
                      // These values must be used if we have IADs
                      // see https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/usb-interface-association-descriptor
    EPT0_MAX_PACKET_SIZE as u8, // bMaxPacketSize0
    0x07, 0x13, // idVendor (Transcend)
    0x65, 0x01, // idProduct (Mass Storage Device)
    (MIN_DEV_VERSION << 4) | BLD_DEV_VERSION, MAJ_DEV_VERSION, // bcdDevice
    0x01, // iManufacturer (string index)
    0x02, // iProduct (string index)
    0x03, // iSerial (string index)
    0x01, // bNumConfigurations
];

const GET_STATUS: u8 = 0;
const SET_ADDRESS: u8 = 5;
const GET_DESCRIPTOR: u8 = 6;
const SET_CONFIGURATION: u8 = 9;

impl server::Server for UsbDeviceServer {
    fn on_start(&mut self, _context: &mut server::ServerContext<Self>) {
        log::debug!("Claiming UDPHS IRQ");
        let int_ctx = Box::into_raw(Box::new(InterruptContext {
            conn: server::CheckedConn::default(),
            hw: self.hw.clone(),
        }));
        xous::claim_interrupt(IrqNumber::Udphs, udphs_irq_handler, int_ctx as *mut usize)
            .expect("Could not claim UHPHS interrupt");
    }
}

impl UsbDeviceServer {
    pub fn new() -> Self {
        let udphs_banks = xous::map_memory(
            xous::MemoryAddress::new(HW_UDPHS_RAM_MEM),
            None,
            HW_UDPHS_RAM_MEM_LEN,
            xous::MemoryFlags::W | xous::MemoryFlags::DEV | xous::MemoryFlags::NO_CACHE,
        )
        .expect("Could not map UDPHS RAM");

        let udphs_csr = xous::map_memory(
            xous::MemoryAddress::new(HW_UDPHS_BASE),
            None,
            0x1000,
            xous::MemoryFlags::W | xous::MemoryFlags::DEV | xous::MemoryFlags::NO_CACHE,
        )
        .expect("Could not map UDPHS registers");

        let power_manager = PowerManagerApi::default();
        power_manager.enable_peripheral(PeripheralId::Udphs).unwrap();
        let mut hw = UsbDevice::new(udphs_csr.as_mut_ptr(), udphs_banks.as_mut_ptr());
        // Disable for now, it will be reenabled (and reset) when we read the OTG gpio line later
        hw.set_enabled(false);
        power_manager.disable_peripheral(PeripheralId::Udphs).unwrap();

        let capabilities = vec![RegisteredCapability {
            descriptors: vec![
                0x07, // bLength
                0x10, // bDescriptorType
                0x02, // bDevCapabilityType: USB 2.0 EXTENSION
                0, 0, 0, 0, // bmAttributes
            ],
        }];

        Self {
            power_manager,
            power_manager_ext: Default::default(),
            hw,
            pending_address: None,
            otg_device_connected: false,
            vbus_has_power: false,
            connection_waiters: Default::default(),
            interfaces: Default::default(),
            capabilities,
            setup_responders: Default::default(),
            config_descriptor: Default::default(),
            endpoints: Default::default(),
            bos_descriptor: Default::default(),
            is_configured: false,
            should_be_enabled: false,
            enabled: false,
            remaining_setup_tx_data: Default::default(),
            end_setup_tx_with_short_packet: false,
            custom_vid: None,
            custom_pid: None,
        }
    }

    fn update_hw_enabled_state(&mut self) {
        if self.should_be_enabled
            && self.vbus_has_power
            && !self.otg_device_connected
            && !self.config_descriptor.is_empty()
        {
            if !self.enabled {
                self.power_manager.enable_peripheral(PeripheralId::Udphs).unwrap();
                self.hw.set_enabled(true);
                self.enabled = true;
            }
        } else if self.enabled {
            self.hw.set_enabled(false);
            self.send_disconnected();
            self.power_manager.disable_peripheral(PeripheralId::Udphs).unwrap();
            self.enabled = false;
            self.is_configured = false;
        }
    }

    fn configure(&mut self) {
        for (ept_num, ept_data) in &mut self.endpoints {
            log::debug!("Setting up EP{ept_num} as {:?}", ept_data.properties);
            self.hw.reset_endpoint(*ept_num as usize);
            let ep = self.hw.endpoint(*ept_num as usize);

            let mut config = EndpointConfiguration(0);
            config.set_ept_size(ept_data.properties.max_packet_len.ilog2().saturating_sub(3));
            config.set_ept_type(ept_data.properties.ep_type);
            config.set_ept_dir(ept_data.properties.ep_direction);
            // FIFO endpoints use 1 bank to conserve peripheral memory.
            // See SAMA5D2 Datasheet Table 41-4: EPT_1 and EPT2 can have 3 banks.
            config.set_bank_number(if !ept_data.use_dma {
                1
            } else if *ept_num == 1 || *ept_num == 2 {
                3
            } else {
                2
            });
            ep.cfg.set(config);
            assert!(ep.cfg.get().mapped());

            let mut control = EndpointControl(0);
            control.set_enable(true);
            if ept_data.use_dma {
                assert!((1..=7).contains(ept_num), "DMA only supported on EP1-7");
                control.set_auto_valid(true);
                // Enable TxComplete interrupt for ZLP support on DMA IN endpoints.
                // The handler ignores it unless pending_zlp is set.
                if ept_data.properties.ep_direction == EndpointDirection::In {
                    control.set_transmission_complete_interrupt(true);
                }
                ep.ctl_enable.set(control);
                self.hw.enable_dma_interrupt(*ept_num as usize);
                if ept_data.properties.ep_direction == EndpointDirection::In {
                    self.hw.enable_endpoint_interrupt(*ept_num as usize);
                }
            } else {
                control.set_received_out_interrupt(true);
                control.set_transmission_complete_interrupt(true);
                ep.ctl_enable.set(control);
                self.hw.enable_endpoint_interrupt(*ept_num as usize);
            }
        }
        self.is_configured = true;

        log::info!("Usb device configured");
        // Drop all waiters, which will return the blocking scalars to the callers.
        self.connection_waiters.truncate(0);
    }

    fn start_dma(&mut self, endpoint_number: u8, buf: *const u8, length: u16) {
        // END_TR_EN is for OUT (read) transfers only (SAMA5D2 datasheet §41.6.10).
        // It lets the device end the DMA on a short packet. For IN (write) it has no effect.
        let is_out = self.endpoints[&endpoint_number].properties.ep_direction == EndpointDirection::Out;
        let mut control = DmaControl(0);
        control.set_enable(true);
        control.set_end_of_transfer_enable(is_out);
        control.set_end_of_buffer_enable(true);
        control.set_end_of_transfer_interrupt(is_out);
        control.set_end_of_buffer_interrupt(true);
        control.set_burst_lock(true);
        control.set_length(length);
        let dma = self.hw.dma(endpoint_number as usize);
        dma.address.set(xous::virt_to_phys(buf as usize).unwrap() as u32);
        dma.control.set(control);
    }

    /// Max DMA chunk size for an endpoint: largest `max_packet_len`-aligned
    /// value that fits in the 16-bit DMA length register.
    fn max_dma_chunk(max_packet_len: u16) -> u32 {
        (u16::MAX as u32 / max_packet_len as u32) * max_packet_len as u32
    }

    /// Start the next DMA chunk for a multi-chunk write on `endpoint_number`.
    /// Returns the chunk length that was started.
    fn start_next_dma_chunk(&mut self, endpoint_number: u8) {
        let ep = &self.endpoints[&endpoint_number];
        let wr = ep.ongoing_write.as_ref().unwrap();
        let remaining = wr.total - wr.offset;
        let max_chunk = Self::max_dma_chunk(ep.properties.max_packet_len);
        let chunk_len = remaining.min(max_chunk);
        let buf_ptr = unsafe { wr.msg.body().buf.as_ptr().add(wr.offset as usize) };
        self.start_dma(endpoint_number, buf_ptr, chunk_len as u16);
        self.endpoints.get_mut(&endpoint_number).unwrap().ongoing_write.as_mut().unwrap().offset += chunk_len;
    }

    fn to_string_descriptor(s: &str) -> Vec<u8> {
        let mut payload: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        // Add a header of [Descriptor length, Descriptor type (3)]
        payload.insert(0, 0x03);
        payload.insert(0, payload.len() as u8 + 1);
        payload
    }

    fn send_disconnected(&mut self) {
        for ep in self.endpoints.values_mut() {
            if let Some(mut read) = ep.ongoing_read.take() {
                read.set_response(Err(UsbError::HostDisconnected));
            }
            if let Some(mut write) = ep.ongoing_write.take() {
                write.msg.set_response(Err(UsbError::HostDisconnected));
            }
            ep.pending_rx.clear();
        }
    }

    fn recalculate_config_descriptor(&mut self) {
        self.config_descriptor = vec![
            // Configuration Descriptor
            0x09, // bLength
            0x02, // bDescriptorType: Configuration
            0x00, // wTotalLength (u16, fixed up later)
            0x00,
            self.interfaces.len() as u8, // bNumInterfaces
            0x01,                        // bConfigurationValue (used to call SetConfiguration)
            2,                           // iConfiguration: index to iProduct
            0xc0,                        // bmAttributes (self-powered, no remote wakeup)
            0x10,                        /* MaxPower: 32mA. This does not have to be accurate,
                                          * but if it's too large,
                                          * the device will be rejected with "insufficient available bus
                                          * power" */
        ];
        for interface in self.interfaces.values() {
            self.config_descriptor.extend_from_slice(&interface.descriptors);
        }
        self.config_descriptor[2] = self.config_descriptor.len() as u8;
        self.config_descriptor[3] = (self.config_descriptor.len() >> 8) as u8;
    }

    // Returns the registered endpoint numbers.
    fn register_interface(&mut self, msg: RegisterInterface) -> Result<Vec<u8>, UsbError> {
        if self.interfaces.contains_key(&msg.interface_number) {
            return Err(UsbError::AlreadyRegistered);
        }

        let mut descriptors = Vec::new();

        if msg.associated_interface_count > 0 {
            descriptors.extend_from_slice(&[
                0x08,                           // bLength
                0x0b,                           // bDescriptorType: Interface Association
                msg.interface_number,           // bFirstInterface: First iface
                msg.associated_interface_count, // Two ifaces
                msg.if_class,                   // bInterfaceClass
                msg.if_subclass,                // bInterfaceSubClass
                msg.if_protocol,                // bInterfaceProtocol
                2,                              // iInterface: index to iProduct
            ]);
        }

        descriptors.extend_from_slice(&[
            // Interface Descriptor
            0x09,                      // bLength
            0x04,                      // bDescriptorType: Interface
            msg.interface_number,      // bInterfaceNumber
            0x00,                      // bAlternateSetting
            msg.endpoints.len() as u8, // bNumEndpoints
            msg.if_class,              // bInterfaceClass
            msg.if_subclass,           // bInterfaceSubClass
            msg.if_protocol,           // bInterfaceProtocol
            2,                         // iInterface: index to iProduct
        ]);
        descriptors.extend_from_slice(&msg.interface_functional_descriptors);
        let mut result = Vec::new();
        for properties in msg.endpoints {
            let ep_number = if properties.use_dma {
                (1..=7u8).find(|n| !self.endpoints.contains_key(n)).expect("no free DMA endpoint (EP1-7)")
            } else {
                (8..=15u8).find(|n| !self.endpoints.contains_key(n)).expect("no free FIFO endpoint (EP8-15)")
            };
            descriptors.extend_from_slice(&[
                // Endpoint Descriptor
                0x07, // bLength
                0x05, // bDescriptorType: Endpoint
                ep_number + if properties.ep_direction == EndpointDirection::In { 0x80 } else { 0 }, // bEndpointAddress
                properties.ep_type as u8, // bmAttributes
                properties.max_packet_len as u8, // wMaxPacketSize
                (properties.max_packet_len >> 8) as u8,
                properties.interval, // bInterval
            ]);
            let use_dma = properties.use_dma;
            self.endpoints.insert(
                ep_number,
                RuntimeEndpointData {
                    properties,
                    use_dma,
                    ongoing_read: None,
                    ongoing_write: None,
                    pending_rx: VecDeque::new(),
                },
            );
            result.push(ep_number);
        }
        self.interfaces.insert(msg.interface_number, RegisteredInterface { descriptors });
        Ok(result)
    }

    fn recalculate_bos_descriptor(&mut self) {
        self.bos_descriptor = vec![
            // Binary Object Store Descriptor
            0x05, // bLength
            0x0f, // bDescriptorType: Binary Object Store
            0x00, // wTotalLength (u16, fixed up later)
            0x00,
            self.capabilities.len() as u8, // bNumDeviceCaps
        ];
        for capability in &self.capabilities {
            self.bos_descriptor.extend_from_slice(&capability.descriptors);
        }
        self.bos_descriptor[2] = self.bos_descriptor.len() as u8;
        self.bos_descriptor[3] = (self.bos_descriptor.len() >> 8) as u8;
    }

    fn register_capability(&mut self, msg: RegisterCapability) {
        let mut descriptors = vec![
            // Platform Device Capability
            20 + msg.capability_functional_descriptors.len() as u8, // bLength
            msg.cap_type,                                           // bDescriptorType
            msg.cap_subtype,                                        // bDevCapabilityType
            0,                                                      // bReserved
        ];
        descriptors.extend_from_slice(&msg.cap_uuid);
        descriptors.extend_from_slice(&msg.capability_functional_descriptors);
        self.capabilities.push(RegisteredCapability { descriptors });
    }

    fn handle_ep0_tx_complete(&mut self) {
        if let Some(addr) = self.pending_address.take() {
            log::debug!("Set address: {}", addr);
            self.hw.set_address(addr);
        }
        if !self.remaining_setup_tx_data.is_empty() || self.end_setup_tx_with_short_packet {
            self.send_remaining_setup_tx();
        }
    }

    fn send_remaining_setup_tx(&mut self) {
        let mut bytes = core::mem::take(&mut self.remaining_setup_tx_data);
        if bytes.len() >= EPT0_MAX_PACKET_SIZE {
            self.remaining_setup_tx_data = bytes.split_off(EPT0_MAX_PACKET_SIZE);
        } else {
            self.end_setup_tx_with_short_packet = false;
        }
        log::trace!("Sending setup response {bytes:02x?}");
        self.hw.write_endpoint_memory(0, 0, &bytes);
        let mut status = EndpointStatus(0x0);
        status.set_tx_packet_ready(true);
        self.hw.endpoint(0).status_set.set(status);
    }
}

impl BlockingArchiveHandler<RegisterInterface> for UsbDeviceServer {
    fn handle(
        &mut self,
        msg: RegisterInterface,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<Vec<u8>, UsbError> {
        log::info!("Registering interface class {} with {} endpoints", msg.if_class, msg.endpoints.len());
        let result = self.register_interface(msg)?;
        self.recalculate_config_descriptor();
        self.recalculate_bos_descriptor();
        self.update_hw_enabled_state();

        Ok(result)
    }
}

impl BlockingArchiveHandler<RegisterCapability> for UsbDeviceServer {
    fn handle(
        &mut self,
        msg: RegisterCapability,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), UsbError> {
        log::info!("Registering capability type {}:{}", msg.cap_type, msg.cap_subtype);
        self.register_capability(msg);
        self.recalculate_bos_descriptor();

        Ok(())
    }
}

impl BlockingScalarHandler<RegisterSetupResponder> for UsbDeviceServer {
    fn handle(
        &mut self,
        msg: RegisterSetupResponder,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), UsbError> {
        self.setup_responders.push(msg.0);
        Ok(())
    }
}

impl ScalarHandler<SetEndpointStalled> for UsbDeviceServer {
    fn handle(
        &mut self,
        msg: SetEndpointStalled,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if !self.is_configured {
            log::warn!("SetEndpointStalled called when device was not configured");
            return;
        }
        log::debug!("Setting stall on endpoint {msg:?}");
        let mut status = EndpointStatus(0x0);
        status.set_force_stall(true);
        if msg.stalled {
            self.hw.endpoint(msg.endpoint as usize).status_set.set(status);
        } else {
            self.hw.endpoint(msg.endpoint as usize).status_clr.set(status);
        }
    }
}

impl BlockingScalarAsyncHandler<WaitForConnection> for UsbDeviceServer {
    fn handle(
        &mut self,
        msg: BlockingScalarRequest<WaitForConnection>,
        _context: &mut server::ServerContext<Self>,
    ) {
        if !self.is_configured {
            self.connection_waiters.push(msg);
        }
    }

    fn default_response() {}
}

impl DeferredLendMutHandler<ReadEndpoint> for UsbDeviceServer {
    fn handle(&mut self, mut msg: DeferredLendMut<ReadEndpoint>, _context: &mut server::ServerContext<Self>) {
        if !self.is_configured {
            msg.set_response(Err(UsbError::HostDisconnected));
            return;
        }
        let endpoint_number = msg.body().endpoint;
        let Some(endpoint) = self.endpoints.get_mut(&endpoint_number) else {
            msg.set_response(Err(UsbError::NotFound));
            return;
        };
        if endpoint.ongoing_read.is_some() {
            msg.set_response(Err(UsbError::Busy));
            return;
        }
        if endpoint.properties.ep_direction != EndpointDirection::Out {
            msg.set_response(Err(UsbError::WrongDirection));
            return;
        }
        log::trace!("Reading {} bytes on EP{}", msg.body().length, msg.body().endpoint);
        let use_dma = endpoint.use_dma;
        if use_dma {
            xous::flush_cache(msg.body().buf, xous::CacheOperation::Invalidate).ok();
            self.start_dma(endpoint_number, msg.body().buf.as_ptr(), msg.body().length);
        } else {
            // For FIFO mode, check if data is already buffered
            let ep = self.endpoints.get_mut(&endpoint_number).unwrap();
            if let Some(data) = ep.pending_rx.pop_front() {
                let requested = msg.body().length as usize;
                let len = data.len().min(requested);
                let dst = msg.body_mut().buf.as_slice_mut::<u8>();
                dst[..len].copy_from_slice(&data[..len]);
                msg.set_response(Ok(len));
                return;
            }
        }
        self.endpoints.get_mut(&endpoint_number).unwrap().ongoing_read = Some(msg);
    }

    fn default_response() -> <ReadEndpoint as server::LendMut>::Response { Err(UsbError::HostDisconnected) }
}

impl DeferredLendMutHandler<WriteEndpoint> for UsbDeviceServer {
    fn handle(
        &mut self,
        mut msg: DeferredLendMut<WriteEndpoint>,
        _context: &mut server::ServerContext<Self>,
    ) {
        if !self.is_configured {
            msg.set_response(Err(UsbError::HostDisconnected));
            return;
        }
        let endpoint_number = msg.body().endpoint;
        let Some(endpoint) = self.endpoints.get_mut(&endpoint_number) else {
            msg.set_response(Err(UsbError::NotFound));
            return;
        };
        if endpoint.ongoing_write.is_some() {
            msg.set_response(Err(UsbError::Busy));
            return;
        }
        if endpoint.properties.ep_direction != EndpointDirection::In {
            msg.set_response(Err(UsbError::WrongDirection));
            return;
        }
        let total = msg.body().length as u32;
        let zlp = msg.body().zlp;
        log::trace!("Writing {total} bytes on EP{endpoint_number}");
        let use_dma = endpoint.use_dma;
        if use_dma {
            xous::flush_cache(msg.body().buf, xous::CacheOperation::Clean).ok();
            let ep = self.endpoints.get_mut(&endpoint_number).unwrap();
            ep.ongoing_write = Some(OngoingWrite { msg, offset: 0, total, zlp, pending_zlp: false });
            // Start first (or only) DMA chunk.
            self.start_next_dma_chunk(endpoint_number);
        } else {
            // FIFO write: copy data to endpoint memory, set tx_packet_ready
            let len = total as usize;
            if len > endpoint.properties.max_packet_len as usize {
                msg.set_response(Err(UsbError::DataTooLarge));
                return;
            }
            let buf = msg.body().buf.as_slice::<u8>();
            self.hw.write_endpoint_memory(endpoint_number as usize, 0, &buf[..len]);
            let mut status = EndpointStatus(0x0);
            status.set_tx_packet_ready(true);
            self.hw.endpoint(endpoint_number as usize).status_set.set(status);
            let ep = self.endpoints.get_mut(&endpoint_number).unwrap();
            ep.ongoing_write = Some(OngoingWrite { msg, offset: 0, total, zlp: false, pending_zlp: false });
        }
    }

    fn default_response() -> <WriteEndpoint as server::LendMut>::Response { Err(UsbError::HostDisconnected) }
}

impl UsbDeviceServer {
    /// Disconnect the device from the USB bus, hold DETACH, then reattach.
    /// Forces the host to re-enumerate so descriptor changes (e.g. a new
    /// VID:PID) take effect. The 100ms hold is required for Windows to
    /// register the disconnect.
    fn reattach_bus(&mut self) {
        if self.enabled {
            self.hw.set_enabled(false);
            self.send_disconnected();
            std::thread::sleep(std::time::Duration::from_millis(100));
            self.hw.set_enabled(true);
        }
    }
}

impl BlockingScalarHandler<ResetController> for UsbDeviceServer {
    fn handle(
        &mut self,
        _msg: ResetController,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <ResetController as server::BlockingScalar>::Response {
        self.reattach_bus();
        Ok(())
    }
}

impl BlockingScalarHandler<SetVidPid> for UsbDeviceServer {
    fn handle(
        &mut self,
        msg: SetVidPid,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetVidPid as server::BlockingScalar>::Response {
        self.custom_vid = msg.vid;
        self.custom_pid = msg.pid;
        self.reattach_bus();
        Ok(())
    }
}

impl ScalarHandler<EndOfReset> for UsbDeviceServer {
    fn handle(&mut self, _msg: EndOfReset, sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        if sender != xous::current_pid().unwrap() {
            return;
        }

        self.send_disconnected();
        self.is_configured = false;

        log::info!("Got End of Reset");
        let ep0 = self.hw.endpoint(0);

        let mut config = EndpointConfiguration(0);
        config.set_ept_size(3); // Size: 8<<3 == 0x40
        config.set_bank_number(1);
        ep0.cfg.set(config);
        if !ep0.cfg.get().mapped() {
            // This should only happen if the host disconnects between the UDPHS EOR signal
            // and us reaching this point.
            log::warn!("Could not map EP0");
            return;
        }

        let mut control = EndpointControl(0);
        control.set_enable(true);
        control.set_received_setup_interupt(true);
        control.set_received_out_interrupt(true);
        control.set_transmission_complete_interrupt(true);
        ep0.ctl_enable.set(control);
        self.hw.enable_endpoint_interrupt(0);
    }
}

impl ScalarHandler<SetupPacket> for UsbDeviceServer {
    fn handle(&mut self, msg: SetupPacket, sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        if sender != xous::current_pid().unwrap() {
            return;
        }
        log::trace!("Setup received: {msg:02x?}");
        let response = match (msg.request_type, msg.request) {
            (0x80, GET_STATUS) => Some(vec![1, 0]), // Self Powered
            (0x80, GET_DESCRIPTOR) => {
                match msg.value {
                    0x100 => {
                        // Type: device(1), index: 0
                        let mut response_bytes = Vec::new();
                        response_bytes.extend_from_slice(&DEVICE_DESCRIPTOR);
                        if let Some(custom_vid) = self.custom_vid {
                            response_bytes[9] = (custom_vid >> 8) as u8;
                            response_bytes[8] = (custom_vid & 0xff) as u8;
                        }
                        if let Some(custom_pid) = self.custom_pid {
                            response_bytes[11] = (custom_pid >> 8) as u8;
                            response_bytes[10] = (custom_pid & 0xff) as u8;
                        }
                        response_bytes.extend_from_slice(&self.config_descriptor);
                        Some(response_bytes)
                    }
                    0x200 => {
                        // Type: configuration(2), index: 0
                        Some(self.config_descriptor.clone())
                    }
                    0x300 => {
                        // Type: string(3), index: 0 (languages)
                        Some(Vec::from([0x04, 0x03, 0x09, 0x04]))
                    }
                    0x301 => {
                        // Type: string(3), index: 1 (manufacturer, see DEVICE_DESCRIPTOR)
                        Some(Self::to_string_descriptor(MANUFACTURER))
                    }
                    0x302 => {
                        // Type: string(3), index: 2 (product, see DEVICE_DESCRIPTOR)
                        Some(Self::to_string_descriptor(PRODUCT))
                    }
                    0x303 => {
                        // Type: string(3), index: 3 (serial, see DEVICE_DESCRIPTOR)
                        Some(Self::to_string_descriptor(
                            &crate::DEVICE_NAME.lock().unwrap_or_else(|e| e.into_inner()).clone(),
                        ))
                    }
                    0xF00 => {
                        // Type: bos(15), index: 0
                        Some(self.bos_descriptor.clone())
                    }
                    _ => {
                        log::warn!("Unknown descriptor request: {msg:02x?}");
                        None
                    }
                }
            }
            (0x00, SET_ADDRESS) => {
                log::debug!("Set address (pending): {}", msg.value);
                // Only set the address once the STATUS phase (IN, i.e. transmission) is over
                self.pending_address = Some(msg.value as u8);
                Some(Vec::new())
            }
            (0x00, SET_CONFIGURATION) => {
                log::debug!("Set configuration: {}", msg.value);
                if !self.is_configured {
                    self.configure();
                }
                Some(Vec::new())
            }
            _ => self.setup_responders.iter().find_map(|setup_responder| {
                // A responder app may exit while the host is still issuing SETUP
                // packets (e.g. during shutdown or right after a reset_controller
                // re-enum). Treat a dead CID as "didn't handle this packet" so
                // we move on to the next responder instead of panicking.
                match try_send_blocking_archive(*setup_responder, SetupPacketCallback(msg.clone())) {
                    Ok(response) => response,
                    Err(e) => {
                        log::warn!("setup responder {setup_responder:?} unreachable: {e:?}");
                        None
                    }
                }
            }),
        };
        match response {
            Some(mut bytes) => {
                bytes.truncate(msg.length as usize);
                self.end_setup_tx_with_short_packet = bytes.len() < msg.length as usize;
                self.remaining_setup_tx_data = bytes;
                self.send_remaining_setup_tx();
            }
            None => {
                log::trace!("Stalling control endpoint");
                let mut status = EndpointStatus(0x0);
                status.set_force_stall(true);
                self.hw.endpoint(0).status_set.set(status);
            }
        }
    }
}

impl MoveHandler<RxCompleteInterrupt> for UsbDeviceServer {
    const LEAK_MESSAGE: bool = false;

    fn handle(
        &mut self,
        msg: RxCompleteInterrupt,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let ep_num = msg.endpoint;
        let byte_count = msg.byte_count as usize;

        // EP0: currently discarded (same as before)
        if ep_num == 0 {
            log::trace!("Rx complete on EP0 ({byte_count} bytes, discarded)");
            return;
        }

        let Some(ep) = self.endpoints.get_mut(&ep_num) else {
            log::warn!("RxCompleteInterrupt for unknown EP{ep_num}");
            return;
        };

        let src = msg.buf.as_slice::<u8>();

        // If a read is pending, fulfill it immediately
        if let Some(mut read) = ep.ongoing_read.take() {
            let requested = read.body().length as usize;
            let len = byte_count.min(requested);
            let dst = read.body_mut().buf.as_slice_mut::<u8>();
            dst[..len].copy_from_slice(&src[..len]);
            read.set_response(Ok(len));
        } else {
            // Buffer for next ReadEndpoint call
            if ep.pending_rx.len() >= MAX_PENDING_RX_PACKETS {
                log::trace!("EP{ep_num}: pending RX queue full, dropping oldest packet");
                ep.pending_rx.pop_front();
            }
            ep.pending_rx.push_back(src[..byte_count].to_vec());
        }
    }
}

impl ScalarHandler<TxCompleteInterrupt> for UsbDeviceServer {
    fn handle(
        &mut self,
        msg: TxCompleteInterrupt,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if sender != xous::current_pid().unwrap() {
            return;
        }
        log::trace!("Tx complete on EP{}", msg.endpoint);

        if msg.endpoint == 0 {
            self.handle_ep0_tx_complete();
            return;
        }

        // EP1+: FIFO-mode write completion.
        let Some(ep) = self.endpoints.get_mut(&msg.endpoint) else {
            return;
        };
        if let Some(ref wr) = ep.ongoing_write {
            if wr.pending_zlp {
                // ZLP was just transmitted — complete the write.
                let total = wr.total as usize;
                let mut write = ep.ongoing_write.take().unwrap();
                write.msg.set_response(Ok(total));
                return;
            }
        }
        if ep.use_dma {
            // Ignore spurious TxComplete on DMA endpoints (auto_valid).
            return;
        }
        // Non-DMA endpoint write completion.
        if let Some(mut write) = ep.ongoing_write.take() {
            write.msg.set_response(Ok(write.msg.body().length as usize));
        }
    }
}

impl ScalarHandler<DmaInterrupt> for UsbDeviceServer {
    fn handle(&mut self, msg: DmaInterrupt, sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        if sender != xous::current_pid().unwrap() {
            return;
        }
        log::trace!("Dma interrupt: {msg:?}");
        let ep_num = msg.endpoint;
        let Some(ep) = self.endpoints.get_mut(&ep_num) else {
            return;
        };
        // DMA read completion.
        if let Some(mut read) = ep.ongoing_read.take() {
            read.set_response(Ok((read.body().length - msg.status.length()) as usize))
        }
        // DMA write completion — may need to continue with next chunk or send ZLP.
        let Some(ref wr) = ep.ongoing_write else {
            return;
        };
        if wr.offset < wr.total {
            // More data to send — start next chunk.
            self.start_next_dma_chunk(ep_num);
        } else {
            let ep = self.endpoints.get_mut(&ep_num).unwrap();
            let wr = ep.ongoing_write.as_mut().unwrap();
            let max_pkt = ep.properties.max_packet_len as u32;
            if wr.zlp && wr.total > 0 && wr.total % max_pkt == 0 {
                // Need a ZLP. The data DMA used end_of_transfer=false so the
                // endpoint is still active. Set tx_packet_ready with nothing
                // in the FIFO to send a zero-length packet.
                wr.pending_zlp = true;
                let mut status = EndpointStatus(0x0);
                status.set_tx_packet_ready(true);
                self.hw.endpoint(ep_num as usize).status_set.set(status);
            } else {
                // Transfer complete — respond to caller.
                let total = wr.total as usize;
                let mut write = ep.ongoing_write.take().unwrap();
                write.msg.set_response(Ok(total));
            }
        }
    }
}

impl ScalarHandler<SetCableConnected> for UsbDeviceServer {
    fn handle(
        &mut self,
        msg: SetCableConnected,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.vbus_has_power = msg.0;
        self.update_hw_enabled_state();
    }
}

impl ScalarHandler<OtgMode> for UsbDeviceServer {
    fn handle(&mut self, msg: OtgMode, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        if msg.0 {
            log::debug!("OTG slave device connected, enabling power to it");
            self.power_manager_ext.set_usb_boost(true).ok();
            self.otg_device_connected = true;
        } else {
            log::debug!("OTG slave device disconnected, disabling power");
            self.power_manager_ext.set_usb_boost(false).ok();
            self.otg_device_connected = false;
        }
        self.update_hw_enabled_state();
    }
}

impl ScalarHandler<SetDeviceEmulationEnabled> for UsbDeviceServer {
    fn handle(
        &mut self,
        msg: SetDeviceEmulationEnabled,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.should_be_enabled = msg.0;
        self.update_hw_enabled_state();
    }
}

impl BlockingScalarHandler<IsDeviceEmulationEnabled> for UsbDeviceServer {
    fn handle(
        &mut self,
        _msg: IsDeviceEmulationEnabled,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> bool {
        self.enabled
    }
}

impl BlockingScalarHandler<IsDeviceEmulationConnected> for UsbDeviceServer {
    fn handle(
        &mut self,
        _msg: IsDeviceEmulationConnected,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> bool {
        self.is_configured
    }
}

impl BlockingScalarHandler<IsCableConnected> for UsbDeviceServer {
    fn handle(
        &mut self,
        _msg: IsCableConnected,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> bool {
        self.vbus_has_power
    }
}

impl BlockingScalarHandler<IsDeviceMode> for UsbDeviceServer {
    fn handle(
        &mut self,
        _msg: IsDeviceMode,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> bool {
        !self.otg_device_connected
    }
}

fn udphs_irq_handler(_irq_no: usize, arg: *mut usize) {
    let context = unsafe { &mut *(arg as *mut InterruptContext) };
    let interrupts = context.hw.interrupt_status();
    if interrupts.end_of_reset() {
        context.conn.send_scalar_nowait(EndOfReset).ok();
    }
    for dma_endpoint in 1..8 {
        if interrupts.dma(dma_endpoint) != 0 {
            // Reading the status clears the interrupt
            let status = context.hw.dma(dma_endpoint).status.get();
            context.conn.send_scalar_nowait(DmaInterrupt { endpoint: dma_endpoint as u8, status }).ok();
        }
    }
    // Unified endpoint interrupt handling (EP0-15)
    for ep_num in 0..16 {
        if interrupts.endpoint(ep_num) == 0 {
            continue;
        }
        let status = context.hw.endpoint(ep_num).status.get();
        let mut clear = EndpointStatus(0x0);

        // EP0 only: setup packet
        if ep_num == 0 && status.received_setup() {
            clear.set_received_setup(true);
            if status.byte_count() == 8 {
                let mut setup_data = [0; 8];
                context.hw.read_endpoint_memory(0, 0, &mut setup_data);
                context.conn.send_scalar_nowait(SetupPacket::from_bytes(&setup_data)).ok();
            }
        }

        // RX complete: read FIFO data into a page and send as Move
        if status.received_out() {
            clear.set_received_out(true);
            let byte_count = status.byte_count() as usize;
            if byte_count > 0 {
                if let Ok(mut page) = xous::map_memory(None, None, 4096, xous::MemoryFlags::W) {
                    let buf = &mut page.as_slice_mut::<u8>()[..byte_count];
                    context.hw.read_endpoint_memory(ep_num, 0, buf);
                    let msg = RxCompleteInterrupt {
                        buf: page,
                        endpoint: ep_num as u8,
                        byte_count: byte_count as u16,
                    };
                    if context.conn.send_move_nowait(msg).is_err() {
                        log::error!("Failed to send RxCompleteInterrupt for EP{ep_num}");
                        xous::unmap_memory(page).ok();
                    }
                }
            }
        }

        // TX complete
        if status.transmission_complete() {
            clear.set_transmmission_complete(true);
            context.conn.send_scalar_nowait(TxCompleteInterrupt { endpoint: ep_num as u8 }).ok();
        }

        context.hw.endpoint(ep_num).status_clr.set(clear);
    }
    context.hw.clear_interrupt(interrupts);
}
