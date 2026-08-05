// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{Error, FileHandle, Server},
    fs::messages::{AsyncRead, ReadFile},
    server::xous,
    std::io::Read,
};

impl server::LendMutHandler<ReadFile> for Server {
    fn handle(
        &mut self,
        read: ReadFile,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, Error> {
        self.read_file(
            read.handle,
            &mut read.buf.subrange(0, read.read_len).ok_or(Error::InvalidBufferLength)?.as_slice_mut(),
            sender,
        )
    }
}

impl server::BlockingArchiveHandler<AsyncRead> for Server {
    fn handle(
        &mut self,
        msg: AsyncRead,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<Vec<u8>, Error> {
        if msg.read_len > fs::MAX_ASYNC_LEN {
            return Err(Error::InvalidBufferLength);
        }
        let mut result = vec![0; msg.read_len];
        let len = self.read_file(msg.handle, &mut result, sender)?;
        result.truncate(len);
        Ok(result)
    }
}

impl Server {
    fn read_file(
        &mut self,
        handle: FileHandle,
        buffer: &mut [u8],
        sender: xous::PID,
    ) -> Result<usize, Error> {
        let open = self
            .mount_mut(handle.location()?)
            .ok_or(Error::NoMedia)?
            .file_mut(sender, handle)
            .ok_or(Error::FileNotOpen)?;
        if !open.flags.read {
            return Err(Error::InvalidOperation);
        }
        let mut offset = 0;
        loop {
            let read_len = open.file.read(&mut buffer[offset..]).map_err(|_| Error::Io)?;
            offset += read_len;
            if read_len == 0 || offset >= buffer.len() {
                break;
            }
        }
        Ok(offset)
    }
}
