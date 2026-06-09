// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{Read, Seek, SeekFrom};

use fs::{messages::MapFileMessage, MappedFileInTheirSpace};
use {
    crate::{Error, Location, Server},
    server::xous,
};

use crate::MappedFile;

impl server::BlockingArchiveHandler<MapFileMessage> for Server {
    fn handle(
        &mut self,
        msg: MapFileMessage,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<MappedFileInTheirSpace, Error> {
        // This is so verbose, so when we add a new Location, it has to be added here too.
        match msg.location {
            Location::Boot
            | Location::System
            | Location::SystemAppData
            | Location::EncryptedRoot
            | Location::AppData
            | Location::AppResources
            | Location::CommonAssets
            | Location::User
            | Location::Airlock => {}
            Location::Usb => return Err(Error::InvalidPath),
        }
        self.check_read_access(sender, msg.location)?;
        let path = self.path_of(msg.location, &msg.path, sender)?;
        if !self.mapped_files.contains_key(&path) {
            let mut file = self.mount(msg.location).ok_or(Error::NoMedia)?.root_dir().open_file(&path)?;
            let size = file.seek(SeekFrom::End(0))? as usize;
            if size == 0 {
                return Err(Error::FileNotFound);
            }
            log::debug!("Allocating buffer of size {size} for file \"{}\"", path);
            let mut buffer = xous::map_memory(
                None,
                None,
                size.next_multiple_of(0x1000),
                xous::MemoryFlags::W | xous::MemoryFlags::POPULATE,
            )
            .map_err(|_| Error::OutOfMemory)?;
            file.seek(SeekFrom::Start(0))?;
            file.read_exact(&mut buffer.as_slice_mut()[..size])?;
            drop(file);
            self.mapped_files.insert(path.clone(), MappedFile { buffer, size });
        }
        let mirrored = server::xous::mirror_memory_to_pid(self.mapped_files[&path].buffer, sender)
            .map_err(|_| Error::OutOfMemory)?;
        Ok(MappedFileInTheirSpace { addr: mirrored.as_ptr() as usize, size: self.mapped_files[&path].size })
    }
}
