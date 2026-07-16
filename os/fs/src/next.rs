// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{Error, Server},
    fs::{messages::NextEntry, DirEntry},
    server::xous,
};

impl server::BlockingArchiveHandler<NextEntry> for Server {
    fn handle(
        &mut self,
        next: NextEntry,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<Option<DirEntry>, Error> {
        let dir = self
            .mount_mut(next.0.location()?)
            .ok_or(Error::NoMedia)?
            .dir_mut(sender, next.0)
            .ok_or(Error::FileNotOpen)?;
        match dir.iter.next() {
            Some(r) => {
                let r = r?;
                Ok(Some(DirEntry {
                    name: r.file_name(),
                    len: r.len(),
                    modified: crate::datetime_from_fatfs(r.modified()),
                    is_dir: r.is_dir(),
                    is_file: r.is_file(),
                }))
            }
            None => Ok(None),
        }
    }
}
