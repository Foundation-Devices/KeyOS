// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Filesystem server API.
//!
//! The primary client handle is [`FileSystem`]. Use [`use_api!`] in app code to
//! define local aliases for [`FileSystem`], [`File`], and [`Dir`] with the
//! app's generated permissions type:
//!
//! ```rust,ignore
//! fs::use_api!();
//!
//! let fs = FileSystem::default();
//! let file = fs.open_file("state.json", fs::Location::AppData, fs::OpenFlags::read_only())?;
//! ```
//!
//! The types in [`messages`] describe the wire protocol between the API handle
//! and the filesystem server. Most app code should use [`FileSystem`] and the
//! returned [`File`] / [`Dir`] handles directly.

use std::io::{Read, Seek, Write};

use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use server::{wrapped_scalar, CheckedConn, CheckedPermissions, MessageAllowed};
use xous::{DropDeallocate, MemoryRange};

pub mod adapter;
pub mod error;
mod flags;
pub mod messages;

pub use error::Error;
use messages::*;

// Enough space for the typical FAT32 cluster read (64 sectors of 512)
pub const FILE_BUFFER_SIZE: usize = 64 * 512;
pub const BLOCK_SIZE: u64 = 512;
pub const SYSTEM_STATE_ROOT: &str = "state";

/// Defines local filesystem API aliases with generated permissions.
///
/// The zero-argument form expands to:
///
/// - `FileSystem = fs::FileSystem<FileSystemPermissions>`
/// - `File = fs::File<FileSystemPermissions>`
/// - `Dir = fs::Dir<FileSystemPermissions>`
///
/// Use this from an app crate before constructing a [`FileSystem`] handle.
#[macro_export]
macro_rules! use_api {
    ($fs:path, $server:path) => {
        mod fs_permissions {
            use fs::messages::*;
            pub use $fs as fs;
            use $server as server;
            #[derive(Clone, Default, Debug, server::Permissions)]
            #[server_name = "os/fs"]
            pub struct FileSystemPermissions;
        }
        type FileSystem = fs_permissions::fs::FileSystem<fs_permissions::FileSystemPermissions>;
        type File = fs_permissions::fs::File<fs_permissions::FileSystemPermissions>;
        type Dir = fs_permissions::fs::Dir<fs_permissions::FileSystemPermissions>;
    };
    () => {
        fs::use_api!(fs, server);
    };
}

/// Client handle for the filesystem server.
///
/// This is the filesystem crate's high-level API surface. It owns a checked
/// connection to `os/fs`, enforces location access checks, and creates [`File`]
/// and [`Dir`] handles for file and directory operations.
#[derive(Debug, Default, Clone)]
pub struct FileSystem<P: CheckedPermissions> {
    conn: CheckedConn<P>,
    read_access_granted: flags::AccessFlags,
    write_access_granted: flags::AccessFlags,
}

impl<P: CheckedPermissions> FileSystem<P> {
    pub fn open_file(
        &self,
        path: impl Into<String>,
        location: Location,
        flags: OpenFlags,
    ) -> Result<File<P>, Error>
    where
        P: MessageAllowed<OpenFileMessage>,
        P: MessageAllowed<CloseFile>,
    {
        if flags.read {
            self.ensure_read_access(location)?;
        }
        if flags.write {
            self.ensure_write_access(location)?;
        }
        Ok(File {
            handle: self.conn.send_blocking_archive(OpenFileMessage {
                path: path.into(),
                location,
                flags,
            })?,
            work_buf: DropDeallocate::new(
                xous::map_memory(None, None, FILE_BUFFER_SIZE, xous::MemoryFlags::W)
                    .map_err(|_| Error::FileNotOpen)?,
            ),
            conn: self.conn.clone(),
        })
    }

    pub fn open_dir(&self, path: impl Into<String>, location: Location) -> Result<Dir<P>, Error>
    where
        P: MessageAllowed<OpenDirMessage>,
        P: MessageAllowed<CloseDir>,
    {
        self.ensure_read_access(location)?;
        Ok(Dir {
            handle: self.conn.send_blocking_archive(OpenDirMessage { path: path.into(), location })?,
            conn: self.conn.clone(),
        })
    }

    pub fn create_dir(&self, path: impl Into<String>, location: Location) -> Result<Dir<P>, Error>
    where
        P: MessageAllowed<CreateDirMessage>,
        P: MessageAllowed<CloseDir>,
    {
        self.ensure_write_access(location)?;
        Ok(Dir {
            handle: self.conn.send_blocking_archive(CreateDirMessage { path: path.into(), location })?,
            conn: self.conn.clone(),
        })
    }

    pub fn create_dir_async(
        &self,
        path: impl Into<String>,
        location: Location,
    ) -> Result<CreateDirMessage, Error>
    where
        P: MessageAllowed<CreateDirMessage>,
        P: MessageAllowed<CloseDir>,
    {
        self.ensure_write_access(location)?;
        Ok(CreateDirMessage { path: path.into(), location })
    }

    pub fn ensure_parent_dir_exists(&self, path: &str, location: Location) -> Result<(), Error>
    where
        P: MessageAllowed<CreateDirMessage>,
        P: MessageAllowed<CloseDir>,
    {
        ensure_parent_dir_exists_impl(|dir| self.create_dir(dir, location).map(|_| ()), path)
    }

    pub fn remove(&self, path: impl Into<String>, location: Location) -> Result<(), Error>
    where
        P: MessageAllowed<Remove>,
    {
        let path = path.into();
        self.ensure_write_access(location)?;
        self.conn.send_blocking_archive(Remove { path, location })
    }

    pub fn remove_async(&self, path: impl Into<String>, location: Location) -> Result<Remove, Error>
    where
        P: MessageAllowed<Remove>,
    {
        let path = path.into();
        self.ensure_write_access(location)?;
        Ok(Remove { path, location })
    }

    /// Copy a source file/directory to a destination directory. If source is a directory, the copy is
    /// recursive.
    ///
    /// Optionally, the copied file/directory can be renamed by providing the `rename` argument.
    ///
    /// The destination directory must be empty; copying into a non-empty directory returns
    /// [`Error::FileAlreadyExists`].
    pub fn atomic_copy(
        &self,
        src: impl Into<String>,
        dest_dir: impl Into<String>,
        rename: Option<impl Into<String>>,
        location: Location,
    ) -> Result<(), Error>
    where
        P: MessageAllowed<AtomicCopy>,
    {
        self.ensure_read_access(location)?;
        self.ensure_write_access(location)?;

        let src = src.into();
        let dest_dir = dest_dir.into();
        let rename = rename.map(|s| s.into());
        self.conn.send_blocking_archive(AtomicCopy { src, dest_dir, rename, location })
    }

    pub fn metadata(&self, path: impl Into<String>, location: Location) -> Result<Metadata, Error>
    where
        P: MessageAllowed<GetMetadata>,
    {
        self.ensure_read_access(location)?;
        self.conn.send_blocking_archive(GetMetadata::Path { path: path.into(), location })
    }

    pub fn rename(
        &self,
        from: impl Into<String>,
        to: impl Into<String>,
        location: Location,
    ) -> Result<(), Error>
    where
        P: MessageAllowed<Rename>,
    {
        self.ensure_write_access(location)?;
        self.conn.send_blocking_archive(Rename { from: from.into(), to: to.into(), location })
    }

    pub fn rename_async(
        &self,
        from: impl Into<String>,
        to: impl Into<String>,
        location: Location,
    ) -> Result<Rename, Error>
    where
        P: MessageAllowed<Rename>,
    {
        self.ensure_write_access(location)?;
        Ok(Rename { from: from.into(), to: to.into(), location })
    }

    pub fn map_file(&self, location: Location, path: impl Into<String>) -> Result<xous::MemoryRange, Error>
    where
        P: MessageAllowed<MapFileMessage>,
    {
        self.ensure_read_access(location)?;
        let result = self.conn.send_blocking_archive(MapFileMessage { path: path.into(), location })?;
        Ok(unsafe { xous::MemoryRange::new(result.addr, result.size).unwrap() })
    }

    pub fn register_app_resources(
        &self,
        app_id: xous::AppId,
        root: AppResourcesRoot,
        app_dir: impl Into<String>,
    ) -> Result<(), Error>
    where
        P: MessageAllowed<RegisterAppResources>,
    {
        self.conn.send_blocking_archive(RegisterAppResources {
            app_id: app_id.0,
            root,
            app_dir: app_dir.into(),
        })
    }

    fn ensure_read_access(&self, location: Location) -> Result<(), Error> {
        if self.read_access_granted.contains(location) {
            return Ok(());
        }
        match location {
            Location::CommonAssets | Location::AppData | Location::AppResources => return Ok(()),
            Location::System => self.conn.unchecked().try_send_blocking_scalar(GetSystemReadAccess)?,
            Location::SystemAppData => {
                self.conn.unchecked().try_send_blocking_scalar(GetSystemAppDataReadAccess)?
            }
            Location::EncryptedRoot => {
                self.conn.unchecked().try_send_blocking_scalar(GetEncryptedRootReadAccess)?
            }
            Location::Usb => self.conn.unchecked().try_send_blocking_scalar(GetUsbReadAccess)?,
            Location::User => self.conn.unchecked().try_send_blocking_scalar(GetUserReadAccess)?,
            Location::Airlock => self.conn.unchecked().try_send_blocking_scalar(GetAirlockReadAccess)?,
            Location::Boot => self.conn.unchecked().try_send_blocking_scalar(GetBootReadAccess)?,
        };
        self.read_access_granted.insert(location);
        Ok(())
    }

    fn ensure_write_access(&self, location: Location) -> Result<(), Error> {
        if self.write_access_granted.contains(location) {
            return Ok(());
        }
        match location {
            Location::CommonAssets | Location::AppResources => return Err(Error::AccessDenied)?,
            Location::AppData => return Ok(()),
            Location::System => self.conn.unchecked().try_send_blocking_scalar(GetSystemWriteAccess)?,
            Location::SystemAppData => {
                self.conn.unchecked().try_send_blocking_scalar(GetSystemAppDataWriteAccess)?
            }
            Location::EncryptedRoot => {
                self.conn.unchecked().try_send_blocking_scalar(GetEncryptedRootWriteAccess)?
            }
            Location::Usb => self.conn.unchecked().try_send_blocking_scalar(GetUsbWriteAccess)?,
            Location::User => self.conn.unchecked().try_send_blocking_scalar(GetUserWriteAccess)?,
            Location::Airlock => self.conn.unchecked().try_send_blocking_scalar(GetAirlockWriteAccess)?,
            Location::Boot => self.conn.unchecked().try_send_blocking_scalar(GetBootWriteAccess)?,
        };
        self.write_access_granted.insert(location);
        Ok(())
    }

    pub fn read_blocks(
        &mut self,
        location: Location,
        block_index: u32,
        block_count: usize,
        buf: MemoryRange,
    ) -> Result<usize, Error>
    where
        P: MessageAllowed<ReadBlocks>,
    {
        self.conn.lend_mut(ReadBlocks { buf, block_index, block_count, location })
    }

    pub fn write_blocks(
        &mut self,
        location: Location,
        block_index: u32,
        block_count: usize,
        buf: MemoryRange,
    ) -> Result<usize, Error>
    where
        P: MessageAllowed<WriteBlocks>,
    {
        self.conn.lend_mut(WriteBlocks { buf, block_index, block_count, location })
    }

    pub fn flush(&mut self, location: Location) -> Result<(), Error>
    where
        P: MessageAllowed<FlushFs>,
    {
        self.conn.try_send_blocking_scalar(FlushFs(location))?
    }

    pub fn block_count(&self, location: Location) -> Result<usize, Error>
    where
        P: MessageAllowed<BlockCount>,
    {
        self.conn.try_send_blocking_scalar(BlockCount(location))?
    }

    pub fn format_encrypted_volume(&self)
    where
        P: MessageAllowed<FormatEncryptedVolume>,
    {
        self.conn.send_blocking_scalar(FormatEncryptedVolume);
    }

    pub fn subscribe_filesystem_events<S>(&self, listener: &mut server::ServerContext<S>, location: Location)
    where
        S: server::Server + server::ScalarEventHandler<FileSystemEvent>,
        P: MessageAllowed<SubscribeFilesystemEvent>,
    {
        self.conn.subscribe_scalar_infallible(SubscribeFilesystemEvent(location), listener)
    }

    pub fn wait_for_filesystem(&self, location: Location)
    where
        P: 'static,
        P: MessageAllowed<SubscribeFilesystemEvent>,
    {
        server::listen(WaitForFs(self.clone(), location));
    }

    pub fn mount_airlock(&mut self) -> Result<(), Error>
    where
        P: MessageAllowed<MountAirlock>,
    {
        self.conn.send_blocking_scalar(MountAirlock(true))
    }

    pub fn unmount_airlock(&mut self) -> Result<(), Error>
    where
        P: MessageAllowed<MountAirlock>,
    {
        self.conn.send_blocking_scalar(MountAirlock(false))
    }

    pub fn format_airlock(&mut self) -> Result<(), Error>
    where
        P: MessageAllowed<FormatAirlock>,
    {
        self.conn.send_blocking_scalar(FormatAirlock)
    }
}

#[derive(Debug)]
pub struct File<P: CheckedPermissions + MessageAllowed<CloseFile>> {
    handle: FileHandle,
    conn: CheckedConn<P>,
    work_buf: DropDeallocate,
}

impl<P: CheckedPermissions + MessageAllowed<CloseFile>> File<P> {
    pub fn truncate(&mut self) -> Result<(), Error>
    where
        P: MessageAllowed<TruncateFile>,
    {
        self.conn.send_blocking_archive(TruncateFile(self.handle))
    }

    // if len is less than the current file size, the file is truncated
    // if len is greater than the current file size, the file is extended with empty bytes
    // the file's cursor does not move
    pub fn set_len(&mut self, len: u64) -> Result<(), Error>
    where
        P: MessageAllowed<SetLen>,
    {
        self.conn.send_blocking_archive(SetLen { handle: self.handle, len })
    }

    pub fn metadata(&self) -> Result<Metadata, Error>
    where
        P: MessageAllowed<GetMetadata>,
    {
        self.conn.send_blocking_archive(GetMetadata::Handle { handle: self.handle })
    }

    pub fn set_mtime(&mut self, datetime: DateTime) -> Result<(), Error>
    where
        P: MessageAllowed<SetMtime>,
    {
        self.conn.send_blocking_archive(SetMtime { handle: self.handle, datetime })
    }

    /// Prepare a message that can be sent with slint_keyos_platform::async_archive
    /// Less efficient than a regular read()
    /// May return less bytes than requested.
    /// Returns an empty buffer on EOF
    pub fn async_read(&mut self, read_len: usize) -> AsyncRead { AsyncRead { handle: self.handle, read_len } }

    /// Prepare a message that can be sent with slint_keyos_platform::async_archive
    /// Less efficient than a regular write()
    /// The actual bytes written is returned, may be less than the buffer size
    pub fn async_write(&mut self, buffer: Vec<u8>) -> AsyncWrite {
        AsyncWrite { handle: self.handle, buffer }
    }

    /// Prepare a message that can be sent with slint_keyos_platform::async_scalar
    /// Efficiently copies between two open files.
    /// Returns the numebr of actually copied bytes.
    /// Returns Ok(0) on EOF
    pub fn async_copy_block_to(&mut self, to: &mut Self, len: usize) -> AsyncCopyBlock {
        AsyncCopyBlock { from: self.handle, to: to.handle, len }
    }

    pub fn copy_block_to(&mut self, to: &mut Self, len: usize) -> Result<usize, Error>
    where
        P: MessageAllowed<AsyncCopyBlock>,
    {
        self.conn.send_blocking_scalar(AsyncCopyBlock { from: self.handle, to: to.handle, len })
    }

    pub fn overwrite(&mut self, buf: &[u8]) -> Result<(), Error>
    where
        P: MessageAllowed<SeekFile>,
        P: MessageAllowed<WriteFile>,
        P: MessageAllowed<TruncateFile>,
        P: MessageAllowed<Flush>,
    {
        self.seek(std::io::SeekFrom::Start(0))?;
        self.write_all(buf)?;
        self.truncate()?;
        Ok(())
    }

    pub fn copy_to(&mut self, to: &mut Self) -> Result<(), Error>
    where
        P: MessageAllowed<SeekFile>,
        P: MessageAllowed<WriteFile>,
        P: MessageAllowed<TruncateFile>,
        P: MessageAllowed<AsyncCopyBlock>,
    {
        to.seek(std::io::SeekFrom::Start(0))?;
        while self.copy_block_to(to, 0x10000)? > 0 {}
        to.truncate()?;
        Ok(())
    }
}

impl<P: CheckedPermissions + MessageAllowed<CloseFile>> Read for File<P>
where
    P: MessageAllowed<ReadFile>,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        #[cfg(keyos)]
        if (buf.as_ptr() as usize) & (xous::keyos::PAGE_SIZE - 1) == 0 && buf.len() >= xous::keyos::PAGE_SIZE
        {
            let read_len = (buf.len() & !(xous::keyos::PAGE_SIZE - 1)).min(FILE_BUFFER_SIZE);
            return Ok(self.conn.lend_mut(ReadFile {
                buf: unsafe { xous::MemoryRange::new(buf.as_ptr() as usize, read_len).unwrap() },
                handle: self.handle,
                read_len,
            })?);
        }

        let read_len = buf.len().min(FILE_BUFFER_SIZE);

        let result = self.conn.lend_mut(ReadFile { buf: *self.work_buf, handle: self.handle, read_len })?;
        buf[..result].copy_from_slice(&self.work_buf.as_slice()[..result]);
        Ok(result)
    }
}

impl<P: CheckedPermissions + MessageAllowed<CloseFile>> Seek for File<P>
where
    P: MessageAllowed<SeekFile>,
{
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.conn.send_blocking_archive(SeekFile { file: self.handle, pos: pos.into() }).map_err(Into::into)
    }
}

impl<P: CheckedPermissions + MessageAllowed<CloseFile>> Write for File<P>
where
    P: MessageAllowed<WriteFile>,
    P: MessageAllowed<Flush>,
{
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        #[cfg(keyos)]
        if (buf.as_ptr() as usize) & (xous::keyos::PAGE_SIZE - 1) == 0 && buf.len() >= xous::keyos::PAGE_SIZE
        {
            let write_len = (buf.len() & !(xous::keyos::PAGE_SIZE - 1)).min(FILE_BUFFER_SIZE);
            return Ok(self.conn.lend_mut(WriteFile {
                buf: unsafe { xous::MemoryRange::new(buf.as_ptr() as usize, write_len).unwrap() },
                handle: self.handle,
                write_len,
            })?);
        }

        let buf_len = buf.len().min(FILE_BUFFER_SIZE);

        self.work_buf.as_slice_mut()[..buf_len].copy_from_slice(&buf[..buf_len]);
        let result =
            self.conn.lend_mut(WriteFile { buf: *self.work_buf, handle: self.handle, write_len: buf_len })?;
        Ok(result)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.conn.try_send_blocking_scalar(Flush(self.handle)).map_err(|_| std::io::ErrorKind::Other)??;
        Ok(())
    }
}

impl<P: CheckedPermissions + MessageAllowed<CloseFile>> Drop for File<P> {
    fn drop(&mut self) {
        if let Err(e) = self.conn.try_send_blocking_scalar(CloseFile(self.handle)) {
            log::error!("Failed to close file: {:?}", e);
        }
    }
}

#[derive(Debug)]
pub struct Dir<P: CheckedPermissions + MessageAllowed<CloseDir>> {
    handle: DirHandle,
    conn: CheckedConn<P>,
}

impl<P: CheckedPermissions + MessageAllowed<CloseDir>> Dir<P> {
    pub fn next_entry(&self) -> Result<Option<DirEntry>, Error>
    where
        P: MessageAllowed<NextEntry>,
    {
        self.conn.send_blocking_archive(NextEntry(self.handle))
    }

    pub fn next_entry_async(&self) -> NextEntry { NextEntry(self.handle) }

    pub fn pick_next_filename(&self, filename: impl Into<String>, pad: Option<usize>) -> Result<String, Error>
    where
        P: MessageAllowed<NextEntry>,
    {
        let filename: String = filename.into();

        // Name can't include subdirectories
        if filename.contains('/') {
            return Err(Error::InvalidPath);
        }

        let pad = pad.unwrap_or(3);

        // Allow getting the next directory name
        let (basename, ext) = match filename.rsplit_once('.') {
            Some((base, ext)) => (base, Some(format!(".{}", ext))),
            None => (filename.as_str(), None),
        };

        let mut highest = 0u32;
        let prefix = format!("{}-", basename);

        while let Some(entry) = self.next_entry().ok().flatten() {
            let name = entry.name;

            // If this entry starts with a match to our filename, get the rest, else ignore
            let remainder = match name.strip_prefix(&prefix) {
                Some(r) => r,
                None => continue,
            };

            // If this entry has the same extension, get the remaining number, else ignore
            let num = match &ext {
                Some(e) => match remainder.strip_suffix(e) {
                    Some(n) => n,
                    None => continue,
                },
                None => remainder,
            };

            if let Ok(n) = num.parse::<u32>() {
                highest = highest.max(n);
            }
        }

        // Example: account.txt => account-001.txt
        let number = highest + 1;
        Ok(format!("{}-{number:0pad$}{}", basename, ext.clone().unwrap_or_default()))
    }
}

impl<P: CheckedPermissions + MessageAllowed<CloseDir>> Drop for Dir<P> {
    fn drop(&mut self) {
        if let Err(e) = self.conn.try_send_blocking_scalar(CloseDir(self.handle)) {
            log::error!("Failed to close dir: {:?}", e);
        }
    }
}

// WaitForFs helper for subscribing to filesystem events
pub struct WaitForFs<P: CheckedPermissions>(pub FileSystem<P>, pub Location);

impl<P: CheckedPermissions> server::ServerMessages for WaitForFs<P> {
    const NAME: &'static str = "";

    fn messages() -> &'static [server::MessageDef<Self>]
    where
        Self: Sized,
    {
        &[]
    }
}

impl<P: CheckedPermissions> server::Server for WaitForFs<P>
where
    P: MessageAllowed<SubscribeFilesystemEvent>,
{
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        self.0.subscribe_filesystem_events(context, self.1);
    }
}

impl<P: CheckedPermissions> server::ScalarEventHandler<FileSystemEvent> for WaitForFs<P>
where
    P: MessageAllowed<SubscribeFilesystemEvent>,
{
    fn handle(
        &mut self,
        msg: FileSystemEvent,
        _sender: xous::PID,
        context: &mut server::ServerContext<Self>,
    ) {
        if msg.location == self.1 && msg.event_type == FileSystemEventType::Mounted {
            context.shutdown();
        }
    }
}

// ==================== Data types ====================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct FileHandle(pub u32);

wrapped_scalar!(FileHandle);

impl FileHandle {
    pub fn new(id: u32, location: Location) -> Self {
        Self(((location.to_usize().unwrap() as u32) << 24) | (id & 0x00FF_FFFF))
    }

    pub fn id(self) -> u32 { self.0 & 0x00FF_FFFF }

    pub fn location(self) -> Result<Location, Error> {
        Location::from_usize((self.0 >> 24) as usize).ok_or(Error::FileNotOpen)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DirHandle(pub u32);

wrapped_scalar!(DirHandle);

impl DirHandle {
    pub fn new(id: u32, location: Location) -> Self {
        Self(((location.to_usize().unwrap() as u32) << 24) | (id & 0x00FF_FFFF))
    }

    pub fn id(self) -> u32 { self.0 & 0x00FF_FFFF }

    pub fn location(self) -> Result<Location, Error> {
        Location::from_usize((self.0 >> 24) as usize).ok_or(Error::FileNotOpen)
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
    FromPrimitive,
    ToPrimitive,
)]
pub enum Location {
    /// KeyOS common assets directory root.
    /// Read-only. Available to all apps.
    /// `<system-volume>/common`
    CommonAssets = 1,

    /// Currently running KeyOS app's RW data directory.
    /// Available to all apps.
    /// `<encrypted>/appdata/<app-id>/`
    AppData,

    /// Privileged access to System Volume.
    /// `<system-volume>/`
    System,

    /// Privileged access to the whole encrypted partition
    /// `<encrypted>/`
    EncryptedRoot,

    /// Privileged access to the Boot Volume. Should only be used by firmware upgrade/recovery
    Boot,

    /// Externally connected USB drive
    Usb,

    /// Encrypted user files
    /// `<encrypted>/user`
    User,

    /// Virtual partition used to share files on USB.
    Airlock,

    /// Per-app unencrypted state directory on System Volume.
    /// `<system-volume>/state/<app-id>/`
    SystemAppData,

    /// Currently running KeyOS app's read-only bundle resources directory.
    /// Registered by app-manager before launch.
    /// `<system-volume>/keyos/apps/<app-name>/resources/` for built-in apps, or
    /// `<system-volume>/keyos/sideloaded-apps/<app-id>/resources/` for sideloaded apps.
    AppResources,
}

impl server::AsScalar<1> for Location {
    fn as_scalar(&self) -> [u32; 1] { [self.to_u32().unwrap()] }
}

impl server::FromScalar<1> for Location {
    fn from_scalar([value]: [u32; 1]) -> Self { Self::from_u32(value).unwrap_or(Location::AppData) }
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub len: u64,
    pub modified: DateTime,
    pub is_dir: bool,
    pub is_file: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct OpenFlags {
    pub read: bool,
    pub write: bool,
    pub create: bool,
}

impl OpenFlags {
    pub const CREATE: Self = Self { read: true, write: true, create: true };
    pub const READ_ONLY: Self = Self { read: true, write: false, create: false };
    pub const READ_WRITE: Self = Self { read: true, write: true, create: false };
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum SeekFrom {
    Start(u64),
    End(i64),
    Current(i64),
}

impl From<SeekFrom> for std::io::SeekFrom {
    fn from(from: SeekFrom) -> Self {
        match from {
            SeekFrom::Start(offset) => std::io::SeekFrom::Start(offset),
            SeekFrom::End(offset) => std::io::SeekFrom::End(offset),
            SeekFrom::Current(offset) => std::io::SeekFrom::Current(offset),
        }
    }
}

impl From<std::io::SeekFrom> for SeekFrom {
    fn from(from: std::io::SeekFrom) -> Self {
        match from {
            std::io::SeekFrom::Start(offset) => SeekFrom::Start(offset),
            std::io::SeekFrom::End(offset) => SeekFrom::End(offset),
            std::io::SeekFrom::Current(offset) => SeekFrom::Current(offset),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileSystemEvent {
    pub location: Location,
    pub event_type: FileSystemEventType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive, ToPrimitive)]
pub enum FileSystemEventType {
    Mounted,
    Unmounted,
    Error,
}

impl server::AsScalar<2> for FileSystemEvent {
    fn as_scalar(&self) -> [u32; 2] {
        let [location] = self.location.as_scalar();
        [location, self.event_type.to_u32().unwrap()]
    }
}

impl server::FromScalar<2> for FileSystemEvent {
    fn from_scalar([location, event_type]: [u32; 2]) -> Self {
        Self {
            location: Location::from_scalar([location]),
            event_type: FileSystemEventType::from_u32(event_type).unwrap_or(FileSystemEventType::Mounted),
        }
    }
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct MappedFileInTheirSpace {
    pub addr: usize,
    pub size: usize,
}

#[derive(Debug, Clone, Copy, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct Metadata {
    pub created: DateTime,
    pub accessed: Date,
    pub modified: DateTime,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct Date {
    pub year: u16,
    pub month: u16,
    pub day: u16,
}

impl Ord for Date {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.year
            .cmp(&other.year)
            .then_with(|| self.month.cmp(&other.month))
            .then_with(|| self.day.cmp(&other.day))
    }
}

impl PartialOrd for Date {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct Time {
    pub hour: u16,
    pub min: u16,
    pub sec: u16,
    pub millis: u16,
}

impl Ord for Time {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.hour
            .cmp(&other.hour)
            .then_with(|| self.min.cmp(&other.min))
            .then_with(|| self.sec.cmp(&other.sec))
            .then_with(|| self.millis.cmp(&other.millis))
    }
}

impl PartialOrd for Time {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DateTime {
    pub date: Date,
    pub time: Time,
}

impl Ord for DateTime {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.date.cmp(&other.date).then_with(|| self.time.cmp(&other.time))
    }
}

impl PartialOrd for DateTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}

pub(crate) fn ensure_parent_dir_exists_impl(
    mut create_dir: impl FnMut(&str) -> Result<(), Error>,
    path: &str,
) -> Result<(), Error> {
    fn recurse(create_dir: &mut impl FnMut(&str) -> Result<(), Error>, path: &str) -> Result<(), Error> {
        if let Some(parent) = path.rsplit_once('/').map(|(parent, _)| parent) {
            if !parent.is_empty() {
                match create_dir(parent) {
                    Ok(_) | Err(Error::FileAlreadyExists) => {}
                    Err(Error::FileNotFound) => {
                        recurse(create_dir, parent)?;
                        match create_dir(parent) {
                            Ok(_) | Err(Error::FileAlreadyExists) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }

    recurse(&mut create_dir, path)
}
