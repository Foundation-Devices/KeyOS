// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Seek;

use byteorder::{LittleEndian, ReadBytesExt};
use fs::{
    messages::{FormatAirlock, MountAirlock},
    FileSystemEventType,
};

use crate::disk::{DynamicDisk, DynamicDiskBlockDevice, PartitionInfo};
use crate::{disk_image::DiskImage, format_fs, Error, FileSystemEvent, Location, Mount, Server};

// --- Airlock ---
// A bit less than 32GB (potentially, on-demand allocated)

// The 0xa00000 is an adjustment (around 14MB), so that the FAT table size comes out to exactly 16374
// sectors, which (with the default 8 reserved sectors) puts the first data sector to 16384 == 0x4000,
// a nicely aligned first data sector.
// This improves the performance by around 30-40% compared to unaligned clusters, but it is also needed
// to ensure offset-less mapping between inner and outer clusters so we can trim them easily.

// WARNING: Changing this value is an incompatible change, as the image file structure depends on it!
//          Use a different filename for a different size.
const AIRLOCK_SIZE: u64 = 32 * 1024 * 1024 * 1024 - 0xa00000; // was e00000
const AIRLOCK_IMAGE_FILE: &str = "airlock.img";
const AIRLOCK_VOLUME_LABEL: [u8; 11] = *b"AIRLOCK    ";

#[derive(Default)]
pub enum AirlockState {
    #[default]
    Uninitialized,
    Unmounted(DynamicDisk),
    Mounted(Mount),
}

impl Server {
    pub fn format_airlock(&mut self) -> Result<(), Error> {
        if matches!(&self.airlock, AirlockState::Mounted(_)) {
            log::info!("Airlock is currently mounted");
            return Ok(());
        }
        let mut disk = self.new_airlock_disk()?;
        format_fs(&mut disk, AIRLOCK_VOLUME_LABEL)?;
        self.airlock = AirlockState::Unmounted(disk);
        Ok(())
    }

    pub fn mount_airlock(&mut self) -> Result<(), Error> {
        let disk = match core::mem::take(&mut self.airlock) {
            AirlockState::Uninitialized => {
                log::debug!("Mounting Airlock from Uninitialized state");
                self.new_airlock_disk()?
            }
            AirlockState::Unmounted(disk) => {
                log::debug!("Mounting Airlock from Unmounted state");
                disk
            }
            AirlockState::Mounted(mount) => {
                log::debug!("Mount airlock: already mounted");
                // Set the state back since we took it for the match above
                self.airlock = AirlockState::Mounted(mount);
                return Ok(());
            }
        };

        match self.mount_airlock_inner(disk) {
            Ok(mount) => {
                log::info!("Mounting Airlock successful");
                self.airlock = AirlockState::Mounted(mount);
                self.send_filesystem_event(FileSystemEvent {
                    location: Location::Airlock,
                    event_type: FileSystemEventType::Mounted,
                });
                match self.trim_airlock() {
                    Err(e) => {
                        log::warn!("Could not trim airlock: {e:?}");
                    }
                    Ok(trimmed) => log::info!("Trimmed {trimmed} blocks from Airlock"),
                };
                Ok(())
            }
            Err(e) => {
                log::error!("Mounting Airlock unsuccessful: {e:?}");
                self.airlock = AirlockState::Unmounted(self.new_airlock_disk()?);
                self.send_filesystem_event(FileSystemEvent {
                    location: Location::Airlock,
                    event_type: FileSystemEventType::Error,
                });
                Err(e.into())
            }
        }
    }

    fn mount_airlock_inner(&self, disk: DynamicDisk) -> std::io::Result<Mount> {
        let mount = Mount::new(disk)?;
        let user_cluster_size = self.fs_user_cluster_size().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "fs_user not mounted")
        })?;
        if user_cluster_size != mount.fs().cluster_size() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid cluster size in Airlock FATFS",
            ));
        }
        let cluster_alignment_check = mount.fs().offset_from_cluster(2);
        log::debug!("Alignment of first non-reserved cluster: 0x{cluster_alignment_check:x}");
        if (cluster_alignment_check % user_cluster_size as u64) != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Data clusters are misaligned in Airlock FATFS",
            ));
        }
        let total_clusters = mount.fs().total_clusters() as u64;
        let data_offset = mount.fs().offset_from_cluster(2);
        let required = total_clusters
            .checked_mul(user_cluster_size as u64)
            .and_then(|data_bytes| data_offset.checked_add(data_bytes))
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Airlock FATFS cluster geometry overflows")
            })?;
        if required > AIRLOCK_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Airlock FATFS claims more clusters than the image can hold",
            ));
        }
        Ok(mount)
    }

    pub fn unmount_airlock(&mut self) -> Result<(), Error> {
        let mount = match core::mem::take(&mut self.airlock) {
            AirlockState::Mounted(m) => m,
            other => {
                self.airlock = other;
                log::debug!("Unmount airlock: not mounted");
                self.send_filesystem_event(FileSystemEvent {
                    location: Location::Airlock,
                    event_type: FileSystemEventType::Unmounted,
                });
                return Ok(());
            }
        };
        log::info!("Unmounting Airlock");
        match mount.into_disk() {
            Ok(mut disk) => {
                disk.seek(std::io::SeekFrom::Start(0)).ok();
                self.airlock = AirlockState::Unmounted(disk);
                self.send_filesystem_event(FileSystemEvent {
                    location: Location::Airlock,
                    event_type: FileSystemEventType::Unmounted,
                });
                Ok(())
            }
            Err(e) => {
                log::error!("Unmounting Airlock unsuccessful: {e:?}");
                self.airlock = AirlockState::Uninitialized;
                self.send_filesystem_event(FileSystemEvent {
                    location: Location::Airlock,
                    event_type: FileSystemEventType::Error,
                });
                Err(e.into())
            }
        }
    }

    pub fn flush_airlock(&mut self) -> Result<(), Error> {
        match &mut self.airlock {
            AirlockState::Mounted(mount) => {
                mount.fs().flush_disk()?;
                Ok(())
            }
            AirlockState::Unmounted(disk) => {
                use std::io::Write;
                disk.flush()?;
                Ok(())
            }
            AirlockState::Uninitialized => Err(Error::NoMedia),
        }
    }

    pub fn trim_airlock(&mut self) -> Result<u32, Error> {
        let cluster_size = self.fs_user_cluster_size().ok_or(Error::NoMedia)?;
        let AirlockState::Mounted(mount) = &mut self.airlock else {
            log::debug!("Trim airlock: not mounted");
            return Ok(0);
        };
        let fs = mount.fs();
        let total_clusters = fs.total_clusters();
        let cluster_offset = (fs.offset_from_cluster(0) / cluster_size as u64) as u32;
        let mut trimmed = 0;
        for start_cluster in (0..total_clusters).step_by(0x2000) {
            let mut fat = fs.fat_slice();
            let mut fatdata = vec![0; 0x2000.min((total_clusters - start_cluster) as usize)];
            fat.seek(std::io::SeekFrom::Start(start_cluster as u64 * 4))?;
            fat.read_u32_into::<LittleEndian>(&mut fatdata)?;

            fs.with_disk(|d| {
                let DynamicDiskBlockDevice::DiskImage(d) = &mut d.block_device else {
                    log::error!("Invalid block device under Airlock");
                    return Err(Error::InternalError);
                };
                trimmed += d.trim_clusters(start_cluster + cluster_offset, &fatdata)?;
                Ok(())
            })?
        }
        if trimmed > 0 {
            fs.flush_disk()?;
        }
        Ok(trimmed)
    }

    fn fs_user_cluster_size(&self) -> Option<u32> {
        self.fs_user.as_ref().map(|m| m.fs().cluster_size())
    }

    fn new_airlock_disk(&self) -> Result<DynamicDisk, Error> {
        let fs_user = self.fs_user.as_ref().ok_or(Error::NoMedia)?.static_fs_unchecked();
        let disk_image = DiskImage::new(fs_user, AIRLOCK_IMAGE_FILE, AIRLOCK_SIZE)?;
        Ok(DynamicDisk::new_with_partition_info(
            disk_image.into(),
            PartitionInfo { start: 0, len_bytes: AIRLOCK_SIZE },
        ))
    }
}

impl server::BlockingScalarHandler<MountAirlock> for Server {
    fn handle(
        &mut self,
        msg: MountAirlock,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <MountAirlock as server::BlockingScalar>::Response {
        if msg.0 {
            self.mount_airlock()
        } else {
            self.unmount_airlock()
        }
    }
}

impl server::BlockingScalarHandler<FormatAirlock> for Server {
    fn handle(
        &mut self,
        _msg: FormatAirlock,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <FormatAirlock as server::BlockingScalar>::Response {
        self.format_airlock()
    }
}
