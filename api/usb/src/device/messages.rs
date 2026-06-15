// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(keyos)]
use atsama5d27::udphs::{EndpointDirection, EndpointType};
use server::{AsScalar, FromScalar};

use super::SetupPacket;
use crate::error::UsbError;

#[cfg(all(doc, not(keyos)))]
#[derive(Debug, Clone, Copy, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum EndpointType {
    Control = 0,
    Isochronous = 1,
    Bulk = 2,
    Interrupt = 3,
}

#[cfg(all(doc, not(keyos)))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum EndpointDirection {
    Out = 0,
    In = 1,
}

// === Messages used by higher level drivers ===
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct SetupPacketCallback(pub SetupPacket);

impl server::BlockingArchive for SetupPacketCallback {
    type Response = Option<Vec<u8>>;
}

impl server::MessageId for SetupPacketCallback {
    const ID: xous::MessageId = 0;
    const SERVER: &str = "";
}

// === External messages ===

#[derive(Debug, server::Message, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<RegisteredInterfaceInfo, UsbError>)]
pub struct RegisterInterface {
    pub interface_number: u8,
    pub if_class: u8,
    pub if_subclass: u8,
    pub if_protocol: u8,
    pub endpoints: Vec<EndpointProperties>,
    pub interface_functional_descriptors: Vec<u8>,
    pub associated_interface_count: u8,
    pub capabilities: Vec<DeviceCapability>,
    pub setup_responder: Option<xous::CID>,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct RegisteredInterfaceInfo {
    pub endpoints: Vec<u8>,
}

#[derive(Debug, server::Message)]
#[response(())]
pub struct WaitForConnection;

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct EndpointProperties {
    pub ep_type: EndpointType,
    pub ep_direction: EndpointDirection,
    pub max_packet_len: u16,
    pub interval: u8,
    /// When `true`, the endpoint uses DMA for data transfer, supporting fragmented reads/writes
    /// across multiple packets (the hardware reassembles automatically). When `false`, the endpoint
    /// uses FIFO mode where each read/write operates on a single packet only — the caller must
    /// ensure the buffer fits within `max_packet_len`.
    pub use_dma: bool,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DeviceCapability {
    pub cap_type: u8,
    pub cap_subtype: u8,
    pub cap_uuid: Vec<u8>,
    pub capability_functional_descriptors: Vec<u8>,
}

#[derive(Debug, server::Message, Clone)]
pub struct SetEndpointStalled {
    pub endpoint: u8,
    pub stalled: bool,
}

impl FromScalar<2> for SetEndpointStalled {
    fn from_scalar(value: [u32; 2]) -> Self { Self { endpoint: value[0] as u8, stalled: value[1] != 0 } }
}

impl AsScalar<2> for SetEndpointStalled {
    fn as_scalar(&self) -> [u32; 2] { [self.endpoint as u32, self.stalled as u32] }
}

#[derive(Debug, server::Message)]
#[response(Result<usize, UsbError>)]
pub struct ReadEndpoint {
    pub buf: xous::MemoryRange,
    pub endpoint: u8,
    pub length: u16,
}

impl From<server::SimpleMemoryMessage> for ReadEndpoint {
    fn from(msg: server::SimpleMemoryMessage) -> Self {
        Self { buf: msg.buf, endpoint: msg.arg1 as u8, length: msg.arg2 as u16 }
    }
}

impl From<ReadEndpoint> for server::SimpleMemoryMessage {
    fn from(read: ReadEndpoint) -> Self {
        Self { buf: read.buf, arg1: read.endpoint as usize, arg2: read.length as usize }
    }
}

#[derive(Debug, server::Message)]
#[response(Result<usize, UsbError>)]
pub struct WriteEndpoint {
    pub buf: xous::MemoryRange,
    pub endpoint: u8,
    pub length: usize,
    /// When true, send a ZLP after max_packet_len-aligned transfers to
    /// signal end-of-transfer to the host.
    pub zlp: bool,
}

const ZLP_FLAG: usize = 1 << 31;

impl From<server::SimpleMemoryMessage> for WriteEndpoint {
    fn from(msg: server::SimpleMemoryMessage) -> Self {
        Self {
            buf: msg.buf,
            endpoint: (msg.arg1 & 0xFF) as u8,
            zlp: msg.arg1 & ZLP_FLAG != 0,
            length: msg.arg2,
        }
    }
}

impl From<WriteEndpoint> for server::SimpleMemoryMessage {
    fn from(val: WriteEndpoint) -> Self {
        let arg1 = val.endpoint as usize | if val.zlp { ZLP_FLAG } else { 0 };
        Self { buf: val.buf, arg1, arg2: val.length }
    }
}

#[derive(Debug, server::Message, Clone)]
#[response(bool)]
pub struct IsDeviceEmulationEnabled;

#[derive(Debug, server::Message, Clone)]
#[response(bool)]
pub struct IsDeviceEmulationConnected;

#[derive(Debug, server::Message, Clone)]
#[response(bool)]
pub struct IsCableConnected;

#[derive(Debug, server::Message, Clone)]
#[response(bool)]
pub struct IsDeviceMode;

#[derive(Debug, server::Message, Clone)]
#[response(Result<(), UsbError>)]
pub struct SetVidPid {
    pub vid: Option<u16>,
    pub pid: Option<u16>,
}

impl FromScalar<4> for SetVidPid {
    fn from_scalar(value: [u32; 4]) -> Self {
        Self {
            vid: if value[0] == 0 { None } else { Some(value[0] as u16) },
            pid: if value[1] == 0 { None } else { Some(value[1] as u16) },
        }
    }
}

impl AsScalar<4> for SetVidPid {
    fn as_scalar(&self) -> [u32; 4] { [self.vid.unwrap_or(0) as u32, self.pid.unwrap_or(0) as u32, 0, 0] }
}

#[derive(Debug, server::Message, Clone)]
#[response(Result<(), UsbError>)]
pub struct ResetController;

#[derive(Debug, server::Message, Clone)]
#[response(Result<(), UsbError>)]
pub struct SetInterfaceEnabled {
    pub interface_number: u8,
    pub enabled: bool,
}

impl FromScalar<2> for SetInterfaceEnabled {
    fn from_scalar(value: [u32; 2]) -> Self {
        Self { interface_number: value[0] as u8, enabled: value[1] != 0 }
    }
}

impl AsScalar<2> for SetInterfaceEnabled {
    fn as_scalar(&self) -> [u32; 2] { [self.interface_number as u32, self.enabled as u32] }
}

impl AsScalar<4> for SetupPacket {
    fn as_scalar(&self) -> [u32; 4] {
        [
            (self.request_type as u32) << 8 | self.request as u32,
            self.value as u32,
            self.index as u32,
            self.length as u32,
        ]
    }
}

impl FromScalar<4> for SetupPacket {
    fn from_scalar(value: [u32; 4]) -> Self {
        Self {
            request_type: (value[0] >> 8) as u8,
            request: value[0] as u8,
            value: value[1] as u16,
            index: value[2] as u16,
            length: value[3] as u16,
        }
    }
}
