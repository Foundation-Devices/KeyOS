// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{Error, FileHandle, Server},
    fs::messages::{AsyncWrite, WriteFile},
    server::xous,
    std::io::Write,
};

impl server::LendMutHandler<WriteFile> for Server {
    fn handle(
        &mut self,
        write: WriteFile,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, Error> {
        self.write_file(
            write.handle,
            write.buf.subrange(0, write.write_len).ok_or(Error::InvalidBufferLength)?.as_slice(),
            sender,
        )
    }
}

impl server::BlockingArchiveHandler<AsyncWrite> for Server {
    fn handle(
        &mut self,
        msg: AsyncWrite,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, Error> {
        self.write_file(msg.handle, &msg.buffer, sender)
    }
}

impl Server {
    fn write_file(&mut self, handle: FileHandle, buffer: &[u8], sender: xous::PID) -> Result<usize, Error> {
        let open = self
            .mount_mut(handle.location()?)
            .ok_or(Error::NoMedia)?
            .file_mut(sender, handle)
            .ok_or(Error::FileNotOpen)?;
        if !open.flags.write {
            return Err(Error::InvalidOperation);
        }
        open.file.write_all(buffer).map_err(|_| Error::Io)?;
        Ok(buffer.len())
    }
}
