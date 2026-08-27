// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{
    ArchiveEventSubscriber, ArchiveEventSubscriptionHandler, ArchiveHandler, BlockingScalarHandler, Owned,
    Server, ServerContext,
};
#[cfg(keyos)]
use server::{BlockingArchiveHandler, MessageId as _};
#[cfg(keyos)]
use usb::device::{
    api::{EnabledGate, EndpointDirection, EndpointType},
    messages::{EndpointProperties, SetupPacketCallback},
};

#[cfg(keyos)]
use crate::hid;
use crate::messages::{DeliverHidApdu, IncomingApdu, SetLegacyMode, SubscribeIncomingApdu, WriteHidApdu};

#[cfg(keyos)]
usb::use_device_api!();

#[cfg(keyos)]
const HID_INTERFACE_CLASS: u8 = 0x03;
#[cfg(keyos)]
const HID_INTERFACE_SUBCLASS: u8 = 0x00;
#[cfg(keyos)]
const HID_INTERFACE_PROTOCOL: u8 = 0x00;
#[cfg(keyos)]
const HID_INTERFACE_PRIORITY: u8 = usb::device::interface_priorities::LEGACY_HID;
#[cfg(keyos)]
const HID_ENDPOINTS: [EndpointProperties; 2] = [
    EndpointProperties {
        ep_type: EndpointType::Interrupt,
        ep_direction: EndpointDirection::In,
        max_packet_len: 64,
        interval: 1,
        use_dma: false,
    },
    EndpointProperties {
        ep_type: EndpointType::Interrupt,
        ep_direction: EndpointDirection::Out,
        max_packet_len: 64,
        interval: 1,
        use_dma: false,
    },
];
#[cfg(keyos)]
const HID_FUNC_DESCRIPTOR: [u8; 9] = [
    0x09, // bLength
    0x21, // bDescriptorType: HID
    0x11, 0x01, // bcdHID: 1.11
    0x00, // bCountryCode
    0x01, // bNumDescriptors
    0x22, // bDescriptorType: Report
    34, 0, // wDescriptorLength
];
#[cfg(keyos)]
const HID_REPORT_DESCRIPTOR: [u8; 34] = [
    0x06, 0xA0, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x09, 0x03, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95,
    0x40, 0x81, 0x08, 0x09, 0x04, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x40, 0x91, 0x08, 0xC0,
];

#[cfg(keyos)]
#[derive(Default)]
pub(crate) struct SetupResponder;

#[cfg(keyos)]
impl server::ServerMessages for SetupResponder {
    const NAME: &'static str = "";

    fn messages() -> &'static [server::MessageDef<Self>] {
        &[(SetupPacketCallback::ID, server::handle_blocking_archive_message::<SetupPacketCallback, _>)]
    }
}

#[cfg(keyos)]
impl Server for SetupResponder {}

#[cfg(keyos)]
impl BlockingArchiveHandler<SetupPacketCallback> for SetupResponder {
    fn handle(
        &mut self,
        SetupPacketCallback(msg): SetupPacketCallback,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> Option<Vec<u8>> {
        log::trace!("Setup packet: {msg:02x?}");
        if msg.request_type == 0x21 && msg.request == 0x0a {
            Some(vec![]) // HID SET_IDLE
        } else if msg.request_type == 0x81 && msg.request == 0x06 {
            if msg.value == 0x2200 {
                let len = usize::min(msg.length as usize, HID_REPORT_DESCRIPTOR.len());
                Some(HID_REPORT_DESCRIPTOR[..len].to_vec())
            } else if msg.value == 0x2100 {
                let len = usize::min(msg.length as usize, HID_FUNC_DESCRIPTOR.len());
                Some(HID_FUNC_DESCRIPTOR[..len].to_vec())
            } else {
                None
            }
        } else {
            None
        }
    }
}

/// Permissions struct used by the in-process OUT thread to deliver
/// reassembled APDUs back into the server's main loop. Only this server
/// itself grants `DeliverHidApdu`.
#[cfg(keyos)]
#[derive(Debug, Default, Clone)]
struct InternalPermissions;

#[cfg(keyos)]
impl server::CheckedPermissions for InternalPermissions {
    const NAME: &str = "os/legacy-hid";
}

#[cfg(keyos)]
impl server::MessageAllowed<DeliverHidApdu> for InternalPermissions {}

#[cfg(keyos)]
fn out_thread(mut ep_out: UsbEmulatedEndpoint, gate: EnabledGate) {
    let usb_api = UsbDeviceEmulation::default();
    let out_buffer = xous::DropDeallocate::new(
        xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W | xous::MemoryFlags::POPULATE)
            .expect("Could not allocate OUT buffer"),
    );
    let api = crate::api::LegacyHidApi::<InternalPermissions>::default();

    let mut reassembler = hid::Reassembler::new();

    loop {
        gate.wait_enabled();
        match ep_out.read_buf(*out_buffer, hid::REPORT_SIZE as u16) {
            Ok(l) => {
                let report = &out_buffer.as_slice::<u8>()[..l];
                log::trace!(
                    "Read {l} bytes from endpoint 0x{:02x}: {:02x?}",
                    ep_out.endpoint_number(),
                    report
                );

                match reassembler.feed(report) {
                    Ok(Some((channel_id, apdu))) => {
                        log::trace!("HID reassembled APDU ({} bytes): {:02x?}", apdu.len(), apdu);
                        if let Err(e) = api.deliver_hid_apdu(channel_id, apdu) {
                            log::error!("Failed to deliver HID APDU to server: {e:?}");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::warn!("HID reassembly error: {e}");
                        reassembler.reset();
                    }
                }
            }
            Err(usb::error::UsbError::HostDisconnected) => {
                log::info!("legacy-hid out_thread: host disconnected, waiting for reconnection");
                reassembler.reset();
                usb_api.wait_for_connection().expect("Error waiting for connection");
                log::info!("legacy-hid out_thread: host reconnected");
            }
            Err(usb::error::UsbError::InterfaceDisabled) => reassembler.reset(),
            Err(e) => log::error!("Error while reading from USB: {e:?}"),
        }
    }
}

#[cfg(keyos)]
struct HidInEndpoint {
    endpoint: UsbEmulatedEndpoint,
    buffer: xous::DropDeallocate,
}

#[cfg(keyos)]
impl HidInEndpoint {
    fn new(endpoint: UsbEmulatedEndpoint) -> Self {
        let buffer = xous::DropDeallocate::new(
            xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W | xous::MemoryFlags::POPULATE)
                .expect("Could not allocate IN buffer"),
        );
        Self { endpoint, buffer }
    }

    fn write_apdu(&mut self, channel_id: u16, apdu: &[u8]) {
        match hid::fragment(channel_id, apdu) {
            Ok(reports) => {
                log::trace!(
                    "Response APDU out ({} bytes) as {} HID reports: {:02x?}",
                    apdu.len(),
                    reports.len(),
                    apdu
                );
                for (i, report) in reports.iter().enumerate() {
                    self.buffer.as_slice_mut::<u8>()[..hid::REPORT_SIZE].copy_from_slice(report);
                    match self.endpoint.write_buf(*self.buffer, hid::REPORT_SIZE) {
                        Ok(_) => log::trace!("Rapdu: wrote HID report {}/{}", i + 1, reports.len()),
                        Err(
                            usb::error::UsbError::HostDisconnected | usb::error::UsbError::InterfaceDisabled,
                        ) => {
                            log::debug!("legacy-hid: endpoint gone; dropping outgoing APDU");
                            break;
                        }
                        Err(e) => {
                            log::error!("Rapdu: error writing HID report {}/{}: {e:?}", i + 1, reports.len())
                        }
                    }
                }
            }
            Err(e) => log::error!("HID fragmentation error: {e}"),
        }
    }
}

#[cfg(keyos)]
fn start_hid() -> (UsbRegisteredInterface, HidInEndpoint, EnabledGate) {
    let mut usb_api = UsbDeviceEmulation::default();
    let (hid_interface, [hid_ep_in, hid_ep_out]) = usb_api
        .register_interface(
            UsbInterfaceConfig::new(
                HID_INTERFACE_PRIORITY,
                HID_INTERFACE_CLASS,
                HID_INTERFACE_SUBCLASS,
                HID_INTERFACE_PROTOCOL,
                &HID_ENDPOINTS,
            )
            .with_functional_descriptors(&HID_FUNC_DESCRIPTOR)
            .with_setup_responder(Some(SetupResponder)),
        )
        .unwrap();

    // Registered but disabled: the interface enters the descriptors only while Legacy mode is
    // active (SetLegacyMode), riding the re-enumeration the identity switch causes anyway.
    let gate = EnabledGate::new(false);
    std::thread::spawn({
        let gate = gate.clone();
        move || out_thread(hid_ep_out, gate)
    });
    (hid_interface, HidInEndpoint::new(hid_ep_in), gate)
}

#[derive(server::Server)]
#[name = "os/legacy-hid"]
pub struct LegacyHidServer {
    #[cfg(keyos)]
    hid_interface: UsbRegisteredInterface,
    #[cfg(keyos)]
    hid_in: HidInEndpoint,
    #[cfg(keyos)]
    gate: EnabledGate,
    subscribers: Vec<ArchiveEventSubscriber<IncomingApdu>>,
}

impl Server for LegacyHidServer {}

impl Default for LegacyHidServer {
    fn default() -> Self {
        #[cfg(keyos)]
        let (hid_interface, hid_in, gate) = start_hid();
        Self {
            #[cfg(keyos)]
            hid_interface,
            #[cfg(keyos)]
            hid_in,
            #[cfg(keyos)]
            gate,
            subscribers: Vec::new(),
        }
    }
}

impl ArchiveHandler<DeliverHidApdu> for LegacyHidServer {
    fn handle(&mut self, msg: Owned<DeliverHidApdu>, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        let Ok(DeliverHidApdu { channel_id, data }) = msg.deserialize() else {
            log::error!("legacy-hid: failed to deserialize DeliverHidApdu");
            return;
        };
        // Inbound APDUs only ever arrive while the host is talking to the
        // Legacy USB identity (0x2C97:0x7011), and that identity is only
        // advertised while gui-app-emu-flux is on screen — so by the time
        // an APDU lands here we always have a subscriber. With no
        // subscribers the APDU is unreachable and we just drop it.
        if self.subscribers.is_empty() {
            log::warn!("legacy-hid: dropping inbound APDU — no subscribers");
            return;
        }
        let event = IncomingApdu { channel_id, data };
        self.subscribers.retain(|s| s.send(&event).is_ok());
    }
}

impl ArchiveHandler<WriteHidApdu> for LegacyHidServer {
    fn handle(&mut self, _msg: Owned<WriteHidApdu>, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        #[cfg(keyos)]
        {
            let Ok(WriteHidApdu { channel_id, data }) = _msg.deserialize() else {
                log::error!("legacy-hid: failed to deserialize WriteHidApdu");
                return;
            };
            log::trace!("WriteHidApdu: channel=0x{channel_id:04x} len={}", data.len());
            self.hid_in.write_apdu(channel_id, &data);
        }
    }
}

impl BlockingScalarHandler<SetLegacyMode> for LegacyHidServer {
    fn handle(
        &mut self,
        SetLegacyMode(_active): SetLegacyMode,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        #[cfg(keyos)]
        {
            // Each step re-enumerates on its own, so the order keeps every intermediate state
            // benign: the normal identity with the HID interface present.
            let mut usb_api = UsbDeviceEmulation::default();
            if _active {
                log::info!("Legacy Mode: enabling HID interface, switching USB identity to 0x2c97:0x7011");
                self.hid_interface
                    .set_enabled(true)
                    .expect("toggling a registered USB interface cannot fail");
                self.gate.set_enabled(true);
                // App-mode PID: hosts read the model from the high byte (0x70);
                // a bare 0x0007 reads as the bootloader identity they won't drive.
                usb_api.set_custom_vid_pid(Some(0x2c97), Some(0x7011));
            } else {
                log::info!("Legacy Mode: reverting USB identity, disabling HID interface");
                // Close the gate first, or out_thread spins on InterfaceDisabled between the
                // interface going down and the gate closing.
                self.gate.set_enabled(false);
                usb_api.set_custom_vid_pid(None, None);
                self.hid_interface
                    .set_enabled(false)
                    .expect("toggling a registered USB interface cannot fail");
            }
        }
    }
}

impl ArchiveEventSubscriptionHandler<SubscribeIncomingApdu> for LegacyHidServer {
    fn handle(
        &mut self,
        _msg: SubscribeIncomingApdu,
        subscriber: ArchiveEventSubscriber<IncomingApdu>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        log::debug!("New IncomingApdu subscriber: {subscriber:?}");
        self.subscribers.push(subscriber);
        Ok(())
    }
}
