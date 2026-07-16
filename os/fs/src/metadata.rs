// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::{messages::GetMetadata, Metadata};

use crate::{date_from_fatfs, datetime_from_fatfs, Error, Server};

impl server::BlockingArchiveHandler<GetMetadata> for Server {
    fn handle(
        &mut self,
        metadata: GetMetadata,
        sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<Metadata, Error> {
        match metadata {
            GetMetadata::Path { path, location } => {
                self.check_read_access(sender, location)?;
                let path = crate::path_of(location, &path, sender);
                let (base, name) = path.rsplit_once('/').unwrap_or(("", &path));
                let root = self.mount(location).ok_or(Error::NoMedia)?.root_dir();
                let dir = if base.is_empty() { root } else { root.open_dir(base)? };
                for entry in dir.iter() {
                    let entry = entry?;
                    if entry.file_name() == name {
                        return Ok(Metadata {
                            created: datetime_from_fatfs(entry.created()),
                            accessed: date_from_fatfs(entry.accessed()),
                            modified: datetime_from_fatfs(entry.modified()),
                            size: entry.len(),
                            is_dir: entry.is_dir(),
                        });
                    }
                }
                Err(Error::FileNotFound)
            }
            GetMetadata::Handle { handle } => {
                let entry = self
                    .mount(handle.location()?)
                    .ok_or(Error::NoMedia)?
                    .file(sender, handle)
                    .ok_or(Error::FileNotOpen)?
                    .file
                    .entry()
                    .ok_or(Error::FileNotOpen)?;
                Ok(Metadata {
                    created: datetime_from_fatfs(entry.created()),
                    accessed: date_from_fatfs(entry.accessed()),
                    modified: datetime_from_fatfs(entry.modified()),
                    size: entry.size().unwrap_or(0) as u64,
                    is_dir: entry.is_dir(),
                })
            }
        }
    }
}
