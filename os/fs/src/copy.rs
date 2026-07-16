// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    io::{self, Read, Write},
    path::Path,
};

use fs::messages::{AsyncCopyBlock, AtomicCopy};
use server::{BlockingArchiveHandler, BlockingScalarHandler};
use xous::{DropDeallocate, MemoryFlags};
use {
    crate::{Error, Location, Server},
    server::xous,
};

use crate::disk::DynamicDisk;

impl Server {
    /// Copy a file or directory (recursively if it's a directory) to the destination directory,
    /// renaming it if needed.
    fn copy_to(
        &self,
        src: &str,
        // Empty if root.
        dest_dir: &str,
        rename: Option<String>,
        location: Location,
    ) -> Result<(), Error> {
        let src_name = Path::new(src).file_name().and_then(|n| n.to_str()).ok_or(Error::InvalidPath)?;
        let target_name = rename.as_deref().unwrap_or(src_name);

        log::debug!("Copying '{src}' to '{dest_dir}/{target_name}'");

        let root_dir = self.mount(location).ok_or(Error::NoMedia)?.root_dir();

        let dest_dir = if dest_dir.is_empty() {
            root_dir.clone()
        } else {
            root_dir.open_dir(dest_dir).map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => Error::FileNotFound,
                io::ErrorKind::NotADirectory => Error::NotADirectory,
                _ => Error::Io,
            })?
        };

        for entry in dest_dir.iter() {
            let entry = entry.map_err(|_| Error::Io)?;
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            return Err(Error::FileAlreadyExists);
        }

        match root_dir.open_dir(src) {
            Ok(src_dir) => {
                // It's directory, copy recursively.
                let target_dir = dest_dir.create_dir(target_name).map_err(|_| Error::Io)?;
                recursively_copy(&src_dir, &target_dir).map_err(|_| Error::Io)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotADirectory => {
                // It's a file, copy it directly.
                let mut src_file = root_dir.open_file(src).expect("file exists");
                let mut target_file = dest_dir.create_file(target_name).map_err(|_| Error::Io)?;
                io::copy(&mut src_file, &mut target_file).map_err(|_| Error::Io)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Error::FileNotFound),
            Err(_) => return Err(Error::Io),
        }

        Ok(())
    }
}

fn recursively_copy(src: &fatfs::Dir<'_, DynamicDisk>, dst: &fatfs::Dir<'_, DynamicDisk>) -> io::Result<()> {
    for entry in src.iter() {
        let entry = entry?;
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        if entry.is_dir() {
            let src_dir = src.open_dir(&name)?;
            let dst_dir = dst.create_dir(&name)?;
            recursively_copy(&src_dir, &dst_dir)?;
        } else {
            let mut src_file = src.open_file(&name)?;
            let mut dst_file = dst.create_file(&name)?;
            io::copy(&mut src_file, &mut dst_file)?;
        }
    }

    Ok(())
}

impl BlockingArchiveHandler<AtomicCopy> for Server {
    fn handle(
        &mut self,
        msg: AtomicCopy,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <AtomicCopy as server::BlockingArchive>::Response {
        self.check_read_access(sender, msg.location)?;
        self.check_write_access(sender, msg.location)?;
        let src = crate::path_of(msg.location, &msg.src, sender);
        let dest_dir = crate::path_of(msg.location, &msg.dest_dir, sender);

        if self.mount(msg.location).ok_or(Error::NoMedia)?.path_in_use(&src)? {
            return Err(Error::FileInUse);
        }

        self.copy_to(&src, &dest_dir, msg.rename, msg.location)?;
        self.flush_fs(msg.location)?;
        Ok(())
    }
}

impl BlockingScalarHandler<AsyncCopyBlock> for Server {
    fn handle(
        &mut self,
        msg: AsyncCopyBlock,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <AsyncCopyBlock as server::BlockingScalar>::Response {
        if msg.to == msg.from {
            return Err(Error::FileAlreadyExists);
        }
        let aligned_len = core::cmp::max(msg.len.next_multiple_of(0x1000), 0x1000);
        let mut buffer = DropDeallocate::new(xous::map_memory(None, None, aligned_len, MemoryFlags::W)?);

        let from_location = msg.from.location()?;
        let to_location = msg.to.location()?;

        // Read phase: borrow the from-mount only for the duration of reads.
        let offset = {
            let open = self
                .mount_mut(from_location)
                .ok_or(Error::NoMedia)?
                .file_mut(sender, msg.from)
                .ok_or(Error::FileNotOpen)?;
            if !open.flags.read {
                return Err(Error::InvalidOperation);
            }
            let mut offset = 0;
            loop {
                let read_size = open
                    .file
                    .read(&mut buffer.as_slice_mut()[offset..msg.len])
                    .map_err(|_| Error::Io)?;
                offset += read_size;
                if read_size == 0 || offset >= msg.len {
                    break;
                }
            }
            offset
        };

        // Write phase: borrow the to-mount separately. Works whether to-mount equals from-mount or not.
        {
            let open = self
                .mount_mut(to_location)
                .ok_or(Error::NoMedia)?
                .file_mut(sender, msg.to)
                .ok_or(Error::FileNotOpen)?;
            if !open.flags.write {
                return Err(Error::InvalidOperation);
            }
            open.file.write_all(&buffer.as_slice_mut()[..offset])?;
        }

        Ok(offset)
    }
}
