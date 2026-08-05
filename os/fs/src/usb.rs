// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::messages::FormatUsb;
#[cfg(keyos)]
use mass_storage_server::MassStorageEvent;
use server::BlockingScalarHandler;
#[cfg(keyos)]
use server::ScalarEventHandler;

use crate::Server;
#[cfg(keyos)]
use crate::{disk::DynamicDisk, FileSystemEvent, FileSystemEventType, Location, MassStorageApi, Mount};

#[cfg(keyos)]
impl ScalarEventHandler<MassStorageEvent> for Server {
    fn handle(
        &mut self,
        msg: MassStorageEvent,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        match msg {
            // TODO: Block count and block size should probably
            //       be cross-checked with the partition info
            MassStorageEvent::Connect { .. } => {
                self.clean_fs_usb();
                let ms = MassStorageApi::default();
                match Mount::new(DynamicDisk::new(ms.into(), 0)) {
                    Ok(mount) => {
                        self.fs_usb = Some(mount);
                        self.send_filesystem_event(FileSystemEvent {
                            location: Location::Usb,
                            event_type: FileSystemEventType::Mounted,
                        });
                    }
                    Err(e) => {
                        log::warn!("Could not initialize FS: {e:?}");
                        self.send_filesystem_event(FileSystemEvent {
                            location: Location::Usb,
                            event_type: FileSystemEventType::Error,
                        });
                    }
                }
            }
            MassStorageEvent::Disconnect => {
                self.send_filesystem_event(FileSystemEvent {
                    location: Location::Usb,
                    event_type: FileSystemEventType::Unmounted,
                });
                self.clean_fs_usb()
            }
        }
    }
}

#[cfg(all(keyos, not(feature = "recovery-os")))]
impl BlockingScalarHandler<FormatUsb> for Server {
    fn handle(
        &mut self,
        _msg: FormatUsb,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <FormatUsb as server::BlockingScalar>::Response {
        if self.fs_usb.is_some() {
            log::info!("USB is already mounted");
            return Err(fs::Error::FileInUse);
        }
        let mut ms = MassStorageApi::default();

        let block_count = ms.block_count().map_err(|e| {
            log::error!("Error getting block count: {e:?}");
            fs::Error::Io
        })? as u32;

        let mut mbr = mbrs::Mbr::default();
        mbr.partition_table.entries[0] = Some(
            mbrs::PartInfo::try_from_lba(
                false,
                1,
                block_count - 1,
                mbrs::PartType::Fat32 { visible: true, scheme: mbrs::AddrScheme::Lba },
            )
            .map_err(|e| {
                log::error!("MBR PartInfo error: {e:?}");
                fs::Error::Io
            })?,
        );
        let mbr = <[u8; 512]>::try_from(&mbr).map_err(|e| {
            log::error!("MBR serialization error: {e:?}");
            fs::Error::Io
        })?;

        ms.write_blocks(0, &mbr).map_err(|e| {
            log::error!("Error writing MBR: {e:?}");
            fs::Error::Io
        })?;

        let mut disk = DynamicDisk::new(ms.into(), 0);
        crate::format_fs(&mut disk, *b"USB STORAGE")?;
        self.fs_usb = Some(Mount::new(disk)?);
        self.send_filesystem_event(FileSystemEvent {
            location: Location::Usb,
            event_type: FileSystemEventType::Mounted,
        });
        Ok(())
    }
}

#[cfg(any(not(keyos), feature = "recovery-os"))]
impl BlockingScalarHandler<FormatUsb> for Server {
    fn handle(
        &mut self,
        _msg: FormatUsb,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <FormatUsb as server::BlockingScalar>::Response {
        return Err(fs::Error::InvalidOperation);
    }
}

#[cfg(keyos)]
impl Server {
    fn clean_fs_usb(&mut self) {
        // Drop runs on the Mount, releasing handles and reclaiming the leaked FileSystem Box.
        self.fs_usb = None;
    }
}
