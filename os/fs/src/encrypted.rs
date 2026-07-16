// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::{messages::FormatEncryptedVolume, Error};
use security::messages::DiskEncryptionKeysReady;

use crate::{disk::DynamicDisk, format_fs, FileSystemEvent, FileSystemEventType, Location, Mount};

const USER_VOLUME_LABEL: [u8; 11] = *b"ENCRYPTED  ";

impl server::ScalarEventHandler<DiskEncryptionKeysReady> for crate::Server {
    fn handle(
        &mut self,
        _msg: DiskEncryptionKeysReady,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if self.fs_user.is_some() {
            log::error!("DiskEncryptionKeysReady received twice");
            return;
        }
        if self.mount_encrypted_fs().is_ok() {
            self.mount_airlock().ok();
        }
    }
}

impl server::BlockingScalarHandler<FormatEncryptedVolume> for crate::Server {
    fn handle(
        &mut self,
        _msg: FormatEncryptedVolume,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if self.fs_user.is_some() {
            log::error!("FormatEncryptedVolume called for a valid fs");
            return;
        }

        log::info!("Formatting encrypted volume");
        if format_fs(&mut new_encrypted_disk().expect("failed to find encrypted fs"), USER_VOLUME_LABEL)
            .is_ok()
        {
            if self.mount_encrypted_fs().is_ok() {
                match self.format_airlock() {
                    Ok(_) => {
                        log::info!("Formatting airlock successful");
                        if let Err(e) = self.mount_airlock() {
                            log::error!("Could not mount freshly formatted airlock: {e}");
                        }
                    }
                    Err(e) => {
                        log::info!("Could not format airlock: {e}");
                    }
                }
            }
        }
    }
}

impl crate::Server {
    pub fn mount_encrypted_fs(&mut self) -> Result<(), std::io::Error> {
        let disk = new_encrypted_disk().expect("failed to find encrypted fs");
        let mount = match Mount::new(disk) {
            Ok(mount) => mount,
            Err(e) => {
                let event =
                    FileSystemEvent { location: Location::User, event_type: FileSystemEventType::Error };

                // Return an error event if we couldn't mount the filesystem.
                // Most likely it needs to be formatted, which is done separately after giving the user a
                // notice.
                log::error!("Failed to create encrypted filesystem ({e:?}), sending event: {event:?}");
                for location in [Location::User, Location::AppData, Location::EncryptedRoot] {
                    self.send_filesystem_event(FileSystemEvent {
                        location,
                        event_type: FileSystemEventType::Error,
                    });
                }

                return Err(e);
            }
        };

        self.fs_user = Some(mount);

        for location in [Location::User, Location::AppData, Location::EncryptedRoot] {
            self.send_filesystem_event(FileSystemEvent {
                location,
                event_type: FileSystemEventType::Mounted,
            });
        }
        Ok(())
    }
}

pub fn new_encrypted_disk() -> Result<DynamicDisk, Error> {
    #[cfg(not(keyos))]
    {
        let bd = DynamicDisk::new(
            std::fs::OpenOptions::new().read(true).write(true).open("disk.dat").unwrap().into(),
            0,
        );
        Ok(bd)
    }

    #[cfg(keyos)]
    {
        // Read encrypted partition info from the MBR

        use crate::ENCRYPTED_PARTITION_INDEX;
        let emmc = crate::EmmcApi::default();
        let temp_disk = crate::disk::Disk::new(emmc, ENCRYPTED_PARTITION_INDEX);
        let encrypted_partition_info = temp_disk.partition_info();
        let start_block_idx = encrypted_partition_info.start;
        let len_blocks = (encrypted_partition_info.len_bytes / fs::BLOCK_SIZE as u64) as u32;
        log::info!(
            "Detected encrypted partition at LBA 0x{:08x}, length 0x{:08x}",
            start_block_idx,
            len_blocks
        );

        // Create a new disk with the encrypted partition
        let emmc = crate::EmmcApi::default();
        let emmc = crate::disk::PartiallyEncryptedEmmc::new(emmc, start_block_idx, len_blocks);
        let disk = DynamicDisk::new(emmc.into(), ENCRYPTED_PARTITION_INDEX);
        Ok(disk)
    }
}
