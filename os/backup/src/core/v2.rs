// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{Read, Write};
use std::time::SystemTime;

use defer::defer;
use fs::adapter::{BasicFsPermissions, FsAdapter};
use fs::OpenFlags;
use server::xous::DropDeallocate;
use server::MessageAllowed;
use whence::{self, WhenceExt};

use crate::core::{
    add_files_to_tar, append_metadata, ensure_parent_dir_exists, extract_backup, map_chunk_buffer,
    BackupFile, BackupKey, BackupMetadata, APPDATA_BACKUP_TEMP_PATH, APPDATA_PATH, CHUNK_SIZE,
};
use crate::utils::{calculate_file_hash, hex};
use crate::CryptoApi;

pub const PREFIX: &[u8; 8] = b"KEYOSBK2";

pub const BASE_NONCE_SIZE: usize = 12;
pub const TAG_SIZE: usize = 16;

pub struct ChunkHeader {
    seq: u32,
    len: u32,
}

impl ChunkHeader {
    pub const SIZE: usize = 8;

    fn new(seq: u32, len: u32) -> Self { Self { seq, len } }

    fn new_eof(seq: u32) -> Self { Self { seq, len: 0 } }

    fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        bytes[..4].copy_from_slice(&self.seq.to_le_bytes());
        bytes[4..].copy_from_slice(&self.len.to_le_bytes());
        bytes
    }

    fn from_bytes(bytes: [u8; Self::SIZE]) -> Self {
        Self {
            seq: u32::from_le_bytes(bytes[..4].try_into().unwrap()),
            len: u32::from_le_bytes(bytes[4..].try_into().unwrap()),
        }
    }

    fn is_eof(&self) -> bool { self.len == 0 }

    fn iv(&self, iv_base: &[u8; BASE_NONCE_SIZE]) -> [u8; 12] {
        let mut iv = *iv_base;
        for (a, b) in iv[8..].iter_mut().zip(self.seq.to_be_bytes()) {
            *a ^= b;
        }
        iv
    }
}

pub struct CryptoLive {
    crypto: CryptoApi,
    key: BackupKey,
    buffer: DropDeallocate,
}

pub trait CryptoAdapter: Sized {
    fn new(key: &BackupKey) -> Result<Self, std::io::Error>;
    fn encrypt_chunk(
        &mut self,
        iv: [u8; 12],
        aad: &[u8],
        chunk: &mut [u8],
        tag: &mut [u8; 16],
    ) -> Result<(), std::io::Error>;
    fn decrypt_chunk(
        &mut self,
        iv: [u8; 12],
        aad: &[u8],
        chunk: &mut [u8],
        tag: &[u8; 16],
    ) -> Result<(), std::io::Error>;
}

impl CryptoAdapter for CryptoLive {
    fn new(key: &BackupKey) -> Result<Self, std::io::Error> {
        Ok(Self { crypto: CryptoApi::default(), key: key.clone(), buffer: map_chunk_buffer()? })
    }

    fn encrypt_chunk(
        &mut self,
        iv: [u8; 12],
        aad: &[u8],
        chunk: &mut [u8],
        tag: &mut [u8; 16],
    ) -> Result<(), std::io::Error> {
        let ctx = self
            .crypto
            .setup_aes(self.key.as_bytes(), crypto::AesMode::Gcm { iv })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "failed to setup aes"))?;

        ctx.add_aad(aad).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        if !chunk.is_empty() {
            self.buffer.as_slice_mut()[..chunk.len()].copy_from_slice(chunk);
            ctx.execute(*self.buffer, 0, chunk.len(), crypto::Direction::Encrypt)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
            chunk.copy_from_slice(&self.buffer.as_slice()[..chunk.len()]);
        }

        *tag = ctx.gcm_tag().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        Ok(())
    }

    fn decrypt_chunk(
        &mut self,
        iv: [u8; 12],
        aad: &[u8],
        chunk: &mut [u8],
        expected_tag: &[u8; 16],
    ) -> Result<(), std::io::Error> {
        let ctx = self
            .crypto
            .setup_aes(self.key.as_bytes(), crypto::AesMode::Gcm { iv })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "failed to setup aes"))?;

        ctx.add_aad(aad).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        if !chunk.is_empty() {
            self.buffer.as_slice_mut()[..chunk.len()].copy_from_slice(chunk);
            ctx.execute(*self.buffer, 0, chunk.len(), crypto::Direction::Decrypt)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        }

        let tag = ctx.gcm_tag().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        if &tag != expected_tag {
            log::error!("GCM tag invalid");
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "GCM tag invalid"));
        }

        if !chunk.is_empty() {
            chunk.copy_from_slice(&self.buffer.as_slice()[..chunk.len()]);
        }
        Ok(())
    }
}

pub fn restore_backup<F, R, C>(
    fs: &F,
    backup_file: R,
    backup_key: &BackupKey,
) -> whence::Result<BackupMetadata, backup::Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions + MessageAllowed<fs::messages::SetMtime>,
    R: Read,
    C: CryptoAdapter,
{
    let mut reader = DecryptingReader::<_, C>::new(backup_file, backup_key).whence()?;
    let metadata = extract_backup(fs, &mut reader)?;
    std::io::copy(&mut reader, &mut std::io::sink()).whence()?;
    Ok(metadata)
}

pub fn create_backup<F, C>(
    fs: &F,
    backup_path: &str,
    backup_location: fs::Location,
    backup_key: &BackupKey,
    device_name: Option<String>,
) -> whence::Result<BackupFile, backup::Error>
where
    F: FsAdapter + Clone,
    F::Permissions: BasicFsPermissions,
    C: CryptoAdapter,
{
    log::info!("creating encrypted backup at {backup_path} {backup_location:?}");

    fs.remove_if_exists(APPDATA_BACKUP_TEMP_PATH, fs::Location::EncryptedRoot).whence()?;
    fs.create_dir(APPDATA_BACKUP_TEMP_PATH, fs::Location::EncryptedRoot).whence()?;
    let _cleanup = defer(|| {
        fs.remove(APPDATA_BACKUP_TEMP_PATH, fs::Location::EncryptedRoot).ok();
    });

    let created_at = SystemTime::now();
    fs.atomic_copy(APPDATA_PATH, APPDATA_BACKUP_TEMP_PATH, None, fs::Location::EncryptedRoot).whence()?;

    log::info!("creating encrypted backup file at {backup_path}");

    ensure_parent_dir_exists(fs, backup_path, backup_location).ok();
    fs.remove_if_exists(backup_path, backup_location).whence()?;
    let mut backup_file = fs.open_file(backup_path, backup_location, OpenFlags::CREATE).whence()?;
    let remove_incomplete_backup = defer(|| {
        fs.remove(backup_path, backup_location).ok();
    });

    let encrypting_writer = EncryptingWriter::<_, C>::new(&mut backup_file, backup_key).whence()?;
    let mut tar = tar::Builder::new(encrypting_writer);

    let metadata = BackupMetadata { created_at, device_name };
    append_metadata(&mut tar, &metadata)?;

    let snapshot_appdata_path = format!("{APPDATA_BACKUP_TEMP_PATH}/{APPDATA_PATH}");
    log::info!("backing up snapshot directory {snapshot_appdata_path}");
    add_files_to_tar(fs, &mut tar, &snapshot_appdata_path, APPDATA_PATH, fs::Location::EncryptedRoot)?;

    tar.finish().whence()?;
    let encrypting_writer = tar.into_inner().whence()?;
    encrypting_writer.finish().whence()?;

    let hash = calculate_file_hash(&mut backup_file).whence()?;
    log::info!("backup created successfully {}", hex(&hash));

    remove_incomplete_backup.cancel();

    Ok(BackupFile { path: backup_path.to_string(), location: backup_location, hash, created_at })
}

pub struct EncryptingWriter<W, C> {
    inner: W,
    crypto: C,
    iv_base: [u8; BASE_NONCE_SIZE],
    counter: u32,
    buffer: Box<[u8]>,
    buffered: usize,
}

impl<W: Write, C: CryptoAdapter> EncryptingWriter<W, C> {
    pub fn new(mut inner: W, key: &BackupKey) -> Result<Self, std::io::Error> {
        inner.write_all(PREFIX)?;
        let mut iv_base = [0; BASE_NONCE_SIZE];
        getrandom::getrandom(&mut iv_base)?;
        inner.write_all(&iv_base)?;
        Ok(Self {
            inner,
            crypto: C::new(key)?,
            iv_base,
            counter: 0,
            buffer: vec![0; CHUNK_SIZE].into_boxed_slice(),
            buffered: 0,
        })
    }

    fn write_chunk(&mut self, len: usize) -> std::io::Result<()> {
        let header = ChunkHeader::new(self.counter, len as u32);
        let iv = header.iv(&self.iv_base);
        let header_bytes = header.to_bytes();

        let mut tag = [0; TAG_SIZE];
        self.crypto.encrypt_chunk(iv, &header_bytes, &mut self.buffer[..len], &mut tag)?;

        self.inner.write_all(&header_bytes)?;
        self.inner.write_all(&self.buffer[..len])?;
        self.inner.write_all(&tag)?;

        self.counter += 1;
        self.buffered = 0;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<W> {
        if self.buffered != 0 {
            self.write_chunk(self.buffered)?;
        }

        let header = ChunkHeader::new_eof(self.counter);
        let iv = header.iv(&self.iv_base);
        let header_bytes = header.to_bytes();

        let mut tag = [0; TAG_SIZE];
        self.crypto.encrypt_chunk(iv, &header_bytes, &mut [], &mut tag)?;

        self.inner.write_all(&header_bytes)?;
        self.inner.write_all(&tag)?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write, C: CryptoAdapter> Write for EncryptingWriter<W, C> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.buffered + buf.len() < CHUNK_SIZE {
            self.buffer[self.buffered..self.buffered + buf.len()].copy_from_slice(buf);
            self.buffered += buf.len();
            Ok(buf.len())
        } else {
            let to_fill = CHUNK_SIZE - self.buffered;
            self.buffer[self.buffered..CHUNK_SIZE].copy_from_slice(&buf[..to_fill]);
            self.write_chunk(CHUNK_SIZE)?;
            Ok(to_fill)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.buffered != 0 {
            self.write_chunk(self.buffered)?;
        }
        self.inner.flush()
    }
}

pub struct DecryptingReader<R, C> {
    inner: R,
    crypto: C,
    iv_base: [u8; BASE_NONCE_SIZE],
    counter: u32,
    buffer: Box<[u8]>,
    buffer_valid: usize,
    buffer_pos: usize,
    finished: bool,
}

impl<R: Read, C: CryptoAdapter> DecryptingReader<R, C> {
    pub fn new(mut inner: R, key: &BackupKey) -> Result<Self, std::io::Error> {
        let mut prefix = [0; PREFIX.len()];
        inner.read_exact(&mut prefix)?;
        if prefix != *PREFIX {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid v2 backup prefix"));
        }
        let mut iv_base = [0; BASE_NONCE_SIZE];
        inner.read_exact(&mut iv_base)?;
        Ok(Self {
            inner,
            crypto: C::new(key)?,
            iv_base,
            counter: 0,
            buffer: vec![0; CHUNK_SIZE].into_boxed_slice(),
            buffer_valid: 0,
            buffer_pos: 0,
            finished: false,
        })
    }

    fn copy_buffered(&mut self, buf: &mut [u8]) -> usize {
        let to_copy = (self.buffer_valid - self.buffer_pos).min(buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[self.buffer_pos..self.buffer_pos + to_copy]);
        self.buffer_pos += to_copy;
        to_copy
    }
}

impl<R: Read, C: CryptoAdapter> Read for DecryptingReader<R, C> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.buffer_pos < self.buffer_valid {
            return Ok(self.copy_buffered(buf));
        }
        if self.finished {
            return Ok(0);
        }

        let mut header_bytes = [0; ChunkHeader::SIZE];
        self.inner.read_exact(&mut header_bytes)?;
        let header = ChunkHeader::from_bytes(header_bytes);

        if header.seq != self.counter {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unexpected chunk sequence {}", header.seq),
            ));
        }

        let len = header.len as usize;
        if len > CHUNK_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid chunk size {len}"),
            ));
        }

        let iv = header.iv(&self.iv_base);
        let mut tag = [0; TAG_SIZE];
        if header.is_eof() {
            self.inner.read_exact(&mut tag)?;
            self.crypto.decrypt_chunk(iv, &header_bytes, &mut [], &tag)?;
            self.finished = true;
            return Ok(0);
        }

        self.inner.read_exact(&mut self.buffer[..len])?;
        self.inner.read_exact(&mut tag)?;
        self.crypto.decrypt_chunk(iv, &header_bytes, &mut self.buffer[..len], &tag)?;

        self.counter += 1;
        self.buffer_valid = len;
        self.buffer_pos = 0;
        Ok(self.copy_buffered(buf))
    }
}
