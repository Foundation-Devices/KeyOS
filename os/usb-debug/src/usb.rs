// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Vendor-specific USB debug interface for passport-drive.
//!
//! Registers a single vendor-specific USB interface (class 0xFF) with two
//! bulk endpoints (IN + OUT). Logs and debug responses share the IN endpoint.
//! Each frame is sent as a single USB bulk transfer (terminated by short
//! packet or ZLP):
//!
//!   IN:  `[TYPE:1][PAYLOAD...]`
//!     TYPE 0x01 = Log data (payload: raw UTF-8 log bytes, 0x1E-terminated records)
//!     TYPE 0x02 = Debug response (payload: [STATUS][response data...])
//!
//!   OUT: `[CMD:1][PAYLOAD:0..N]`

use server::{Server, ServerContext, ServerMessages};
use usb::device::{
    api::{EnabledGate, EndpointDirection, EndpointType},
    messages::EndpointProperties,
};
use usb_debug_protocol::{Response, USB_DEBUG_OUT_TRANSFER_MAX};

use crate::dispatch;

usb::use_device_api!();
settings::use_api!();

const DEBUG_INTERFACE_PRIORITY: u8 = usb::device::interface_priorities::USB_DEBUG;
const DEBUG_OUT_BUFFER_SIZE: usize = USB_DEBUG_OUT_TRANSFER_MAX + 1;
const DEBUG_OUT_READ_LEN: u16 = USB_DEBUG_OUT_TRANSFER_MAX as u16;

struct DeveloperModeWatcher {
    interface: UsbRegisteredInterface,
    gate: EnabledGate,
}

impl ServerMessages for DeveloperModeWatcher {
    const NAME: &'static str = "";

    fn messages() -> &'static [server::MessageDef<Self>] { &[] }
}

impl Server for DeveloperModeWatcher {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        SettingsApi::default().server_subscribe_developer_mode(context);
    }
}

impl server::ScalarEventHandler<settings::global::DeveloperMode> for DeveloperModeWatcher {
    fn handle(
        &mut self,
        settings::global::DeveloperMode(enabled): settings::global::DeveloperMode,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        if let Err(e) = self.interface.set_enabled(enabled) {
            log::warn!("failed to set usb-debug interface enabled={enabled}: {e:?}");
            return;
        }
        self.gate.set_enabled(enabled);
    }
}

// Debug OUT drain thread — reads debug protocol commands from the
// vendor-specific bulk OUT endpoint.
fn debug_out_drain_thread(
    mut ep_out: UsbEmulatedEndpoint,
    writer_tx: std::sync::mpsc::SyncSender<Response>,
    gate: EnabledGate,
) {
    let usb_api = UsbDeviceEmulation::default();
    let mut debug = dispatch::DebugProtocol::new();
    let usb_recv_buffer = xous::map_memory(
        None,
        None,
        DEBUG_OUT_BUFFER_SIZE,
        xous::MemoryFlags::W | xous::MemoryFlags::POPULATE,
    )
    .expect("Could not allocate buffer");

    loop {
        gate.wait_enabled();
        match ep_out.read_buf(usb_recv_buffer, DEBUG_OUT_READ_LEN) {
            Ok(l) => {
                if gate.is_enabled() {
                    let data = &usb_recv_buffer.as_slice()[..l];
                    debug.process(data, |response| {
                        let _ = writer_tx.send(response);
                    });
                }
            }
            Err(e) => match e {
                usb::error::UsbError::HostDisconnected => {
                    usb_api.wait_for_connection().expect("Error waiting for connection");
                }
                usb::error::UsbError::InterfaceDisabled => {}
                _ => log::error!("Error reading debug OUT: {e:?}"),
            },
        }
    }
}

// Log drain thread — blocks on `log_reader.read()` and forwards log
// data to the main loop via the bounded channel. The sync_channel
// naturally blocks when the queue is full, providing backpressure
// without needing to call log_reader.read() when the host isn't draining.
fn log_drain_thread(writer_tx: std::sync::mpsc::SyncSender<Response>, gate: EnabledGate) {
    let log_reader = log_server::reader::LogReader::default();
    let log_buffer =
        xous::map_memory(None, None, 0x4000, xous::MemoryFlags::W).expect("Could not allocate log buffer");
    loop {
        gate.wait_enabled();
        let len = log_reader.read(log_buffer);
        if len > 0 && gate.is_enabled() {
            let data = log_buffer.as_slice()[..len].to_vec();
            if writer_tx.send(Response::Log(data)).is_err() {
                break;
            }
        }
    }
}

/// Write one frame to the bulk IN endpoint as a single USB transfer. The USB
/// server handles DMA chunking internally.
fn write_frame(
    ep_in: &mut UsbEmulatedEndpoint,
    usb_api: &UsbDeviceEmulation,
    header: &[u8],
    payload: &[u8],
    scratch: &mut xous::MemoryRange,
) {
    if header.is_empty() {
        log::error!("Error writing frame: empty header");
        return;
    }

    let total = header.len() + payload.len();
    let buf = scratch.as_slice_mut();
    if total > buf.len() {
        log::error!("Error writing frame: {total} bytes exceeds scratch buffer {}", buf.len());
        return;
    }

    buf[..header.len()].copy_from_slice(header);
    let offset = header.len();
    buf[offset..total].copy_from_slice(payload);

    // Keep the ZLP behavior: usb-debug IN framing intentionally uses USB short
    // packets/ZLPs as frame terminators instead of carrying a protocol length.
    // Max-packet-aligned frames need the ZLP or the host cannot distinguish a
    // complete frame from one that still has more chunks coming.
    match ep_in.write_buf_zlp(*scratch, total) {
        Ok(_) => {}
        Err(usb::error::UsbError::HostDisconnected) => {
            usb_api.wait_for_connection().expect("Error waiting for connection");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Err(usb::error::UsbError::InterfaceDisabled) => {}
        Err(e) => log::error!("Error writing frame: {e:?}"),
    }
}

pub fn run() -> ! {
    let mut usb_api = UsbDeviceEmulation::default();
    let initial_enabled = !cfg!(feature = "production");
    let gate = EnabledGate::new(initial_enabled);

    let (debug_interface, [debug_ep_out, mut ep_in]) = usb_api
        .register_interface(
            UsbInterfaceConfig::new(
                DEBUG_INTERFACE_PRIORITY,
                0xFF, // Class: Vendor Specific
                0x00,
                0x00,
                &[
                    EndpointProperties {
                        ep_type: EndpointType::Bulk,
                        ep_direction: EndpointDirection::Out,
                        max_packet_len: 512,
                        interval: 0,
                        use_dma: true,
                    },
                    EndpointProperties {
                        ep_type: EndpointType::Bulk,
                        ep_direction: EndpointDirection::In,
                        max_packet_len: 512,
                        interval: 0,
                        use_dma: true,
                    },
                ],
            )
            .with_msos20_features(&crate::msos20::FEATURES),
        )
        .expect("Error registering debug interface");

    debug_interface.set_enabled(initial_enabled).expect("Error setting initial debug interface state");

    // Bounded channel for both debug responses and log data.
    // The capacity provides backpressure: log_drain_thread blocks when full.
    let (writer_tx, writer_rx) = std::sync::mpsc::sync_channel::<Response>(crate::MAX_PENDING_LOGS);

    // Debug OUT: binary protocol commands
    std::thread::spawn({
        let writer_tx = writer_tx.clone();
        let gate = gate.clone();
        move || debug_out_drain_thread(debug_ep_out, writer_tx, gate)
    });

    // Log drain: reads from log server, sends Log variants
    std::thread::spawn({
        let gate = gate.clone();
        move || log_drain_thread(writer_tx, gate)
    });

    // Reusable scratch buffer — large enough for the biggest frame (screenshot ≈ 1.5 MB).
    // POPULATE guarantees physically contiguous pages for DMA.
    let scratch_size = 2 * 1024 * 1024;
    let mut scratch =
        xous::map_memory(None, None, scratch_size, xous::MemoryFlags::W | xous::MemoryFlags::POPULATE)
            .expect("scratch alloc");

    if cfg!(feature = "production") {
        std::thread::spawn({
            let gate = gate.clone();
            let interface = debug_interface.clone();
            move || server::listen(DeveloperModeWatcher { interface, gate })
        });
    }

    // Main loop: blocking recv on the unified channel.
    loop {
        match writer_rx.recv() {
            Ok(response) => {
                let (header, payload) = response.parts();
                write_frame(&mut ep_in, &usb_api, header, payload, &mut scratch);
            }
            Err(_) => {
                log::error!("All senders dropped, exiting main loop");
                break;
            }
        }
    }

    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
