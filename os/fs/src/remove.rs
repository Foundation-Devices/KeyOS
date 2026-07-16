// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::messages::Remove;
use server::BlockingArchiveHandler;
use {
    crate::{Error, Location, Server},
    server::xous,
};

impl Server {
    /// Removes a file or directory (recursively if it's a directory)
    fn remove_recursive(&self, path: &str, location: Location) -> Result<(), Error> {
        let root_dir = self.mount(location).ok_or(Error::NoMedia)?.root_dir();

        // Try to open as directory first
        match root_dir.open_dir(path) {
            Ok(dir) => {
                // It's a directory, remove its contents recursively
                recursively_remove_contents(&dir)?;
                // Then remove the directory itself
                root_dir.remove(path)?;
            }
            Err(_) => {
                // Not a directory or doesn't exist, try to remove as file
                root_dir.remove(path)?;
            }
        }

        Ok(())
    }
}

pub fn recursively_remove_contents<D: fatfs::ReadWriteSeek>(
    dir: &fatfs::Dir<'_, D>,
) -> std::io::Result<()> {
    for entry in dir.iter() {
        let entry = entry?;
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        if entry.is_dir() {
            let subdir = dir.open_dir(&name)?;
            recursively_remove_contents(&subdir)?;
        }
        dir.remove(&name)?;
    }
    Ok(())
}

impl BlockingArchiveHandler<Remove> for Server {
    fn handle(
        &mut self,
        msg: Remove,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <Remove as server::BlockingArchive>::Response {
        self.check_write_access(sender, msg.location)?;
        let path = crate::path_of(msg.location, &msg.path, sender);
        if self.mount(msg.location).ok_or(Error::NoMedia)?.path_in_use(&path)? {
            return Err(Error::FileInUse);
        }

        // Use recursive removal for both files and directories
        self.remove_recursive(&path, msg.location)?;
        self.flush_fs(msg.location).inspect_err(|e| log::error!("Failed to flush fs: {:?}", e))?;
        Ok(())
    }
}
