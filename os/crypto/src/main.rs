// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use crypto::error::{CryptoError, ShamirError};
use crypto::messages::*;
use server::{
    xous::{self},
    BlockingArchiveHandler, LendMutHandler, ScalarHandler,
};
#[cfg(keyos)]
use server::{ScalarEventHandler, ScalarEventSubscriber, ScalarEventSubscriptionHandler};

#[cfg(keyos)]
mod atsama5d2;
#[cfg(not(keyos))]
mod hosted;

#[cfg(keyos)]
use atsama5d2::{ClientDisconnected, CryptoServer};
#[cfg(not(keyos))]
use hosted::CryptoServer;

#[cfg(keyos)]
power_manager::use_api!();

impl BlockingArchiveHandler<AesSetup> for CryptoServer {
    fn handle(
        &mut self,
        msg: AesSetup,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <AesSetup as server::BlockingArchive>::Response {
        self.aes_setup(msg, sender)
    }
}

impl LendMutHandler<AesExecute> for CryptoServer {
    fn handle(
        &mut self,
        msg: AesExecute,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, CryptoError> {
        self.aes_execute(msg, sender)
    }
}

impl BlockingArchiveHandler<AesAad> for CryptoServer {
    fn handle(
        &mut self,
        msg: AesAad,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, CryptoError> {
        self.aes_aad(msg, sender)
    }
}

impl BlockingArchiveHandler<AesGcmTag> for CryptoServer {
    fn handle(
        &mut self,
        msg: AesGcmTag,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<[u8; 16], CryptoError> {
        self.aes_get_tag(msg, sender)
    }
}

#[cfg(keyos)]
impl BlockingArchiveHandler<DiskEncryptUnsafe> for CryptoServer {
    fn handle(
        &mut self,
        msg: DiskEncryptUnsafe,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), CryptoError> {
        self.disk_encrypt_start(msg, sender)
    }
}

#[cfg(keyos)]
impl ScalarEventSubscriptionHandler<SubscribeDiskEncryptComplete> for CryptoServer {
    fn handle(
        &mut self,
        _msg: SubscribeDiskEncryptComplete,
        subscriber: ScalarEventSubscriber<DiskEncryptComplete>,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), CryptoError> {
        self.set_disk_encrypt_subscriber(subscriber);
        Ok(())
    }
}

#[cfg(keyos)]
impl ScalarEventHandler<dma::messages::TransferComplete> for CryptoServer {
    fn handle(
        &mut self,
        msg: dma::messages::TransferComplete,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.on_dma_transfer_complete(msg.transfer_id);
    }
}

impl ScalarHandler<AesClear> for CryptoServer {
    fn handle(&mut self, msg: AesClear, sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        self.aes_clear(msg, sender);
    }
}

// AES keys live in SECURAM and both context maps are keyed by pid, so a client that exits without
// clearing them would strand its key slots; a reused pid could then reach a stale key. Clear the
// departed client's contexts here.
#[cfg(keyos)]
impl ScalarHandler<ClientDisconnected> for CryptoServer {
    fn handle(
        &mut self,
        _msg: ClientDisconnected,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.clear_client_contexts(sender);
    }
}

#[cfg(keyos)]
impl BlockingArchiveHandler<ShaSetContext> for CryptoServer {
    fn handle(
        &mut self,
        msg: ShaSetContext,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, CryptoError> {
        self.sha_set_context(sender, msg)
    }
}

#[cfg(keyos)]
impl LendMutHandler<ShaUpdate> for CryptoServer {
    fn handle(
        &mut self,
        msg: ShaUpdate,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, CryptoError> {
        self.sha_update(sender, msg.context_id, msg.buf, msg.length)
    }
}

#[cfg(keyos)]
impl BlockingArchiveHandler<ShaGetContext> for CryptoServer {
    fn handle(
        &mut self,
        msg: ShaGetContext,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<ShaContextSnapshot, CryptoError> {
        self.sha_get_context(sender, msg.context_id)
    }
}

#[cfg(keyos)]
impl ScalarHandler<ShaDrop> for CryptoServer {
    fn handle(&mut self, msg: ShaDrop, sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        self.sha_drop(sender, msg.0);
    }
}

impl BlockingArchiveHandler<Hmac> for CryptoServer {
    fn handle(
        &mut self,
        msg: Hmac,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <Hmac as server::BlockingArchive>::Response {
        self.hmac(msg.algo, &msg.key, &msg.data)
    }
}

impl BlockingArchiveHandler<ShamirSplit> for CryptoServer {
    fn handle(
        &mut self,
        msg: ShamirSplit,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <ShamirSplit as server::BlockingArchive>::Response {
        bc_shamir::split_secret(
            msg.threshold,
            msg.num_shares,
            &msg.secret,
            &mut bc_rand::SecureRandomNumberGenerator,
        )
        .map_err(convert_shamir_error)
    }
}

impl BlockingArchiveHandler<ShamirRecover> for CryptoServer {
    fn handle(
        &mut self,
        msg: ShamirRecover,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <ShamirRecover as server::BlockingArchive>::Response {
        bc_shamir::recover_secret(&msg.indexes, &msg.shares).map_err(convert_shamir_error)
    }
}

fn convert_shamir_error(err: bc_shamir::Error) -> ShamirError {
    match err {
        bc_shamir::Error::SecretTooLong => ShamirError::SecretTooLong,
        bc_shamir::Error::TooManyShares => ShamirError::TooManyShares,
        bc_shamir::Error::InterpolationFailure => ShamirError::InterpolationFailure,
        bc_shamir::Error::ChecksumFailure => ShamirError::ChecksumFailure,
        bc_shamir::Error::SecretTooShort => ShamirError::SecretTooShort,
        bc_shamir::Error::SecretNotEvenLen => ShamirError::SecretNotEvenLen,
        bc_shamir::Error::InvalidThreshold => ShamirError::InvalidThreshold,
        bc_shamir::Error::SharesUnequalLength => ShamirError::SharesUnequalLength,
    }
}

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::System5).unwrap();

    server::listen(CryptoServer::new())
}
