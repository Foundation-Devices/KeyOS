// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::{Duration, Instant};

use crate::sys::{EncryptedSettings, SystemSettings};

fs::use_api!();

#[derive(Debug)]
pub struct Store {
    pub fs: FileSystem,
    system: SettingFile<SystemSettings>,
    encrypted: Option<SettingFile<EncryptedSettings>>,
}

impl Default for Store {
    fn default() -> Self {
        let fs = FileSystem::default();
        let system = SettingFile::<SystemSettings>::new(
            &fs,
            "system_settings.json".to_string(),
            fs::Location::SystemAppData,
        )
        .expect("system settings load");
        Self { fs, system, encrypted: None }
    }
}

impl Store {
    pub fn get_system(&mut self) -> FileGuard<'_, SystemSettings> { self.system.guard() }

    pub fn get_encrypted(&mut self) -> Option<FileGuard<'_, EncryptedSettings>> {
        self.encrypted.as_mut().map(|f| f.guard())
    }

    pub fn try_mount_encrypted(&mut self) -> Result<(), fs::Error> {
        if self.encrypted.is_some() {
            return Ok(());
        }

        let settings = SettingFile::<EncryptedSettings>::new(
            &self.fs,
            "encrypted_settings.json".to_string(),
            fs::Location::AppData,
        )?;

        self.encrypted = Some(settings);

        Ok(())
    }

    // if `force` is true, flushes all dirty files regardless of age
    pub fn flush_dirty_files(&mut self, force: bool) {
        let now = Instant::now();

        if self.system.should_flush(now, force) {
            log::debug!("flushing dirty system settings file");
            if let Err(e) = self.system.flush_settings(&self.fs) {
                log::error!("Failed to flush stale system settings: {e:?}");
            }
        }

        if let Some(encrypted) = &mut self.encrypted {
            if encrypted.should_flush(now, force) {
                log::debug!("flushing dirty encrypted settings file");
                if let Err(e) = encrypted.flush_settings(&self.fs) {
                    log::error!("Failed to flush stale encrypted settings: {e:?}");
                }
            }
        }
    }
}

const DIRTY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub struct SettingFile<T: serde::Serialize> {
    pub settings: T,
    path: String,
    location: fs::Location,
    // the oldest time the file was written to
    dirty: Option<Instant>,
}

impl<T> SettingFile<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default,
{
    pub fn new(fs: &FileSystem, path: String, location: fs::Location) -> Result<Self, fs::Error> {
        let settings = match fs.durable_file_read(&path, location) {
            Ok(data) => serde_json::from_slice(&data).unwrap_or_default(),
            // Nothing has been saved yet. Any other error means the volume is not usable,
            // which is how `try_mount_encrypted` learns the encrypted one is still locked.
            Err(fs::Error::FileNotFound) => T::default(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, location, settings, dirty: None })
    }
}

impl<T: serde::Serialize> SettingFile<T> {
    pub fn flush_settings(&mut self, fs: &FileSystem) -> Result<(), fs::Error> {
        let data = serde_json::to_vec(&self.settings).map_err(|_| fs::Error::Io)?;
        fs.durable_file_write(&self.path, self.location, &data)?;
        self.dirty = None;
        Ok(())
    }

    pub fn mark_dirty(&mut self) {
        if self.dirty.is_none() {
            self.dirty = Some(Instant::now());
        }
    }

    pub fn guard(&mut self) -> FileGuard<'_, T> { FileGuard { file: self } }

    fn should_flush(&self, now: Instant, force: bool) -> bool {
        match self.dirty {
            Some(dirty) => force || now.duration_since(dirty) > DIRTY_TIMEOUT,
            None => false,
        }
    }
}

impl<T: serde::Serialize> Drop for SettingFile<T> {
    fn drop(&mut self) {
        if self.dirty.is_some() {
            let fs = FileSystem::default();
            if let Err(e) = self.flush_settings(&fs) {
                log::error!("Failed to flush stale settings on drop: {e:?}")
            }
        }
    }
}

pub struct FileGuard<'a, T: serde::Serialize> {
    file: &'a mut SettingFile<T>,
}

impl<T> std::ops::Deref for FileGuard<'_, T>
where
    T: serde::Serialize,
{
    type Target = T;

    fn deref(&self) -> &Self::Target { &self.file.settings }
}

// auto-marks the file as dirty when mutated
impl<T> std::ops::DerefMut for FileGuard<'_, T>
where
    T: serde::Serialize,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.file.mark_dirty();
        &mut self.file.settings
    }
}
