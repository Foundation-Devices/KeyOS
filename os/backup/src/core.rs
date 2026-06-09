// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{Read, Write};
use std::time::SystemTime;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use defer::defer;
use fs::adapter::{BasicFsPermissions, FileAdapter, FsAdapter};
use fs::OpenFlags;
use server::xous::{self, DropDeallocate};
use server::MessageAllowed;
use whence::{self, WhenceExt};

use super::utils::{calculate_file_hash, hex};
use crate::{crypto_permissions::CryptoPermissions, CryptoApi};

#[derive(Clone)]
pub struct BackupKey([u8; 32]);

impl BackupKey {
    pub fn from_app_seed(app_seed: [u8; 32]) -> Self {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(app_seed);
        hasher.update(b"backup_encryption");
        Self(hasher.finalize().into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] { &self.0 }
}

impl std::fmt::Debug for BackupKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BackupKey").field(&"<redacted>").finish()
    }
}

const APPDATA_PATH: &str = "appdata";
const APPDATA_OLD_PATH: &str = "appdata-old";
const APPDATA_BACKUP_TEMP_PATH: &str = "appdata-backup-temp";
const APPDATA_RESTORE_TEMP_PATH: &str = "appdata-restore-temp";
const METADATA_FILE: &str = ".backup_metadata.json";

const CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct BackupFile {
    pub created_at: SystemTime,
    pub path: String,
    pub location: fs::Location,
    pub hash: [u8; 32],
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupMetadata {
    pub created_at: SystemTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
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

    let mut iv = [0u8; 12];
    getrandom::getrandom(&mut iv).unwrap();
    backup_file.write_all(&iv).whence()?;

    let crypto = C::new(backup_key, iv).whence()?;

    let encrypting_writer = EncryptingWriter::new(backup_file, crypto);
    let mut tar = tar::Builder::new(encrypting_writer);

    // metadata file as first entry
    let metadata = BackupMetadata { created_at, device_name };
    let metadata_json = serde_json::to_vec(&metadata)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        .whence()?;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(metadata_json.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    tar.append_data(&mut header, METADATA_FILE, metadata_json.as_slice()).whence()?;

    let snapshot_appdata_path = format!("{APPDATA_BACKUP_TEMP_PATH}/{APPDATA_PATH}");
    log::info!("backing up snapshot directory {snapshot_appdata_path}");
    add_files_to_tar(fs, &mut tar, &snapshot_appdata_path, APPDATA_PATH, fs::Location::EncryptedRoot)?;

    tar.finish().whence()?;
    let mut encrypting_writer = tar.into_inner().whence()?;
    encrypting_writer.flush().whence()?;

    let mut file = encrypting_writer.inner;
    let hash = calculate_file_hash(&mut file).whence()?;
    log::info!("backup created successfully {}", hex(&hash));

    remove_incomplete_backup.cancel();

    Ok(BackupFile { path: backup_path.to_string(), location: backup_location, hash, created_at })
}

fn add_files_to_tar<F, W>(
    fs: &F,
    tar: &mut tar::Builder<W>,
    source_path: &str,
    tar_prefix: &str,
    location: fs::Location,
) -> whence::Result<(), backup::Error>
where
    F: FsAdapter + Clone,
    F::Permissions: BasicFsPermissions,
    W: Write,
{
    let walker = fs.walk_dir(source_path, location).whence()?;
    let no_backup = format!("/{}", backup::DO_NOT_BACKUP_FOLDER);

    for entry_result in walker {
        let (path, entry) = entry_result.whence()?;

        if path.contains(&no_backup) {
            continue;
        }

        let relative_path = path.strip_prefix(&format!("{}/", source_path)).unwrap_or(&path);
        let tar_path = format!("{}/{}", tar_prefix, relative_path);

        if entry.is_file {
            let mut file = fs.open_file(&path, location, OpenFlags::READ_ONLY).whence()?;
            let metadata = file.metadata().whence()?;

            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(metadata.size);
            header.set_mode(0o644);
            header.set_mtime(datetime_to_timestamp(&metadata.modified));
            header.set_cksum();

            tar.append_data(&mut header, &tar_path, &mut file).whence()?;
        }
    }

    Ok(())
}

pub fn restore_backup<F, C>(
    fs: &F,
    backup_path: &str,
    backup_location: fs::Location,
    backup_key: &BackupKey,
) -> whence::Result<BackupMetadata, backup::Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions + MessageAllowed<fs::messages::SetMtime>,
    C: CryptoAdapter,
{
    log::info!("restoring backup from {backup_path} at location {backup_location:?}");

    let mut backup_file = match fs.open_file(backup_path, backup_location, OpenFlags::READ_ONLY) {
        Ok(file) => file,
        Err(fs::Error::FileNotFound) => return Err(backup::Error::InvalidBackupFile).whence()?,
        Err(e) => return Err(e).whence(),
    };

    let hash = calculate_file_hash(&mut backup_file).whence()?;
    log::info!("restore file SHA256: {}", hex(&hash));

    fs.remove_if_exists(APPDATA_RESTORE_TEMP_PATH, fs::Location::EncryptedRoot).whence()?;
    fs.create_dir(APPDATA_RESTORE_TEMP_PATH, fs::Location::EncryptedRoot).whence()?;
    let _defer = defer(|| {
        fs.remove(APPDATA_RESTORE_TEMP_PATH, fs::Location::EncryptedRoot).ok();
    });

    let mut iv = [0u8; 12];
    backup_file.read_exact(&mut iv).whence()?;
    let crypto = C::new(backup_key, iv).whence()?;

    let decrypting_reader = DecryptingReader::new(backup_file, crypto);
    let mut tar_archive = tar::Archive::new(decrypting_reader);

    log::info!("extracting backup tar to {APPDATA_RESTORE_TEMP_PATH}");
    let entries = tar_archive.entries().whence()?;

    let mut metadata = None;

    for entry_result in entries {
        let mut entry = entry_result.whence()?;
        let path = entry.path().whence()?;
        let Some(entry_path) = path.to_str() else {
            continue;
        };

        if entry_path == METADATA_FILE {
            let mut metadata_bytes = Vec::new();
            entry.read_to_end(&mut metadata_bytes).whence()?;
            metadata = Some(
                serde_json::from_slice::<BackupMetadata>(&metadata_bytes)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
                    .whence()?,
            );
            continue;
        }

        let dest_path = format!("{APPDATA_RESTORE_TEMP_PATH}/{entry_path}");

        log::debug!("restoring file {entry_path}");

        if entry.header().entry_type().is_file() {
            ensure_parent_dir_exists(fs, &dest_path, fs::Location::EncryptedRoot)?;
            let mut dest_file =
                fs.open_file(&dest_path, fs::Location::EncryptedRoot, OpenFlags::CREATE).whence()?;

            std::io::copy(&mut entry, &mut dest_file).whence()?;

            if let Ok(mtime) = entry.header().mtime() {
                if let Some(datetime) = timestamp_to_datetime(mtime) {
                    dest_file.set_mtime(datetime).ok();
                }
            }
        }
    }

    let metadata = metadata.ok_or_else(|| backup::Error::InvalidBackupFile).whence()?;

    log::info!("renaming {APPDATA_PATH} to {APPDATA_OLD_PATH}");
    fs.remove_if_exists(APPDATA_OLD_PATH, fs::Location::EncryptedRoot).whence()?;
    fs.rename(APPDATA_PATH, APPDATA_OLD_PATH, fs::Location::EncryptedRoot).whence()?;
    let rollback = defer(|| {
        log::info!("failed to restore appdata, rolling back");
        fs.rename(APPDATA_OLD_PATH, APPDATA_PATH, fs::Location::EncryptedRoot)
            .inspect_err(|rollback_err| {
                log::error!("rollback failed: {rollback_err:?}");
            })
            .ok();
    });

    let extracted_appdata = format!("{APPDATA_RESTORE_TEMP_PATH}/{APPDATA_PATH}");
    log::info!("renaming {extracted_appdata} to {APPDATA_PATH}");
    fs.rename(&extracted_appdata, APPDATA_PATH, fs::Location::EncryptedRoot).whence()?;
    rollback.cancel();

    log::info!("removing old appdata directory");
    fs.remove(APPDATA_OLD_PATH, fs::Location::EncryptedRoot)
        .inspect_err(|e| {
            log::warn!("failed to remove {APPDATA_OLD_PATH}: {e:?}");
        })
        .ok();

    log::info!("backup restored successfully");
    Ok(metadata)
}

pub trait CryptoAdapter: Sized {
    fn new(key: &BackupKey, iv: [u8; 12]) -> Result<Self, std::io::Error>;
    fn encrypt_chunk(&mut self, chunk: &mut [u8], tag: &mut [u8; 16]) -> Result<(), std::io::Error>;
    fn decrypt_chunk(&mut self, chunk: &mut [u8], tag: &[u8; 16]) -> Result<(), std::io::Error>;
}

pub struct CryptoLive {
    aes_ctx: crypto::AesContext<CryptoPermissions>,
    buffer: DropDeallocate,
}

impl CryptoAdapter for CryptoLive {
    fn new(key: &BackupKey, iv: [u8; 12]) -> Result<Self, std::io::Error> {
        let crypto = CryptoApi::default();

        let aes_ctx = crypto
            .setup_aes(key.as_bytes(), crypto::AesMode::Gcm { iv })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "failed to setup aes"))?;

        let mapped_buffer =
            xous::map_memory(None, None, CHUNK_SIZE.next_multiple_of(0x1000), xous::MemoryFlags::W)
                .map(DropDeallocate::new)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "failed to map memory"))?;

        Ok(Self { aes_ctx, buffer: mapped_buffer })
    }

    fn encrypt_chunk(&mut self, chunk: &mut [u8], tag: &mut [u8; 16]) -> Result<(), std::io::Error> {
        self.buffer.as_slice_mut()[..chunk.len()].copy_from_slice(chunk);

        self.aes_ctx
            .execute(*self.buffer, 0, chunk.len(), crypto::Direction::Encrypt)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        *tag = self
            .aes_ctx
            .gcm_tag()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        chunk.copy_from_slice(&self.buffer.as_slice()[..chunk.len()]);

        Ok(())
    }

    fn decrypt_chunk(&mut self, chunk: &mut [u8], expected_tag: &[u8; 16]) -> Result<(), std::io::Error> {
        self.buffer.as_slice_mut()[..chunk.len()].copy_from_slice(chunk);

        self.aes_ctx
            .execute(*self.buffer, 0, chunk.len(), crypto::Direction::Decrypt)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        let tag = self
            .aes_ctx
            .gcm_tag()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        if &tag != expected_tag {
            log::error!("GCM tag invalid");
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "GCM tag invalid"));
        }
        chunk.copy_from_slice(&self.buffer.as_slice()[..chunk.len()]);

        Ok(())
    }
}

struct EncryptingWriter<W, C> {
    inner: W,
    crypto: C,
    buffer: Vec<u8>,
    buffered: usize,
}

impl<W, C> EncryptingWriter<W, C> {
    fn new(inner: W, crypto: C) -> Self { Self { inner, crypto, buffer: vec![0u8; CHUNK_SIZE], buffered: 0 } }
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

        let mut tag = [0; 16];
        self.crypto.encrypt_chunk(&mut self.buffer, &mut tag)?;
        self.inner.write_all(&(self.buffer.len() as u32).to_le_bytes())?;
        self.inner.write_all(&self.buffer)?;
        self.inner.write_all(&tag)?;

        self.buffered = 0;

        Ok(to_fill)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let padded_len = self.buffered.next_multiple_of(crypto::AES_BLOCK_SIZE);
        self.buffer[self.buffered..padded_len].fill(0);
        let mut tag = [0; 16];
        self.crypto.encrypt_chunk(&mut self.buffer[..padded_len], &mut tag)?;
        self.inner.write_all(&(self.buffered as u32).to_le_bytes())?;
        self.inner.write_all(&self.buffer[..self.buffered])?;
        self.inner.write_all(&tag)?;
        self.buffered = 0;
        self.inner.flush()
    }
}

struct DecryptingReader<R, C> {
    inner: R,
    crypto: C,
    buffer: [u8; CHUNK_SIZE],
    buffer_valid: usize,
    buffer_pos: usize,
}

impl<R, C> DecryptingReader<R, C>
where
    R: Read,
{
    fn new(inner: R, crypto: C) -> Self {
        Self { inner, crypto, buffer: [0u8; CHUNK_SIZE], buffer_valid: 0, buffer_pos: 0 }
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

        let mut size_bytes = [0; 4];
        self.inner.read_exact(&mut size_bytes)?;
        let len = u32::from_le_bytes(size_bytes) as usize;
        if len > CHUNK_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid chunk size: {len}"),
            ));
        }
        self.inner.read_exact(&mut self.buffer[..len])?;
        let mut tag = [0; 16];
        self.inner.read_exact(&mut tag)?;

        let padded_len = len.next_multiple_of(crypto::AES_BLOCK_SIZE);
        self.buffer[len..padded_len].fill(0);
        self.crypto.decrypt_chunk(&mut self.buffer[..padded_len], &tag)?;

        self.buffer_valid = len;

        self.buffer_pos = 0;
        let to_copy = self.buffer_valid.min(buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);
        self.buffer_pos = to_copy;
        Ok(to_copy)
    }
}

fn ensure_parent_dir_exists<F: FsAdapter>(
    fs: &F,
    path: &str,
    location: fs::Location,
) -> Result<(), backup::Error>
where
    F::Permissions: server::MessageAllowed<fs::messages::CreateDirMessage>,
    F::Permissions: server::MessageAllowed<fs::messages::CloseDir>,
{
    if let Some(parent) = path.rsplit_once('/').map(|(parent, _)| parent) {
        if !parent.is_empty() {
            match fs.create_dir(parent, location) {
                Ok(_) => {}
                Err(fs::Error::FileAlreadyExists) => {}
                Err(fs::Error::FileNotFound) => {
                    // parent doesn't exist, create it first then retry
                    ensure_parent_dir_exists(fs, parent, location)?;
                    fs.create_dir(parent, location).ok();
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

fn datetime_to_timestamp(dt: &fs::DateTime) -> u64 {
    let date = NaiveDate::from_ymd_opt(dt.date.year as i32, dt.date.month as u32, dt.date.day as u32);
    let time = NaiveTime::from_hms_milli_opt(
        dt.time.hour as u32,
        dt.time.min as u32,
        dt.time.sec as u32,
        dt.time.millis as u32,
    );

    date.zip(time)
        .map(|(d, t)| {
            let datetime = NaiveDateTime::new(d, t);
            datetime.and_utc().timestamp() as u64
        })
        .unwrap_or_default()
}

fn timestamp_to_datetime(timestamp: u64) -> Option<fs::DateTime> {
    use chrono::{DateTime, Datelike, Timelike};

    let datetime = DateTime::from_timestamp(timestamp as i64, 0)?;

    Some(fs::DateTime {
        date: fs::Date {
            year: datetime.year() as u16,
            month: datetime.month() as u16,
            day: datetime.day() as u16,
        },
        time: fs::Time {
            hour: datetime.hour() as u16,
            min: datetime.minute() as u16,
            sec: datetime.second() as u16,
            millis: 0,
        },
    })
}

#[cfg(test)]
mod tests {

    use backup::DO_NOT_BACKUP_FOLDER;
    use fs::adapter::test_utils::FsTest;

    use super::*;

    struct BackupCryptoTest {
        key: BackupKey,
        iv: [u8; 12],
        chksum: u8,
    }

    impl CryptoAdapter for BackupCryptoTest {
        fn new(key: &BackupKey, iv: [u8; 12]) -> Result<Self, std::io::Error> {
            Ok(Self { key: key.clone(), iv, chksum: 0 })
        }

        fn encrypt_chunk(&mut self, chunk: &mut [u8], tag: &mut [u8; 16]) -> Result<(), std::io::Error> {
            for block in chunk.chunks_exact_mut(16) {
                for (b, k) in block.iter_mut().zip(&self.iv) {
                    *b = b.wrapping_add(*k);
                }
                for (b, k) in block.iter_mut().zip(self.key.as_bytes()) {
                    *b = *b ^ k;
                }
            }
            for c in chunk {
                self.chksum = self.chksum.wrapping_add(*c);
            }
            tag.fill(self.chksum);
            Ok(())
        }

        fn decrypt_chunk(&mut self, chunk: &mut [u8], expected_tag: &[u8; 16]) -> Result<(), std::io::Error> {
            for c in &*chunk {
                self.chksum = self.chksum.wrapping_add(*c);
            }
            if !expected_tag.iter().all(|c| *c == self.chksum) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Invalid tag: {expected_tag:?} != [{};16]", self.chksum),
                ));
            }
            for block in chunk.chunks_exact_mut(16) {
                for (b, k) in block.iter_mut().zip(self.key.as_bytes()) {
                    *b = *b ^ k;
                }
                for (b, k) in block.iter_mut().zip(&self.iv) {
                    *b = b.wrapping_sub(*k);
                }
            }
            Ok(())
        }
    }

    struct TarEntry {
        path: String,
        is_dir: bool,
        mode: u32,
        contents: Vec<u8>,
    }

    fn tar_entries<C: CryptoAdapter>(
        fs: &FsTest,
        encrypted_path: &str,
        backup_key: &BackupKey,
    ) -> Vec<TarEntry> {
        let mut backup_file =
            fs.open_file(encrypted_path, fs::Location::EncryptedRoot, OpenFlags::READ_ONLY).unwrap();

        let mut iv = [0u8; 12];
        backup_file.read_exact(&mut iv).unwrap();

        let crypto = C::new(backup_key, iv).unwrap();

        let decrypting_reader = DecryptingReader::new(backup_file, crypto);
        let mut archive = tar::Archive::new(decrypting_reader);
        let mut result = Vec::new();

        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_str().unwrap().to_string();
            let is_dir = entry.header().entry_type().is_dir();
            let mode = entry.header().mode().unwrap() & 0o777;
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).unwrap();

            result.push(TarEntry { path, is_dir, mode, contents });
        }

        result
    }

    fn init() { simple_logger::SimpleLogger::new().with_level(log::LevelFilter::Debug).env().init().ok(); }

    #[test]
    fn single_app_backup() {
        init();
        let fs = FsTest::default();

        let backup_path = "backup/backup.tar";

        let test_file_path = "appdata/app1/data.txt";
        let test_content = b"hello world";

        fs.write_file(test_file_path, test_content, fs::Location::EncryptedRoot);

        let test_key = BackupKey::from_app_seed([0x42; 32]);
        let backup_file = create_backup::<_, BackupCryptoTest>(
            &fs,
            backup_path,
            fs::Location::EncryptedRoot,
            &test_key,
            None,
        )
        .unwrap();
        let entries = tar_entries::<BackupCryptoTest>(&fs, &backup_file.path, &test_key);

        assert_eq!(entries.len(), 2);

        assert_eq!(entries[0].path, METADATA_FILE);
        let metadata: BackupMetadata = serde_json::from_slice(&entries[0].contents).unwrap();
        assert_eq!(metadata.created_at, backup_file.created_at);
        assert_eq!(metadata.device_name, None);

        let file = &entries[1];
        assert_eq!(file.path, test_file_path);
        assert!(!file.is_dir);
        assert_eq!(file.mode, 0o644);
        assert_eq!(file.contents, test_content);
    }

    #[test]
    fn device_name_in_metadata() {
        init();
        let fs = FsTest::default();

        fs.write_file("appdata/app1/data.txt", b"x", fs::Location::EncryptedRoot);
        let test_key = BackupKey::from_app_seed([0x42; 32]);
        let backup_file = create_backup::<_, BackupCryptoTest>(
            &fs,
            "backup/backup.tar",
            fs::Location::EncryptedRoot,
            &test_key,
            Some("My Prime".to_string()),
        )
        .unwrap();

        let entries = tar_entries::<BackupCryptoTest>(&fs, &backup_file.path, &test_key);
        let metadata: BackupMetadata = serde_json::from_slice(&entries[0].contents).unwrap();
        assert_eq!(metadata.device_name.as_deref(), Some("My Prime"));

        // backups produced before this field was added (i.e. no `device_name` key) still parse.
        let legacy: BackupMetadata =
            serde_json::from_str(r#"{"created_at":{"secs_since_epoch":0,"nanos_since_epoch":0}}"#).unwrap();
        assert_eq!(legacy.device_name, None);
    }

    #[test]
    fn backup_and_restore() {
        init();
        let fs = FsTest::default();

        let backup_path = "backup/backup.tar";

        let small_bin_path = "appdata/wallet/small.bin";
        let medium_bin_path = "appdata/wallet/medium.bin";
        let large_bin_path = "appdata/photos/vacation.jpg";
        let config_path = "appdata/settings/config.json";
        let nested_path = "appdata/photos/deep/nested/image.png";
        let new_app_path = "appdata/new_app/data.txt";

        let small_binary: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
        fs.write_file(small_bin_path, &small_binary, fs::Location::EncryptedRoot);

        let medium_binary: Vec<u8> = (0..100_000).map(|i| ((i * 7) % 256) as u8).collect();
        fs.write_file(medium_bin_path, &medium_binary, fs::Location::EncryptedRoot);

        let large_binary: Vec<u8> = (0..2_000_000).map(|i| ((i * 13 + 42) % 256) as u8).collect();
        fs.write_file(large_bin_path, &large_binary, fs::Location::EncryptedRoot);

        let text_data = b"some text configuration data";
        fs.write_file(config_path, text_data, fs::Location::EncryptedRoot);

        let nested_binary: Vec<u8> = (0..50_000).map(|i| ((i * 3) % 256) as u8).collect();
        fs.write_file(nested_path, &nested_binary, fs::Location::EncryptedRoot);

        let original_mtime = {
            let original_file =
                fs.open_file(small_bin_path, fs::Location::EncryptedRoot, OpenFlags::READ_ONLY).unwrap();

            let original_metadata = original_file.metadata().unwrap();
            original_metadata.modified
        };

        let test_key = BackupKey::from_app_seed([0x42; 32]);
        let backup_file = create_backup::<_, BackupCryptoTest>(
            &fs,
            backup_path,
            fs::Location::EncryptedRoot,
            &test_key,
            None,
        )
        .unwrap();

        let encrypted_backup = fs.read_file_contents(&backup_file.path, fs::Location::EncryptedRoot).unwrap();
        log::info!(
            "encrypted backup size: {} bytes ({:.2} MB)",
            encrypted_backup.len(),
            encrypted_backup.len() as f64 / 1_000_000.0
        );

        fs.write_file(small_bin_path, b"corrupted", fs::Location::EncryptedRoot);
        fs.write_file(large_bin_path, b"corrupted", fs::Location::EncryptedRoot);
        fs.write_file(new_app_path, b"should not exist after restore", fs::Location::EncryptedRoot);

        log::info!("restoring backup");
        let metadata = restore_backup::<_, BackupCryptoTest>(
            &fs,
            &backup_file.path,
            fs::Location::EncryptedRoot,
            &test_key,
        )
        .unwrap();

        assert_eq!(metadata.created_at, backup_file.created_at, "backup metadata created_at mismatch");

        log::info!("verifying restored files");
        let restored_small = fs.read_file_contents(small_bin_path, fs::Location::EncryptedRoot).unwrap();
        assert_eq!(restored_small, small_binary, "small binary file mismatch");

        let restored_medium = fs.read_file_contents(medium_bin_path, fs::Location::EncryptedRoot).unwrap();
        assert_eq!(restored_medium, medium_binary, "medium binary file mismatch");

        let restored_large = fs.read_file_contents(large_bin_path, fs::Location::EncryptedRoot).unwrap();
        assert_eq!(restored_large, large_binary, "large binary file mismatch");

        let restored_text = fs.read_file_contents(config_path, fs::Location::EncryptedRoot).unwrap();
        assert_eq!(restored_text, text_data, "text file mismatch");

        let restored_nested = fs.read_file_contents(nested_path, fs::Location::EncryptedRoot).unwrap();
        assert_eq!(restored_nested, nested_binary, "nested binary file mismatch");

        let result = fs.open_file(new_app_path, fs::Location::EncryptedRoot, OpenFlags::READ_ONLY);
        assert!(result.is_err(), "new_app should not exist after restore");

        let restored_file =
            fs.open_file(small_bin_path, fs::Location::EncryptedRoot, OpenFlags::READ_ONLY).unwrap();
        let restored_metadata = restored_file.metadata().unwrap();
        let restored_mtime = restored_metadata.modified;
        drop(restored_file);

        // Compare whole timestamps and allow small drift for tar/fs timestamp resolution
        // (some filesystems store mtime with 2-second granularity).
        let original_ts = datetime_to_timestamp(&original_mtime);
        let restored_ts = datetime_to_timestamp(&restored_mtime);
        let ts_diff = restored_ts.abs_diff(original_ts);
        assert!(
            ts_diff <= 2,
            "mtime mismatch: restored={restored_ts}, original={original_ts}, diff={ts_diff}s"
        );

        log::info!("backup and restore completed successfully");
    }

    #[test]
    fn do_not_backup_folder_omitted() {
        init();
        let fs = FsTest::default();

        let backup_path = "backup/backup.tar";

        let app1_data_path = "appdata/app1/data.txt";
        let app2_config_path = "appdata/app2/config.json";
        let cache_path = format!("appdata/app2/{DO_NOT_BACKUP_FOLDER}/cache.tmp");
        let log_path = format!("appdata/app1/{DO_NOT_BACKUP_FOLDER}/temp.log");
        let nested_path = format!("appdata/app1/some_folder/{DO_NOT_BACKUP_FOLDER}/secret.txt");

        fs.write_file(&app1_data_path, b"important data", fs::Location::EncryptedRoot);
        fs.write_file(&app2_config_path, b"config", fs::Location::EncryptedRoot);

        fs.write_file(&cache_path, b"cache data", fs::Location::EncryptedRoot);
        fs.write_file(&log_path, b"log data", fs::Location::EncryptedRoot);
        fs.write_file(&nested_path, b"nested secret", fs::Location::EncryptedRoot);

        let test_key = BackupKey::from_app_seed([0x42; 32]);
        let backup_file = create_backup::<_, BackupCryptoTest>(
            &fs,
            &backup_path,
            fs::Location::EncryptedRoot,
            &test_key,
            None,
        )
        .unwrap();

        let entries = tar_entries::<BackupCryptoTest>(&fs, &backup_file.path, &test_key);

        assert_eq!(
            entries.len(),
            3,
            "Expected 3 files in backup, got {}: {:?}",
            entries.len(),
            entries.iter().map(|e| &e.path).collect::<Vec<_>>()
        );

        let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
        assert!(paths.contains(&METADATA_FILE.to_string()));
        assert!(paths.contains(&app1_data_path.to_string()));
        assert!(paths.contains(&app2_config_path.to_string()));

        assert!(
            !paths.iter().any(|p| p.contains(DO_NOT_BACKUP_FOLDER)),
            "do_not_backup files should not be in backup, but found: {:?}",
            paths
        );
    }

    #[test]
    fn chunk_size_aligned_padding() {
        init();

        // Write exactly CHUNK_SIZE (64KB) bytes, ending with 0x10
        let mut data = vec![0xAA; CHUNK_SIZE];
        // Last byte looks like padding length
        data[CHUNK_SIZE - 1] = 0x10;

        let test_key = BackupKey::from_app_seed([0x42; 32]);
        let mut encrypted = Vec::new();
        let iv = [0x24u8; 12];
        {
            let crypto = BackupCryptoTest::new(&test_key, iv).unwrap();
            let mut writer = EncryptingWriter::new(&mut encrypted, crypto);
            writer.write_all(&data).unwrap();
            writer.flush().unwrap();
        }

        let mut decrypted = Vec::new();
        {
            let crypto = BackupCryptoTest::new(&test_key, iv).unwrap();
            let mut reader = DecryptingReader::new(&encrypted[..], crypto);
            reader.read_to_end(&mut decrypted).unwrap();
        }

        assert_eq!(
            decrypted.len(),
            data.len(),
            "Length mismatch: got {} bytes, expected {}",
            decrypted.len(),
            data.len()
        );
        assert_eq!(decrypted, data);
    }
}
