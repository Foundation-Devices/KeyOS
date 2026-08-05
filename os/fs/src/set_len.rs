// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{Seek, Write};

use fs::messages::SetLen;

use crate::{Error, Server};

impl server::BlockingArchiveHandler<SetLen> for Server {
    fn handle(
        &mut self,
        msg: SetLen,
        sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetLen as server::BlockingArchive>::Response {
        let mount = self.mount_mut(msg.handle.location()?).ok_or(Error::NoMedia)?;
        let open = mount.file_mut(sender, msg.handle).ok_or(Error::FileNotOpen)?;
        if !open.flags.write {
            return Err(Error::InvalidOperation);
        }

        let current_size = open.file.entry().and_then(|e| e.size()).ok_or(Error::InvalidOperation)? as u64;
        let file_id = open.file_id();

        if msg.len < current_size && mount.has_other_handle(file_id, sender, msg.handle) {
            return Err(Error::FileInUse);
        }

        // has_other_handle reborrowed mount, ending `open`'s borrow; re-borrow.
        let file = &mut mount.file_mut(sender, msg.handle).ok_or(Error::FileNotOpen)?.file;
        let current_pos = file.stream_position()?;

        if msg.len > current_size {
            // extend the file
            file.seek(std::io::SeekFrom::End(0))?;
            let mut remaining = (msg.len - current_size) as usize;
            let zeros = [0u8; ZERO_BUF_SIZE];
            while remaining > 0 {
                let write_size = std::cmp::min(remaining, ZERO_BUF_SIZE);
                file.write_all(&zeros[..write_size])?;
                remaining -= write_size;
            }
        } else if msg.len < current_size {
            // truncate the file
            file.seek(std::io::SeekFrom::Start(msg.len))?;
            file.truncate()?;
        }

        // restore original position
        file.seek(std::io::SeekFrom::Start(current_pos))?;
        Ok(())
    }
}

const ZERO_BUF_SIZE: usize = 2048;
