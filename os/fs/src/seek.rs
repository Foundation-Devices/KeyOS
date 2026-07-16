// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{Error, Server},
    fs::messages::SeekFile,
    server::xous,
    std::io::Seek,
};

impl server::BlockingArchiveHandler<SeekFile> for Server {
    fn handle(
        &mut self,
        seek: SeekFile,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<u64, Error> {
        self.mount_mut(seek.file.location()?)
            .ok_or(Error::NoMedia)?
            .file_mut(sender, seek.file)
            .ok_or(Error::FileNotOpen)?
            .file
            .seek(seek.pos.into())
            .map_err(Into::into)
    }
}
