// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{handle_blocking_archive_message, BlockingArchiveHandler, MessageId, Server, ServerMessages};
use usb::device::{
    api::{EndpointDirection, EndpointType},
    messages::{EndpointProperties, SetupPacketCallback},
};
use xous::debug_command;

usb::use_device_api!();

#[derive(Default)]
pub(crate) struct SetupResponder {
    pub(crate) interface_num: u16,
}
impl ServerMessages for SetupResponder {
    const NAME: &str = "";

    fn messages() -> &'static [server::MessageDef<Self>]
    where
        Self: Sized,
    {
        &[(SetupPacketCallback::ID, handle_blocking_archive_message::<SetupPacketCallback, _>)]
    }
}
impl Server for SetupResponder {}
impl BlockingArchiveHandler<SetupPacketCallback> for SetupResponder {
    fn handle(
        &mut self,
        SetupPacketCallback(msg): SetupPacketCallback,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Option<Vec<u8>> {
        log::debug!("Setup packet: {msg:02x?}");
        if msg.index == self.interface_num {
            // SetControlLineState
            if msg.request_type == 0x21 && msg.request == 0x22 {
                Some(Vec::new())
            // SetLineConfig
            } else if msg.request_type == 0x21 && msg.request == 0x20 {
                Some(Vec::new())
            } else {
                None
            }
        } else {
            None
        }
    }
}

// Accept and drop incoming communication, so that serial apps don't get pipe errors and full buffers when
// trying to write.
// Note: OUT in this case means OUT from the PC, so it's actually incoming characters from
// the device's perspective
fn out_drain_thread(mut ep_out: UsbEmulatedEndpoint) {
    let usb_api = UsbDeviceEmulation::default();
    let usb_recv_buffer =
        xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W).expect("Could not allocate buffer");
    let debug_command_buffer =
        xous::map_memory(None, None, 0x40000, xous::MemoryFlags::W).expect("Could not allocate buffer");
    loop {
        match ep_out.read_buf(usb_recv_buffer, 512) {
            Ok(l) => {
                // Only react to the last received command, so we
                // don't freeze on too many commands coming in.
                let cmd = usb_recv_buffer.as_slice()[l - 1];
                let len = debug_command(debug_command_buffer, cmd).unwrap_or(0);
                for part in debug_command_buffer.as_slice()[..len].chunks(0x1000) {
                    print!("{}", core::str::from_utf8(part).unwrap_or("?"));
                    // Don't overwhelm logging and ourselves with dumping 128K at once
                    std::thread::sleep(core::time::Duration::from_millis(5));
                }
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

fn main() -> ! {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::AppBackground0).unwrap();

    let log_buffer =
        xous::map_memory(None, None, 0x4000, xous::MemoryFlags::W).expect("Could not allocate buffer");
    let log_reader = log_server::reader::LogReader::default();

    let mut usb_api = UsbDeviceEmulation::default();
    let interface_num = usb_api.registered_interfaces() as u16;
    usb_api
        .register_setup_responder(SetupResponder { interface_num })
        .expect("Could not register setup responder");
    let [_ep_ctrl] = usb_api
        .register_interface(
            0x02, // Class: CDC
            0x02, // Subclass: ACM
            0x00, // Protocol: Nothing special
            &[EndpointProperties {
                ep_type: EndpointType::Interrupt,
                ep_direction: EndpointDirection::In,
                max_packet_len: 64,
                interval: 16,
                use_dma: false,
            }],
            &[
                // Additional descriptors
                0x05, // Len
                0x24, // Type: Interface functional
                0x00, // Subtype: Header
                0x10, // CDC release number minor version
                0x01, // CDC release number major version
                // -----
                0x05,                    // Len
                0x24,                    // Type: Interface functional
                0x01,                    // Subtype: Call Management
                0x00,                    // Capabilities: no additional capabilities
                interface_num as u8 + 1, // Data interface
                // -----
                0x04, // Len
                0x24, // Type: Interface functional
                0x02, // Subtype: ACM
                0x00, // Capabilities: no additional capabilities
                // -----
                0x05,                    // Len
                0x24,                    // Type: Interface functional
                0x06,                    // Subtype: Union
                interface_num as u8,     // Control interface
                interface_num as u8 + 1, // Data interface
            ],
            2,
        )
        .expect("Error registering USB interface");
    let [ep_out, mut ep_in] = usb_api
        .register_interface(
            0x0A, // Class: CDC Data
            0x00, // Subclass: unused
            0x00, // Protocol: unused
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
        .expect("Error registering USB interface");

    std::thread::spawn(move || out_drain_thread(ep_out));

    let mut len = 0;
    loop {
        if len == 0 {
            len = log_reader.read(log_buffer);
        }
        match ep_in.write_buf(log_buffer, len) {
            Ok(_) => len = 0,
            Err(e) => match e {
                usb::error::UsbError::HostDisconnected => {
                    log::debug!("Waiting for connection");
                    usb_api.wait_for_connection().expect("Error waiting for connection");
                    // XXX: This is a workaround to not lose logs on connection.
                    // Linux, right after connecting to an ACM device, immediately starts reading it, and then
                    // just throwing away the data, before minicom or friends had a chance to actually connect
                    // and read it. One solution is to wait for DTR, but not all clients
                    // send that and it also still lost logs because of a race condition. So the next best
                    // thing is to just wait a bit to give the serial client a chance to react to the new
                    // device.
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                _ => log::error!("Error while writing to USB: {e:?}"),
            },
        }
    }
}
