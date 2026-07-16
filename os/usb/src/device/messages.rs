// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use atsama5d27::udphs::DmaStatus;
use server::{AsScalar, FromScalar, SimpleMemoryMessage};

// === Internal messages ===
#[derive(Debug, server::Message, Clone)]
pub struct EndOfReset;

#[derive(Debug, server::Message, Clone)]
pub struct DmaInterrupt {
    pub endpoint: u8,
    pub status: DmaStatus,
}

impl AsScalar<2> for DmaInterrupt {
    fn as_scalar(&self) -> [u32; 2] { [self.endpoint as u32, self.status.0] }
}

impl FromScalar<2> for DmaInterrupt {
    fn from_scalar(value: [u32; 2]) -> Self { Self { endpoint: value[0] as u8, status: DmaStatus(value[1]) } }
}

/// Received data on an endpoint (Move message carrying FIFO data read in IRQ handler)
#[derive(Debug, server::Message)]
pub struct RxCompleteInterrupt {
    pub buf: xous::MemoryRange,
    pub endpoint: u8,
    pub byte_count: u16,
}

impl From<SimpleMemoryMessage> for RxCompleteInterrupt {
    fn from(value: SimpleMemoryMessage) -> Self {
        Self { buf: value.buf, endpoint: value.arg1 as u8, byte_count: value.arg2 as u16 }
    }
}

impl From<RxCompleteInterrupt> for SimpleMemoryMessage {
    fn from(value: RxCompleteInterrupt) -> Self {
        SimpleMemoryMessage { buf: value.buf, arg1: value.endpoint as usize, arg2: value.byte_count as usize }
    }
}

/// Transmission complete on an endpoint (Scalar message)
#[derive(Debug, server::Message, Clone)]
pub struct TxCompleteInterrupt {
    pub endpoint: u8,
}

impl AsScalar<1> for TxCompleteInterrupt {
    fn as_scalar(&self) -> [u32; 1] { [self.endpoint as u32] }
}

impl FromScalar<1> for TxCompleteInterrupt {
    fn from_scalar(value: [u32; 1]) -> Self { Self { endpoint: value[0] as u8 } }
}

#[derive(Debug, server::Message, Clone)]
pub struct SetCableConnected(pub bool);

#[derive(Debug, server::Message, Clone)]
pub struct OtgMode(pub bool);

#[derive(Debug, server::Message, Clone)]
pub struct SetDeviceEmulationEnabled(pub bool);
