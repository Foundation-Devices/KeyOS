// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::messages::{CloseDir, CloseFile};
use server::xous;

use crate::{Error, Server};

impl server::BlockingScalarHandler<CloseFile> for Server {
    fn handle(
        &mut self,
        close: CloseFile,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), Error> {
        let mount = self.mount_mut(close.0.location()?).ok_or(Error::NoMedia)?;
        mount.close_file(sender, close.0)
    }
}

impl server::BlockingScalarHandler<CloseDir> for Server {
    fn handle(
        &mut self,
        close: CloseDir,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), Error> {
        let mount = self.mount_mut(close.0.location()?).ok_or(Error::NoMedia)?;
        mount.close_dir(sender, close.0)
    }
}
