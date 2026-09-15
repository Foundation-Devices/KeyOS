// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use ehci::{descriptors, EndpointDirection};
use server::{AsScalar, FromScalar};
use xous::MemoryRange;

use crate::error::UsbError;

#[derive(Debug, server::Message, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(UsbEvent)]
pub struct Subscribe;

#[derive(Debug, server::Message, Clone)]
#[response(Result<(), UsbError>)]
pub struct Claim(pub usize);

#[derive(Debug, server::Message, Clone)]
#[response(Result<(), UsbError>)]
pub struct OpenEndpoint {
    pub handle: usize,
    pub endpoint: u8,
    pub max_packet_length: u16,
    pub direction: EndpointDirection,
}

impl AsScalar<4> for OpenEndpoint {
    fn as_scalar(&self) -> [u32; 4] {
        [
            self.handle as u32,
            self.endpoint as u32,
            self.max_packet_length as u32,
            if self.direction == EndpointDirection::In { 0 } else { 1 },
        ]
    }
}

impl FromScalar<4> for OpenEndpoint {
    fn from_scalar(value: [u32; 4]) -> Self {
        Self {
            handle: value[0] as usize,
            endpoint: value[1] as u8,
            max_packet_length: value[2] as u16,
            direction: if value[3] == 0 { EndpointDirection::In } else { EndpointDirection::Out },
        }
    }
}

#[derive(Debug, server::Message)]
#[response(Result<usize, UsbError>)]
pub struct BulkOut {
    pub buffer: MemoryRange,
    pub handle: usize,
    pub endpoint: u8,
    pub length: usize,
}

impl From<server::SimpleMemoryMessage> for BulkOut {
    fn from(msg: server::SimpleMemoryMessage) -> Self {
        Self { buffer: msg.buf, endpoint: msg.arg1 as u8, length: msg.arg2, handle: msg.arg1 >> 8 }
    }
}

impl From<BulkOut> for server::SimpleMemoryMessage {
    fn from(val: BulkOut) -> Self {
        Self { buf: val.buffer, arg1: val.endpoint as usize | (val.handle << 8), arg2: val.length }
    }
}

#[derive(Debug, server::Message)]
#[response(Result<usize, UsbError>)]
pub struct BulkIn {
    pub buffer: MemoryRange,
    pub handle: usize,
    pub endpoint: u8,
    pub length: usize,
}

impl From<server::SimpleMemoryMessage> for BulkIn {
    fn from(msg: server::SimpleMemoryMessage) -> Self {
        Self { buffer: msg.buf, endpoint: msg.arg1 as u8, length: msg.arg2, handle: msg.arg1 >> 8 }
    }
}

impl From<BulkIn> for server::SimpleMemoryMessage {
    fn from(val: BulkIn) -> Self {
        Self { buf: val.buffer, arg1: val.endpoint as usize | (val.handle << 8), arg2: val.length }
    }
}

/// A control transfer on endpoint zero. The direction is the top bit of
/// `request_type`, as on the wire.
#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Vec<u8>, UsbError>)]
pub struct ControlTransfer {
    pub handle: usize,
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    /// Payload sent to the device; empty for a device-to-host transfer.
    pub data: Vec<u8>,
    /// Bytes to read back; zero for a host-to-device transfer.
    pub read_len: usize,
}

#[derive(Debug, server::Message, Clone)]
pub struct SetEnabled(pub bool);

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum UsbEvent {
    Connect { handle: usize, descriptors: descriptors::DescriptorSet },
    Disconnect { handle: usize },
}

#[derive(Debug, server::Message)]
#[response(bool)]
pub struct IsEnabled;

#[derive(Debug, server::Message)]
#[response(bool)]
pub struct IsConnected;
