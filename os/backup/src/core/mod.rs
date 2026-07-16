// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(test)]
mod test;
pub mod v1;
pub mod v2;

use std::io::{Read, Seek, SeekFrom, Write};
use std::time::SystemTime;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use defer::defer;
use fs::adapter::{BasicFsPermissions, FileAdapter, FsAdapter};
use fs::OpenFlags;
use server::xous::{self, DropDeallocate};
use server::MessageAllowed;
pub use v2::create_backup;
use whence::{self, WhenceExt};

use super::utils::{calculate_file_hash, hex};

const CHUNK_SIZE: usize = 64 * 1024;
const APPDATA_PATH: &str = "appdata";
const APPDATA_OLD_PATH: &str = "appdata-old";
const APPDATA_BACKUP_TEMP_PATH: &str = "appdata-backup-temp";
const APPDATA_RESTORE_TEMP_PATH: &str = "appdata-restore-temp";
const METADATA_FILE: &str = ".backup_metadata.json";

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
    #[serde(default)]
    pub device_name: Option<String>,
}

pub fn restore_backup<F, C1, C2>(
    fs: &F,
    backup_path: &str,
    backup_location: fs::Location,
    backup_key: &BackupKey,
) -> whence::Result<BackupMetadata, backup::Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions + MessageAllowed<fs::messages::SetMtime>,
    C1: v1::CryptoAdapter,
    C2: v2::CryptoAdapter,
{
    log::info!("restoring backup from {backup_path} at location {backup_location:?}");

    let mut backup_file = match fs.open_file(backup_path, backup_location, OpenFlags::READ_ONLY) {
        Ok(file) => file,
        Err(fs::Error::FileNotFound) => return Err(backup::Error::InvalidBackupFile).whence()?,
        Err(e) => return Err(e).whence(),
    };

    let hash = calculate_file_hash(&mut backup_file).whence()?;
    log::info!("restore file SHA256: {}", hex(&hash));

    let file_size = backup_file.metadata().whence()?.size as usize;
    let format = detect_backup_format(&mut backup_file).whence()?;

    fs.remove_if_exists(APPDATA_RESTORE_TEMP_PATH, fs::Location::EncryptedRoot).whence()?;
    fs.create_dir(APPDATA_RESTORE_TEMP_PATH, fs::Location::EncryptedRoot).whence()?;
    let _defer = defer(|| {
        fs.remove(APPDATA_RESTORE_TEMP_PATH, fs::Location::EncryptedRoot).ok();
    });

    let metadata = match format {
        BackupFormat::V1 => {
            log::info!("detected legacy backup format");
            v1::restore_backup::<_, _, C1>(fs, backup_file, backup_key, file_size)?
        }
        BackupFormat::V2 => {
            log::info!("detected v2 backup format");
            v2::restore_backup::<_, _, C2>(fs, backup_file, backup_key)?
        }
    };

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

fn map_chunk_buffer() -> Result<DropDeallocate, std::io::Error> {
    xous::map_memory(None, None, CHUNK_SIZE.next_multiple_of(0x1000), xous::MemoryFlags::W)
        .map(DropDeallocate::new)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "failed to map memory"))
}

fn append_metadata<W: Write>(
    tar: &mut tar::Builder<W>,
    metadata: &BackupMetadata,
) -> whence::Result<(), backup::Error> {
    let metadata_json = serde_json::to_vec(metadata)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        .whence()?;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(metadata_json.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    tar.append_data(&mut header, METADATA_FILE, metadata_json.as_slice()).whence()?;
    Ok(())
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

enum BackupFormat {
    V1,
    V2,
}

fn detect_backup_format<R: Read + Seek>(backup_file: &mut R) -> Result<BackupFormat, std::io::Error> {
    let mut prefix = [0; v2::PREFIX.len()];
    backup_file.read_exact(&mut prefix)?;
    let format = match &prefix {
        v2::PREFIX => BackupFormat::V2,
        _ => BackupFormat::V1,
    };
    backup_file.seek(SeekFrom::Start(0))?;
    Ok(format)
}

fn extract_backup<F, R>(fs: &F, reader: R) -> whence::Result<BackupMetadata, backup::Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions + MessageAllowed<fs::messages::SetMtime>,
    R: Read,
{
    let mut tar_archive = tar::Archive::new(reader);

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

    metadata.ok_or_else(|| backup::Error::InvalidBackupFile).whence()
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
