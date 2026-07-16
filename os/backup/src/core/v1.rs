// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Read;

use fs::adapter::{BasicFsPermissions, FsAdapter};
use server::xous::DropDeallocate;
use server::MessageAllowed;
use whence::{self, WhenceExt};

use crate::core::{extract_backup, map_chunk_buffer, BackupKey, BackupMetadata, CHUNK_SIZE};
use crate::crypto_permissions::CryptoPermissions;
use crate::CryptoApi;

pub trait CryptoAdapter: Sized {
    fn new(key: &BackupKey) -> Result<Self, std::io::Error>;
    #[cfg(test)]
    fn encrypt_chunk(&mut self, chunk: &mut [u8]) -> Result<(), std::io::Error>;
    fn decrypt_chunk(&mut self, chunk: &mut [u8]) -> Result<(), std::io::Error>;
}

pub struct CryptoLive {
    pub aes_ctx: crypto::AesContext<CryptoPermissions>,
    pub buffer: DropDeallocate,
}

impl CryptoLive {
    fn new(key: &BackupKey, mode: crypto::AesMode) -> Result<Self, std::io::Error> {
        let crypto = CryptoApi::default();
        let aes_ctx = crypto
            .setup_aes(key.as_bytes(), mode)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "failed to setup aes"))?;
        Ok(Self { aes_ctx, buffer: map_chunk_buffer()? })
    }
}

impl CryptoAdapter for CryptoLive {
    fn new(key: &BackupKey) -> Result<Self, std::io::Error> {
        let mut legacy_key = *key.as_bytes();
        // pre-94169345fd legacy backups used the old big-endian aes key word layout
        for word in legacy_key.chunks_exact_mut(size_of::<u32>()) {
            word.reverse();
        }
        Self::new(&BackupKey(legacy_key), crypto::AesMode::Ecb)
    }

    #[cfg(test)]
    fn encrypt_chunk(&mut self, chunk: &mut [u8]) -> Result<(), std::io::Error> {
        self.buffer.as_slice_mut()[..chunk.len()].copy_from_slice(chunk);

        self.aes_ctx
            .execute(*self.buffer, 0, chunk.len(), crypto::Direction::Encrypt)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        chunk.copy_from_slice(&self.buffer.as_slice()[..chunk.len()]);

        Ok(())
    }

    fn decrypt_chunk(&mut self, chunk: &mut [u8]) -> Result<(), std::io::Error> {
        self.buffer.as_slice_mut()[..chunk.len()].copy_from_slice(chunk);

        self.aes_ctx
            .execute(*self.buffer, 0, chunk.len(), crypto::Direction::Decrypt)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        chunk.copy_from_slice(&self.buffer.as_slice()[..chunk.len()]);

        Ok(())
    }
}

pub fn restore_backup<F, R, C>(
    fs: &F,
    backup_file: R,
    backup_key: &BackupKey,
    file_size: usize,
) -> whence::Result<BackupMetadata, backup::Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions + MessageAllowed<fs::messages::SetMtime>,
    R: Read,
    C: CryptoAdapter,
{
    let reader = DecryptingReader::<_, C>::new(backup_file, backup_key, file_size).whence()?;
    extract_backup(fs, reader)
}

pub struct DecryptingReader<R, C> {
    inner: R,
    crypto: C,
    buffer: Box<[u8]>,
    buffer_valid: usize,
    buffer_pos: usize,
    file_size: usize,
    total_read: usize,
}

impl<R: Read, C: CryptoAdapter> DecryptingReader<R, C> {
    pub fn new(mut inner: R, key: &BackupKey, file_size: usize) -> Result<Self, std::io::Error> {
        let mut iv = [0; 16];
        inner.read_exact(&mut iv)?;
        let crypto = C::new(key)?;
        let file_size = file_size.saturating_sub(iv.len());
        if file_size % crypto::AES_BLOCK_SIZE != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid legacy ciphertext length: {file_size}"),
            ));
        }
        Ok(Self {
            inner,
            crypto,
            buffer: vec![0; CHUNK_SIZE].into_boxed_slice(),
            buffer_valid: 0,
            buffer_pos: 0,
            file_size,
            total_read: 0,
        })
    }
}

impl<R: Read, C: CryptoAdapter> Read for DecryptingReader<R, C> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.buffer_pos < self.buffer_valid {
            let to_copy = (self.buffer_valid - self.buffer_pos).min(buf.len());
            buf[..to_copy].copy_from_slice(&self.buffer[self.buffer_pos..self.buffer_pos + to_copy]);
            self.buffer_pos += to_copy;
            return Ok(to_copy);
        }

        let remaining = self.file_size.saturating_sub(self.total_read);
        if remaining == 0 {
            return Ok(0);
        }
        let n = remaining.min(self.buffer.len());
        self.inner.read_exact(&mut self.buffer[..n])?;

        self.total_read += n;
        let is_last_chunk = self.total_read >= self.file_size;

        self.crypto.decrypt_chunk(&mut self.buffer[..n])?;

        self.buffer_valid = if is_last_chunk { unpad_aes_block(&self.buffer[..n])?.len() } else { n };

        self.buffer_pos = 0;
        let to_copy = self.buffer_valid.min(buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);
        self.buffer_pos = to_copy;
        Ok(to_copy)
    }
}

fn unpad_aes_block(data: &[u8]) -> Result<&[u8], std::io::Error> {
    if data.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty data for unpadding"));
    };
    let padding_len = data[data.len() - 1] as usize;
    if padding_len == 0 || padding_len > crypto::AES_BLOCK_SIZE || padding_len > data.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid padding len {padding_len}"),
        ));
    }
    for &byte in &data[data.len() - padding_len..] {
        if byte != padding_len as u8 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid padding bytes"));
        }
    }
    Ok(&data[..data.len() - padding_len])
}

#[cfg(test)]
pub use encrypt::create_backup;

#[cfg(test)]
mod encrypt {
    use std::io::Write;

    use defer::defer;
    use fs::adapter::{BasicFsPermissions, FsAdapter};
    use fs::OpenFlags;
    use whence::{self, WhenceExt};

    use super::CryptoAdapter;
    use crate::core::{
        add_files_to_tar, append_metadata, ensure_parent_dir_exists, BackupFile, BackupKey, BackupMetadata,
        APPDATA_BACKUP_TEMP_PATH, APPDATA_PATH, CHUNK_SIZE,
    };
    use crate::utils::{calculate_file_hash, hex};

    pub fn create_backup<F, C>(
        fs: &F,
        backup_path: &str,
        backup_location: fs::Location,
        backup_key: &BackupKey,
        created_at: std::time::SystemTime,
    ) -> whence::Result<BackupFile, backup::Error>
    where
        F: FsAdapter + Clone,
        F::Permissions: BasicFsPermissions,
        C: CryptoAdapter,
    {
        log::info!("creating legacy encrypted backup at {backup_path} {backup_location:?}");

        fs.remove_if_exists(APPDATA_BACKUP_TEMP_PATH, fs::Location::EncryptedRoot).whence()?;
        fs.create_dir(APPDATA_BACKUP_TEMP_PATH, fs::Location::EncryptedRoot).whence()?;
        let _cleanup = defer(|| {
            fs.remove(APPDATA_BACKUP_TEMP_PATH, fs::Location::EncryptedRoot).ok();
        });

        fs.atomic_copy(APPDATA_PATH, APPDATA_BACKUP_TEMP_PATH, None, fs::Location::EncryptedRoot).whence()?;

        ensure_parent_dir_exists(fs, backup_path, backup_location).ok();
        fs.remove_if_exists(backup_path, backup_location).whence()?;
        let mut backup_file = fs.open_file(backup_path, backup_location, OpenFlags::CREATE).whence()?;
        let remove_incomplete_backup = defer(|| {
            fs.remove(backup_path, backup_location).ok();
        });

        let mut iv = [0; 16];
        getrandom::getrandom(&mut iv).unwrap();
        backup_file.write_all(&iv).whence()?;

        let crypto = C::new(backup_key).whence()?;
        let encrypting_writer = EncryptingWriter::new(&mut backup_file, crypto);
        let mut tar = tar::Builder::new(encrypting_writer);

        append_metadata(&mut tar, &BackupMetadata { created_at, device_name: None })?;

        let snapshot_appdata_path = format!("{APPDATA_BACKUP_TEMP_PATH}/{APPDATA_PATH}");
        add_files_to_tar(fs, &mut tar, &snapshot_appdata_path, APPDATA_PATH, fs::Location::EncryptedRoot)?;

        tar.finish().whence()?;
        let mut encrypting_writer = tar.into_inner().whence()?;
        encrypting_writer.flush().whence()?;

        let hash = calculate_file_hash(&mut backup_file).whence()?;
        log::info!("legacy backup created successfully {}", hex(&hash));

        remove_incomplete_backup.cancel();

        Ok(BackupFile { path: backup_path.to_string(), location: backup_location, hash, created_at })
    }

    struct EncryptingWriter<W, C> {
        inner: W,
        crypto: C,
        buffer: Box<[u8]>,
        buffered: usize,
    }

    impl<W, C> EncryptingWriter<W, C> {
        fn new(inner: W, crypto: C) -> Self {
            Self { inner, crypto, buffer: vec![0; CHUNK_SIZE].into_boxed_slice(), buffered: 0 }
        }
    }

    impl<W: Write, C: CryptoAdapter> Write for EncryptingWriter<W, C> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.buffered + buf.len() < CHUNK_SIZE {
                self.buffer[self.buffered..self.buffered + buf.len()].copy_from_slice(buf);
                self.buffered += buf.len();
                return Ok(buf.len());
            }

            let to_fill = CHUNK_SIZE - self.buffered;
            self.buffer[self.buffered..CHUNK_SIZE].copy_from_slice(&buf[..to_fill]);

            self.crypto.encrypt_chunk(&mut self.buffer)?;
            self.inner.write_all(&self.buffer)?;

            self.buffered = 0;

            Ok(to_fill)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            let mut padded = pad_to_aes_block(&self.buffer[..self.buffered]);
            self.crypto.encrypt_chunk(&mut padded)?;
            self.inner.write_all(&padded)?;
            self.buffered = 0;
            self.inner.flush()
        }
    }

    fn pad_to_aes_block(data: &[u8]) -> Vec<u8> {
        let padding_len = crypto::AES_BLOCK_SIZE - (data.len() % crypto::AES_BLOCK_SIZE);
        let mut padded = data.to_vec();
        padded.resize(data.len() + padding_len, padding_len as u8);
        padded
    }
}
