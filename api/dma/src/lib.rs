// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg(keyos)]
pub mod error;
pub mod messages;

use atsama5d27::dma::DmaPeripheralTransferConfig;
// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
use server::{CheckedConn, CheckedPermissions, MessageAllowed};
use xous::MemoryRange;

use crate::{
    error::DmaError,
    messages::{
        DropTransferMsg, ExecuteTransferMsg, FlushTransferMsg, PeripheralTransferMsg, StopTransferMsg,
        SubscribeTransferComplete, TransferComplete, WaitTransferMsg,
    },
};

#[macro_export]
macro_rules! use_api {
    () => {
        mod dma_permissions {
            use dma::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/dma"]
            pub struct DmaPermissions;
        }
        type Dma = dma::Dma<dma_permissions::DmaPermissions>;
        type DmaTransfer = dma::DmaTransfer<dma_permissions::DmaPermissions>;
    };
}

#[derive(Default)]
pub struct Dma<P: CheckedPermissions> {
    conn: CheckedConn<P>,
}

pub struct DmaTransfer<P: CheckedPermissions + MessageAllowed<DropTransferMsg>> {
    conn: CheckedConn<P>,
    id: usize,
}

impl<P: CheckedPermissions> Dma<P> {
    /// Setup a new memory-to-peripheral or peripheral-to-memory transfer.
    /// `address` is the virtual address of the mapped peripheral. Must be mapped in the calling process'
    /// memory to prove that they own the device.
    pub fn peripheral_transfer(
        &self,
        address: usize,
        config: DmaPeripheralTransferConfig,
    ) -> Result<DmaTransfer<P>, DmaError>
    where
        P: MessageAllowed<PeripheralTransferMsg>,
        P: MessageAllowed<DropTransferMsg>,
    {
        let id = self.conn.send_blocking_archive(PeripheralTransferMsg { address, config })?;
        Ok(DmaTransfer { conn: self.conn.clone(), id })
    }
}

impl<P: CheckedPermissions + MessageAllowed<DropTransferMsg>> DmaTransfer<P> {
    /// Returns the channel index for this transfer for matching with TransferCompletes.
    pub fn id(&self) -> usize { self.id }

    /// Subscribe to `TransferComplete` events for this specific transfer/channel.
    pub fn subscribe_transfer_complete<SR>(
        &self,
        context: &mut server::ServerContext<SR>,
    ) -> Result<(), DmaError>
    where
        P: MessageAllowed<SubscribeTransferComplete>,
        SR: server::ScalarEventHandler<TransferComplete>,
    {
        self.conn.subscribe_scalar(SubscribeTransferComplete(self.id), context)
    }

    /// Start the transfer. Does not block.
    ///
    /// Be sure that caches are cleaned before Memory->peripheral transfers, and invalidated before and after
    /// Peripheral->memory transfers.
    ///
    /// A transfer can be reused multiple times with different (or the same) buffers.
    ///
    /// The address and length of `buf` must be aligned to the data element width.
    ///
    /// # Safety
    /// The caller should guarantee that the provided memory range remains valid for the duration of the
    /// transfer.
    pub unsafe fn execute(&self, buf: MemoryRange) -> Result<(), DmaError>
    where
        P: MessageAllowed<ExecuteTransferMsg>,
    {
        self.conn.try_send_blocking_scalar(ExecuteTransferMsg {
            buf,
            transfer_id: self.id,
            pid: xous::current_pid().unwrap(),
        })?
    }

    /// Start the transfer using the address space of the provided PID.
    /// Same as [`execute`], but virt_to_phys mapping of `buf` uses `pid` instead of the sender.
    ///
    /// # Safety
    /// The caller should guarantee that the provided memory range remains valid for the duration of the
    /// transfer.
    pub unsafe fn execute_for_pid(&self, buf: MemoryRange, pid: xous::PID) -> Result<(), DmaError>
    where
        P: MessageAllowed<ExecuteTransferMsg>,
    {
        self.conn.try_send_blocking_scalar(ExecuteTransferMsg { buf, transfer_id: self.id, pid })?
    }

    /// Wait for the completion of the running transfer.
    /// Returns immediately if there is no transfer in progress.
    /// Returns the number of bytes transferred.
    pub fn wait(&self) -> Result<usize, DmaError>
    where
        P: MessageAllowed<WaitTransferMsg>,
    {
        self.conn.try_send_blocking_scalar(WaitTransferMsg(self.id))?
    }

    /// Flush the currently buffered data, but continue transfer
    /// Returns the number of bytes transferred since start
    pub fn flush(&self) -> Result<usize, DmaError>
    where
        P: MessageAllowed<FlushTransferMsg>,
    {
        self.conn.try_send_blocking_scalar(FlushTransferMsg(self.id))?
    }

    /// Stop the ongoing execute() call. Does not block.
    /// Safe to call from a different thread.
    /// Flushes the DMA FIFO.
    pub fn stop(&self) -> Result<(), DmaError>
    where
        P: MessageAllowed<StopTransferMsg>,
    {
        Ok(self.conn.send_scalar_nowait(StopTransferMsg(self.id))?)
    }
}

impl<P: CheckedPermissions + MessageAllowed<DropTransferMsg>> Drop for DmaTransfer<P> {
    fn drop(&mut self) { self.conn.try_send_scalar(DropTransferMsg(self.id)).ok(); }
}
