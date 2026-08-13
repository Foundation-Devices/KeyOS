// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use fs::{
    messages::{CloseDir, CreateDirMessage},
    DurableFilePermissions, FileSystem,
};
use server::permission_set;

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
///
/// Every mutation is persisted, with no way to batch them up. Construction establishes the
/// invariant the rest of the type stands on: the backing file was readable or creatable at
/// `location`, so it stays writable until the process exits, and nothing else may write it
/// behind our back. A write that fails after that is a bug, not a fault to recover from, so
/// it panics.
#[derive(Debug)]
pub struct FileBacked<T, P>
where
    T: ByteCodec,
    P: FileBackedPermissions,
{
    path: String,
    location: fs::Location,
    dirty: bool,
    value: T,
    _marker: PhantomData<fn() -> P>,
}

permission_set!(pub trait FileBackedPermissions: DurableFilePermissions {
    CreateDirMessage, CloseDir
});

impl<T, P> FileBacked<T, P>
where
    T: ByteCodec,
    P: FileBackedPermissions,
{
    /// Load the file, or start from `T::default()` when it is missing or unreadable. The
    /// returned flag is false in the latter case.
    ///
    /// Panics if the backing file cannot be written; `location` must be mounted first, since
    /// this is where the writability invariant is established.
    pub fn new(path: impl Into<String>, location: fs::Location) -> (Self, bool) {
        Self::try_new(path, location)
            .unwrap_or_else(|e| panic!("FileBacked: could not create the backing file: {e}"))
    }

    /// [`FileBacked::new`], reporting a write failure instead of panicking on it.
    pub fn try_new(path: impl Into<String>, location: fs::Location) -> Result<(Self, bool), T::Error> {
        let path = path.into();
        if let Ok(state) = Self::load(&path, location) {
            return Ok((state, true));
        }

        let mut state =
            Self { value: T::default(), path, location, dirty: true, _marker: PhantomData::default() };
        if let Err(e) = state.try_save() {
            // Drop retries a dirty value and panics on failure, so clear it before handing
            // the error to the caller.
            state.dirty = false;
            return Err(e);
        }
        Ok((state, false))
    }

    /// load an existing file if it exists
    pub fn load(path: impl Into<String>, location: fs::Location) -> Result<Self, T::Error> {
        // Usb and Airlock can be unmounted while we hold the file, which breaks the invariant
        // that opening it once means it stays writable.
        assert!(
            !matches!(location, fs::Location::Usb | fs::Location::Airlock),
            "FileBacked: {location:?} can go away at runtime"
        );
        let fs = FileSystem::<P>::default();
        let path = path.into();

        let value = T::from_reader(fs.durable_file_read(&path, location)?.as_slice())?;

        Ok(Self { value, path, location, dirty: false, _marker: PhantomData::default() })
    }

    fn try_save(&mut self) -> Result<(), T::Error> {
        if !self.dirty {
            return Ok(());
        }

        let fs = FileSystem::<P>::default();
        fs.ensure_parent_dir_exists(&self.path, self.location)
            .inspect_err(|e| log::warn!("Could not create parent dir: {e:?}"))?;
        fs.durable_file_write(&self.path, self.location, &self.value.to_bytes()?)?;

        self.dirty = false;
        Ok(())
    }

    /// Persist to disk, panicking on failure.
    ///
    /// Construction already proved the file writable and nothing else writes it, so a failure
    /// here is a bug or a dead filesystem, not a transient to recover from.
    fn save(&mut self) {
        if let Err(e) = self.try_save() {
            panic!("FileBacked: failed to persist to local fs: {e}");
        }
    }

    pub fn guard(&mut self) -> FileBackedGuard<'_, T, P> { FileBackedGuard { inner: self } }
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
    fn drop(&mut self) { self.inner.save(); }
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
