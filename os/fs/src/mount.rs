// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-filesystem mount state.
//!
//! The `fs` field is `&'static`, but only because the underlying `Box` is leaked
//! at `Mount::new` and reclaimed in `Drop`. To keep that lie sound, the maps holding
//! `fatfs::File<'static, _>` / `fatfs::DirIter<'static, _>` values are private and
//! never hand out ownership; the only external paths to a registered handle are
//! borrowed accessors that tie the borrow to `&mut Mount`. The single audited
//! escape hatch is [`Mount::static_fs_unchecked`].

use std::collections::HashMap;

use fs::{DirHandle, Error, FileHandle, Location, OpenFlags};

use crate::disk::DynamicDisk;

pub struct Mount {
    // Field order matters: handles must drop before fs is reclaimed in Drop.
    files: HashMap<xous::PID, HashMap<FileHandle, OpenFile>>,
    dirs: HashMap<xous::PID, HashMap<DirHandle, OpenDir>>,
    // Per-PID monotonic id allocator for new file/dir handles. Shared between files and
    // dirs (the handle's Location bits keep the two namespaces apart).
    next_id: HashMap<xous::PID, u32>,
    // Private on purpose. See the module-level docs for why.
    fs: &'static fatfs::FileSystem<DynamicDisk>,
}

impl Mount {
    pub fn new(disk: DynamicDisk) -> std::io::Result<Self> {
        let fs = fatfs::FileSystem::new(disk, fatfs::FsOptions::new())?;
        Ok(Self {
            files: Default::default(),
            dirs: Default::default(),
            next_id: Default::default(),
            fs: Box::leak(Box::new(fs)),
        })
    }

    /// Borrow the underlying filesystem for transient operations (flush, with_disk,
    /// total_clusters, fat_slice, ...). The returned reference is tied to `&self`,
    /// not `'static` -- callers cannot stash it past this `Mount`'s lifetime.
    pub fn fs(&self) -> &fatfs::FileSystem<DynamicDisk> { self.fs }

    /// Returns the filesystem root directory for transient traversal (rename, remove,
    /// recursive copy, metadata probe, ...). The returned `Dir` borrows from `&self`,
    /// so it cannot outlive this `Mount`. To create a file/dir handle that gets
    /// stored on this `Mount`, use [`Mount::open_file`] / [`Mount::create_file`] /
    /// [`Mount::open_dir`] / [`Mount::create_dir`] instead.
    pub fn root_dir(&self) -> fatfs::Dir<'_, DynamicDisk> { self.fs.root_dir() }

    /// Open an existing file at `path` and register it. Returns the freshly allocated
    /// handle. Allocation runs *after* the fatfs open, so a failed open doesn't burn
    /// an id.
    pub fn open_file(
        &mut self,
        pid: xous::PID,
        location: Location,
        path: String,
        flags: OpenFlags,
    ) -> Result<FileHandle, Error> {
        let entry_positions = self.fs.root_dir().resolve_entry_positions(&path)?;
        let file_id = entry_positions.last().copied().ok_or(Error::InvalidPath)?;
        if flags.write && self.has_writer(file_id) {
            return Err(Error::FileInUse);
        }
        let file = self.fs.root_dir().open_file(&path)?;
        let handle = self.alloc_file_handle(pid, location)?;
        self.files.entry(pid).or_default().insert(handle, OpenFile { file, entry_positions, flags });
        Ok(handle)
    }

    /// Create a file at `path` and register it. Returns the freshly allocated handle.
    pub fn create_file(
        &mut self,
        pid: xous::PID,
        location: Location,
        path: String,
        flags: OpenFlags,
    ) -> Result<FileHandle, Error> {
        // Create before checking: the entry's position does not exist until the
        // entry does. A pre-existing writer means the file was already there and
        // `create_file` just opened it, so dropping `file` on conflict undoes
        // nothing.
        let file = self.fs.root_dir().create_file(&path)?;
        let entry_positions = self.fs.root_dir().resolve_entry_positions(&path)?;
        let file_id = entry_positions.last().copied().ok_or(Error::InvalidPath)?;
        if flags.write && self.has_writer(file_id) {
            return Err(Error::FileInUse);
        }
        let handle = self.alloc_file_handle(pid, location)?;
        self.files.entry(pid).or_default().insert(handle, OpenFile { file, entry_positions, flags });
        Ok(handle)
    }

    /// True if any registered handle holds the file `file_id` open for writing.
    fn has_writer(&self, file_id: u64) -> bool {
        self.files.values().flat_map(|m| m.values()).any(|o| o.file_id() == file_id && o.flags.write)
    }

    /// Whether a handle other than `(except_pid, except)` is open on the file
    /// identified by `file_id`. A shrink frees clusters whose contents another
    /// handle still caches (it snapshots size and first_cluster at open and
    /// never refreshes), so shrinking is refused while any independent handle is
    /// open.
    pub fn has_other_handle(&self, file_id: u64, except_pid: xous::PID, except: FileHandle) -> bool {
        self.files.iter().any(|(pid, files)| {
            files
                .iter()
                .any(|(handle, of)| of.file_id() == file_id && !(*pid == except_pid && *handle == except))
        })
    }

    /// Open a directory iterator at `path` and register it. An empty `path` iterates
    /// the filesystem root. Returns the freshly allocated handle.
    pub fn open_dir(&mut self, pid: xous::PID, location: Location, path: String) -> Result<DirHandle, Error> {
        let dir = if path.is_empty() { self.fs.root_dir() } else { self.fs.root_dir().open_dir(&path)? };
        let entry_positions = self.fs.root_dir().resolve_entry_positions(&path)?;
        let handle = self.alloc_dir_handle(pid, location)?;
        self.dirs.entry(pid).or_default().insert(handle, OpenDir { iter: dir.iter(), entry_positions });
        Ok(handle)
    }

    /// Create a directory at `path` and register an iterator over it. Returns the
    /// freshly allocated handle.
    pub fn create_dir(
        &mut self,
        pid: xous::PID,
        location: Location,
        path: String,
    ) -> Result<DirHandle, Error> {
        let dir = self.fs.root_dir().create_dir(&path)?;
        let entry_positions = self.fs.root_dir().resolve_entry_positions(&path)?;
        let handle = self.alloc_dir_handle(pid, location)?;
        self.dirs.entry(pid).or_default().insert(handle, OpenDir { iter: dir.iter(), entry_positions });
        Ok(handle)
    }

    fn alloc_id(&mut self, pid: xous::PID) -> u32 {
        let id = self.next_id.entry(pid).or_default();
        let value = *id;
        *id = id.wrapping_add(1) & 0x00FF_FFFF;
        value
    }

    // Probe up to 2^24 candidate ids (the 24-bit id space) for an unused FileHandle.
    fn alloc_file_handle(&mut self, pid: xous::PID, location: Location) -> Result<FileHandle, Error> {
        for _ in 0..0x0100_0000u32 {
            let handle = FileHandle::new(self.alloc_id(pid), location);
            let taken = self.files.get(&pid).is_some_and(|m| m.contains_key(&handle));
            if !taken {
                return Ok(handle);
            }
        }
        log::error!("Out of FileHandle ids for PID {pid:?} at {location:?}");
        Err(Error::OutOfMemory)
    }

    fn alloc_dir_handle(&mut self, pid: xous::PID, location: Location) -> Result<DirHandle, Error> {
        for _ in 0..0x0100_0000u32 {
            let handle = DirHandle::new(self.alloc_id(pid), location);
            let taken = self.dirs.get(&pid).is_some_and(|m| m.contains_key(&handle));
            if !taken {
                return Ok(handle);
            }
        }
        log::error!("Out of DirHandle ids for PID {pid:?} at {location:?}");
        Err(Error::OutOfMemory)
    }

    /// Borrow the registered file for `(pid, handle)`. The returned reference can be used
    /// to read flags, the path, and to drive read/write/seek on the underlying fatfs file,
    /// but cannot extend its lifetime past this `Mount`.
    pub fn file_mut(&mut self, pid: xous::PID, handle: FileHandle) -> Option<&mut OpenFile> {
        self.files.get_mut(&pid)?.get_mut(&handle)
    }

    /// Read-only counterpart to [`Mount::file_mut`].
    pub fn file(&self, pid: xous::PID, handle: FileHandle) -> Option<&OpenFile> {
        self.files.get(&pid)?.get(&handle)
    }

    /// Borrow the registered dir iterator for `(pid, handle)`.
    pub fn dir_mut(&mut self, pid: xous::PID, handle: DirHandle) -> Option<&mut OpenDir> {
        self.dirs.get_mut(&pid)?.get_mut(&handle)
    }

    /// Iterator over every registered file/dir identity chain on this mount.
    fn open_chains(&self) -> impl Iterator<Item = &[u64]> {
        let files = self.files.values().flat_map(|m| m.values().map(|o| o.entry_positions.as_slice()));
        let dirs = self.dirs.values().flat_map(|m| m.values().map(|o| o.entry_positions.as_slice()));
        files.chain(dirs)
    }

    /// Whether any open handle covers `path` or sits beneath it: if `path` is a
    /// directory, an open handle anywhere inside it (at any depth) also counts.
    ///
    /// Matching is on the [`OpenFile::entry_positions`] identity chain, with a
    /// prefix counting as containment. An empty `path` is the root, so any open
    /// handle counts; a path that doesn't resolve has no handle, so it is not in use.
    pub fn path_in_use(&self, path: &str) -> Result<bool, Error> {
        let target = match self.fs.root_dir().resolve_entry_positions(path) {
            Ok(positions) => positions,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        if target.is_empty() {
            return Ok(self.open_chains().next().is_some());
        }
        Ok(self.open_chains().any(|c| c.starts_with(&target)))
    }

    /// Remove the registered file for `(pid, handle)`. The dropped `OpenFile` flushes
    /// the file on its way out. Cleans up the per-PID entry when its last handle goes
    /// away.
    pub fn close_file(&mut self, pid: xous::PID, handle: FileHandle) -> Result<(), Error> {
        let files = self.files.get_mut(&pid).ok_or(Error::FileNotOpen)?;
        files.remove(&handle).ok_or(Error::FileNotOpen)?;
        if files.is_empty() {
            self.files.remove(&pid);
        }
        Ok(())
    }

    /// Remove the registered dir iterator for `(pid, handle)`. Cleans up the per-PID
    /// entry when its last handle goes away.
    pub fn close_dir(&mut self, pid: xous::PID, handle: DirHandle) -> Result<(), Error> {
        let dirs = self.dirs.get_mut(&pid).ok_or(Error::FileNotOpen)?;
        dirs.remove(&handle).ok_or(Error::FileNotOpen)?;
        if dirs.is_empty() {
            self.dirs.remove(&pid);
        }
        Ok(())
    }

    /// Drop all open files and dirs for the given process and forget its id counter.
    pub fn clear_pid(&mut self, pid: xous::PID) {
        self.files.remove(&pid);
        self.dirs.remove(&pid);
        self.next_id.remove(&pid);
    }

    /// Tear down the mount and return the underlying disk via `FileSystem::unmount`.
    /// Drops the handle maps first so the borrows inside release before the box is
    /// reclaimed.
    #[cfg(not(feature = "recovery-os"))]
    pub fn into_disk(self) -> std::io::Result<DynamicDisk> {
        let mut me = std::mem::ManuallyDrop::new(self);
        // Drop everything else before reclaiming the FS (matches Drop ordering).
        let _ = std::mem::take(&mut me.files);
        let _ = std::mem::take(&mut me.dirs);
        let _ = std::mem::take(&mut me.next_id);
        // SAFETY: me.fs was produced by Box::leak in Mount::new and, per the module docs,
        // no `'static` reference to it has escaped this module.
        let boxed = unsafe { Box::from_raw((me.fs as *const _) as *mut fatfs::FileSystem<DynamicDisk>) };
        boxed.unmount()
    }

    /// Escape hatch that hands out a `&'static FileSystem` reference, as required
    /// by APIs like `DiskImage::new` whose internal storage demands `'static`.
    ///
    /// **This re-introduces the same lifetime lie as `self.fs`; treat it as
    /// reserved for the one call site that needs it, not a general accessor.**
    /// The single legitimate
    /// caller today is `airlock::new_airlock_disk`, where the long-lived
    /// `fs_user` mount is guaranteed by external sequencing to outlive any
    /// airlock state derived from it. If you reach for this method, you are
    /// promising to maintain that ordering by hand; everything else should use
    /// [`Mount::fs`] instead.
    #[cfg(not(feature = "recovery-os"))]
    pub fn static_fs_unchecked(&self) -> &'static fatfs::FileSystem<DynamicDisk> { self.fs }
}

impl Drop for Mount {
    fn drop(&mut self) {
        // Explicitly drop handles before reclaiming the FS, otherwise the
        // File<'static>/DirIter<'static> borrows inside would dangle when the
        // FileSystem Box is freed below.
        let _ = std::mem::take(&mut self.files);
        let _ = std::mem::take(&mut self.dirs);
        // SAFETY: self.fs was produced by Box::leak in Mount::new and, per the module docs,
        // no `'static` reference to it has escaped this module.
        let boxed = unsafe { Box::from_raw((self.fs as *const _) as *mut fatfs::FileSystem<DynamicDisk>) };
        boxed.unmount().ok();
    }
}

/// A registered open file. Only ever obtainable as `&mut` via [`Mount::file_mut`] /
/// [`Mount::file`] -- the owning maps on `Mount` are private. The `'static` borrow
/// inside `file` cannot escape: `fatfs::File` is neither `Copy` nor `Default`, so
/// callers can drive it through the `&mut` borrow but cannot move it out, take
/// ownership, or swap it for a constructible replacement.
pub struct OpenFile {
    pub file: fatfs::File<'static, DynamicDisk>,
    /// On-disk identity of the open file: the chain of directory-entry positions
    /// from the top-most path component down to the file itself. Keyed on
    /// positions rather than the path string so that a long name and its 8.3
    /// alias resolve to the same file. A position only changes if the entry is
    /// deleted or renamed, or its containing directory is deleted -- all blocked
    /// while the file is open.
    entry_positions: Vec<u64>,
    pub flags: OpenFlags,
}

impl OpenFile {
    /// The file's own directory-entry position, its on-disk identity.
    pub fn file_id(&self) -> u64 { *self.entry_positions.last().unwrap() }
}

impl Drop for OpenFile {
    fn drop(&mut self) {
        use std::io::Write;
        let _ = self.file.flush();
    }
}

/// Registered open directory iterator. Same containment story as [`OpenFile`].
pub struct OpenDir {
    pub iter: fatfs::DirIter<'static, DynamicDisk>,
    /// On-disk identity of the open directory. See [`OpenFile::entry_positions`].
    entry_positions: Vec<u64>,
}
