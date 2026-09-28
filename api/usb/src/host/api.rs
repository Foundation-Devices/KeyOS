// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub use ehci::{descriptors, EndpointDirection};
use server::{CheckedConn, CheckedPermissions, MessageAllowed};
use xous::MemoryRange;

pub use super::messages::UsbEvent;
use super::messages::{
    BulkIn, BulkOut, Claim, ControlTransfer, IsConnected, OpenEndpoint, ResetHostController, Subscribe,
};
use crate::error::UsbError;

const CLEAR_FEATURE: u8 = 1;
const ENDPOINT_HALT: u16 = 0;
const RECIPIENT_ENDPOINT: u8 = 0x02;
const ENDPOINT_IN: u8 = 0x80;

#[macro_export]
macro_rules! use_host_api {
    () => {
        mod usb_host_permissions {
            use $crate::host::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/usb"]
            pub struct UsbHostPermissions;
        }
        type UsbHost = $crate::host::api::UsbHost<usb_host_permissions::UsbHostPermissions>;
        type ConnectedUsbDevice =
            $crate::host::api::ConnectedUsbDevice<usb_host_permissions::UsbHostPermissions>;
        type UsbInEndpoint = $crate::host::api::UsbInEndpoint<usb_host_permissions::UsbHostPermissions>;
        type UsbOutEndpoint = $crate::host::api::UsbOutEndpoint<usb_host_permissions::UsbHostPermissions>;
    };
}

#[derive(Default)]
pub struct UsbHost<P: CheckedPermissions>(CheckedConn<P>);

pub struct ConnectedUsbDevice<P: CheckedPermissions> {
    conn: CheckedConn<P>,
    handle: usize,
}

pub struct UsbInEndpoint<P: CheckedPermissions> {
    conn: CheckedConn<P>,
    handle: usize,
    endpoint: u8,
}

pub struct UsbOutEndpoint<P: CheckedPermissions> {
    conn: CheckedConn<P>,
    handle: usize,
    endpoint: u8,
}

impl<P: CheckedPermissions> UsbHost<P> {
    pub fn subscribe<S>(&self, context: &mut server::ServerContext<S>)
    where
        S: server::Server + server::ArchiveEventHandler<UsbEvent>,
        P: MessageAllowed<Subscribe>,
    {
        self.0.subscribe_archive_infallible(Subscribe, context)
    }

    pub fn claim(&self, handle: usize) -> Result<ConnectedUsbDevice<P>, UsbError>
    where
        P: MessageAllowed<Claim>,
    {
        self.0.try_send_blocking_scalar(Claim(handle))??;
        Ok(ConnectedUsbDevice { conn: self.0.clone(), handle })
    }

    pub fn reset_host_controller(&self) -> Result<(), UsbError>
    where
        P: MessageAllowed<ResetHostController>,
    {
        self.0.send_scalar_nowait(ResetHostController)?;
        Ok(())
    }

    pub fn is_connected(&self) -> Result<bool, UsbError>
    where
        P: MessageAllowed<IsConnected>,
    {
        Ok(self.0.try_send_blocking_scalar(IsConnected)?)
    }
}

impl<P: CheckedPermissions> ConnectedUsbDevice<P> {
    pub fn open_in_endpoint(
        &mut self,
        endpoint: u8,
        max_packet_length: u16,
    ) -> Result<UsbInEndpoint<P>, UsbError>
    where
        P: MessageAllowed<OpenEndpoint>,
    {
        self.conn.try_send_blocking_scalar(OpenEndpoint {
            handle: self.handle,
            endpoint,
            max_packet_length,
            direction: EndpointDirection::In,
        })??;
        Ok(UsbInEndpoint { conn: self.conn.clone(), handle: self.handle, endpoint })
    }

    pub fn open_out_endpoint(
        &mut self,
        endpoint: u8,
        max_packet_length: u16,
    ) -> Result<UsbOutEndpoint<P>, UsbError>
    where
        P: MessageAllowed<OpenEndpoint>,
    {
        self.conn.try_send_blocking_scalar(OpenEndpoint {
            handle: self.handle,
            endpoint,
            max_packet_length,
            direction: EndpointDirection::Out,
        })??;
        Ok(UsbOutEndpoint { conn: self.conn.clone(), handle: self.handle, endpoint })
    }
}

impl<P: CheckedPermissions> UsbInEndpoint<P> {
    pub fn bulk_in(&mut self, buffer: MemoryRange, length: usize) -> Result<usize, UsbError>
    where
        P: MessageAllowed<BulkIn>,
    {
        self.conn.lend_mut(BulkIn { handle: self.handle, endpoint: self.endpoint, buffer, length })
    }

    /// Clears a STALL condition on this pipe, leaving both sides on DATA0.
    pub fn clear_halt(&mut self) -> Result<(), UsbError>
    where
        P: MessageAllowed<ControlTransfer>,
    {
        self.conn.send_blocking_archive(ControlTransfer {
            handle: self.handle,
            request_type: RECIPIENT_ENDPOINT,
            request: CLEAR_FEATURE,
            value: ENDPOINT_HALT,
            index: u16::from(self.endpoint | ENDPOINT_IN),
            data: Vec::new(),
            read_len: 0,
        })?;
        Ok(())
    }
}

impl<P: CheckedPermissions> UsbOutEndpoint<P> {
    pub fn bulk_out(&mut self, buffer: MemoryRange, length: usize) -> Result<usize, UsbError>
    where
        P: MessageAllowed<BulkOut>,
    {
        self.conn.lend_mut(BulkOut { handle: self.handle, endpoint: self.endpoint, buffer, length })
    }
}
