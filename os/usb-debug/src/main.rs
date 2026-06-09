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
//!
//! Commands:
//!   0x01 SCREENSHOT  – no payload.  Response: raw pixel data (480×800×4 bytes)
//!   0x02 TAP         – payload: x_lo x_hi y_lo y_hi kind (5 bytes)
//!   0x03 POWER_BTN   – payload: 1 byte (1=pressed, 0=released)
//!   0x04 REBOOT_SAMBA – no payload.  Reboots into SAM-BA mode.
//!   0x05 CLOSE_APP   – payload: pid_lo pid_hi (2 bytes)
//!   0x06 KERNEL_CMD  – payload: 1 byte (command char h/i/m/p/t/s/c/a/o/k)
//!   0x07 INPUT_TEXT  – payload: UTF-8 text
//!   0x08 GET_VERSION – no payload.  Response: UTF-8 KeyOS version bytes.
//!   0x09 LAUNCH_APP  – payload: 16-byte AppId.  Response: pid_lo pid_hi
//!
//! **This crate must NEVER be included in production firmware.**

#[cfg(feature = "production")]
compile_error!("usb-debug must not be included in production firmware");

mod dispatch;
mod msos20;

use usb::device::{
    api::{EndpointDirection, EndpointType},
    messages::EndpointProperties,
};
use usb_debug_protocol::Response;

usb::use_device_api!();

/// Max pending log messages in the channel before the log drain starts dropping.
/// Each chunk is up to 16 KB, so 8 chunks ≈ 128 KB max buffered.
const MAX_PENDING_LOGS: usize = 8;

// Debug OUT drain thread — reads debug protocol commands from the
// vendor-specific bulk OUT endpoint.
fn debug_out_drain_thread(
    mut ep_out: UsbEmulatedEndpoint,
    tx: std::sync::mpsc::SyncSender<usb_debug_protocol::Response>,
) {
    let usb_api = UsbDeviceEmulation::default();
    let mut debug = dispatch::DebugProtocol::new();
    let usb_recv_buffer =
        xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W).expect("Could not allocate buffer");

    loop {
        match ep_out.read_buf(usb_recv_buffer, 512) {
            Ok(l) => {
                let data = &usb_recv_buffer.as_slice()[..l];
                debug.process(data, &tx);
            }
            Err(e) => match e {
                usb::error::UsbError::HostDisconnected => {
                    usb_api.wait_for_connection().expect("Error waiting for connection");
                }
                _ => log::error!("Error reading debug OUT: {e:?}"),
            },
        }
    }
}

// Log drain thread — blocks on `log_reader.read()` and forwards log
// data to the main loop via the bounded channel. The sync_channel
// naturally blocks when the queue is full, providing backpressure
// without needing to call log_reader.read() when the host isn't draining.
fn log_drain_thread(tx: std::sync::mpsc::SyncSender<Response>) {
    let log_reader = log_server::reader::LogReader::default();
    let log_buffer =
        xous::map_memory(None, None, 0x4000, xous::MemoryFlags::W).expect("Could not allocate log buffer");
    loop {
        let len = log_reader.read(log_buffer);
        if len > 0 {
            let data = log_buffer.as_slice()[..len].to_vec();
            if tx.send(Response::Log(data)).is_err() {
                break;
            }
        }
    }
}

/// Write a frame (header + payload) to the bulk IN endpoint as a single
/// USB transfer. The USB server handles DMA chunking internally.
fn write_frame(
    ep_in: &mut UsbEmulatedEndpoint,
    usb_api: &UsbDeviceEmulation,
    header: &[u8],
    payload: &[u8],
    scratch: &mut xous::MemoryRange,
) {
    let total = header.len() + payload.len();
    let buf = scratch.as_slice_mut();
    buf[..header.len()].copy_from_slice(header);
    buf[header.len()..total].copy_from_slice(payload);

    match ep_in.write_buf_zlp(*scratch, total) {
        Ok(_) => {}
        Err(usb::error::UsbError::HostDisconnected) => {
            usb_api.wait_for_connection().expect("Error waiting for connection");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Err(e) => log::error!("Error writing frame: {e:?}"),
    }
}

fn main() -> ! {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::AppBackground0).unwrap();

    let mut usb_api = UsbDeviceEmulation::default();

    // Advertise WinUSB binding for the debug interface via MS OS 2.0
    // descriptors so Windows hosts auto-bind winusb.sys without Zadig.
    let debug_interface_num = usb::device::interface_numbers::USB_DEBUG;
    usb_api
        .register_setup_responder(msos20::SetupResponder {
            descriptor_set: msos20::descriptor_set(debug_interface_num),
        })
        .expect("Error registering MS OS 2.0 setup responder");
    usb_api
        .register_capability(
            0x10, // bDescriptorType: DEVICE_CAPABILITY
            0x05, // bDevCapabilityType: PLATFORM
            msos20::PLATFORM_CAPABILITY_UUID,
            &msos20::PLATFORM_CAPABILITY,
        )
        .expect("Error registering MS OS 2.0 platform capability");

    let [debug_ep_out, mut ep_in] = usb_api
        .register_interface(
            usb::device::interface_numbers::USB_DEBUG,
            0xFF, // Class: Vendor Specific
            0x00,
            0x00,
            &[
                EndpointProperties {
                    ep_type: EndpointType::Bulk,
                    ep_direction: EndpointDirection::Out,
                    max_packet_len: 512,
                    interval: 0,
                    use_dma: false,
                },
                EndpointProperties {
                    ep_type: EndpointType::Bulk,
                    ep_direction: EndpointDirection::In,
                    max_packet_len: 512,
                    interval: 0,
                    use_dma: true,
                },
            ],
            &[],
            0,
        )
        .expect("Error registering debug interface");

    // Bounded channel for both debug responses and log data.
    // The capacity provides backpressure: log_drain_thread blocks when full.
    let (tx, rx) = std::sync::mpsc::sync_channel::<usb_debug_protocol::Response>(MAX_PENDING_LOGS);

    // Debug OUT: binary protocol commands
    let cmd_tx = tx.clone();
    std::thread::spawn(move || debug_out_drain_thread(debug_ep_out, cmd_tx));

    // Log drain: reads from log server, sends Log variants
    std::thread::spawn(move || log_drain_thread(tx));

    // Reusable scratch buffer — large enough for the biggest frame (screenshot ≈ 1.5 MB).
    // POPULATE guarantees physically contiguous pages for DMA.
    let scratch_size = 2 * 1024 * 1024;
    let mut scratch =
        xous::map_memory(None, None, scratch_size, xous::MemoryFlags::W | xous::MemoryFlags::POPULATE)
            .expect("scratch alloc");

    // Main loop: blocking recv on the unified channel.
    loop {
        match rx.recv() {
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
