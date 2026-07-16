// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use atsama5d27::dma::DmaPeripheralTransferConfig;
use server::{AsScalar, FromScalar};
use xous::MemoryRange;

use crate::error::DmaError;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(TransferComplete)]
#[error(DmaError)]
pub struct SubscribeTransferComplete(pub usize);

#[derive(Debug, Clone, Copy)]
pub struct TransferComplete {
    pub transfer_id: u32,
    pub bytes: u32,
}

impl AsScalar<2> for TransferComplete {
    fn as_scalar(&self) -> [u32; 2] { [self.transfer_id, self.bytes] }
}

impl FromScalar<2> for TransferComplete {
    fn from_scalar([transfer_id, bytes]: [u32; 2]) -> Self { Self { transfer_id, bytes } }
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<usize, DmaError>)]
pub struct PeripheralTransferMsg {
    pub address: usize,
    pub config: DmaPeripheralTransferConfig,
}

#[derive(Debug, server::Message)]
#[response(Result<(), DmaError>)]
pub struct ExecuteTransferMsg {
    pub buf: xous::MemoryRange,
    pub transfer_id: usize,
    pub pid: xous::PID,
}

impl AsScalar<4> for ExecuteTransferMsg {
    fn as_scalar(&self) -> [u32; 4] {
        [
            self.buf.as_ptr() as usize as u32,
            self.buf.len() as u32,
            self.transfer_id as u32,
            self.pid.get() as u32,
        ]
    }
}

impl FromScalar<4> for ExecuteTransferMsg {
    fn from_scalar([ptr, len, transfer_id, pid]: [u32; 4]) -> Self {
        Self {
            buf: unsafe { MemoryRange::new(ptr as _, len as _).expect("Invalid address") },
            transfer_id: transfer_id as _,
            pid: xous::PID::try_from(pid as u8).unwrap(),
        }
    }
}

#[derive(Debug, server::Message)]
pub struct StopTransferMsg(pub usize);

#[derive(Debug, server::Message)]
#[response(Result<usize, DmaError>)]
pub struct WaitTransferMsg(pub usize);

#[derive(Debug, server::Message)]
pub struct DropTransferMsg(pub usize);

#[derive(Debug, server::Message)]
#[response(Result<usize, DmaError>)]
pub struct FlushTransferMsg(pub usize);
