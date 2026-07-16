// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::messages::SetMtime;

use crate::{Error, Server};

impl server::BlockingArchiveHandler<SetMtime> for Server {
    fn handle(
        &mut self,
        msg: SetMtime,
        sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetMtime as server::BlockingArchive>::Response {
        let open = self
            .mount_mut(msg.handle.location()?)
            .ok_or(Error::NoMedia)?
            .file_mut(sender, msg.handle)
            .ok_or(Error::FileNotOpen)?;
        if !open.flags.write {
            return Err(Error::InvalidOperation);
        }

        #[allow(deprecated)]
        open.file.set_modified(crate::datetime_to_fatfs(msg.datetime));
        Ok(())
    }
}
