// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{Read, Seek, Write};

use server::{CheckedPermissions, MessageAllowed};

use crate::{messages::*, DirEntry, Error, FileSystem, Location, Metadata, OpenFlags};

/// Marker trait that bundles all basic filesystem permissions.
/// Corresponds to the `fs-generic` permission template in `permission_templates.toml`.
pub trait BasicFsPermissions:
    CheckedPermissions
    + MessageAllowed<OpenDirMessage>
    + MessageAllowed<OpenFileMessage>
    + MessageAllowed<CloseFile>
    + MessageAllowed<CloseDir>
    + MessageAllowed<CreateDirMessage>
    + MessageAllowed<ReadFile>
    + MessageAllowed<SeekFile>
    + MessageAllowed<WriteFile>
    + MessageAllowed<TruncateFile>
    + MessageAllowed<SetLen>
    + MessageAllowed<GetMetadata>
    + MessageAllowed<NextEntry>
    + MessageAllowed<Flush>
    + MessageAllowed<FlushFs>
    + MessageAllowed<Remove>
    + MessageAllowed<Rename>
    + MessageAllowed<AtomicCopy>
    + MessageAllowed<AsyncRead>
    + MessageAllowed<AsyncWrite>
    + MessageAllowed<AsyncCopyBlock>
    + MessageAllowed<SubscribeFilesystemEvent>
{
}

impl<P> BasicFsPermissions for P where
    P: CheckedPermissions
        + MessageAllowed<OpenDirMessage>
        + MessageAllowed<OpenFileMessage>
        + MessageAllowed<CloseFile>
        + MessageAllowed<CloseDir>
        + MessageAllowed<CreateDirMessage>
        + MessageAllowed<ReadFile>
        + MessageAllowed<SeekFile>
        + MessageAllowed<WriteFile>
        + MessageAllowed<TruncateFile>
        + MessageAllowed<SetLen>
        + MessageAllowed<GetMetadata>
        + MessageAllowed<NextEntry>
        + MessageAllowed<Flush>
        + MessageAllowed<FlushFs>
        + MessageAllowed<Remove>
        + MessageAllowed<Rename>
        + MessageAllowed<AtomicCopy>
        + MessageAllowed<AsyncRead>
        + MessageAllowed<AsyncWrite>
        + MessageAllowed<AsyncCopyBlock>
        + MessageAllowed<SubscribeFilesystemEvent>
{
}

/// Abstraction over filesystem operations for testing and generic code.
///
/// - [`FileSystem`]: actual keyos fs server
/// - `FsTest`: uses temporary directories (test-only)
pub trait FsAdapter {
    type File: FileAdapter<Self::Permissions>;
    type Permissions: CheckedPermissions;
    type DirIter: Iterator<Item = Result<DirEntry, Error>>;

    fn create_dir(&self, path: &str, location: Location) -> Result<(), Error>
    where
        Self::Permissions: MessageAllowed<CreateDirMessage>,
        Self::Permissions: MessageAllowed<CloseDir>;

    fn remove(&self, path: &str, location: Location) -> Result<(), Error>
    where
        Self::Permissions: MessageAllowed<Remove>;

    fn atomic_copy(
        &self,
        src: &str,
        dest: &str,
        rename: Option<String>,
        location: Location,
    ) -> Result<(), Error>
    where
        Self::Permissions: MessageAllowed<AtomicCopy>;

    fn open_file(&self, path: &str, location: Location, flags: OpenFlags) -> Result<Self::File, Error>
    where
        Self::Permissions: MessageAllowed<OpenFileMessage>,
        Self::Permissions: MessageAllowed<CloseFile>;

    fn open_dir(&self, path: &str, location: Location) -> Result<Self::DirIter, Error>
    where
        Self::Permissions: MessageAllowed<OpenDirMessage>,
        Self::Permissions: MessageAllowed<CloseDir>,
        Self::Permissions: MessageAllowed<NextEntry>;

    fn metadata(&self, path: &str, location: Location) -> Result<Metadata, Error>
    where
        Self::Permissions: MessageAllowed<GetMetadata>;

    fn rename(&self, src: &str, dest: &str, location: Location) -> Result<(), Error>
    where
        Self::Permissions: MessageAllowed<Rename>;

    fn flush(&mut self, location: Location) -> Result<(), Error>
    where
        Self::Permissions: MessageAllowed<FlushFs>;

    fn walk_dir(&self, path: &str, location: Location) -> Result<DirWalker<Self>, Error>
    where
        Self: Clone,
        Self::Permissions: MessageAllowed<OpenDirMessage>,
        Self::Permissions: MessageAllowed<CloseDir>,
        Self::Permissions: MessageAllowed<NextEntry>,
    {
        DirWalker::new(self.clone(), path, location)
    }

    fn ensure_parent_dir_exists(&self, path: &str, location: Location) -> Result<(), Error>
    where
        Self::Permissions: MessageAllowed<CreateDirMessage>,
        Self::Permissions: MessageAllowed<CloseDir>,
    {
        crate::ensure_parent_dir_exists_impl(|dir| self.create_dir(dir, location), path)
    }

    fn remove_if_exists(&self, path: &str, location: Location) -> Result<(), Error>
    where
        Self::Permissions: MessageAllowed<Remove>,
    {
        match self.remove(path, location) {
            Ok(_) => Ok(()),
            Err(Error::FileNotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl<P> FsAdapter for FileSystem<P>
where
    P: CheckedPermissions
        + MessageAllowed<CloseFile>
        + MessageAllowed<CloseDir>
        + MessageAllowed<NextEntry>
        + MessageAllowed<ReadFile>
        + MessageAllowed<WriteFile>
        + MessageAllowed<Flush>
        + MessageAllowed<SeekFile>,
{
    type DirIter = DirIterator<P>;
    type File = crate::File<P>;
    type Permissions = P;

    fn create_dir(&self, path: &str, location: Location) -> Result<(), Error>
    where
        P: MessageAllowed<CreateDirMessage>,
        P: MessageAllowed<CloseDir>,
    {
        Ok(self.create_dir(path, location).map(|_| ())?)
    }

    fn remove(&self, path: &str, location: Location) -> Result<(), Error>
    where
        P: MessageAllowed<Remove>,
    {
        Ok(self.remove(path, location)?)
    }

    fn atomic_copy(
        &self,
        src: &str,
        dest: &str,
        rename: Option<String>,
        location: Location,
    ) -> Result<(), Error>
    where
        P: MessageAllowed<AtomicCopy>,
    {
        Ok(self.atomic_copy(src, dest, rename, location)?)
    }

    fn open_file(&self, path: &str, location: Location, flags: OpenFlags) -> Result<Self::File, Error>
    where
        P: MessageAllowed<OpenFileMessage>,
        P: MessageAllowed<CloseFile>,
    {
        Ok(self.open_file(path, location, flags)?)
    }

    fn open_dir(&self, path: &str, location: Location) -> Result<Self::DirIter, Error>
    where
        P: MessageAllowed<OpenDirMessage>,
        P: MessageAllowed<CloseDir>,
        P: MessageAllowed<NextEntry>,
    {
        let dir = self.open_dir(path, location)?;
        Ok(DirIterator { dir })
    }

    fn metadata(&self, path: &str, location: Location) -> Result<Metadata, Error>
    where
        Self::Permissions: MessageAllowed<GetMetadata>,
    {
        Ok(self.metadata(path, location)?)
    }

    fn rename(&self, src: &str, dest: &str, location: Location) -> Result<(), Error>
    where
        P: MessageAllowed<Rename>,
    {
        Ok(self.rename(src, dest, location)?)
    }

    fn flush(&mut self, location: Location) -> Result<(), Error>
    where
        P: MessageAllowed<FlushFs>,
    {
        Ok(FileSystem::flush(self, location)?)
    }
}

pub trait FileAdapter<P: CheckedPermissions>: Read + Write + Seek {
    fn metadata(&self) -> Result<Metadata, Error>
    where
        P: MessageAllowed<GetMetadata>;

    fn truncate(&mut self) -> Result<(), Error>
    where
        P: MessageAllowed<TruncateFile>;

    fn set_mtime(&mut self, datetime: crate::DateTime) -> Result<(), Error>
    where
        P: MessageAllowed<SetMtime>;

    fn copy_block_to(&mut self, to: &mut Self, len: usize) -> Result<usize, Error>
    where
        P: MessageAllowed<AsyncCopyBlock>;
}

impl<P> FileAdapter<P> for crate::File<P>
where
    P: CheckedPermissions
        + MessageAllowed<CloseFile>
        + MessageAllowed<ReadFile>
        + MessageAllowed<WriteFile>
        + MessageAllowed<Flush>
        + MessageAllowed<SeekFile>,
{
    fn metadata(&self) -> Result<Metadata, Error>
    where
        P: MessageAllowed<GetMetadata>,
    {
        self.metadata()
    }

    fn truncate(&mut self) -> Result<(), Error>
    where
        P: MessageAllowed<TruncateFile>,
    {
        self.truncate()
    }

    fn set_mtime(&mut self, datetime: crate::DateTime) -> Result<(), Error>
    where
        P: MessageAllowed<SetMtime>,
    {
        self.set_mtime(datetime)
    }

    fn copy_block_to(&mut self, to: &mut Self, len: usize) -> Result<usize, Error>
    where
        P: MessageAllowed<AsyncCopyBlock>,
    {
        self.copy_block_to(to, len)
    }
}

pub struct DirIterator<P: CheckedPermissions + MessageAllowed<CloseDir>> {
    dir: crate::Dir<P>,
}

impl<P: CheckedPermissions + MessageAllowed<CloseDir>> Iterator for DirIterator<P>
where
    P: MessageAllowed<NextEntry>,
{
    type Item = Result<DirEntry, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.dir.next_entry() {
            Ok(Some(entry)) => Some(Ok(entry)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

pub struct DirWalker<F>
where
    F: FsAdapter,
    F::Permissions: MessageAllowed<CloseDir> + MessageAllowed<OpenDirMessage> + MessageAllowed<NextEntry>,
{
    fs: F,
    /// current directory being iterated
    current_iter: Option<F::DirIter>,
    /// current path prefix (e.g. "subdir/nested")
    current_path: String,
    /// stack of not-yet-visited directory paths to traverse
    stack: Vec<String>,
    location: Location,
}

impl<F> DirWalker<F>
where
    F: FsAdapter,
    F::Permissions: MessageAllowed<CloseDir> + MessageAllowed<OpenDirMessage> + MessageAllowed<NextEntry>,
{
    pub fn new(fs: F, path: impl Into<String>, location: Location) -> Result<Self, Error> {
        let path = path.into();
        let current_iter = fs.open_dir(&path, location)?;

        Ok(Self { fs, current_iter: Some(current_iter), current_path: path, stack: Vec::new(), location })
    }
}

impl<F> Iterator for DirWalker<F>
where
    F: FsAdapter,
    F::Permissions: MessageAllowed<CloseDir> + MessageAllowed<OpenDirMessage> + MessageAllowed<NextEntry>,
{
    type Item = Result<(String, DirEntry), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(iter) = &mut self.current_iter {
                match iter.next() {
                    Some(Ok(entry)) => {
                        if entry.name == "." || entry.name == ".." {
                            continue;
                        }

                        let full_path = if self.current_path.is_empty() || self.current_path == "/" {
                            entry.name.clone()
                        } else {
                            format!("{}/{}", self.current_path.trim_end_matches('/'), entry.name)
                        };

                        if entry.is_dir {
                            self.stack.push(full_path.clone());
                        }

                        return Some(Ok((full_path, entry)));
                    }
                    Some(Err(e)) => {
                        return Some(Err(e));
                    }
                    None => {
                        self.current_iter = None;
                    }
                }
            }

            // current iterator exhausted, pop next directory from stack
            if let Some(next_path) = self.stack.pop() {
                match self.fs.open_dir(&next_path, self.location) {
                    Ok(iter) => {
                        self.current_path = next_path;
                        self.current_iter = Some(iter);
                    }
                    Err(e) => {
                        return Some(Err(e));
                    }
                }
            } else {
                return None;
            }
        }
    }
}

#[cfg(feature = "test")]
pub mod test_utils {
    use std::collections::HashMap;
    use std::marker::PhantomData;
    use std::path::PathBuf;
    use std::sync::Arc;

    use chrono::{DateTime, Datelike, Local, Timelike};
    use server::AllPermissions;

    use super::*;

    /// Wrapper for std::fs::File that implements FileAdapter.
    pub struct TestFile<P: CheckedPermissions> {
        file: std::fs::File,
        _phantom: PhantomData<P>,
    }

    impl<P: CheckedPermissions> TestFile<P> {
        pub fn new(file: std::fs::File) -> Self { Self { file, _phantom: PhantomData } }
    }

    impl<P: CheckedPermissions> Read for TestFile<P> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> { self.file.read(buf) }
    }

    impl<P: CheckedPermissions> Write for TestFile<P> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> { self.file.write(buf) }

        fn flush(&mut self) -> std::io::Result<()> { self.file.flush() }
    }

    impl<P: CheckedPermissions> Seek for TestFile<P> {
        fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> { self.file.seek(pos) }
    }

    impl<P: CheckedPermissions> FileAdapter<P> for TestFile<P> {
        fn metadata(&self) -> Result<Metadata, Error>
        where
            P: MessageAllowed<GetMetadata>,
        {
            let metadata: Metadata = self.file.metadata()?.try_into()?;
            Ok(metadata)
        }

        fn truncate(&mut self) -> Result<(), Error>
        where
            P: MessageAllowed<TruncateFile>,
        {
            let pos = self.file.stream_position()?;
            self.file.set_len(pos)?;
            Ok(())
        }

        fn set_mtime(&mut self, datetime: crate::DateTime) -> Result<(), Error>
        where
            P: MessageAllowed<SetMtime>,
        {
            use chrono::{Local, TimeZone};

            let datetime_local = Local
                .with_ymd_and_hms(
                    datetime.date.year as i32,
                    datetime.date.month as u32,
                    datetime.date.day as u32,
                    datetime.time.hour as u32,
                    datetime.time.min as u32,
                    datetime.time.sec as u32,
                )
                .single()
                .ok_or(Error::Io)?;
            let system_time: std::time::SystemTime = datetime_local.into();

            self.file.set_modified(system_time)?;
            Ok(())
        }

        fn copy_block_to(&mut self, to: &mut Self, len: usize) -> Result<usize, Error>
        where
            P: MessageAllowed<AsyncCopyBlock>,
        {
            use std::io::{Read, Write};

            let mut buf = vec![0u8; len];
            let bytes_read = self.file.read(&mut buf)?;
            to.file.write_all(&buf[..bytes_read])?;
            Ok(bytes_read)
        }
    }

    pub struct TestDirIterator {
        entries: std::vec::IntoIter<std::result::Result<std::fs::DirEntry, std::io::Error>>,
    }

    impl Iterator for TestDirIterator {
        type Item = Result<DirEntry, Error>;

        fn next(&mut self) -> Option<Self::Item> {
            use chrono::Local;

            self.entries.next().map(|entry| {
                let entry = entry?;
                let metadata = entry.metadata()?;
                let modified: chrono::DateTime<Local> = metadata.modified()?.into();
                let modified = modified.into();

                Ok(DirEntry {
                    name: entry.file_name().to_string_lossy().to_string(),
                    modified,
                    len: metadata.len(),
                    is_dir: metadata.is_dir(),
                    is_file: metadata.is_file(),
                })
            })
        }
    }

    /// Test implementation of `FsAdapter` using temporary directories.
    #[derive(Clone)]
    pub struct FsTest {
        _temp_dir: Arc<tempfile::TempDir>,
        roots: Arc<HashMap<Location, PathBuf>>,
    }

    impl Default for FsTest {
        fn default() -> Self {
            let temp_dir = tempfile::TempDir::new().unwrap();
            let base = temp_dir.path();

            let mut roots = HashMap::new();
            roots.insert(Location::EncryptedRoot, base.join("encrypted"));
            roots.insert(Location::System, base.join("system"));
            roots.insert(Location::SystemAppData, base.join(crate::SYSTEM_STATE_ROOT));
            roots.insert(Location::CommonAssets, base.join("common"));
            roots.insert(Location::AppData, base.join("appdata"));
            roots.insert(Location::Usb, base.join("usb"));
            roots.insert(Location::User, base.join("user"));
            roots.insert(Location::Boot, base.join("boot"));
            roots.insert(Location::AppResources, base.join("app-resources"));

            for path in roots.values() {
                std::fs::create_dir_all(path).unwrap();
            }

            Self { _temp_dir: Arc::new(temp_dir), roots: Arc::new(roots) }
        }
    }

    impl FsTest {
        fn root(&self, location: Location) -> &PathBuf { self.roots.get(&location).unwrap() }

        /// Set the file to exactly these bytes. The adapter's create opens an existing file in
        /// place, as the device does, so an existing file is removed first.
        pub fn write_file(&self, path: &str, contents: &[u8], location: Location) {
            let _ = self.remove(path, location);
            let parts: Vec<&str> = path.rsplitn(2, '/').collect();
            if parts.len() == 2 {
                self.create_dir(parts[1], location).unwrap();
            }
            let mut file = self.open_file(path, location, OpenFlags::CREATE).unwrap();
            file.write_all(contents).unwrap();
        }

        pub fn read_file_contents(&self, path: &str, location: Location) -> Result<Vec<u8>, Error> {
            let mut file = self.open_file(path, location, OpenFlags::READ_ONLY)?;
            let mut contents = Vec::new();
            file.read_to_end(&mut contents)?;
            Ok(contents)
        }

        /// Print a tree view of the filesystem at a given location.
        /// Useful for debugging tests.
        pub fn print_tree(&self, location: Location) {
            let root = self.root(location);
            println!("-----");
            self.print_tree_recursive(root, 0, None);
            println!("-----");
        }

        /// Print a tree view with a maximum depth.
        pub fn print_tree_with_depth(&self, location: Location, max_depth: usize) {
            let root = self.root(location);
            println!("-----");
            self.print_tree_recursive(root, 0, Some(max_depth));
            println!("-----");
        }

        fn print_tree_recursive(&self, dir_path: &std::path::Path, depth: usize, max_depth: Option<usize>) {
            if let Some(max) = max_depth {
                if depth >= max {
                    return;
                }
            }

            let Ok(entries) = std::fs::read_dir(dir_path) else {
                return;
            };

            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                for _ in 0..depth {
                    print!("\t");
                }
                println!("{name_str}");

                if entry.path().is_dir() {
                    self.print_tree_recursive(&entry.path(), depth + 1, max_depth);
                }
            }
        }
    }

    impl TryFrom<std::fs::Metadata> for Metadata {
        type Error = std::io::Error;

        fn try_from(metadata: std::fs::Metadata) -> Result<Self, Self::Error> {
            let created: DateTime<Local> = metadata.created()?.into();
            let accessed: DateTime<Local> = metadata.accessed()?.into();
            let modified: DateTime<Local> = metadata.modified()?.into();

            let accessed_date = accessed.date_naive();
            Ok(crate::Metadata {
                is_dir: metadata.is_dir(),
                size: metadata.len(),
                created: created.into(),
                accessed: crate::Date {
                    year: accessed_date.year() as u16,
                    month: accessed_date.month() as u16,
                    day: accessed_date.day() as u16,
                },
                modified: modified.into(),
            })
        }
    }

    impl FsAdapter for FsTest {
        type DirIter = TestDirIterator;
        type File = TestFile<AllPermissions>;
        type Permissions = AllPermissions;

        fn create_dir(&self, path: &str, location: Location) -> Result<(), Error> {
            let root = self.root(location);
            std::fs::create_dir_all(root.join(path.trim_start_matches('/')))?;
            Ok(())
        }

        fn remove(&self, path: &str, location: Location) -> Result<(), Error> {
            let root = self.root(location);
            let full_path = root.join(path.trim_start_matches('/'));
            if full_path.is_dir() {
                std::fs::remove_dir_all(&full_path)?;
            } else {
                std::fs::remove_file(&full_path)?;
            }
            Ok(())
        }

        fn atomic_copy(
            &self,
            src: &str,
            dest: &str,
            rename: Option<String>,
            location: Location,
        ) -> Result<(), Error> {
            fn copy_recursive(src: &std::path::Path, dest: &std::path::Path) -> Result<(), Error> {
                if src.is_dir() {
                    std::fs::create_dir(dest)?;
                    for entry in std::fs::read_dir(src)? {
                        let entry = entry?;
                        copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
                    }
                } else {
                    std::fs::copy(src, dest)?;
                }
                Ok(())
            }
            let root = self.root(location);
            let src_path = root.join(src.trim_start_matches('/'));
            let dest_path = root.join(dest.trim_start_matches('/'));

            if !dest_path.exists() {
                return Err(Error::FileNotFound);
            }

            let final_dest = if let Some(new_name) = rename {
                dest_path.join(new_name)
            } else {
                dest_path.join(src_path.file_name().unwrap())
            };

            copy_recursive(&src_path, &final_dest)
        }

        fn open_file(&self, path: &str, location: Location, flags: OpenFlags) -> Result<Self::File, Error> {
            let root = self.root(location);
            let full_path = root.join(path.trim_start_matches('/'));

            let file = std::fs::OpenOptions::new()
                .read(flags.read)
                .write(flags.write)
                .create(flags.create)
                .truncate(false)
                .open(&full_path)?;

            Ok(TestFile::new(file))
        }

        fn open_dir(&self, path: &str, location: Location) -> Result<Self::DirIter, Error> {
            let root = self.root(location);
            let full_path = root.join(path.trim_start_matches('/'));

            let entries: Vec<_> = std::fs::read_dir(&full_path)?.collect();

            Ok(TestDirIterator { entries: entries.into_iter() })
        }

        fn rename(&self, src: &str, dest: &str, location: Location) -> Result<(), Error> {
            let root = self.root(location);
            let src_path = root.join(src.trim_start_matches('/'));
            let dest_path = root.join(dest.trim_start_matches('/'));
            std::fs::rename(&src_path, &dest_path)?;
            Ok(())
        }

        fn metadata(&self, path: &str, location: Location) -> Result<Metadata, Error>
        where
            Self::Permissions: MessageAllowed<GetMetadata>,
        {
            let root = self.root(location);
            Ok(std::fs::metadata(root.join(path.trim_start_matches('/')))?.try_into()?)
        }

        fn flush(&mut self, _location: Location) -> Result<(), Error> { Ok(()) }
    }

    impl From<chrono::DateTime<Local>> for crate::DateTime {
        fn from(dt: chrono::DateTime<Local>) -> Self {
            crate::DateTime {
                date: crate::Date { year: dt.year() as u16, month: dt.month() as u16, day: dt.day() as u16 },
                time: crate::Time {
                    hour: dt.hour() as u16,
                    min: dt.minute() as u16,
                    sec: dt.second() as u16,
                    millis: (dt.nanosecond() / 1_000_000) as u16,
                },
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_dir_walker() {
            let fs = FsTest::default();
            let location = Location::AppData;

            fs.write_file("root.txt", b"root", location);
            fs.write_file("dir1/file1.txt", b"file1", location);
            fs.write_file("dir1/file2.txt", b"file2", location);
            fs.write_file("dir1/subdir/file3.txt", b"file3", location);
            fs.write_file("dir2/file4.txt", b"file4", location);

            let walker = fs.walk_dir("/", location).unwrap();
            let mut paths: Vec<String> = walker.map(|r| r.unwrap().0).collect();
            paths.sort();

            let expected = vec![
                "dir1",
                "dir1/file1.txt",
                "dir1/file2.txt",
                "dir1/subdir",
                "dir1/subdir/file3.txt",
                "dir2",
                "dir2/file4.txt",
                "root.txt",
            ];

            assert_eq!(paths, expected);
        }
    }
}
