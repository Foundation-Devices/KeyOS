// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{BlockingScalarHandler, LendMutHandler, ScalarEventSubscriber, ScalarEventSubscriptionHandler};
use usb::error::{EhciError, UsbError};
use usb::host::api::descriptors::DescriptorSet;
use usb::host::api::{descriptors::EndpointType, EndpointDirection, UsbEvent};
use xous::MemoryRange;

use crate::{error::MassStorageError, messages::*, MassStorageEvent};

usb::use_host_api!();

const INTERFACE_CLASS_MASS_STORAGE: u8 = 8;
const INTERFACE_SUBCLASS_SCSI: u8 = 6;
const INTERFACE_PROTOCOL_BULK_ONLY: u8 = 0x50;

#[derive(Default, server::Server)]
#[name = "os/mass-storage"]
pub struct MassStorageServer {
    backend: Option<mass_storage::MassStorageHost<UsbWrapper>>,
    device_handle: Option<usize>,
    usb: UsbHost,
    subscribers: Vec<ScalarEventSubscriber<MassStorageEvent>>,
}

struct UsbWrapper {
    ep_in: UsbInEndpoint,
    ep_out: UsbOutEndpoint,
    block_buffer: Option<MemoryRange>,
    temp_buffer: MemoryRange,
}

impl Drop for UsbWrapper {
    fn drop(&mut self) { xous::unmap_memory(self.temp_buffer).ok(); }
}

impl server::Server for MassStorageServer {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) { self.usb.subscribe(context); }
}

impl server::ArchiveEventHandler<UsbEvent> for MassStorageServer {
    fn handle(
        &mut self,
        msg: server::Owned<UsbEvent>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let Ok(msg) = msg.deserialize() else { return };

        match msg {
            UsbEvent::Connect { handle, descriptors } => {
                let Some((ep_in, ep_out)) = self.open_endpoints(handle, descriptors) else {
                    return;
                };
                match mass_storage::MassStorageHost::new(UsbWrapper {
                    ep_in,
                    ep_out,
                    block_buffer: None,
                    temp_buffer: xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W).unwrap(),
                }) {
                    Ok(backend) => {
                        let block_size = backend.block_size() as usize;
                        let block_count = backend.block_count() as usize;
                        self.backend = Some(backend);
                        self.device_handle = Some(handle);
                        self.subscribers.retain(|subscriber| {
                            subscriber.send(&MassStorageEvent::Connect { block_size, block_count }).is_ok()
                        });
                        log::info!("Connected mass storage device, block_count={block_count}");
                    }

                    Err(e) => {
                        log::warn!("Could not initialize mass storage backend: {e:?}")
                    }
                }
            }
            UsbEvent::Disconnect { handle } => {
                if self.device_handle == Some(handle) {
                    self.subscribers
                        .retain(|subscriber| subscriber.send(&MassStorageEvent::Disconnect).is_ok());
                    self.device_handle = None;
                    self.backend = None;
                }
            }
        }
    }
}

impl mass_storage::UsbHostCommands for UsbWrapper {
    fn bulk_in(&mut self, data: &mut [u8]) -> core::result::Result<usize, mass_storage::UsbError> {
        if let Some(block_buffer) = self.block_buffer {
            if data.as_ptr() == block_buffer.as_ptr() {
                // We got back the buffer we passed in, we can use it as-is
                return self.ep_in.bulk_in(block_buffer, data.len()).map_err(convert_ehci_error);
            }
        }
        if data.len() > self.temp_buffer.len() {
            log::error!("Mass storage requested an unexpected big read");
            return Err(mass_storage::UsbError::Other);
        }
        match self.ep_in.bulk_in(self.temp_buffer, data.len()) {
            Ok(len) => {
                data[..len].copy_from_slice(&self.temp_buffer.as_slice()[..len]);
                Ok(len)
            }
            Err(e) => Err(convert_ehci_error(e)),
        }
    }

    fn bulk_out(&mut self, data: &[u8]) -> core::result::Result<usize, mass_storage::UsbError> {
        if let Some(block_buffer) = self.block_buffer {
            if data.as_ptr() == block_buffer.as_ptr() {
                // We got back the buffer we passed in, we can use it as-is
                return self.ep_out.bulk_out(block_buffer, data.len()).map_err(convert_ehci_error);
            }
        }
        if data.len() > self.temp_buffer.len() {
            log::error!("Mass storage requested an unexpected big write");
            return Err(mass_storage::UsbError::Other);
        }
        self.temp_buffer.as_slice_mut()[..data.len()].copy_from_slice(data);
        self.ep_out.bulk_out(self.temp_buffer, data.len()).map_err(convert_ehci_error)
    }
}

fn convert_ehci_error(e: UsbError) -> mass_storage::UsbError {
    match e {
        UsbError::EhciError(EhciError::Stalled) => mass_storage::UsbError::Stalled,
        UsbError::EhciError(EhciError::Disconnected) => mass_storage::UsbError::Disconnected,
        UsbError::NotFound => mass_storage::UsbError::Disconnected,
        _ => mass_storage::UsbError::Other,
    }
}

impl ScalarEventSubscriptionHandler<Subscribe> for MassStorageServer {
    fn handle(
        &mut self,
        _msg: Subscribe,
        subscriber: server::ScalarEventSubscriber<MassStorageEvent>,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.subscribers.push(subscriber);
        Ok(())
    }
}

impl LendMutHandler<ReadBlocks> for MassStorageServer {
    fn handle(
        &mut self,
        mut msg: ReadBlocks,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, MassStorageError> {
        if let Some(backend) = self.backend.as_mut() {
            backend.usb_mut().block_buffer = Some(msg.buf);
            let result = backend.read(msg.block_index, &mut msg.buf.as_slice_mut()[..msg.length]);
            backend.usb_mut().block_buffer = None;
            Ok(result?)
        } else {
            Err(MassStorageError::NotConnected)
        }
    }
}

impl LendMutHandler<WriteBlocks> for MassStorageServer {
    fn handle(
        &mut self,
        msg: WriteBlocks,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, MassStorageError> {
        if let Some(backend) = self.backend.as_mut() {
            backend.usb_mut().block_buffer = Some(msg.buf);
            let result = backend.write(msg.block_index, &msg.buf.as_slice()[..msg.length]);
            backend.usb_mut().block_buffer = None;
            Ok(result?)
        } else {
            Err(MassStorageError::NotConnected)
        }
    }
}

impl BlockingScalarHandler<BlockCount> for MassStorageServer {
    fn handle(
        &mut self,
        _msg: BlockCount,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <BlockCount as server::BlockingScalar>::Response {
        if let Some(backend) = self.backend.as_ref() {
            Ok(backend.block_count() as usize)
        } else {
            Err(MassStorageError::NotConnected)
        }
    }
}

impl MassStorageServer {
    fn open_endpoints(
        &mut self,
        handle: usize,
        descriptors: DescriptorSet,
    ) -> Option<(UsbInEndpoint, UsbOutEndpoint)> {
        let mut ep_in = None;
        let mut ep_out = None;
        let Some(configuration) = descriptors.configurations().next() else {
            log::debug!("No config descriptor");
            return None;
        };
        let Some(interface) = configuration.interfaces().find(|interface| {
            interface.interface_class == INTERFACE_CLASS_MASS_STORAGE
                && interface.interface_sub_class == INTERFACE_SUBCLASS_SCSI
                && interface.interface_protocol == INTERFACE_PROTOCOL_BULK_ONLY
        }) else {
            log::debug!("Could not find mass storage interface");
            return None;
        };

        macro_rules! try_to {
            ($e:expr, $text:expr) => {
                match $e {
                    Ok(device) => device,
                    Err(e) => {
                        log::error!($text, e);
                        return None;
                    }
                }
            };
        }

        let mut device = try_to!(self.usb.claim(handle), "Could not open device: {:?}");

        for endpoint in interface.endpoints() {
            if endpoint.get_endpoint_type() == Some(EndpointType::Bulk) {
                match endpoint.get_direction() {
                    EndpointDirection::Out => {
                        ep_out = Some(try_to!(
                            device
                                .open_out_endpoint(endpoint.get_endpoint_number(), endpoint.max_packet_size),
                            "Could not open endpoint: {:?}"
                        ))
                    }
                    EndpointDirection::In => {
                        ep_in = Some(try_to!(
                            device.open_in_endpoint(endpoint.get_endpoint_number(), endpoint.max_packet_size),
                            "Could not open endpoint: {:?}"
                        ))
                    }
                }
            }
        }
        if let (Some(ep_in), Some(ep_out)) = (ep_in, ep_out) {
            Some((ep_in, ep_out))
        } else {
            log::warn!("Could not find needed endpoints");
            None
        }
    }
}
