// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod error;
pub mod messages;
pub mod sha2;

pub use messages::AesMode;
use server::{xous::MemoryRange, CheckedConn, CheckedPermissions, MessageAllowed};
#[cfg(keyos)]
use server::{ScalarEventHandler, ServerContext};
pub use sha2::{
    Sha256StreamingContext, ShaAlgo, ShaPermissions, ShaStreamingContext, SHA224_HASH_SIZE, SHA256_HASH_SIZE,
    SHA384_HASH_SIZE, SHA512_HASH_SIZE,
};

use crate::error::{CryptoError, ShamirError};
use crate::messages::*;

pub const AES_BLOCK_SIZE: usize = 16;

#[macro_export]
macro_rules! use_api {
    () => {
        mod crypto_permissions {
            use crypto::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/crypto"]
            pub struct CryptoPermissions;
        }
        type CryptoApi = crypto::CryptoApi<crypto_permissions::CryptoPermissions>;
    };
}

#[derive(Default)]
pub struct CryptoApi<P: CheckedPermissions> {
    pub(crate) conn: CheckedConn<P>,
}

pub struct AesContext<P: CheckedPermissions + MessageAllowed<AesClear>> {
    conn: CheckedConn<P>,
    id: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum Direction {
    Encrypt,
    Decrypt,
}

impl<P: CheckedPermissions> CryptoApi<P> {
    pub fn setup_aes(&self, key: &[u8], mode: AesMode) -> Result<AesContext<P>, CryptoError>
    where
        P: MessageAllowed<AesSetup>,
        P: MessageAllowed<AesClear>,
    {
        let result = self.conn.send_blocking_archive(AesSetup { key: key.into(), mode });
        Ok(AesContext { id: result? as u8, conn: self.conn.clone() })
    }

    /// Encrypt/decrypt using DMA on the provided memory ranges directly.
    /// Source and destination pointer and length do not need to be page-aligned, but they need to be
    /// word-aligned.
    /// Cache on the source buffer needs to be cleaned before the operation, and invalidated
    /// on the destination after the operation.
    ///
    /// Returns synchronously after pre-DMA validation: an `Err` here means the DMA never
    /// started. Once `Ok(())` is returned, completion is signalled via a `DiskEncryptComplete`
    /// event to the persistent subscription.
    ///
    /// # Safety
    /// Caller has to make sure both buffers stay mapped for the duration of the operation.
    #[cfg(keyos)]
    pub unsafe fn disk_encrypt_unsafe(
        &self,
        tweak: [u8; 16],
        j: usize,
        src: MemoryRange,
        dst: MemoryRange,
        direction: Direction,
    ) -> Result<(), CryptoError>
    where
        P: MessageAllowed<DiskEncryptUnsafe>,
    {
        self.conn.send_blocking_archive(DiskEncryptUnsafe {
            tweak,
            j,
            src: src.as_ptr() as usize,
            dst: dst.as_ptr() as usize,
            len: src.len().min(dst.len()),
            direction,
        })
    }

    /// Subscribe to `DiskEncryptComplete` events. Must be called once at startup.
    #[cfg(keyos)]
    pub fn subscribe_disk_encrypt_complete<SR>(
        &self,
        context: &mut ServerContext<SR>,
    ) -> Result<(), CryptoError>
    where
        P: MessageAllowed<SubscribeDiskEncryptComplete>,
        SR: ScalarEventHandler<DiskEncryptComplete>,
    {
        self.conn.subscribe_scalar(SubscribeDiskEncryptComplete, context)
    }

    pub fn hmac224(&self, key: Vec<u8>, data: Vec<u8>) -> Result<Vec<u8>, CryptoError>
    where
        P: MessageAllowed<Hmac>,
    {
        self.conn.send_blocking_archive(Hmac { algo: ShaAlgo::Sha224, key, data })
    }

    pub fn hmac256(&self, key: Vec<u8>, data: Vec<u8>) -> Result<Vec<u8>, CryptoError>
    where
        P: MessageAllowed<Hmac>,
    {
        self.conn.send_blocking_archive(Hmac { algo: ShaAlgo::Sha256, key, data })
    }

    pub fn hmac384(&self, key: Vec<u8>, data: Vec<u8>) -> Result<Vec<u8>, CryptoError>
    where
        P: MessageAllowed<Hmac>,
    {
        self.conn.send_blocking_archive(Hmac { algo: ShaAlgo::Sha384, key, data })
    }

    pub fn hmac512(&self, key: Vec<u8>, data: Vec<u8>) -> Result<Vec<u8>, CryptoError>
    where
        P: MessageAllowed<Hmac>,
    {
        self.conn.send_blocking_archive(Hmac { algo: ShaAlgo::Sha512, key, data })
    }

    pub fn split_secret(
        &self,
        secret: Vec<u8>,
        num_shares: usize,
        threshold: usize,
    ) -> Result<Vec<Vec<u8>>, ShamirError>
    where
        P: MessageAllowed<ShamirSplit>,
    {
        self.conn.send_blocking_archive(ShamirSplit { secret, num_shares, threshold })
    }

    pub fn recover_secret(&self, indexes: Vec<usize>, shares: Vec<Vec<u8>>) -> Result<Vec<u8>, ShamirError>
    where
        P: MessageAllowed<ShamirRecover>,
    {
        self.conn.send_blocking_archive(ShamirRecover { indexes, shares })
    }
}

impl<P: CheckedPermissions + MessageAllowed<AesClear>> AesContext<P> {
    pub fn execute(
        &self,
        buf: MemoryRange,
        offset: usize,
        len: usize,
        direction: Direction,
    ) -> Result<usize, CryptoError>
    where
        P: MessageAllowed<AesExecute>,
    {
        self.conn.lend_mut(AesExecute { buf, transfer_id: self.id, len, direction, offset })
    }

    pub fn add_aad(&self, aad: &[u8]) -> Result<usize, CryptoError>
    where
        P: MessageAllowed<AesAad>,
    {
        self.conn.send_blocking_archive(AesAad { transfer_id: self.id, aad: aad.into() })
    }

    pub fn gcm_tag(&self) -> Result<[u8; 16], CryptoError>
    where
        P: MessageAllowed<AesGcmTag>,
    {
        self.conn.send_blocking_archive(AesGcmTag { transfer_id: self.id })
    }
}

impl<P: CheckedPermissions + MessageAllowed<AesClear>> Drop for AesContext<P> {
    fn drop(&mut self) { self.conn.try_send_scalar(AesClear(self.id)).ok(); }
}
