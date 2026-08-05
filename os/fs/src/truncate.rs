// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Seek;

use fs::messages::TruncateFile;

use crate::{Error, Server};

impl server::BlockingArchiveHandler<TruncateFile> for Server {
    fn handle(
        &mut self,
        msg: TruncateFile,
        sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <TruncateFile as server::BlockingArchive>::Response {
        let mount = self.mount_mut(msg.0.location()?).ok_or(Error::NoMedia)?;
        let open = mount.file_mut(sender, msg.0).ok_or(Error::FileNotOpen)?;
        if !open.flags.write {
            return Err(Error::InvalidOperation);
        }
        let new_len = open.file.stream_position()?;
        let current_size = open.file.entry().and_then(|e| e.size()).ok_or(Error::InvalidOperation)? as u64;
        let file_id = open.file_id();
        if new_len < current_size && mount.has_other_handle(file_id, sender, msg.0) {
            return Err(Error::FileInUse);
        }
        mount.file_mut(sender, msg.0).ok_or(Error::FileNotOpen)?.file.truncate()?;
        Ok(())
    }
}
