// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::messages::Rename;

use crate::{Error, Server};

impl server::BlockingArchiveHandler<Rename> for Server {
    fn handle(
        &mut self,
        msg: Rename,
        sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <Rename as server::BlockingArchive>::Response {
        self.check_write_access(sender, msg.location)?;
        let from = self.path_of(msg.location, &msg.from, sender)?;
        let to = self.path_of(msg.location, &msg.to, sender)?;
        let mount = self.mount(msg.location).ok_or(Error::NoMedia)?;
        if mount.path_in_use(&from)? || mount.path_in_use(&to)? {
            return Err(Error::FileInUse);
        }
        let root = mount.root_dir();
        root.rename(&from, &root, &to)?;
        self.flush_fs(msg.location)?;
        Ok(())
    }
}
