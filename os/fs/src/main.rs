// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{collections::HashMap, collections::HashSet};

use disk::DynamicDisk;
use fs::messages::*;
pub use fs::{DirHandle, Error, FileHandle, FileSystemEvent, FileSystemEventType, Location, OpenFlags};
use mount::Mount;
use server::{MessageId as _, ScalarEventSubscriber};

#[cfg(not(feature = "recovery-os"))]
use crate::airlock::AirlockState;

mod access;
#[cfg(not(feature = "recovery-os"))]
mod airlock;
mod close;
mod copy;
mod disk;
#[cfg(not(feature = "recovery-os"))]
mod disk_image;
#[cfg(not(feature = "recovery-os"))]
mod encrypted;
mod flush;
mod fs_event;
mod map;
mod metadata;
mod mount;
mod next;
mod open;
mod raw_access;
mod read;
mod remove;
mod rename;
mod seek;
mod set_len;
mod set_mtime;
mod truncate;
mod usb;
mod write;

#[cfg(keyos)]
emmc::use_api!();
#[cfg(keyos)]
mass_storage_server::use_api!();
#[cfg(all(keyos, not(feature = "recovery-os")))]
security::use_api!();

// 0: Boot Volume, 1: System volume, 2: Encrypted Volume
pub const DEFAULT_PARTITION_INDEX: u8 = 1;
pub const ENCRYPTED_PARTITION_INDEX: u8 = 2;

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::System4).unwrap();

    let fs_internal = Mount::new(system_disk()).unwrap();
    #[cfg(not(feature = "recovery-os"))]
    fixup_interrupted_update(fs_internal.fs());
    #[cfg(all(keyos, feature = "recovery-os"))]
    let fs_boot = Some(Mount::new(DynamicDisk::new(EmmcApi::default().into(), 0)).unwrap());
    #[cfg(all(not(keyos), feature = "recovery-os"))]
    let fs_boot: Option<Mount> = None;
    let server = Server {
        mapped_files: Default::default(),
        app_resources: Default::default(),
        #[cfg(feature = "recovery-os")]
        fs_boot,
        fs_internal,
        #[cfg(not(feature = "recovery-os"))]
        fs_user: None,
        fs_usb: None,
        #[cfg(not(feature = "recovery-os"))]
        airlock: AirlockState::Uninitialized,
        read_access: Default::default(),
        write_access: Default::default(),
        fs_event_subscribers: Default::default(),
        last_fs_event: Default::default(),
    };

    server::listen(server);
}

#[derive(server::Server)]
#[name = "os/fs"]
pub struct Server {
    mapped_files: HashMap<String, MappedFile>,
    app_resources: HashMap<xous::AppId, String>,
    #[cfg(feature = "recovery-os")]
    fs_boot: Option<Mount>,
    fs_internal: Mount,
    // airlock borrows from fs_user's leaked FS via DiskImage, so it must drop first.
    #[cfg(not(feature = "recovery-os"))]
    airlock: AirlockState,
    #[cfg(not(feature = "recovery-os"))]
    fs_user: Option<Mount>,
    fs_usb: Option<Mount>,
    read_access: HashSet<(xous::PID, Location)>,
    write_access: HashSet<(xous::PID, Location)>,
    fs_event_subscribers: HashMap<Location, Vec<ScalarEventSubscriber<FileSystemEvent>>>,
    last_fs_event: HashMap<Location, FileSystemEventType>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").finish()
    }
}

struct MappedFile {
    buffer: xous::MemoryRange,
    size: usize,
}

impl Server {
    /// Converts the path to a full path within the specified location. If resulting path points to
    /// root of the location, an empty string is returned.
    pub(crate) fn path_of(&self, location: Location, path: &str, pid: xous::PID) -> Result<String, Error> {
        let path = match location {
            Location::Boot
            | Location::System
            | Location::EncryptedRoot
            | Location::Usb
            | Location::Airlock => path.to_owned(),
            Location::SystemAppData => {
                let app_id = app_id_for_pid(pid)?;
                format!("{}/{app_id}/{path}", fs::SYSTEM_STATE_ROOT)
            }
            #[cfg(not(feature = "recovery-os"))]
            Location::CommonAssets => format!("keyos/common/{path}"),
            #[cfg(feature = "recovery-os")]
            Location::CommonAssets => format!("common/{path}"),
            Location::AppData => {
                let app_id = app_id_for_pid(pid)?;
                format!("appdata/{app_id}/{path}")
            }
            Location::AppResources => {
                let app_id = app_id_for_pid(pid)?;
                let root = app_resources_root(&self.app_resources, app_id)?;
                format!("{root}/{path}")
            }
            Location::User => format!("user/{path}"),
        };
        // TODO: we outright remove '..' here to prevent traversal, but it would be friendlier to resolve
        // ".."-s before prepending the base path
        Ok(path.split('/').filter(|s| !s.is_empty() && *s != "." && *s != "..").collect::<Vec<_>>().join("/"))
    }
}

fn app_id_for_pid(pid: xous::PID) -> Result<xous::AppId, Error> {
    xous::get_app_id(pid).map_err(Error::from)?.ok_or(Error::InternalError)
}

impl server::BlockingArchiveHandler<RegisterAppResources> for Server {
    fn handle(
        &mut self,
        msg: RegisterAppResources,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <RegisterAppResources as server::BlockingArchive>::Response {
        let app_id = msg.app_id;
        let Some(path) = app_resources_path(app_id, msg.root, &msg.app_dir) else {
            return Err(Error::InvalidPath);
        };

        self.app_resources.insert(app_id, path);
        Ok(())
    }
}

fn app_resources_root(
    app_resources: &HashMap<xous::AppId, String>,
    app_id: xous::AppId,
) -> Result<&str, Error> {
    app_resources.get(&app_id).map(String::as_str).ok_or(Error::FileNotFound)
}

fn app_resources_path(app_id: xous::AppId, root: AppResourcesRoot, app_dir: &str) -> Option<String> {
    // Built-in app bundles live under keyos/apps; sideloaded app bundles live
    // under keyos/sideloaded-apps and are named by their 32-character app ID.
    if app_dir.is_empty() || app_dir.contains('/') || matches!(app_dir, "." | "..") {
        return None;
    }

    let root = match root {
        AppResourcesRoot::BuiltIn => "keyos/apps",
        AppResourcesRoot::Sideloaded => {
            if app_dir != app_id.to_string() {
                return None;
            }
            "keyos/sideloaded-apps"
        }
    };

    Some(format!("{root}/{app_dir}/resources"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{app_resources_path, app_resources_root};
    use fs::Error;
    use fs::messages::AppResourcesRoot;
    use xous::AppId;

    const APP_ID: AppId = AppId([
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
    ]);

    #[test]
    fn app_resources_path_constructs_from_root_and_app_dir() {
        assert_eq!(
            app_resources_path(APP_ID, AppResourcesRoot::BuiltIn, "settings").as_deref(),
            Some("keyos/apps/settings/resources")
        );
        assert_eq!(
            app_resources_path(APP_ID, AppResourcesRoot::Sideloaded, "00112233445566778899aabbccddeeff")
                .as_deref(),
            Some("keyos/sideloaded-apps/00112233445566778899aabbccddeeff/resources")
        );

        assert!(app_resources_path(APP_ID, AppResourcesRoot::BuiltIn, "").is_none());
        assert!(app_resources_path(APP_ID, AppResourcesRoot::BuiltIn, ".").is_none());
        assert!(app_resources_path(APP_ID, AppResourcesRoot::BuiltIn, "../settings").is_none());
        assert!(app_resources_path(APP_ID, AppResourcesRoot::Sideloaded, "nested/example").is_none());
        assert!(app_resources_path(APP_ID, AppResourcesRoot::Sideloaded, "example").is_none());
    }

    #[test]
    fn app_resources_root_returns_file_not_found_when_unregistered() {
        assert!(matches!(
            app_resources_root(&HashMap::new(), APP_ID),
            Err(Error::FileNotFound)
        ));
    }

    #[test]
    fn app_resources_root_returns_registered_root() {
        let root = "keyos/apps/settings/resources".to_string();
        let app_resources = HashMap::from([(APP_ID, root.clone())]);

        assert_eq!(app_resources_root(&app_resources, APP_ID).unwrap(), root);
    }
}

#[cfg(not(feature = "recovery-os"))]
fn fixup_interrupted_update<T: fatfs::ReadWriteSeek>(fs: &fatfs::FileSystem<T>) {
    const KEYOS_DIR: &str = "keyos";
    const UPDATED_DIR: &str = "keyos.update";
    const OLD_DIR: &str = "keyos.old";

    let root = fs.root_dir();
    if root.open_dir(KEYOS_DIR).is_err() {
        log::warn!("{KEYOS_DIR:?} directory not found. Trying to recover from {UPDATED_DIR:?}");
        if let Err(e) = root.rename(UPDATED_DIR, &root, KEYOS_DIR) {
            panic!("{KEYOS_DIR:?} directory not found, and renaming from {UPDATED_DIR:?} also failed: {e:?}");
        }
        fs.flush_disk().ok();
    }
    if let Ok(old) = root.open_dir(OLD_DIR) {
        if let Err(e) = crate::remove::recursively_remove_contents(&old) {
            log::error!("Error removing contents {OLD_DIR:?}: {e:?}");
        } else {
            core::mem::drop(old);
            if let Err(e) = root.remove(OLD_DIR) {
                log::error!("Error removing {OLD_DIR:?}: {e:?}");
            }
            fs.flush_disk().ok();
        }
    }
}

impl server::Server for Server {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        #[cfg(keyos)]
        MassStorageApi::default().subscribe(context);
        #[cfg(all(keyos, not(feature = "recovery-os")))]
        Security::default().subscribe_disk_encryption_keys_ready(context);
        #[cfg(all(not(keyos), not(feature = "recovery-os")))]
        self.mount_encrypted_fs().ok();
        xous::register_system_event_handler(
            xous::SystemEvent::Disconnected,
            context.sid(),
            SubscriberDisconnected::ID,
        )
        .unwrap();
        xous::register_server_event_handler(
            xous::ServerEvent::Disconnected,
            context.sid(),
            ProcessDisconnected::ID,
        )
        .unwrap();
    }
}

impl Server {
    fn create_base_dir(&self, location: Location, pid: xous::PID) -> Result<(), Error> {
        match location {
            Location::SystemAppData => {
                let state_dir = self
                    .mount(Location::SystemAppData)
                    .ok_or(Error::NoMedia)?
                    .root_dir()
                    .create_dir(fs::SYSTEM_STATE_ROOT)?;
                let app_id = xous::get_app_id(pid)?.ok_or(Error::InternalError)?.to_string();
                state_dir.create_dir(&app_id)?;
            }
            Location::AppData => {
                let app_dir = self
                    .mount(Location::AppData)
                    .ok_or(Error::NoMedia)?
                    .root_dir()
                    .create_dir("appdata")?;
                let app_id = xous::get_app_id(pid)?.ok_or(Error::InternalError)?.to_string();
                app_dir.create_dir(&app_id)?;
            }
            #[cfg(not(feature = "recovery-os"))]
            Location::User => {
                self.mount(Location::User).ok_or(Error::NoMedia)?.root_dir().create_dir("user")?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn mount(&self, location: Location) -> Option<&Mount> {
        #[cfg(feature = "recovery-os")]
        match location {
            Location::Boot => self.fs_boot.as_ref(),
            Location::System | Location::SystemAppData | Location::AppResources => Some(&self.fs_internal),
            Location::AppData | Location::User | Location::EncryptedRoot | Location::Airlock => None,
            Location::CommonAssets => self.fs_boot.as_ref(),
            Location::Usb => self.fs_usb.as_ref(),
        }
        #[cfg(not(feature = "recovery-os"))]
        match location {
            Location::Boot => None,
            Location::System | Location::SystemAppData | Location::CommonAssets | Location::AppResources => {
                Some(&self.fs_internal)
            }
            Location::AppData | Location::User | Location::EncryptedRoot => self.fs_user.as_ref(),
            Location::Airlock => match &self.airlock {
                AirlockState::Mounted(m) => Some(m),
                _ => None,
            },
            Location::Usb => self.fs_usb.as_ref(),
        }
    }

    pub fn mount_mut(&mut self, location: Location) -> Option<&mut Mount> {
        #[cfg(feature = "recovery-os")]
        match location {
            Location::Boot => self.fs_boot.as_mut(),
            Location::System | Location::SystemAppData | Location::AppResources => {
                Some(&mut self.fs_internal)
            }
            Location::AppData | Location::User | Location::EncryptedRoot | Location::Airlock => None,
            Location::CommonAssets => self.fs_boot.as_mut(),
            Location::Usb => self.fs_usb.as_mut(),
        }
        #[cfg(not(feature = "recovery-os"))]
        match location {
            Location::Boot => None,
            Location::System | Location::SystemAppData | Location::CommonAssets | Location::AppResources => {
                Some(&mut self.fs_internal)
            }
            Location::AppData | Location::User | Location::EncryptedRoot => self.fs_user.as_mut(),
            Location::Airlock => match &mut self.airlock {
                AirlockState::Mounted(m) => Some(m),
                _ => None,
            },
            Location::Usb => self.fs_usb.as_mut(),
        }
    }
}

#[derive(Debug, server::Message)]
pub struct SubscriberDisconnected(pub xous::CID);

#[derive(Debug, server::Message)]
pub struct ProcessDisconnected;

impl server::ScalarHandler<ProcessDisconnected> for Server {
    fn handle(
        &mut self,
        _msg: ProcessDisconnected,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.fs_internal.clear_pid(sender);
        #[cfg(feature = "recovery-os")]
        if let Some(mount) = self.fs_boot.as_mut() {
            mount.clear_pid(sender);
        }
        #[cfg(not(feature = "recovery-os"))]
        if let Some(mount) = self.fs_user.as_mut() {
            mount.clear_pid(sender);
        }
        if let Some(mount) = self.fs_usb.as_mut() {
            mount.clear_pid(sender);
        }
        #[cfg(not(feature = "recovery-os"))]
        if let AirlockState::Mounted(mount) = &mut self.airlock {
            mount.clear_pid(sender);
        }
    }
}

fn system_disk() -> disk::DynamicDisk {
    #[cfg(not(keyos))]
    let result = disk::DynamicDisk::new(
        std::fs::OpenOptions::new().read(true).write(true).open("disk_system.dat").unwrap().into(),
        0,
    );
    #[cfg(keyos)]
    let result = disk::DynamicDisk::new(EmmcApi::default().into(), DEFAULT_PARTITION_INDEX);
    result
}

#[cfg(not(feature = "recovery-os"))]
fn format_fs(disk: &mut DynamicDisk, label: [u8; 11]) -> Result<(), Error> {
    use std::io::{Seek, SeekFrom, Write};

    disk.seek(SeekFrom::Start(0))?;

    let total_blocks = (disk.partition_info().len_bytes / 512) as u32;
    log::info!("Volume is not formatted, formatting {total_blocks} blocks");
    let format_options = fatfs::FormatVolumeOptions::new()
        .volume_label(label)
        .total_sectors(total_blocks)
        .bytes_per_cluster(64 * 512)
        .fat_type(fatfs::FatType::Fat32);
    let res = fatfs::format_volume(&mut *disk, format_options);
    if res.is_err() {
        log::error!("Failed to format volume: {:?}", res);
        return Err(Error::Io);
    }

    disk.flush()?;
    disk.seek(SeekFrom::Start(0))?;

    Ok(())
}

fn date_from_fatfs(d: fatfs::Date) -> fs::Date { fs::Date { year: d.year, month: d.month, day: d.day } }

fn datetime_from_fatfs(dt: fatfs::DateTime) -> fs::DateTime {
    fs::DateTime {
        date: fs::Date { year: dt.date.year, month: dt.date.month, day: dt.date.day },
        time: fs::Time { hour: dt.time.hour, min: dt.time.min, sec: dt.time.sec, millis: dt.time.millis },
    }
}

fn datetime_to_fatfs(dt: fs::DateTime) -> fatfs::DateTime {
    fatfs::DateTime {
        date: fatfs::Date { year: dt.date.year, month: dt.date.month, day: dt.date.day },
        time: fatfs::Time { hour: dt.time.hour, min: dt.time.min, sec: dt.time.sec, millis: dt.time.millis },
    }
}
