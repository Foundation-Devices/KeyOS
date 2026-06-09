// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{DirHandle, Error, FileHandle, Server},
    fs::messages::{CreateDirMessage, OpenDirMessage, OpenFileMessage},
    server::xous,
};

impl server::BlockingArchiveHandler<OpenFileMessage> for Server {
    fn handle(
        &mut self,
        msg: OpenFileMessage,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<FileHandle, Error> {
        if msg.flags.read {
            self.check_read_access(sender, msg.location)?
        }
        if msg.flags.write {
            self.check_write_access(sender, msg.location)?
        }
        if !msg.flags.read && !msg.flags.write {
            return Err(Error::InvalidOperation);
        }
        if msg.flags.create && !msg.flags.write {
            return Err(Error::InvalidOperation);
        }
        self.create_base_dir(msg.location, sender)?;

        let path = self.path_of(msg.location, &msg.path, sender)?;
        let mount = self.mount_mut(msg.location).ok_or(Error::NoMedia)?;
        if msg.flags.create {
            mount.create_file(sender, msg.location, path, msg.flags)
        } else {
            mount.open_file(sender, msg.location, path, msg.flags)
        }
    }
}

impl server::BlockingArchiveHandler<OpenDirMessage> for Server {
    fn handle(
        &mut self,
        msg: OpenDirMessage,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<DirHandle, Error> {
        self.check_read_access(sender, msg.location)?;
        self.create_base_dir(msg.location, sender)?;
        let path = self.path_of(msg.location, &msg.path, sender)?;
        let mount = self.mount_mut(msg.location).ok_or(Error::NoMedia)?;
        mount.open_dir(sender, msg.location, path)
    }
}

impl server::BlockingArchiveHandler<CreateDirMessage> for Server {
    fn handle(
        &mut self,
        msg: CreateDirMessage,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<DirHandle, Error> {
        self.check_write_access(sender, msg.location)?;
        self.create_base_dir(msg.location, sender)?;
        let path = self.path_of(msg.location, &msg.path, sender)?;
        let mount = self.mount_mut(msg.location).ok_or(Error::NoMedia)?;
        mount.create_dir(sender, msg.location, path)
    }
}
