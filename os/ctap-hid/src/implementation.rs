// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(all(keyos, not(feature = "test-app")))]
use std::sync::mpsc;
use std::{collections::BTreeMap, time::Instant};

use fido::messages::{Transport, U2fApduCommand};
#[cfg(not(test))]
use rgb_led::{RgbAnimation, RgbColor};
#[cfg(feature = "test-app")]
use server::{send_blocking_archive, BlockingScalarHandler};
use server::{BlockingArchiveHandler, Server, ServerContext};
#[cfg(all(keyos, not(feature = "test-app")))]
use usb::device::{
    api::{EndpointDirection, EndpointType},
    messages::{EndpointProperties, SetupPacketCallback},
};
#[cfg(keyos)]
use usb::device::{BLD_DEV_VERSION, MAJ_DEV_VERSION, MIN_DEV_VERSION};

#[cfg(feature = "test-app")]
use crate::messages::{RegisterSimuUsbReceiver, SimuUsbReceiveCallback};
use crate::{
    command::Command,
    error::CtapHidError,
    header::{CmdSeq, Header},
    messages::ProcessHidPacket,
};

#[cfg(not(test))]
fido::use_api!();
rgb_led::use_api!();

// Unit tests have no fido server to connect to, so use the stub in its place.
#[cfg(test)]
use fido::api::FidoApiStub as FidoApi;

#[cfg(all(keyos, not(feature = "test-app")))]
usb::use_device_api!();

#[cfg(all(keyos, not(feature = "test-app")))]
const USB_U2F_IFCE_CLASS: u8 = 0x03; // Human Interface Device Class
#[cfg(all(keyos, not(feature = "test-app")))]
const USB_U2F_IFCE_SUBCLASS: u8 = 0x00; // No Subclass
#[cfg(all(keyos, not(feature = "test-app")))]
const USB_U2F_IFCE_PROTOCOL: u8 = 0x00; // No Protocol
#[cfg(all(keyos, not(feature = "test-app")))]
const USB_U2F_IFCE_PRIORITY: u8 = usb::device::interface_priorities::CTAP_HID;
#[cfg(all(keyos, not(feature = "test-app")))]
const USB_U2F_ENDPOINTS: [EndpointProperties; 2] = [
    EndpointProperties {
        ep_type: EndpointType::Interrupt,
        ep_direction: EndpointDirection::Out,
        max_packet_len: 64,
        interval: 5,
        use_dma: true,
    },
    EndpointProperties {
        ep_type: EndpointType::Interrupt,
        ep_direction: EndpointDirection::In,
        max_packet_len: 64,
        interval: 5,
        use_dma: true,
    },
];
#[cfg(all(keyos, not(feature = "test-app")))]
const USB_U2F_FUNC_DESCRIPTOR: [u8; 9] = [
    0x09, // bLength: 9
    0x21, // bDescriptorType: HID
    0x11, 0x01, // bcdHID: 1.11
    0x21, // bCountryCode: US
    0x01, // bNumDescriptors: 1
    0x22, // bDescriptorType: Report
    34, 0, // wDescriptorLength: 34
];
#[cfg(all(keyos, not(feature = "test-app")))]
const USB_U2F_REPORT_DESCRIPTOR: [u8; 34] = [
    0x06, 0xD0, 0xF1, // Usage Page: FIDO Alliance Page
    0x09, 0x01, // Usage: U2F Authenticator Device
    0xA1, 0x01, // Collection: Application
    0x09, 0x20, // Usage: Input Report Data (HID spec)
    0x15, 0x00, // Logical Minimum: 0
    0x26, 0xFF, 0x00, // Logical Maximum: 255
    0x75, 0x08, // Report Size: 8 bits
    0x95, 64, // Report Count: 64 fields (must be same as EP max_packet_len)
    0x81, 0x02, // Input: Data | Variable | Absolute (U2F spec)
    0x09, 0x21, // Usage: Output Report Data (HID spec)
    0x15, 0x00, // Logical Minimum: 0
    0x26, 0xFF, 0x00, // Logical Maximum: 255
    0x75, 0x08, // Report Size: 8 bits
    0x95, 64, // Report Count: 64 fields (must be same as EP max_packet_len)
    0x91, 0x02, // Output: Data | Variable | Absolute (U2F spec)
    0xC0, // End Collection
];

#[cfg(not(keyos))]
const MAJ_DEV_VERSION: u8 = 1;
#[cfg(not(keyos))]
const MIN_DEV_VERSION: u8 = 0;
#[cfg(not(keyos))]
const BLD_DEV_VERSION: u8 = 0;

const CTAPHID_BROADCAST_CID: u32 = u32::MAX;
const CTAPHID_PROTOCOL_VERSION: u8 = 2;

/// Transaction timeout in milliseconds (per CTAP2 spec, typically 3 seconds)
const TRANSACTION_TIMEOUT_MS: u64 = 3000;
/// Maximum lock duration in seconds (per U2FHID spec)
const MAX_LOCK_DURATION_S: u8 = 10;

#[cfg(all(keyos, not(feature = "test-app")))]
#[derive(Default)]
pub(crate) struct SetupResponder;
#[cfg(all(keyos, not(feature = "test-app")))]
impl server::ServerMessages for SetupResponder {
    const NAME: &'static str = "";

    fn messages() -> &'static [server::MessageDef<Self>]
    where
        Self: Sized,
    {
        use server::MessageId;
        use usb::device::messages::SetupPacketCallback;
        &[(SetupPacketCallback::ID, server::handle_blocking_archive_message::<SetupPacketCallback, _>)]
    }
}
#[cfg(all(keyos, not(feature = "test-app")))]
impl Server for SetupResponder {}

#[cfg(all(keyos, not(feature = "test-app")))]
impl BlockingArchiveHandler<SetupPacketCallback> for SetupResponder {
    fn handle(
        &mut self,
        SetupPacketCallback(msg): SetupPacketCallback,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> Option<Vec<u8>> {
        log::debug!("Setup packet: {msg:02x?}");
        if msg.request_type == 0x81 && msg.request == 0x06 {
            // HID GET_DESCRIPTOR
            if msg.value == 0x2200 {
                // REPORT_DESC
                Some(USB_U2F_REPORT_DESCRIPTOR.to_vec())
            } else if msg.value == 0x2100 {
                // DESCRIPTOR_TYPE
                Some(USB_U2F_FUNC_DESCRIPTOR.to_vec())
            } else {
                None
            }
        } else if msg.request_type == 0x21 && msg.request == 0x0a {
            // HID SET_IDLE
            Some(vec![])
        } else {
            None
        }
    }
}

#[derive(Debug, Default)]
pub struct Channel {
    pub fido: FidoApi,
    pub cid: u32,
    pub cmd: Command,
    pub payload_len: u16,
    pub buf: Vec<u8>,
    pub prev_seq: u8,
    pub new_cid: Option<u32>,
}

impl Channel {
    fn process(&mut self) -> (Command, Vec<u8>) {
        match self.cmd {
            Command::Ping => {
                log::info!("CTAPHID_PING");
                (Command::Ping, self.buf.clone())
            }
            Command::Init => {
                log::info!("CTAPHID_INIT");
                if self.buf.len() == 8 {
                    // self.buf already contain the nonce from the Request
                    self.buf.extend_from_slice(&self.new_cid.unwrap_or(self.cid).to_be_bytes());
                    self.buf.push(CTAPHID_PROTOCOL_VERSION);
                    self.buf.push(MAJ_DEV_VERSION);
                    self.buf.push(MIN_DEV_VERSION);
                    self.buf.push(BLD_DEV_VERSION);
                    self.buf.push(0x05); // Capabilities flags: CAPABILITY_WINK | CAPABILITY_CBOR
                    (Command::Init, self.buf.clone())
                } else {
                    log::error!("CTAPHID_INIT wrong payload length {}", self.buf.len());
                    CtapHidError::InvalidPayloadLen.to_cmd_payload()
                }
            }
            Command::Message => {
                log::debug!("CTAPHID_MSG");
                if self.buf.len() >= 4 {
                    let payload = match U2fApduCommand::parse(&self.buf) {
                        Ok(command) => self.fido.u2f_process_apdu(command, Transport::Usb),
                        Err(e) => {
                            log::error!("CTAPHID_MSG: invalid U2F APDU: {e:?}");
                            e.to_u2f_response()
                        }
                    };
                    log::debug!("CTAPHID_MSG response: {payload:02x?}");
                    (Command::Message, payload)
                } else {
                    log::error!("CTAPHID_MSG: wrong payload length {}", self.buf.len());
                    CtapHidError::InvalidPayloadLen.to_cmd_payload()
                }
            }
            Command::Wink => {
                log::debug!("CTAPHID_WINK");
                if self.payload_len == 0 {
                    #[cfg(not(test))]
                    RgbApi::default().animate_all(RgbAnimation::new(
                        RgbColor::new(0xF1, 0xD0, 0x00),
                        RgbColor::new(0x00, 0xF1, 0xD0),
                        1000,
                        true,
                    ));
                    (Command::Wink, vec![])
                } else {
                    log::error!("CTAPHID_WINK: wrong payload length {}", self.payload_len);
                    CtapHidError::InvalidPayloadLen.to_cmd_payload()
                }
            }
            Command::Cbor => {
                log::debug!("CTAPHID_CBOR");
                if self.buf.len() >= 1 {
                    let payload = self.fido.ctap_process_cbor(self.buf[0], self.buf[1..].to_vec());
                    (Command::Cbor, payload)
                } else {
                    log::error!("CTAPHID_CBOR: wrong payload length {}", self.buf.len());
                    CtapHidError::InvalidPayloadLen.to_cmd_payload()
                }
            }
            Command::Cancel => {
                log::info!("CTAPHID_CANCEL");
                if self.payload_len == 0 {
                    // Cancel any outstanding requests on this CID. If there is an outstanding request that
                    // can be cancelled, the authenticator MUST cancel it and that cancelled request will
                    // reply with the error CTAP2_ERR_KEEPALIVE_CANCEL. As the
                    // CTAPHID_CANCEL command is sent during an ongoing transaction,
                    // transaction semantics do not apply. Whether a request was cancelled
                    // or not, the authenticator MUST NOT reply to the CTAPHID_CANCEL
                    // message itself. The CTAPHID_CANCEL command MAY be sent by the client during
                    // ongoing processing of a CTAPHID_CBOR request. The CTAP2_ERR_KEEPALIVE_CANCEL response
                    // MUST be the response to that request, not an error response in the
                    // HID transport. A CTAPHID_CANCEL received while no CTAPHID_CBOR
                    // request is being processed, or on a non-active CID SHALL be ignored
                    // by the authenticator.
                    //
                    // Note: In the current synchronous architecture, CBOR processing blocks until
                    // completion. User presence cancellation is handled by the fido crate via GUI
                    // navigation. We return a special marker (Cancel command with empty payload)
                    // to signal the server not to send a response.
                    (Command::Cancel, vec![])
                } else {
                    log::error!("CTAPHID_CANCEL: wrong payload length {}", self.payload_len);
                    CtapHidError::InvalidPayloadLen.to_cmd_payload()
                }
            }
            Command::Error => {
                log::error!("CTAPHID_ERROR");
                // should not be received by the authenticator
                CtapHidError::InvalidCommand.to_cmd_payload()
            }
            Command::Lock => {
                log::info!("CTAPHID_LOCK");
                // The lock command places an exclusive lock for one channel to communicate with the
                // device. As long as the lock is active, any other channel trying to send a message will
                // fail. In order to prevent a stalling or crashing application to lock the device
                // indefinitely, a lock time up to 10 seconds MAY be set. An application requiring a longer
                // lock has to send repeating lock commands to maintain the lock.
                //
                // Return Lock command with the duration byte as payload - the server will handle
                // the actual lock acquisition and respond appropriately.
                if self.buf.len() == 1 {
                    (Command::Lock, self.buf.clone())
                } else {
                    log::error!("CTAPHID_LOCK: wrong payload length {}", self.buf.len());
                    CtapHidError::InvalidPayloadLen.to_cmd_payload()
                }
            }
            Command::KeepAlive => {
                log::error!("CTAPHID_KEEPALIVE");
                // should not be received by the authenticator
                CtapHidError::InvalidCommand.to_cmd_payload()
            }
        }
    }
}

#[derive(server::Server)]
#[name = "os/ctap-hid"]
pub struct CtapHidServer {
    #[cfg(all(keyos, not(feature = "test-app")))]
    usb_ep_sender: mpsc::Sender<Vec<u8>>,
    pub channels: BTreeMap<u32, Channel>,
    #[cfg(feature = "test-app")]
    simu_usb_receiver: Option<xous::CID>,
    /// CID of the currently active transaction (if any)
    active_cid: Option<u32>,
    /// When the current transaction started
    transaction_start: Option<Instant>,
    /// CID holding an exclusive lock (if any)
    locked_cid: Option<u32>,
    /// When the exclusive lock expires
    lock_expiry: Option<Instant>,
}

impl Server for CtapHidServer {}

#[cfg(all(keyos, not(feature = "test-app")))]
#[derive(Debug, Default, Clone)]
struct InternalPermissions;

#[cfg(all(keyos, not(feature = "test-app")))]
impl server::CheckedPermissions for InternalPermissions {
    const NAME: &str = "os/ctap-hid";
}

#[cfg(all(keyos, not(feature = "test-app")))]
impl server::MessageAllowed<crate::messages::ProcessHidPacket> for InternalPermissions {}

#[cfg(all(keyos, not(feature = "test-app")))]
fn receive_request_thread(mut ep_out: UsbEmulatedEndpoint) {
    let usb_api = UsbDeviceEmulation::default();
    let out_buffer = xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W | xous::MemoryFlags::POPULATE)
        .expect("Could not allocate buffer");
    let mut api = crate::api::CtapHidApi::<InternalPermissions>::default();

    loop {
        match ep_out.read_buf(out_buffer, 64) {
            Ok(pkt_len) => {
                let pkt = &out_buffer.as_slice::<u8>()[..pkt_len];
                log::trace!(
                    "Read {pkt_len} bytes from endpoint {:02x} : {:02x?}",
                    ep_out.endpoint_number(),
                    pkt
                );
                api.process_hid_packet(pkt);
            }
            Err(e) => match e {
                usb::error::UsbError::HostDisconnected => {
                    usb_api.wait_for_connection().expect("Error waiting for connection");
                }
                _ => log::error!("Error while reading from USB: {e:?}"),
            },
        }
    }
}

#[cfg(all(keyos, not(feature = "test-app")))]
fn send_response_thread(mut ep_in: UsbEmulatedEndpoint, receiver: mpsc::Receiver<Vec<u8>>) {
    loop {
        let pkt = receiver.recv().unwrap();
        let mut buffer =
            xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W).expect("Could not allocate buffer");
        buffer.as_slice_mut()[..pkt.len()].copy_from_slice(&pkt);
        log::trace!("Write {} bytes to endpoint {:02x} : {:02x?}", pkt.len(), ep_in.endpoint_number(), &pkt);
        if let Err(e) = ep_in.write_buf(buffer, pkt.len()) {
            match e {
                usb::error::UsbError::HostDisconnected => {
                    log::debug!("Waiting for connection");
                    UsbDeviceEmulation::default()
                        .wait_for_connection()
                        .expect("Error waiting for connection");
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                _ => log::error!("Error while writing to USB: {e:?}"),
            }
        }
    }
}

impl CtapHidServer {
    pub fn new() -> Result<Self, CtapHidError> {
        #[cfg(all(keyos, not(feature = "test-app")))]
        let usb_ep_sender = {
            let mut usb_api = UsbDeviceEmulation::default();
            let (usb_interface, [ep_out, ep_in]) = usb_api.register_interface(
                UsbInterfaceConfig::new(
                    USB_U2F_IFCE_PRIORITY,
                    USB_U2F_IFCE_CLASS,
                    USB_U2F_IFCE_SUBCLASS,
                    USB_U2F_IFCE_PROTOCOL,
                    &USB_U2F_ENDPOINTS,
                )
                .with_functional_descriptors(&USB_U2F_FUNC_DESCRIPTOR)
                .with_setup_responder(Some(SetupResponder)),
            )?;
            usb_interface.set_enabled(true)?;
            std::thread::spawn(|| receive_request_thread(ep_out));
            let (usb_ep_sender, receiver_ep_in) = mpsc::channel();
            std::thread::spawn(|| send_response_thread(ep_in, receiver_ep_in));
            usb_ep_sender
        };

        let mut channels = BTreeMap::new();
        // make sure we already know about the broadcast channel
        channels.insert(CTAPHID_BROADCAST_CID, Channel::default());

        Ok(Self {
            #[cfg(all(keyos, not(feature = "test-app")))]
            usb_ep_sender,
            channels,
            #[cfg(feature = "test-app")]
            simu_usb_receiver: None,
            active_cid: None,
            transaction_start: None,
            locked_cid: None,
            lock_expiry: None,
        })
    }

    fn channel(&mut self, cid: u32) -> Option<&mut Channel> { self.channels.get_mut(&cid) }

    /// Check if transaction has timed out and clear it if so
    fn check_transaction_timeout(&mut self) {
        if let (Some(_active_cid), Some(start)) = (self.active_cid, self.transaction_start) {
            let elapsed = start.elapsed().as_millis() as u64;
            if elapsed > TRANSACTION_TIMEOUT_MS {
                log::warn!("Transaction timed out after {}ms, clearing stalled transaction", elapsed);
                self.active_cid = None;
                self.transaction_start = None;
            }
        }
    }

    /// Check if lock has expired and clear it if so
    fn check_lock_expiry(&mut self) {
        if let Some(expiry) = self.lock_expiry {
            if Instant::now() >= expiry {
                log::debug!("Lock expired for CID {:08x}", self.locked_cid.unwrap_or(0));
                self.locked_cid = None;
                self.lock_expiry = None;
            }
        }
    }

    /// Check if the device is busy for the given CID
    /// Returns Some(error) if busy, None if the CID can proceed
    fn check_busy(&self, cid: u32, cmd: Command) -> Option<(Header, Vec<u8>)> {
        // CANCEL command is always allowed (per CTAP2 spec)
        if cmd == Command::Cancel {
            return None;
        }

        // Check if another channel holds the lock
        if let Some(locked_cid) = self.locked_cid {
            if locked_cid != cid {
                log::debug!("CID {:08x} rejected: device locked by CID {:08x}", cid, locked_cid);
                return Some(CtapHidError::BusyChannel.to_msg(cid));
            }
        }

        // Check if another channel has an active transaction
        if let Some(active_cid) = self.active_cid {
            if active_cid != cid {
                log::debug!("CID {:08x} rejected: transaction active on CID {:08x}", cid, active_cid);
                return Some(CtapHidError::BusyChannel.to_msg(cid));
            }
        }

        None
    }

    /// Start tracking a new transaction
    fn start_transaction(&mut self, cid: u32) {
        self.active_cid = Some(cid);
        self.transaction_start = Some(Instant::now());
    }

    /// End the current transaction
    fn end_transaction(&mut self) {
        self.active_cid = None;
        self.transaction_start = None;
    }

    /// Acquire an exclusive lock for the given CID
    fn acquire_lock(&mut self, cid: u32, duration_s: u8) -> Result<(), CtapHidError> {
        if duration_s == 0 {
            // Release lock
            if self.locked_cid == Some(cid) {
                log::debug!("CID {:08x} released lock", cid);
                self.locked_cid = None;
                self.lock_expiry = None;
            }
            Ok(())
        } else if duration_s <= MAX_LOCK_DURATION_S {
            // Acquire or extend lock
            log::debug!("CID {:08x} acquired lock for {}s", cid, duration_s);
            self.locked_cid = Some(cid);
            self.lock_expiry = Some(Instant::now() + std::time::Duration::from_secs(duration_s as u64));
            Ok(())
        } else {
            Err(CtapHidError::InvalidParam)
        }
    }

    /// Process the channel's command and handle server-level commands (Lock, Cancel)
    /// Returns None if no response should be sent (e.g., for Cancel command)
    fn process_channel_command(&mut self, cid: u32) -> Option<(Command, Vec<u8>)> {
        let channel = self.channels.get_mut(&cid)?;
        let (cmd, payload) = channel.process();

        match cmd {
            Command::Cancel => {
                // Per spec: authenticator MUST NOT reply to CANCEL itself
                // The cancel affects user presence handling in the fido crate
                log::debug!("CANCEL processed for CID {:08x}, no response sent", cid);
                None
            }
            Command::Lock => {
                // Handle lock acquisition at server level
                if payload.len() == 1 {
                    let duration_s = payload[0];
                    match self.acquire_lock(cid, duration_s) {
                        Ok(()) => Some((Command::Lock, vec![])),
                        Err(e) => Some(e.to_cmd_payload()),
                    }
                } else {
                    Some(CtapHidError::InvalidPayloadLen.to_cmd_payload())
                }
            }
            _ => Some((cmd, payload)),
        }
    }

    fn create_channel(&mut self) -> u32 {
        let mut cid = rand::random();
        while self.channels.contains_key(&cid) {
            cid = rand::random();
        }
        self.channels.insert(
            cid,
            Channel {
                fido: FidoApi::default(),
                cid,
                cmd: Default::default(),
                payload_len: 0,
                buf: Vec::new(),
                prev_seq: 0,
                new_cid: None,
            },
        );
        cid
    }

    fn process(&mut self, pkt: &[u8]) -> Option<(Header, Vec<u8>)> {
        log::trace!("CTAPHID process: {:02x?}", &pkt);

        // Check for expired timeouts and locks before processing
        self.check_transaction_timeout();
        self.check_lock_expiry();

        match Header::deserialize(pkt) {
            Ok((header, payload)) => {
                match header.cmd_seq {
                    CmdSeq::Cmd { cmd, payload_len } => {
                        // Initialization packet - check busy state
                        if let Some(busy_response) = self.check_busy(header.cid, cmd) {
                            return Some(busy_response);
                        }

                        // Start tracking this transaction
                        self.start_transaction(header.cid);

                        let new_cid = if header.cid == CTAPHID_BROADCAST_CID && cmd == Command::Init {
                            Some(self.create_channel())
                        } else {
                            None
                        };
                        if let Some(channel) = self.channel(header.cid) {
                            channel.cid = header.cid;
                            channel.buf = payload[..payload.len().min(payload_len as usize)].to_vec();
                            channel.payload_len = payload_len;
                            channel.cmd = cmd;
                            channel.prev_seq = 0;
                            channel.new_cid = new_cid;
                            if payload_len <= payload.len() as u16 {
                                // Single packet Message - process and end transaction
                                let result = self.process_channel_command(header.cid);
                                self.end_transaction();
                                result.map(|(cmd, payload)| {
                                    let header = Header::new(
                                        header.cid,
                                        CmdSeq::Cmd { cmd, payload_len: payload.len() as u16 },
                                    );
                                    (header, payload)
                                })
                            } else {
                                // Multi-packet message, waiting for continuation packets
                                None
                            }
                        } else {
                            self.end_transaction();
                            log::error!("Unknown CID {}", header.cid);
                            Some(CtapHidError::InvalidChannel.to_msg(header.cid))
                        }
                    }
                    CmdSeq::Seq(seq) => {
                        // Continuation packet - must be for the active transaction
                        if self.active_cid != Some(header.cid) {
                            // Spurious continuation packet - ignore per spec
                            log::warn!(
                                "Ignoring continuation packet for CID {:08x} (active: {:?})",
                                header.cid,
                                self.active_cid
                            );
                            return None;
                        }

                        if let Some(channel) = self.channel(header.cid) {
                            if seq == 0 || seq == channel.prev_seq + 1 {
                                channel.prev_seq = seq;
                                let missing_len = (channel.payload_len as usize) - channel.buf.len();
                                channel.buf.extend_from_slice(&payload[..payload.len().min(missing_len)]);
                                if channel.payload_len <= channel.buf.len() as u16 {
                                    // Last packet of Message - process and end transaction
                                    let result = self.process_channel_command(header.cid);
                                    self.end_transaction();
                                    result.map(|(cmd, payload)| {
                                        let header = Header::new(
                                            header.cid,
                                            CmdSeq::Cmd { cmd, payload_len: payload.len() as u16 },
                                        );
                                        (header, payload)
                                    })
                                } else {
                                    None
                                }
                            } else {
                                self.end_transaction();
                                log::error!("Seq out of order {}", seq);
                                Some(CtapHidError::InvalidSequence.to_msg(header.cid))
                            }
                        } else {
                            self.end_transaction();
                            log::error!("Unknown CID {}", header.cid);
                            Some(CtapHidError::InvalidChannel.to_msg(header.cid))
                        }
                    }
                }
            }
            Err(e) => {
                log::error!("Failed to deserialize header: {:?}", e);
                Some(e.to_msg(CTAPHID_BROADCAST_CID))
            }
        }
    }

    fn send_hid_packet(&mut self, pkt: Vec<u8>) {
        log::trace!("CTAPHID send_hid_packet: {:02x?}", pkt);
        #[cfg(all(keyos, not(feature = "test-app")))]
        self.usb_ep_sender.send(pkt).unwrap();
        #[cfg(feature = "test-app")]
        if let Some(simu_usb_receiver) = self.simu_usb_receiver {
            send_blocking_archive(simu_usb_receiver, SimuUsbReceiveCallback(pkt))
        }
    }

    fn send_response(&mut self, mut header: Header, payload: Vec<u8>) {
        let mut pkt = header.serialize();
        pkt.extend_from_slice(&payload);
        pkt.resize(64, 0);
        self.send_hid_packet(pkt);

        let used = 64 - header.len();
        if payload.len() > used {
            let payload = &payload[used..];
            for (i, chunk) in payload.chunks(64 - Header::LEN).enumerate() {
                header.cmd_seq = CmdSeq::Seq(i as u8);
                let mut pkt = header.serialize();
                pkt.extend_from_slice(chunk);
                pkt.resize(64, 0);
                self.send_hid_packet(pkt);
            }
        }
    }
}

impl BlockingArchiveHandler<ProcessHidPacket> for CtapHidServer {
    fn handle(&mut self, msg: ProcessHidPacket, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        let resp = self.process(&msg.0);
        if let Some(resp) = resp {
            self.send_response(resp.0, resp.1);
        }
    }
}

#[cfg(feature = "test-app")]
impl BlockingScalarHandler<RegisterSimuUsbReceiver> for CtapHidServer {
    fn handle(
        &mut self,
        msg: RegisterSimuUsbReceiver,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), CtapHidError> {
        self.simu_usb_receiver = Some(msg.0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_very_broken() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let pkt = [0xff, 0xff];
        assert_eq!(
            ctaphid.process(&pkt),
            Some(
                // CTAPHID_ERROR response on CTAPHID_BROADCAST_CID with payload ERR_INVALID_PAR
                (
                    Header {
                        cid: CTAPHID_BROADCAST_CID,
                        cmd_seq: CmdSeq::Cmd { cmd: Command::Error, payload_len: 1 },
                    },
                    vec![2]
                )
            )
        );
    }

    #[test]
    fn request_init_valid() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        // CTAPHID_INIT request on broadcast
        let pkt = [
            0xff, 0xff, 0xff, 0xff, 0x86, 0x00, 0x08, 0x12, 0x23, 0x34, 0x45, 0x56, 0x67, 0x78, 0x89, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let reply = ctaphid.process(&pkt);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        // CTAPHID_INIT response on CTAPHID_BROADCAST_CID with a new CID
        assert_eq!(
            reply.0,
            Header {
                cid: CTAPHID_BROADCAST_CID,
                cmd_seq: CmdSeq::Cmd { cmd: Command::Init, payload_len: 17 }
            }
        );
        let msg = reply.1;
        assert_eq!(&msg[..8], &[0x12, 0x23, 0x34, 0x45, 0x56, 0x67, 0x78, 0x89]);
        let new_cid = u32::from_be_bytes(msg[8..12].try_into().unwrap());
        assert_eq!(&msg[12..], &[0x02, 0x01, 0x00, 0x00, 0x05]);
        // CTAPHID_INIT request on unicast channel just created
        let pkt2 = [
            msg[8], msg[9], msg[10], msg[11], 0x86, 0x00, 0x08, 0x12, 0x23, 0x34, 0x45, 0x56, 0x67, 0x78,
            0x89, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let reply = ctaphid.process(&pkt2);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        let msg2 = reply.1;
        // CTAPHID_INIT response on same CID with own CID
        assert_eq!(
            reply.0,
            Header { cid: new_cid, cmd_seq: CmdSeq::Cmd { cmd: Command::Init, payload_len: 17 } }
        );
        assert_eq!(
            &msg2,
            &[
                0x12, 0x23, 0x34, 0x45, 0x56, 0x67, 0x78, 0x89, msg[8], msg[9], msg[10], msg[11], 0x02, 0x01,
                0x00, 0x00, 0x05,
            ]
        );
    }

    #[test]
    fn request_init_invalid_cid() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let pkt = [
            0xaa, 0xbb, 0xcc, 0xdd, 0x86, 0x00, 0x08, 0x12, 0x23, 0x34, 0x45, 0x56, 0x67, 0x78, 0x89, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(
            ctaphid.process(&pkt),
            Some(
                // CTAPHID_ERROR response on same CID with payload ERR_INVALID_CHANNEL
                (
                    Header { cid: 0xaabbccdd, cmd_seq: CmdSeq::Cmd { cmd: Command::Error, payload_len: 1 } },
                    vec![0x0B]
                )
            )
        );
    }

    #[test]
    fn request_cbor_valid() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        // CTAPHID_INIT request on broadcast
        let pkt = [
            0xff, 0xff, 0xff, 0xff, 0x86, 0x00, 0x08, 0x12, 0x23, 0x34, 0x45, 0x56, 0x67, 0x78, 0x89, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let reply = ctaphid.process(&pkt).unwrap();
        let msg = reply.1;
        let _new_cid = u32::from_be_bytes(msg[8..12].try_into().unwrap());
        // CTAPHID_CBOR request on unicast channel just created with authenticatorGetInfo
        let _pkt2 = [
            msg[8], msg[9], msg[10], msg[11], 0x90, 0x00, 0x01, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        // let reply = ctaphid.process(&pkt2);
        // assert!(reply.is_some());
        // let reply = reply.unwrap();
        // let msg2 = reply.1;
        // // CTAPHID_CBOR response Initialization packet with CTAP1_ERR_SUCCESS
        // println!("msg2: {:02x?}", msg2);
        // assert_eq!(
        //     reply.0,
        //     Header { cid: new_cid, cmd_seq: CmdSeq::Cmd { cmd: Command::Cbor, payload_len: 29 } }
        // );
        // assert_eq!(&msg2, &[0x00, 0xa2, 0x01, 0x81, 0x66, 0x55, 0x32, 0x46, 0x5f, 0x56, 0x32, 0x03,
        // 0x50,]);
    }

    /// Helper to create a channel and return its CID
    fn create_test_channel(ctaphid: &mut CtapHidServer) -> u32 {
        // CTAPHID_INIT request on broadcast
        let pkt = [
            0xff, 0xff, 0xff, 0xff, 0x86, 0x00, 0x08, 0x12, 0x23, 0x34, 0x45, 0x56, 0x67, 0x78, 0x89, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let reply = ctaphid.process(&pkt).unwrap();
        let msg = reply.1;
        u32::from_be_bytes(msg[8..12].try_into().unwrap())
    }

    /// Helper to build a CTAPHID packet
    fn build_packet(cid: u32, cmd: u8, payload: &[u8]) -> [u8; 64] {
        let mut pkt = [0u8; 64];
        pkt[0..4].copy_from_slice(&cid.to_be_bytes());
        pkt[4] = 0x80 | cmd; // Command byte with high bit set
        pkt[5..7].copy_from_slice(&(payload.len() as u16).to_be_bytes());
        let copy_len = payload.len().min(57); // Max payload in init packet
        pkt[7..7 + copy_len].copy_from_slice(&payload[..copy_len]);
        pkt
    }

    #[test]
    fn request_lock_acquire_and_release() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid = create_test_channel(&mut ctaphid);

        // Acquire lock for 5 seconds
        let pkt = build_packet(cid, 0x04, &[5]); // CTAPHID_LOCK = 0x04
        let reply = ctaphid.process(&pkt);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        assert_eq!(reply.0, Header { cid, cmd_seq: CmdSeq::Cmd { cmd: Command::Lock, payload_len: 0 } });
        assert!(ctaphid.locked_cid == Some(cid));
        assert!(ctaphid.lock_expiry.is_some());

        // Release lock (duration = 0)
        let pkt = build_packet(cid, 0x04, &[0]);
        let reply = ctaphid.process(&pkt);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        assert_eq!(reply.0, Header { cid, cmd_seq: CmdSeq::Cmd { cmd: Command::Lock, payload_len: 0 } });
        assert!(ctaphid.locked_cid.is_none());
        assert!(ctaphid.lock_expiry.is_none());
    }

    #[test]
    fn request_lock_invalid_duration() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid = create_test_channel(&mut ctaphid);

        // Try to acquire lock for 11 seconds (> MAX_LOCK_DURATION_S)
        let pkt = build_packet(cid, 0x04, &[11]);
        let reply = ctaphid.process(&pkt);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        // Should get ERR_INVALID_PAR error
        assert_eq!(reply.0, Header { cid, cmd_seq: CmdSeq::Cmd { cmd: Command::Error, payload_len: 1 } });
        assert_eq!(reply.1, vec![0x02]); // ERR_INVALID_PAR
        assert!(ctaphid.locked_cid.is_none());
    }

    #[test]
    fn request_lock_invalid_payload_len() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid = create_test_channel(&mut ctaphid);

        // Send LOCK with wrong payload length (2 bytes instead of 1)
        let pkt = build_packet(cid, 0x04, &[5, 0]);
        let reply = ctaphid.process(&pkt);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        // Should get ERR_INVALID_LEN error
        assert_eq!(reply.0, Header { cid, cmd_seq: CmdSeq::Cmd { cmd: Command::Error, payload_len: 1 } });
        assert_eq!(reply.1, vec![0x03]); // ERR_INVALID_LEN
    }

    #[test]
    fn busy_channel_when_locked() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid1 = create_test_channel(&mut ctaphid);
        let cid2 = create_test_channel(&mut ctaphid);

        // Channel 1 acquires lock
        let pkt = build_packet(cid1, 0x04, &[5]);
        let _reply = ctaphid.process(&pkt);
        assert!(ctaphid.locked_cid == Some(cid1));

        // Channel 2 tries to send a PING - should get BUSY error
        let pkt = build_packet(cid2, 0x01, &[]); // CTAPHID_PING = 0x01
        let reply = ctaphid.process(&pkt);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        assert_eq!(
            reply.0,
            Header { cid: cid2, cmd_seq: CmdSeq::Cmd { cmd: Command::Error, payload_len: 1 } }
        );
        assert_eq!(reply.1, vec![0x06]); // ERR_CHANNEL_BUSY

        // Channel 1 can still communicate
        let pkt = build_packet(cid1, 0x01, &[0xaa, 0xbb]);
        let reply = ctaphid.process(&pkt);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        assert_eq!(
            reply.0,
            Header { cid: cid1, cmd_seq: CmdSeq::Cmd { cmd: Command::Ping, payload_len: 2 } }
        );
        assert_eq!(reply.1, vec![0xaa, 0xbb]);
    }

    #[test]
    fn request_cancel_no_response() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid = create_test_channel(&mut ctaphid);

        // Send CANCEL command
        let pkt = build_packet(cid, 0x11, &[]); // CTAPHID_CANCEL = 0x11
        let reply = ctaphid.process(&pkt);
        // Per spec: authenticator MUST NOT reply to CANCEL
        assert!(reply.is_none());
    }

    #[test]
    fn request_cancel_invalid_payload() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid = create_test_channel(&mut ctaphid);

        // Send CANCEL command with non-zero payload
        let pkt = build_packet(cid, 0x11, &[0x01]);
        let reply = ctaphid.process(&pkt);
        // Should get ERR_INVALID_LEN error
        assert!(reply.is_some());
        let reply = reply.unwrap();
        assert_eq!(reply.0, Header { cid, cmd_seq: CmdSeq::Cmd { cmd: Command::Error, payload_len: 1 } });
        assert_eq!(reply.1, vec![0x03]); // ERR_INVALID_LEN
    }

    #[test]
    fn cancel_allowed_when_busy() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid1 = create_test_channel(&mut ctaphid);
        let cid2 = create_test_channel(&mut ctaphid);

        // Channel 1 acquires lock
        let pkt = build_packet(cid1, 0x04, &[5]);
        let _reply = ctaphid.process(&pkt);
        assert!(ctaphid.locked_cid == Some(cid1));

        // Channel 2 tries to send CANCEL - should be allowed (not get BUSY error)
        let pkt = build_packet(cid2, 0x11, &[]);
        let reply = ctaphid.process(&pkt);
        // CANCEL returns None (no response), not a BUSY error
        assert!(reply.is_none());
    }

    #[test]
    fn busy_channel_during_multi_packet_transaction() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid1 = create_test_channel(&mut ctaphid);
        let cid2 = create_test_channel(&mut ctaphid);

        // Channel 1 starts a multi-packet message (PING with payload > 57 bytes)
        let mut pkt = [0u8; 64];
        pkt[0..4].copy_from_slice(&cid1.to_be_bytes());
        pkt[4] = 0x81; // CTAPHID_PING
        pkt[5..7].copy_from_slice(&100u16.to_be_bytes()); // 100 bytes payload
                                                          // First init packet contains 57 bytes of payload
        for i in 7..64 {
            pkt[i] = i as u8;
        }
        let reply = ctaphid.process(&pkt);
        // Should return None, waiting for continuation packet
        assert!(reply.is_none());
        assert_eq!(ctaphid.active_cid, Some(cid1));

        // Channel 2 tries to send a request - should get BUSY
        let pkt2 = build_packet(cid2, 0x01, &[]);
        let reply = ctaphid.process(&pkt2);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        assert_eq!(
            reply.0,
            Header { cid: cid2, cmd_seq: CmdSeq::Cmd { cmd: Command::Error, payload_len: 1 } }
        );
        assert_eq!(reply.1, vec![0x06]); // ERR_CHANNEL_BUSY
    }

    #[test]
    fn spurious_continuation_packet_ignored() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid1 = create_test_channel(&mut ctaphid);
        let cid2 = create_test_channel(&mut ctaphid);

        // Channel 1 starts a multi-packet message
        let mut pkt = [0u8; 64];
        pkt[0..4].copy_from_slice(&cid1.to_be_bytes());
        pkt[4] = 0x81; // CTAPHID_PING
        pkt[5..7].copy_from_slice(&100u16.to_be_bytes());
        let reply = ctaphid.process(&pkt);
        assert!(reply.is_none());
        assert_eq!(ctaphid.active_cid, Some(cid1));

        // Channel 2 sends a continuation packet (should be ignored per spec)
        let mut cont_pkt = [0u8; 64];
        cont_pkt[0..4].copy_from_slice(&cid2.to_be_bytes());
        cont_pkt[4] = 0x00; // SEQ = 0 (continuation packet)
        let reply = ctaphid.process(&cont_pkt);
        // Should be ignored (return None, not an error)
        assert!(reply.is_none());
        // Active transaction should still be for cid1
        assert_eq!(ctaphid.active_cid, Some(cid1));
    }

    #[test]
    fn request_ping_valid() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid = create_test_channel(&mut ctaphid);

        // PING with some payload
        let pkt = build_packet(cid, 0x01, &[0x11, 0x22, 0x33, 0x44]);
        let reply = ctaphid.process(&pkt);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        assert_eq!(reply.0, Header { cid, cmd_seq: CmdSeq::Cmd { cmd: Command::Ping, payload_len: 4 } });
        assert_eq!(reply.1, vec![0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn request_wink_valid() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid = create_test_channel(&mut ctaphid);

        // WINK with no payload
        let pkt = build_packet(cid, 0x08, &[]); // CTAPHID_WINK = 0x08
        let reply = ctaphid.process(&pkt);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        assert_eq!(reply.0, Header { cid, cmd_seq: CmdSeq::Cmd { cmd: Command::Wink, payload_len: 0 } });
        assert!(reply.1.is_empty());
    }

    #[test]
    fn request_wink_invalid_payload() {
        let mut ctaphid = CtapHidServer::new().unwrap();
        let cid = create_test_channel(&mut ctaphid);

        // WINK with payload (should fail)
        let pkt = build_packet(cid, 0x08, &[0x01]);
        let reply = ctaphid.process(&pkt);
        assert!(reply.is_some());
        let reply = reply.unwrap();
        assert_eq!(reply.0, Header { cid, cmd_seq: CmdSeq::Cmd { cmd: Command::Error, payload_len: 1 } });
        assert_eq!(reply.1, vec![0x03]); // ERR_INVALID_LEN
    }
}
