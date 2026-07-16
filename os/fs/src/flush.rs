// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Write;

use fs::messages::{Flush, FlushFs};
use {
    crate::{Error, Location, Server},
    server::xous,
};

impl server::BlockingScalarHandler<Flush> for Server {
    fn handle(
        &mut self,
        flush: Flush,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), Error> {
        let mount = self.mount_mut(flush.0.location()?).ok_or(Error::NoMedia)?;
        mount.file_mut(sender, flush.0).ok_or(Error::FileNotOpen)?.file.flush()?;
        Ok(())
    }
}

impl server::BlockingScalarHandler<FlushFs> for Server {
    fn handle(
        &mut self,
        flush: FlushFs,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), Error> {
        match flush.0 {
            #[cfg(not(feature = "recovery-os"))]
            Location::Airlock => self.flush_airlock(),
            other => self.flush_fs(other),
        }
    }
}

impl Server {
    pub fn flush_fs(&self, location: Location) -> Result<(), Error> {
        let mount = self.mount(location).ok_or(Error::NoMedia)?;
        mount.fs().flush_disk()?;
        Ok(())
    }
}
