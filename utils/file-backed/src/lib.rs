// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    io::{BufReader, Seek, SeekFrom, Write},
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use fs::{
    messages::{
        CloseDir, CloseFile, CreateDirMessage, Flush, OpenFileMessage, ReadFile, Remove, Rename, SeekFile,
        TruncateFile, WriteFile,
    },
    File, FileSystem,
};
use server::MessageAllowed;

pub trait ByteCodec: Default + Sized {
    type Error: From<fs::Error> + From<std::io::Error> + std::fmt::Display;

    fn from_reader(reader: impl std::io::Read) -> Result<Self, Self::Error>;

    fn to_bytes(&self) -> Result<Vec<u8>, Self::Error>;
}

impl ByteCodec for Vec<u8> {
    type Error = std::io::Error;

    fn from_reader(mut reader: impl std::io::Read) -> Result<Self, Self::Error> {
        let mut buf = vec![];
        reader.read_to_end(&mut buf)?;
        Ok(buf)
    }

    fn to_bytes(&self) -> Result<Vec<u8>, Self::Error> { Ok(self.clone()) }
}

impl ByteCodec for String {
    type Error = std::io::Error;

    fn from_reader(mut reader: impl std::io::Read) -> Result<Self, Self::Error> {
        let mut buf = String::new();
        reader.read_to_string(&mut buf)?;
        Ok(buf)
    }

    fn to_bytes(&self) -> Result<Vec<u8>, Self::Error> { Ok(self.clone().into_bytes()) }
}

/// A wrapper around a value, persisted to a file.
///
/// The only way to get access to the inner value is via [`FileBacked::guard()`] or [`FileBacked::deref()`]
///
/// [`FileBacked::guard()`] will return a [`FileBackedGuard`], which will mark the file as dirty on a
/// mutation and when the guard is dropped, the file will be saved.
#[derive(Debug)]
pub struct FileBacked<T, P>
where
    T: ByteCodec,
    P: FileBackedPermissions,
{
    path: String,
    location: fs::Location,
    auto_save: bool,
    dirty: bool,
    value: T,
    _marker: PhantomData<fn() -> P>,
}

pub trait FileBackedPermissions:
    server::CheckedPermissions
    + MessageAllowed<CreateDirMessage>
    + MessageAllowed<CloseDir>
    + MessageAllowed<OpenFileMessage>
    + MessageAllowed<CloseFile>
    + MessageAllowed<ReadFile>
    + MessageAllowed<SeekFile>
    + MessageAllowed<WriteFile>
    + MessageAllowed<TruncateFile>
    + MessageAllowed<Flush>
    + MessageAllowed<Rename>
    + MessageAllowed<Remove>
{
}

impl<T> FileBackedPermissions for T where
    T: server::CheckedPermissions
        + MessageAllowed<CreateDirMessage>
        + MessageAllowed<CloseDir>
        + MessageAllowed<OpenFileMessage>
        + MessageAllowed<CloseFile>
        + MessageAllowed<ReadFile>
        + MessageAllowed<SeekFile>
        + MessageAllowed<WriteFile>
        + MessageAllowed<TruncateFile>
        + MessageAllowed<Flush>
        + MessageAllowed<Rename>
        + MessageAllowed<Remove>
{
}

impl<T, P> FileBacked<T, P>
where
    T: ByteCodec,
    P: FileBackedPermissions,
{
    // load or create the file backed type
    pub fn new(path: impl Into<String>, location: fs::Location) -> (Self, bool) {
        let fs = FileSystem::<P>::default();
        let path = path.into();
        let old_path = format!("{}.old", path);

        let (value, restored) = Self::try_restore(&fs, &path, location, true)
            .or_else(|_| Self::try_restore(&fs, &old_path, location, true))
            .map(|t| (t, true))
            .unwrap_or_else(|_| (T::default(), false));

        let mut state = Self {
            value,
            path,
            location,
            auto_save: true,
            dirty: !restored,
            _marker: PhantomData::default(),
        };
        // The initial save is the only persist that can run before a caller has
        // confirmed its fs location is mounted (every explicit save and guard-Drop runs
        // during active operation, when the location is up). Degrade gracefully here so
        // constructing against a not-yet-ready or unformatted AppData location logs and
        // continues instead of crashing; a genuine persistence failure still surfaces as
        // a panic on the next explicit save.
        if let Err(e) = state.try_save() {
            log::warn!("FileBacked: initial save failed (fs not ready?), continuing: {e}");
        }
        (state, restored)
    }

    /// Enable or disable automatic saving of changes to the backing file.
    ///
    /// By default, instances created via [`new`] or [`load`] start with
    /// `auto_save` set to `true`. In that mode, any mutation that marks the
    /// value as dirty will cause the data to be written to disk automatically
    /// (e.g. when the guard is dropped or `save` is called internally).
    ///
    /// Setting `auto_save` to `false` disables this behavior. This can be
    /// useful when you plan to perform a large number of updates and want to
    /// avoid incurring a filesystem write for each change; instead, you can
    /// batch your modifications and explicitly call [`save`] or [`try_save`]
    /// once when you are done.
    ///
    /// When auto-save is disabled, **you are responsible** for ensuring that
    /// [`save`] or [`try_save`] is called at appropriate times. If the process
    /// exits, crashes, or the device loses power before an explicit save,
    /// any unsaved changes will be lost.
    pub fn set_auto_save(&mut self, auto_save: bool) { self.auto_save = auto_save; }

    /// load an existing file if it exists
    pub fn load(path: impl Into<String>, location: fs::Location) -> Result<Self, T::Error> {
        let fs = FileSystem::<P>::default();
        let path = path.into();
        let old_path = format!("{}.old", path);

        let value = Self::try_restore(&fs, &path, location, false)
            .or_else(|_| Self::try_restore(&fs, &old_path, location, false))?;

        Ok(Self { value, path, location, dirty: false, auto_save: true, _marker: PhantomData::default() })
    }

    pub fn try_save(&mut self) -> Result<(), T::Error> {
        if !self.dirty {
            return Ok(());
        }

        let fs = FileSystem::<P>::default();
        let new_path = format!("{}.new", self.path);
        let old_path = format!("{}.old", self.path);

        {
            let mut file = Self::try_open(&fs, &new_path, self.location, true)?;
            let data = self.value.to_bytes()?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&data)?;
            file.truncate()?;
            file.flush()?;
        }

        let _ = fs.remove(&old_path, self.location);
        let _ = fs.rename(&self.path, &old_path, self.location);
        fs.rename(&new_path, &self.path, self.location)?;
        let _ = fs.remove(&old_path, self.location);

        self.dirty = false;
        Ok(())
    }

    /// Persist to disk, panicking on failure. These writes go to local AppData and
    /// can only fail on a genuine bug or unrecoverable condition, not a transient
    /// error worth threading through every caller (CLAUDE.md, panic over
    /// fallibility). Callers that must handle a failure use `try_save` directly.
    pub fn save(&mut self) {
        if let Err(e) = self.try_save() {
            panic!("FileBacked: failed to persist to local fs: {e}");
        }
    }

    pub fn guard(&mut self) -> FileBackedGuard<'_, T, P> { FileBackedGuard { inner: self } }

    fn try_restore(
        fs: &FileSystem<P>,
        path: impl Into<String>,
        location: fs::Location,
        create: bool,
    ) -> Result<T, T::Error> {
        let file = Self::try_open(fs, path, location, create)?;
        let mut reader = BufReader::with_capacity(fs::FILE_BUFFER_SIZE, file);
        let value = T::from_reader(&mut reader)?;
        Ok(value)
    }

    fn try_open(
        fs: &FileSystem<P>,
        path: impl Into<String>,
        location: fs::Location,
        create: bool,
    ) -> Result<File<P>, T::Error> {
        let path = path.into();
        if create {
            fs.ensure_parent_dir_exists(&path, location)
                .inspect_err(|e| log::warn!("Could not create parent dir: {e:?}"))?;
        }
        let file = fs
            .open_file(&path, location, fs::OpenFlags { read: true, write: true, create })
            .inspect_err(|e| log::warn!("Could not open file: {e:?}"))?;
        Ok(file)
    }
}

impl<T, P> Drop for FileBacked<T, P>
where
    T: ByteCodec,
    P: FileBackedPermissions,
{
    fn drop(&mut self) { self.save(); }
}

impl<T, P> Deref for FileBacked<T, P>
where
    T: ByteCodec,
    P: FileBackedPermissions,
{
    type Target = T;

    fn deref(&self) -> &Self::Target { &self.value }
}

/// A guard that marks the [`FileBacked`] as dirty upon a mutation
/// if the value is mutated, the value will be persisted when the guard is dropped
pub struct FileBackedGuard<'a, T, P>
where
    T: ByteCodec,
    P: FileBackedPermissions,
{
    inner: &'a mut FileBacked<T, P>,
}

impl<T, P> Deref for FileBackedGuard<'_, T, P>
where
    T: ByteCodec,
    P: FileBackedPermissions,
{
    type Target = T;

    fn deref(&self) -> &Self::Target { &self.inner.value }
}

impl<T, P> DerefMut for FileBackedGuard<'_, T, P>
where
    T: ByteCodec,
    P: FileBackedPermissions,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.dirty = true;
        &mut self.inner.value
    }
}

impl<T, P> Drop for FileBackedGuard<'_, T, P>
where
    T: ByteCodec,
    P: FileBackedPermissions,
{
    fn drop(&mut self) {
        if self.inner.auto_save {
            self.inner.save();
        }
    }
}

pub type JsonBacked<T, P> = FileBacked<JsonCodec<T>, P>;

#[derive(Default, Debug)]
pub struct JsonCodec<T>(pub T);

impl<T> Deref for JsonCodec<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target { &self.0 }
}

impl<T> DerefMut for JsonCodec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}

impl<T> ByteCodec for JsonCodec<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default,
{
    type Error = fs::Error;

    fn from_reader(reader: impl std::io::Read) -> Result<Self, Self::Error> {
        let value = serde_json::from_reader(reader).map_err(|_| fs::Error::Io)?;
        Ok(JsonCodec(value))
    }

    fn to_bytes(&self) -> Result<Vec<u8>, Self::Error> {
        serde_json::to_vec(&self.0).map_err(|_| fs::Error::Io)
    }
}
