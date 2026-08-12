// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use crypto::messages::DiskEncryptComplete;
use server::{
    BlockingScalarAsyncHandler, BlockingScalarRequest, DeferredLendMut, DeferredLendMutHandler,
    ScalarEventHandler, ScalarHandler,
};
use xous::keyos::TOTAL_FLASH_BLOCKS;

use crate::{
    atsama5d2::EmmcServer,
    error::EmmcError,
    messages::*,
    pipeline::{BlockRange, DmaBuf, OperationType, FULL_RANGE},
    BLOCK_SIZE, SD_BUFFER_BLOCKS,
};

impl ScalarHandler<SdmmcDone> for EmmcServer {
    fn handle(&mut self, msg: SdmmcDone, _sender: xous::PID, _ctx: &mut server::ServerContext<Self>) {
        self.pipeline.on_sdmmc_done(msg.0);
    }
}

impl ScalarEventHandler<DiskEncryptComplete> for EmmcServer {
    fn handle(
        &mut self,
        _msg: DiskEncryptComplete,
        _sender: xous::PID,
        _ctx: &mut server::ServerContext<Self>,
    ) {
        self.pipeline.on_disk_encrypt_complete();
    }
}

impl DeferredLendMutHandler<ReadBlocks> for EmmcServer {
    fn handle(&mut self, mut msg: DeferredLendMut<ReadBlocks>, _ctx: &mut server::ServerContext<Self>) {
        let block_index = msg.body().block_index;
        let block_count = msg.body().block_count;

        if block_count == 0 {
            msg.set_response(Err(EmmcError::EmptyRequest));
            return;
        }
        if block_count * BLOCK_SIZE > msg.body().buf.len() || block_count > SD_BUFFER_BLOCKS {
            msg.set_response(Err(EmmcError::BufferTooLarge));
            return;
        }
        if (block_index as usize).saturating_add(block_count) > TOTAL_FLASH_BLOCKS {
            msg.set_response(Err(EmmcError::OutOfRange));
            return;
        }

        let caller_buf = msg.body().buf;
        let caller_phys = match xous::virt_to_phys(caller_buf.as_ptr() as usize) {
            Ok(phys) => phys,
            Err(e) => {
                msg.set_response(Err(e.into()));
                return;
            }
        };
        let dma_buf = if xous::keyos::is_address_encrypted(caller_phys) {
            match self.pipeline.pool.acquire() {
                Ok(b) => DmaBuf::Owned(b),
                Err(e) => {
                    msg.set_response(Err(e));
                    return;
                }
            }
        } else {
            let borrowed = caller_buf.subrange(0, block_count * BLOCK_SIZE).unwrap();
            xous::flush_cache(borrowed, xous::CacheOperation::Invalidate).ok();
            DmaBuf::Borrowed(borrowed)
        };

        self.pipeline.admit(
            OperationType::PlainReadSdmmc { dma_buf, deferred: msg },
            BlockRange { start: block_index, count: block_count },
        );
    }

    fn default_response() -> Result<usize, EmmcError> { Err(EmmcError::InternalError) }
}

impl DeferredLendMutHandler<WriteBlocks> for EmmcServer {
    fn handle(&mut self, mut msg: DeferredLendMut<WriteBlocks>, _ctx: &mut server::ServerContext<Self>) {
        let block_index = msg.body().block_index;
        let block_count = msg.body().block_count;

        if block_count == 0 {
            msg.set_response(Err(EmmcError::EmptyRequest));
            return;
        }
        if block_count * BLOCK_SIZE > msg.body().buf.len() || block_count > SD_BUFFER_BLOCKS {
            msg.set_response(Err(EmmcError::BufferTooLarge));
            return;
        }
        if (block_index as usize).saturating_add(block_count) > TOTAL_FLASH_BLOCKS {
            msg.set_response(Err(EmmcError::OutOfRange));
            return;
        }

        let src = msg.body().buf.subrange(0, block_count * BLOCK_SIZE).unwrap();
        let mut owned_buf = match self.pipeline.pool.acquire() {
            Ok(b) => b,
            Err(e) => {
                msg.set_response(Err(e));
                return;
            }
        };
        owned_buf.as_slice_mut::<u8>()[..block_count * BLOCK_SIZE]
            .copy_from_slice(&src.as_slice::<u8>()[..block_count * BLOCK_SIZE]);

        xous::flush_cache(*owned_buf, xous::CacheOperation::CleanAndInvalidate).ok();
        msg.set_response(Ok(block_count));
        self.pipeline.admit(
            OperationType::PlainWriteSdmmc { owned_buf },
            BlockRange { start: block_index, count: block_count },
        );
    }

    fn default_response() -> Result<usize, EmmcError> { Err(EmmcError::InternalError) }
}

impl DeferredLendMutHandler<ReadEncryptedBlocks> for EmmcServer {
    fn handle(
        &mut self,
        mut msg: DeferredLendMut<ReadEncryptedBlocks>,
        _ctx: &mut server::ServerContext<Self>,
    ) {
        let block_index = msg.body().block_index;
        let block_count = msg.body().block_count;

        if block_count == 0 {
            msg.set_response(Err(EmmcError::EmptyRequest));
            return;
        }
        if block_count * BLOCK_SIZE > msg.body().buf.len() || block_count > SD_BUFFER_BLOCKS {
            msg.set_response(Err(EmmcError::BufferTooLarge));
            return;
        }
        if (block_index as usize).saturating_add(block_count) > TOTAL_FLASH_BLOCKS {
            msg.set_response(Err(EmmcError::OutOfRange));
            return;
        }

        // ciphertext: plaintext-memory bounce buffer - SDMMC DMA reads into this.
        let ciphertext = match self.pipeline.pool.acquire() {
            Ok(b) => b,
            Err(e) => {
                msg.set_response(Err(e));
                return;
            }
        };

        xous::flush_cache(
            msg.body().buf.subrange(0, block_count * BLOCK_SIZE).unwrap(),
            xous::CacheOperation::Invalidate,
        )
        .ok();
        self.pipeline.admit(
            OperationType::EncReadSdmmc { ciphertext, deferred: msg },
            BlockRange { start: block_index, count: block_count },
        );
    }

    fn default_response() -> Result<usize, EmmcError> { Err(EmmcError::InternalError) }
}

impl DeferredLendMutHandler<WriteEncryptedBlocks> for EmmcServer {
    fn handle(
        &mut self,
        mut msg: DeferredLendMut<WriteEncryptedBlocks>,
        _ctx: &mut server::ServerContext<Self>,
    ) {
        let block_index = msg.body().block_index;
        let block_count = msg.body().block_count;

        if block_count == 0 {
            msg.set_response(Err(EmmcError::EmptyRequest));
            return;
        }
        if block_count * BLOCK_SIZE > msg.body().buf.len() || block_count > SD_BUFFER_BLOCKS {
            msg.set_response(Err(EmmcError::BufferTooLarge));
            return;
        }
        if (block_index as usize).saturating_add(block_count) > TOTAL_FLASH_BLOCKS {
            msg.set_response(Err(EmmcError::OutOfRange));
            return;
        }

        // ciphertext: plaintext-memory output buffer - crypto writes here, then SDMMC DMA reads it.
        let ciphertext = match self.pipeline.pool.acquire() {
            Ok(b) => b,
            Err(e) => {
                msg.set_response(Err(e));
                return;
            }
        };

        xous::flush_cache(
            msg.body().buf.subrange(0, block_count * BLOCK_SIZE).unwrap(),
            xous::CacheOperation::Clean,
        )
        .ok();
        self.pipeline.admit(
            OperationType::EncWriteCrypt { ciphertext, crypt_offset: 0, deferred: msg },
            BlockRange { start: block_index, count: block_count },
        );
    }

    fn default_response() -> Result<usize, EmmcError> { Err(EmmcError::InternalError) }
}

impl server::BlockingScalarHandler<BlockCount> for EmmcServer {
    fn handle(
        &mut self,
        _msg: BlockCount,
        _sender: xous::PID,
        _ctx: &mut server::ServerContext<Self>,
    ) -> usize {
        TOTAL_FLASH_BLOCKS
    }
}

impl BlockingScalarAsyncHandler<Flush> for EmmcServer {
    fn handle(&mut self, request: BlockingScalarRequest<Flush>, _ctx: &mut server::ServerContext<Self>) {
        let response = request.response;
        if self.pipeline.is_idle() && self.pipeline.inbox_is_empty() {
            response.respond(()).ok();
            return;
        }
        self.pipeline.admit(OperationType::Flush { deferred: response }, FULL_RANGE);
    }

    fn default_response() {}
}

impl ScalarHandler<Suspend> for EmmcServer {
    fn handle(&mut self, _msg: Suspend, _sender: xous::PID, _ctx: &mut server::ServerContext<Self>) {
        self.pipeline.handle_suspend();
    }
}
