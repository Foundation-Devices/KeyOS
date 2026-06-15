// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(keyos)]
pub use atsama5d27::udphs::{EndpointDirection, EndpointType};
use server::{CheckedConn, CheckedPermissions, MessageAllowed, MessageId as _};

use super::messages::*;
#[cfg(all(doc, not(keyos)))]
pub use super::messages::{EndpointDirection, EndpointType};
use crate::error::UsbError;

#[macro_export]
macro_rules! use_device_api {
    () => {
        mod usb_device_permissions {
            use $crate::device::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/usbdev"]
            pub struct UsbDevicePermissions;
        }
        type UsbDeviceEmulation =
            $crate::device::api::UsbDeviceEmulation<usb_device_permissions::UsbDevicePermissions>;
        type UsbEmulatedEndpoint =
            $crate::device::api::UsbEmulatedEndpoint<usb_device_permissions::UsbDevicePermissions>;
        type UsbInterfaceConfig<'a, const N: usize> = $crate::device::api::UsbInterfaceConfig<'a, N>;
        type UsbRegisteredInterface =
            $crate::device::api::UsbRegisteredInterface<usb_device_permissions::UsbDevicePermissions>;
    };
}

#[derive(Default)]
pub struct UsbDeviceEmulation<P: CheckedPermissions>(CheckedConn<P>);

pub struct UsbInterfaceConfig<'a, const N: usize> {
    interface_number: u8,
    if_class: u8,
    if_subclass: u8,
    if_protocol: u8,
    endpoints: &'a [EndpointProperties; N],
    interface_functional_descriptors: &'a [u8],
    associated_interface_count: u8,
    capabilities: Vec<DeviceCapability>,
    setup_responder: Option<Box<dyn FnOnce(xous::PID) -> Result<xous::CID, UsbError>>>,
}

impl<'a, const N: usize> UsbInterfaceConfig<'a, N> {
    pub fn new(
        interface_number: u8,
        if_class: u8,
        if_subclass: u8,
        if_protocol: u8,
        endpoints: &'a [EndpointProperties; N],
    ) -> Self {
        Self {
            interface_number,
            if_class,
            if_subclass,
            if_protocol,
            endpoints,
            interface_functional_descriptors: &[],
            associated_interface_count: 0,
            capabilities: Vec::new(),
            setup_responder: None,
        }
    }

    pub fn with_functional_descriptors(mut self, descriptors: &'a [u8]) -> Self {
        self.interface_functional_descriptors = descriptors;
        self
    }

    pub fn with_associated_interface_count(mut self, count: u8) -> Self {
        self.associated_interface_count = count;
        self
    }

    pub fn with_capability(
        mut self,
        cap_type: u8,
        cap_subtype: u8,
        cap_uuid: uuid::Uuid,
        capability_functional_descriptors: &[u8],
    ) -> Self {
        self.capabilities.push(DeviceCapability {
            cap_type,
            cap_subtype,
            cap_uuid: cap_uuid.to_bytes_le().to_vec(),
            capability_functional_descriptors: capability_functional_descriptors.into(),
        });
        self
    }

    pub fn with_setup_responder<S>(mut self, setup_responder: Option<S>) -> Self
    where
        S: server::Server + server::BlockingArchiveHandler<SetupPacketCallback> + Send + 'static,
    {
        if let Some(setup_responder) = setup_responder {
            self.setup_responder = Some(Box::new(move |pid| {
                let cid = server::listen_and_connect(setup_responder, pid);
                xous::allow_messages_on_connection(
                    pid,
                    cid,
                    SetupPacketCallback::ID..(SetupPacketCallback::ID + 1),
                )?;
                Ok(cid)
            }));
        }
        self
    }
}

impl<P: CheckedPermissions> UsbDeviceEmulation<P> {
    /// Register a disabled interface driver with explicit runtime visibility control.
    /// Returns a stable interface handle and the allocated endpoints.
    /// Call [`UsbRegisteredInterface::set_enabled`] after local setup is ready.
    pub fn register_interface<const N: usize>(
        &mut self,
        config: UsbInterfaceConfig<'_, N>,
    ) -> Result<(UsbRegisteredInterface<P>, [UsbEmulatedEndpoint<P>; N]), UsbError>
    where
        P: MessageAllowed<RegisterInterface>,
    {
        let setup_responder = if let Some(connect) = config.setup_responder {
            Some(connect(self.0.get_remote_pid())?)
        } else {
            None
        };
        let registered = self.0.send_blocking_archive(RegisterInterface {
            interface_number: config.interface_number,
            if_class: config.if_class,
            if_subclass: config.if_subclass,
            if_protocol: config.if_protocol,
            endpoints: config.endpoints.into(),
            interface_functional_descriptors: config.interface_functional_descriptors.into(),
            associated_interface_count: config.associated_interface_count,
            capabilities: config.capabilities,
            setup_responder,
        })?;
        let interface =
            UsbRegisteredInterface { connection: self.0.clone(), interface_number: config.interface_number };
        let endpoints = core::array::from_fn(|i| UsbEmulatedEndpoint {
            connection: self.0.clone(),
            endpoint_number: registered.endpoints[i],
        });
        Ok((interface, endpoints))
    }

    /// Wait until the device is configured by the host
    pub fn wait_for_connection(&self) -> Result<(), UsbError>
    where
        P: MessageAllowed<WaitForConnection>,
    {
        self.0.try_send_blocking_scalar(WaitForConnection)?;
        Ok(())
    }

    pub fn is_enabled(&self) -> Result<bool, UsbError>
    where
        P: MessageAllowed<IsDeviceEmulationEnabled>,
    {
        Ok(self.0.try_send_blocking_scalar(IsDeviceEmulationEnabled)?)
    }

    pub fn is_connected(&self) -> Result<bool, UsbError>
    where
        P: MessageAllowed<IsDeviceEmulationConnected>,
    {
        Ok(self.0.try_send_blocking_scalar(IsDeviceEmulationConnected)?)
    }

    /// Returns true if the USB cable is connected (VBUS has power)
    pub fn is_cable_connected(&self) -> Result<bool, UsbError>
    where
        P: MessageAllowed<IsCableConnected>,
    {
        Ok(self.0.try_send_blocking_scalar(IsCableConnected)?)
    }

    /// Returns true if in USB device mode (not acting as USB host via OTG)
    pub fn is_device_mode(&self) -> Result<bool, UsbError>
    where
        P: MessageAllowed<IsDeviceMode>,
    {
        Ok(self.0.try_send_blocking_scalar(IsDeviceMode)?)
    }

    pub fn set_custom_vid_pid(&mut self, vid: Option<u16>, pid: Option<u16>)
    where
        P: MessageAllowed<SetVidPid>,
    {
        self.0.try_send_blocking_scalar(SetVidPid { vid, pid }).unwrap().unwrap();
    }

    pub fn reset_controller(&mut self)
    where
        P: MessageAllowed<ResetController>,
    {
        self.0.try_send_blocking_scalar(ResetController).unwrap().unwrap()
    }
}

#[derive(Clone)]
pub struct UsbRegisteredInterface<P: CheckedPermissions> {
    connection: CheckedConn<P>,
    interface_number: u8,
}

impl<P: CheckedPermissions> UsbRegisteredInterface<P> {
    pub fn number(&self) -> u8 { self.interface_number }

    pub fn set_enabled(&self, enabled: bool) -> Result<(), UsbError>
    where
        P: MessageAllowed<SetInterfaceEnabled>,
    {
        self.connection.try_send_blocking_scalar(SetInterfaceEnabled {
            interface_number: self.interface_number,
            enabled,
        })??;
        Ok(())
    }
}

pub struct UsbEmulatedEndpoint<P: CheckedPermissions> {
    connection: CheckedConn<P>,
    endpoint_number: u8,
}

impl<P: CheckedPermissions> UsbEmulatedEndpoint<P> {
    /// The endpoint number without the 0x80 (IN/OUT marker) bit
    pub fn endpoint_number(&self) -> u8 { self.endpoint_number }

    /// Received data from the host (OUT transaction and endpoint)
    /// Returns the actual number of bytes received
    pub fn read_buf(&mut self, buf: xous::MemoryRange, length: u16) -> Result<usize, UsbError>
    where
        P: MessageAllowed<ReadEndpoint>,
    {
        self.connection.lend_mut(ReadEndpoint { buf, endpoint: self.endpoint_number, length })
    }

    /// Transmit data to the host (IN transaction and endpoint).
    ///
    /// The USB server handles DMA chunking internally for transfers larger
    /// than one DMA descriptor (64 KB).
    ///
    /// **`buf` must be backed by physically contiguous pages** (allocate with
    /// `MemoryFlags::POPULATE`). The DMA controller reads from a single
    /// physical start address; non-contiguous pages cause data corruption.
    pub fn write_buf(&mut self, buf: xous::MemoryRange, length: usize) -> Result<usize, UsbError>
    where
        P: MessageAllowed<WriteEndpoint>,
    {
        self.connection.lend_mut(WriteEndpoint { buf, endpoint: self.endpoint_number, length, zlp: false })
    }

    /// Like [`write_buf`](Self::write_buf) but sends a ZLP after the transfer
    /// if the total length is an exact multiple of `max_packet_len`, so the
    /// host sees a proper USB transfer boundary.
    ///
    /// See [`write_buf`](Self::write_buf) for the physical contiguity requirement.
    pub fn write_buf_zlp(&mut self, buf: xous::MemoryRange, length: usize) -> Result<usize, UsbError>
    where
        P: MessageAllowed<WriteEndpoint>,
    {
        self.connection.lend_mut(WriteEndpoint { buf, endpoint: self.endpoint_number, length, zlp: true })
    }

    /// Set or unset the stalled (a.k.a. halted) state on the endpoint
    pub fn set_stalled(&mut self, stalled: bool)
    where
        P: MessageAllowed<SetEndpointStalled>,
    {
        self.connection
            .try_send_scalar(SetEndpointStalled { endpoint: self.endpoint_number, stalled })
            .unwrap();
    }
}
