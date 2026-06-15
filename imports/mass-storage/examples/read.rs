use std::time::Duration;

use clap::Parser;
use clap_num::maybe_hex;
use log::{debug, error};
use mass_storage::{MassStorageHost, UsbError, UsbHostCommands};
use yusb::{DeviceHandle, Direction, TransferType};

/// Read some sectors from a Mass Storage device
#[derive(clap::Parser, Debug)]
struct CliArgs {
    /// Vendor ID
    #[clap(value_parser=maybe_hex::<u16>)]
    vid: u16,
    /// Product ID
    #[clap(value_parser=maybe_hex::<u16>)]
    pid: u16,
}

struct UsbWrapper {
    ep_in: u8,
    ep_out: u8,
    device: DeviceHandle,
}

impl UsbHostCommands for UsbWrapper {
    fn bulk_in(&mut self, data: &mut [u8]) -> core::result::Result<usize, UsbError> {
        debug!("Bulk in on EP={}, data_len={}", self.ep_out, data.len());
        match self.device.read_bulk(self.ep_in, data, Duration::from_secs(5)) {
            Ok(len) => {
                debug!("Bulk in data={:x?}", &data[..len]);
                Ok(len)
            }
            Err(e) => {
                error!("Yusb error: {e}");
                Err(UsbError::Other)
            }
        }
    }

    fn bulk_out(&mut self, data: &[u8]) -> core::result::Result<usize, UsbError> {
        debug!("Bulk out on EP={}, data={data:x?}", self.ep_out);
        match self
            .device
            .write_bulk(self.ep_out, data, Duration::from_secs(5))
        {
            Ok(len) => Ok(len),
            Err(e) => {
                error!("Yusb error: {e}");
                Err(UsbError::Other)
            }
        }
    }
}
fn main() {
    env_logger::init();
    let args = CliArgs::parse();
    let mut device = yusb::open_device_with_vid_pid(args.vid, args.pid).unwrap();
    device.set_auto_detach_kernel_driver(true);
    let mut ep_in = 0;
    let mut ep_out = 0;
    let mut success = false;
    for interface in device
        .device()
        .active_config_descriptor()
        .unwrap()
        .interfaces()
    {
        let interface_desc = interface.descriptors().next().unwrap();
        if interface_desc.class_code() == 8 && interface_desc.protocol_code() == 0x50 {
            device.claim_interface(interface.number());
            for endpoint in interface_desc.endpoint_descriptors() {
                if endpoint.transfer_type() == TransferType::Bulk {
                    if endpoint.direction() == Direction::In {
                        ep_in = endpoint.address();
                    } else {
                        ep_out = endpoint.address();
                    }
                }
            }
            success = true;
            break;
        }
    }
    if !success {
        panic!("Could not find mass storage interface");
    }
    let usb = UsbWrapper {
        ep_in,
        ep_out,
        device,
    };
    let mut mass_storage = MassStorageHost::new(usb).unwrap();
    let mut buffer = vec![0u8; 512];
    let len = mass_storage.read(0, &mut buffer).unwrap();
    println!("{:x?}", &buffer[..len]);
}
