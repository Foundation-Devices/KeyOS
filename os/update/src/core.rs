// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{self, BufReader, Read, Seek, Write};
use std::time::Instant;

use fs::{
    adapter::{BasicFsPermissions, FileAdapter, FsAdapter},
    FILE_BUFFER_SIZE,
};
use server::xous::{self, DropDeallocate};
use update::messages::InstallProgress;
use update::Error;
use update_image::{Action, Header, ReleaseManifest, Version};
use whence::WhenceExt;

/// The main directory that contains the OS files.
pub const KEYOS_DIR_PATH: &str = "/keyos";

/// The backup directory for the previous OS version.
pub const KEYOS_OLD_DIR_PATH: &str = "/keyos.old";

/// The temporary directory used during the update process.
pub const KEYOS_UPDATE_DIR_PATH: &str = "/keyos.update";

/// The directory where the release tar is extracted to.
pub const RELEASE_DIR_PATH: &str = "/release";
/// The path to the manifest file inside the release directory.
pub const MANIFEST_FILE_PATH: &str = "/release/manifest.json";

/// The path to the firmware file.
pub const FIRMWARE_FILE_PATH: &str = "/keyos/app.bin";

/// The path to the staged firmware file.
pub const STAGED_FIRMWARE_FILE_PATH: &str = "/keyos.update/app.bin";

/// The outcome of applying updates.
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum UpdateOutcome {
    /// All releases were applied successfully.
    Done { firmware_timestamp: Option<u32> },
    /// Some releases were applied, but a reboot is required before applying the remaining ones.
    Partial {
        remaining_release_paths: Vec<String>,
        progress_percentage: u32,
        firmware_timestamp: Option<u32>,
    },
}

pub struct Release<F> {
    pub path: String,
    pub guard: F,
    pub firmware_timestamp: Option<u32>,
}

pub struct Estimator {
    total_work: u64,
    completed_work: u64,
    progress_base: u32,
    started_at: Instant,
}

impl Estimator {
    const COPY_WEIGHT: u64 = 4;
    const HASH_WEIGHT: u64 = 7;
    const MIB: u64 = 1024 * 1024;
    const PATCH_WEIGHT: u64 = 14;
    // relative work for 4 MiB/s hashing, 7 MiB/s copies and 2 MiB/s patching
    const WORK_PER_SECOND: u64 = 28 * Self::MIB;

    pub fn record_copy(&mut self, bytes: u64) -> &Self { self.record(bytes, Self::COPY_WEIGHT) }

    pub fn snapshot(&self) -> InstallProgress {
        let segment = if self.total_work == 0 {
            100
        } else {
            (self.completed_work * 100 / self.total_work).min(99) as u32
        };
        let remaining_work = self.total_work - self.completed_work;
        InstallProgress {
            completion_percentage: (self.progress_base + segment * (100 - self.progress_base) / 100).min(99),
            estimated_seconds_remaining: if self.completed_work == 0 {
                remaining_work.div_ceil(Self::WORK_PER_SECOND)
            } else {
                let elapsed_ms = u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                (elapsed_ms * remaining_work / self.completed_work).div_ceil(1000)
            },
        }
    }

    fn record_hash(&mut self, bytes: u64) -> &Self { self.record(bytes, Self::HASH_WEIGHT) }

    fn record(&mut self, bytes: u64, weight: u64) -> &Self {
        self.completed_work = (self.completed_work + bytes * weight).min(self.total_work);
        self
    }
}

pub fn analyze_update<F>(
    fs: &F,
    releases: &[Release<F::File>],
    progress_base: u32,
) -> whence::Result<Estimator, Error>
where
    F: FsAdapter + Clone,
    F::Permissions: BasicFsPermissions,
{
    let mut firmware_bytes = 0u64;
    for entry in fs.walk_dir(KEYOS_DIR_PATH, fs::Location::System).whence()? {
        let (_, entry) = entry.whence()?;
        if !entry.is_dir {
            firmware_bytes += entry.len;
        }
    }
    let mut release_bytes = 0u64;
    let mut action_work = 0u64;
    let mut future_firmware_copies = 0u64;

    for (index, release) in releases.iter().enumerate() {
        let file = open_release_file(fs, &release.path)?;
        let mut archive = tar::Archive::new(file);
        let manifest = {
            let mut manifest: Option<ReleaseManifest> = None;
            for entry in archive.entries_with_seek().whence()? {
                let mut entry = entry.whence()?;
                if entry.path().whence()?.as_ref() == std::path::Path::new("manifest.json") {
                    let mut buf = Vec::new();
                    entry.read_to_end(&mut buf).whence()?;
                    manifest = Some(serde_json::from_slice(&buf).map_err(|e| {
                        log::error!("failed to parse manifest: {e:?}");
                        Error::InvalidManifest
                    })?);
                    break;
                }
            }
            manifest.ok_or(Error::InvalidManifest).whence()?
        };
        if manifest.reboot_required && index + 1 < releases.len() {
            future_firmware_copies += 1;
        }

        let file = open_release_file(fs, &release.path)?;
        let mut archive = tar::Archive::new(file);
        for entry in archive.entries_with_seek().whence()? {
            let mut entry = entry.whence()?;
            let size = entry.header().size().whence()?;
            release_bytes += size;
            let path = entry.path().whence()?;
            let Some(path) = path.to_str().and_then(|path| path.strip_prefix("patch/")) else {
                continue;
            };
            let mut patch_count = 0u64;
            let mut add_count = 0u64;
            for action in manifest.transactions.iter().flat_map(|tx| tx.actions()) {
                match action {
                    Action::Patch { patch_file, .. } | Action::PatchAdd { patch_file, .. }
                        if patch_file == path =>
                    {
                        patch_count += 1;
                    }
                    Action::Add { source, .. } if source == path => add_count += 1,
                    _ => {}
                }
            }
            if patch_count > 0 {
                let header = Header::read_from(&mut entry).whence()?;
                let patch_work = header.old_file_size * Estimator::HASH_WEIGHT
                    + (size - Header::SIZE as u64) * Estimator::COPY_WEIGHT
                    + header.new_file_size * (Estimator::PATCH_WEIGHT + Estimator::HASH_WEIGHT);
                action_work += patch_work * patch_count;
            }
            action_work += size * Estimator::COPY_WEIGHT * add_count;
        }
    }

    let total_work = firmware_bytes * (1 + future_firmware_copies) * Estimator::COPY_WEIGHT
        + release_bytes * Estimator::COPY_WEIGHT
        + action_work;
    Ok(Estimator {
        total_work,
        completed_work: 0,
        progress_base: progress_base.min(99),
        started_at: Instant::now(),
    })
}

/// Copies the current OS firmware from /keyos to /keyos.update in preparation for patching.
pub fn make_firmware_copy<F>(fs: &F, mut progress: impl FnMut(u64)) -> whence::Result<(), Error>
where
    F: FsAdapter + Clone,
    F::Permissions: BasicFsPermissions,
{
    log::info!("making firmware copy");

    fs.remove_if_exists(KEYOS_UPDATE_DIR_PATH, fs::Location::System).whence()?;
    fs.create_dir(KEYOS_UPDATE_DIR_PATH, fs::Location::System).whence()?;

    let walker = fs.walk_dir(KEYOS_DIR_PATH, fs::Location::System).whence()?;

    for entry_result in walker {
        let (path, entry) = entry_result.whence()?;

        let relative_path = path.strip_prefix("/keyos/").unwrap_or(&path);
        let dest_path = format!("{}/{}", KEYOS_UPDATE_DIR_PATH, relative_path);

        if entry.is_dir {
            fs.create_dir(&dest_path, fs::Location::System).whence()?;
        } else if entry.is_file {
            let mut src = fs.open_file(&path, fs::Location::System, fs::OpenFlags::READ_ONLY).whence()?;
            let mut dst = fs.open_file(&dest_path, fs::Location::System, fs::OpenFlags::CREATE).whence()?;

            let mut remaining = entry.len as usize;
            while remaining > 0 {
                let block_size = remaining.min(fs::MAX_ASYNC_LEN);
                let written = src.copy_block_to(&mut dst, block_size).whence()?;
                // copy_block_to returns 0 at EOF; stop, or else an over-long entry.len loops forever.
                if written == 0 {
                    break;
                }
                remaining = remaining.saturating_sub(written);
                progress(written as u64);
            }
        }
    }

    Ok(())
}

/// Finalizes the update by swapping the updated firmware into place and removing the old version.
///
/// This is idempotent so an interrupted swap can be completed on the next boot.
pub fn finalize_update<F>(fs: &mut F) -> whence::Result<(), Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    fs.flush(fs::Location::System).whence()?;

    let keyos_exists = path_exists(fs, KEYOS_DIR_PATH)?;
    let update_exists = path_exists(fs, KEYOS_UPDATE_DIR_PATH)?;
    let old_exists = path_exists(fs, KEYOS_OLD_DIR_PATH)?;

    if !keyos_exists && !update_exists {
        return Err(Error::Unexpected("firmware swap has no current or staged firmware".into())).whence();
    }

    if keyos_exists && update_exists {
        if old_exists {
            return Err(Error::Unexpected("firmware swap already has an old firmware directory".into()))
                .whence();
        }
        fs.rename(KEYOS_DIR_PATH, KEYOS_OLD_DIR_PATH, fs::Location::System).whence()?;
    }

    if update_exists {
        fs.rename(KEYOS_UPDATE_DIR_PATH, KEYOS_DIR_PATH, fs::Location::System).whence()?;
    }

    if old_exists || (keyos_exists && update_exists) {
        fs.remove(KEYOS_OLD_DIR_PATH, fs::Location::System).whence()?;
    }

    Ok(())
}

fn path_exists<F>(fs: &F, path: &str) -> Result<bool, Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    match fs.metadata(path, fs::Location::System) {
        Ok(_) => Ok(true),
        Err(fs::Error::FileNotFound) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Applies a series of releases to the update directory.
///
/// This is a pure function that performs the core update logic without side effects like
/// rebooting or persisting state. The caller is responsible for handling the outcome.
///
/// # Arguments
/// * `fs` - File system API
/// * `hash_file` - SHA256 of a file in [`fs::Location::System`]
/// * `progress` - Callback invoked when the estimate changes
pub fn apply_update<F>(
    fs: &F,
    hash_file: &impl Fn(&str) -> whence::Result<[u8; 32], Error>,
    releases: Vec<Release<F::File>>,
    estimator: &mut Estimator,
    mut progress: impl FnMut(&Estimator),
) -> whence::Result<UpdateOutcome, Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    if releases.is_empty() {
        return Err(Error::NoUpdateDownloaded).whence();
    }

    let mut releases = releases.into_iter();
    let mut applied_timestamp = None;

    while let Some(Release { path: release_path, guard, firmware_timestamp }) = releases.next() {
        log::info!("applying release from {release_path}");

        let file = open_release_file(fs, &release_path)?;
        let mut release_tar = tar::Archive::new(file);

        // Extract the release tar to a clean release directory.
        log::info!("extracting release tar");
        match fs.remove(RELEASE_DIR_PATH, fs::Location::System) {
            Ok(_) | Err(fs::Error::FileNotFound) => {}
            Err(e) => return Err(e).whence(),
        }
        fs.create_dir(RELEASE_DIR_PATH, fs::Location::System).whence()?;

        let entries = release_tar.entries().whence()?;
        for entry in entries {
            let mut entry = entry.whence()?;
            let entry_path =
                entry.path().whence()?.to_str().ok_or(Error::InvalidManifest).whence()?.to_string();
            let dest_path = format!("{RELEASE_DIR_PATH}/{entry_path}");
            if entry.header().entry_type().is_dir() {
                fs.create_dir(&dest_path, fs::Location::System).whence()?;
            } else {
                let mut dest_file =
                    fs.open_file(&dest_path, fs::Location::System, fs::OpenFlags::CREATE).whence()?;
                dest_file.truncate().whence()?;
                let mut writer = ProgressIo::new(&mut dest_file, |bytes| {
                    progress(estimator.record_copy(bytes));
                });
                io::copy(&mut entry, &mut writer).whence()?;
                writer.finish();
            }
        }

        // Load the manifest file (now extracted to disk)
        let manifest = {
            let manifest_size: usize = fs
                .metadata(MANIFEST_FILE_PATH, fs::Location::System)
                .whence()?
                .size
                .try_into()
                .map_err(|_| Error::Unexpected("manifest file size too large".to_string()))
                .whence()?;
            let mut buf = Vec::with_capacity(manifest_size);
            fs.open_file(MANIFEST_FILE_PATH, fs::Location::System, fs::OpenFlags::READ_ONLY)
                .whence()?
                .read_to_end(&mut buf)
                .whence()?;
            serde_json::from_slice::<ReleaseManifest>(&buf).map_err(|e| {
                let data_str = str::from_utf8(&buf);
                log::error!("failed to parse manifest {e:?}\n{buf:?}\n{data_str:?}");
                Error::InvalidManifest
            })?
        };

        log::info!("applying release changes");

        for tx in manifest.transactions {
            execute_transaction(fs, hash_file, tx.actions(), estimator, &mut progress)?;
        }

        log::info!("cleaning up update files");

        fs.remove(RELEASE_DIR_PATH, fs::Location::System).whence()?;
        drop(release_tar);
        drop(guard);
        fs.remove(&release_path, fs::Location::System).whence()?;

        log::info!("release applied successfully");
        if firmware_timestamp.is_some() {
            applied_timestamp = firmware_timestamp;
        }

        if manifest.reboot_required {
            let remaining_releases = releases.map(|release| release.path).collect::<Vec<_>>();
            if remaining_releases.is_empty() {
                break;
            }
            log::info!("release requires a reboot, returning partial outcome");
            return Ok(UpdateOutcome::Partial {
                remaining_release_paths: remaining_releases,
                progress_percentage: estimator.snapshot().completion_percentage,
                firmware_timestamp: applied_timestamp,
            });
        }
    }

    Ok(UpdateOutcome::Done { firmware_timestamp: applied_timestamp })
}

/// Execute the actions from a single transaction on a copy of the OS firmware.
fn execute_transaction<F>(
    fs: &F,
    hash_file: &impl Fn(&str) -> whence::Result<[u8; 32], Error>,
    actions: &[Action],
    estimator: &mut Estimator,
    progress: &mut impl FnMut(&Estimator),
) -> whence::Result<(), Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    for action in actions {
        match action {
            Action::Patch { patch_file, patch_source, base_version, new_version } => {
                log::debug!("patch file {patch_source}");
                patch_to(
                    fs,
                    hash_file,
                    patch_file,
                    patch_source,
                    patch_source,
                    base_version,
                    new_version,
                    estimator,
                    progress,
                )?;
            }
            Action::PatchAdd { patch_file, patch_source, dest, base_version, new_version } => {
                log::debug!("patch-add file {patch_source}");
                patch_to(
                    fs,
                    hash_file,
                    patch_file,
                    patch_source,
                    dest,
                    base_version,
                    new_version,
                    estimator,
                    progress,
                )?;
            }
            Action::Add { source, dest } => {
                log::debug!("add file {source}");
                let source_file_path = format!("{RELEASE_DIR_PATH}/patch/{source}");
                let dest_file_path = format!("{KEYOS_UPDATE_DIR_PATH}/{dest}");

                fs.ensure_parent_dir_exists(&dest_file_path, fs::Location::System).whence()?;
                let mut source_file = fs
                    .open_file(&source_file_path, fs::Location::System, fs::OpenFlags::READ_ONLY)
                    .whence()?;
                let mut dest_file =
                    fs.open_file(&dest_file_path, fs::Location::System, fs::OpenFlags::CREATE).whence()?;
                dest_file.truncate().whence()?;

                let mut writer = ProgressIo::new(&mut dest_file, |bytes| {
                    progress(estimator.record_copy(bytes));
                });
                io::copy(&mut source_file, &mut writer).whence()?;
                writer.finish();
            }
            Action::Rename { source, dest } | Action::Move { source, dest } => {
                log::debug!("rename/move file {source} -> {dest}");
                let src_path = format!("{KEYOS_UPDATE_DIR_PATH}/{source}");
                let dest_path = format!("{KEYOS_UPDATE_DIR_PATH}/{dest}");
                fs.ensure_parent_dir_exists(&dest_path, fs::Location::System).whence()?;
                fs.rename(&src_path, &dest_path, fs::Location::System).whence()?;
            }
            Action::Delete { path } => {
                log::debug!("delete file {path}");
                let path = format!("{KEYOS_UPDATE_DIR_PATH}/{path}");
                fs.remove(&path, fs::Location::System).whence()?;
            }

            unsupported => {
                log::error!("unsupported action: {unsupported:?}");
                return Err(Error::Unexpected(format!("unsupported action: {unsupported:?}"))).whence();
            }
        }
    }

    Ok(())
}

struct ProgressIo<T, P> {
    inner: T,
    progress: P,
    pending: u64,
}

impl<T, P: FnMut(u64)> ProgressIo<T, P> {
    const PROGRESS_INTERVAL_BYTES: u64 = 256 * 1024;

    fn new(inner: T, progress: P) -> Self { Self { inner, progress, pending: 0 } }

    fn finish(&mut self) {
        if self.pending > 0 {
            (self.progress)(self.pending);
            self.pending = 0;
        }
    }
}

impl<T: Write, P: FnMut(u64)> Write for ProgressIo<T, P> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.pending += written as u64;
        if self.pending >= Self::PROGRESS_INTERVAL_BYTES {
            self.finish();
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.finish();
        self.inner.flush()
    }
}

fn patch_to<F>(
    fs: &F,
    hash_file: &impl Fn(&str) -> whence::Result<[u8; 32], Error>,
    patch_file: &str,
    patch_source: &str,
    patch_dest: &str,
    base_version: &str,
    new_version: &str,
    estimator: &mut Estimator,
    progress: &mut impl FnMut(&Estimator),
) -> whence::Result<(), Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    let base_version =
        Version::parse(base_version).map_err(|_| Error::ParseVersion(base_version.to_string())).whence()?;
    let new_version =
        Version::parse(new_version).map_err(|_| Error::ParseVersion(new_version.to_string())).whence()?;
    let patch_file_path = format!("{RELEASE_DIR_PATH}/patch/{patch_file}");
    let patch_file_size: usize = fs
        .metadata(&patch_file_path, fs::Location::System)
        .whence()?
        .size
        .try_into()
        .map_err(|_| Error::Unexpected("patch file size too large".to_string()))
        .whence()?;
    let mut patch =
        fs.open_file(&patch_file_path, fs::Location::System, fs::OpenFlags::READ_ONLY).whence()?;

    let header = Header::read_from(&mut patch).whence()?;
    if header.old_version != base_version || header.new_version != new_version {
        return Err(Error::PatchVersionMismatch).whence();
    }

    let old_file_path = format!("{KEYOS_UPDATE_DIR_PATH}/{patch_source}");
    let new_file_path = format!("{KEYOS_UPDATE_DIR_PATH}/{patch_dest}");

    check_patch_file_integrity(fs, hash_file, &old_file_path, header.old_file_size, &header.old_file_hash)?;
    progress(estimator.record_hash(header.old_file_size));

    /// One merged run of source reads. Page aligned and a whole number of pages,
    /// so the filesystem lends it to the server rather than copying through the
    /// file's own buffer, and long enough that a run is worth merging at all.
    const SOURCE_SCRATCH: usize = 64 * 1024;

    let body_size = patch_file_size
        .checked_sub(Header::SIZE)
        .ok_or_else(|| Error::Unexpected("patch file is smaller than its header".into()))
        .whence()?;
    let mut patches = [
        patch,
        fs.open_file(&patch_file_path, fs::Location::System, fs::OpenFlags::READ_ONLY).whence()?,
        fs.open_file(&patch_file_path, fs::Location::System, fs::OpenFlags::READ_ONLY).whence()?,
    ];
    for patch in &mut patches[1..] {
        patch.seek(io::SeekFrom::Start(Header::SIZE as u64)).whence()?;
    }
    let patches = patches.map(|patch| BufReader::with_capacity(FILE_BUFFER_SIZE, patch));

    let mut scratch = DropDeallocate::new(
        xous::map_memory(None, None, SOURCE_SCRATCH, xous::MemoryFlags::W)
            .map_err(|_| Error::Unexpected("no memory for the source scratch".into()))
            .whence()?,
    );
    let tempfile_path = (patch_source == patch_dest).then(|| format!("{KEYOS_UPDATE_DIR_PATH}/tempfile"));
    if tempfile_path.is_none() {
        fs.ensure_parent_dir_exists(&new_file_path, fs::Location::System).whence()?;
    }
    let output_path = tempfile_path.as_deref().unwrap_or(&new_file_path);
    {
        let mut old_file =
            fs.open_file(&old_file_path, fs::Location::System, fs::OpenFlags::READ_ONLY).whence()?;
        let mut output_file =
            fs.open_file(output_path, fs::Location::System, fs::OpenFlags::CREATE).whence()?;
        output_file.truncate().whence()?;

        let output = ProgressIo::new(&mut output_file, |bytes| {
            progress(estimator.record(bytes, Estimator::PATCH_WEIGHT));
        });
        update_image::patch::apply(
            patches,
            body_size as u64,
            &mut old_file,
            scratch.as_slice_mut::<u8>(),
            output,
        )
        .map_err(|e| Error::Bsdiff(e.to_string()))
        .whence()?;
        progress(estimator.record_copy(body_size as u64));
        output_file.flush().whence()?;
    }
    if let Some(tempfile_path) = tempfile_path {
        fs.remove(&old_file_path, fs::Location::System).whence()?;
        fs.ensure_parent_dir_exists(&new_file_path, fs::Location::System).whence()?;
        fs.rename(&tempfile_path, &new_file_path, fs::Location::System).whence()?;
    }
    check_patch_file_integrity(fs, hash_file, &new_file_path, header.new_file_size, &header.new_file_hash)?;
    progress(estimator.record_hash(header.new_file_size));

    Ok(())
}

/// Checks whether the source/target files of the patching process are valid,
/// based on the data about them in the [Header].
fn check_patch_file_integrity<F>(
    fs: &F,
    hash_file: &impl Fn(&str) -> whence::Result<[u8; 32], Error>,
    file_path: &str,
    expected_file_size: u64,
    expected_file_hash: &[u8; 32],
) -> whence::Result<(), Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    let file_size = fs.metadata(file_path, fs::Location::System).whence()?.size;
    if file_size != expected_file_size {
        return Err(Error::PatchSizeMismatch {
            file_name: file_path.to_string(),
            expected_size: expected_file_size,
            actual_size: file_size,
        })
        .whence();
    }

    if &hash_file(file_path)? != expected_file_hash {
        return Err(Error::PatchHashMismatch).whence();
    }

    Ok(())
}

struct ReleaseFile<F> {
    file: F,
    offset: u64,
}

impl<F: Read> Read for ReleaseFile<F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> { self.file.read(buf) }
}

impl<F: Seek> Seek for ReleaseFile<F> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        let pos = match pos {
            io::SeekFrom::Start(pos) => self.file.seek(io::SeekFrom::Start(self.offset + pos))?,
            io::SeekFrom::End(pos) => self.file.seek(io::SeekFrom::End(pos))?,
            io::SeekFrom::Current(pos) => self.file.seek(io::SeekFrom::Current(pos))?,
        };
        Ok(pos.saturating_sub(self.offset))
    }
}

/// Opens a release file and seeks past the cosign2 header.
fn open_release_file<F>(fs: &F, release_path: &str) -> whence::Result<ReleaseFile<F::File>, Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    let mut file = fs.open_file(release_path, fs::Location::System, fs::OpenFlags::READ_ONLY).whence()?;
    let offset = cosign2::Header::DEFAULT_SIZE.try_into().unwrap();
    file.seek(io::SeekFrom::Start(offset)).whence()?;
    Ok(ReleaseFile { file, offset })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use fs::adapter::test_utils::FsTest;
    use sha2::Digest;
    use update_image::{Action, ReleaseManifest, Transaction};

    use super::*;

    /// The hosted tests have no crypto server to hash for them.
    fn hash_file(fs: &FsTest, path: &str) -> whence::Result<[u8; 32], Error> {
        let mut file = fs.open_file(path, fs::Location::System, fs::OpenFlags::READ_ONLY).whence()?;
        let mut data = Vec::new();
        file.read_to_end(&mut data).whence()?;
        Ok(sha2::Sha256::digest(&data).into())
    }

    fn create_updiff_patch(
        old_content: &[u8],
        new_content: &[u8],
        old_version: &str,
        new_version: &str,
    ) -> Vec<u8> {
        let mut updiff = Vec::new();
        update_image::patch::build(
            old_content,
            Version::parse(old_version).unwrap(),
            new_content,
            Version::parse(new_version).unwrap(),
            update_image::patch::Format::Zstd,
            &mut updiff,
        )
        .unwrap();
        updiff
    }

    /// creates a "signed" release tar file with the given manifest and patch files
    fn create_release_tar(manifest: &ReleaseManifest, patch_files: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let mut tar_buffer = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut tar_buffer);

            let manifest_json = serde_json::to_vec(manifest).unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_size(manifest_json.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, "manifest.json", &manifest_json[..]).unwrap();

            let mut dir_header = tar::Header::new_gnu();
            dir_header.set_entry_type(tar::EntryType::Directory);
            dir_header.set_size(0);
            dir_header.set_mode(0o755);
            dir_header.set_cksum();
            tar.append_data(&mut dir_header, "patch/", &[][..]).unwrap();

            let mut created_dirs = BTreeSet::from(["patch".to_string()]);
            for (name, content) in patch_files {
                let mut current_dir = String::new();
                for component in
                    name.split('/').filter(|component| !component.is_empty()).take_while(|_| true)
                {
                    if !current_dir.is_empty() {
                        current_dir.push('/');
                    }
                    current_dir.push_str(component);
                    if current_dir == name {
                        break;
                    }

                    let dir_path = format!("patch/{current_dir}/");
                    if created_dirs.insert(dir_path.clone()) {
                        let mut dir_header = tar::Header::new_gnu();
                        dir_header.set_entry_type(tar::EntryType::Directory);
                        dir_header.set_size(0);
                        dir_header.set_mode(0o755);
                        dir_header.set_cksum();
                        tar.append_data(&mut dir_header, &dir_path, &[][..]).unwrap();
                    }
                }

                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, format!("patch/{name}"), &content[..]).unwrap();
            }

            tar.finish().unwrap();
        }

        // prepend empty cosign2 header
        let mut signed_release = vec![0u8; cosign2::Header::DEFAULT_SIZE];
        signed_release.extend_from_slice(&tar_buffer);
        signed_release
    }

    fn guarded_releases(fs: &FsTest, paths: &[&str]) -> Vec<Release<<FsTest as FsAdapter>::File>> {
        paths
            .iter()
            .map(|path| Release {
                path: (*path).to_string(),
                guard: fs.open_file(path, fs::Location::System, fs::OpenFlags::READ_WRITE).unwrap(),
                firmware_timestamp: Some(1),
            })
            .collect()
    }

    fn apply_release(fs: &FsTest, path: &str) {
        let releases = guarded_releases(fs, &[path]);
        let mut estimator = analyze_update(fs, &releases, 0).unwrap();
        make_firmware_copy(fs, |bytes| {
            estimator.record_copy(bytes);
        })
        .unwrap();
        let hasher = |path: &str| hash_file(fs, path);
        apply_update(fs, &hasher, releases, &mut estimator, |_| {}).unwrap();
    }

    #[test]
    fn apply_update_happy_path() {
        let mut fs = FsTest::default();

        let old_app_content = b"Version 1.0.0";
        fs.write_file("keyos/app.bin", old_app_content, fs::Location::System);

        let old_lib_content = b"Library v1.0.0";
        fs.write_file("keyos/lib.so", old_lib_content, fs::Location::System);

        let old_config_content = b"config_key=old_value";
        fs.write_file("keyos/old_config.txt", old_config_content, fs::Location::System);

        let deprecated_content = b"This file will be deleted";
        fs.write_file("keyos/deprecated.txt", deprecated_content, fs::Location::System);

        let new_app_content = b"Version 1.1.0 - updated ";
        let new_lib_content = b"Library v1.1.0 with improvements";

        let app_patch = create_updiff_patch(old_app_content, new_app_content, "v1.0.0", "v1.1.0");
        let lib_patch = create_updiff_patch(old_lib_content, new_lib_content, "v1.0.0", "v1.1.0");

        let new_module_content = b"Brand new module for v1.1.0";

        let manifest = ReleaseManifest {
            label: "v1.1.0".to_string(),
            mandatory: false,
            reboot_required: false,
            date: "2025-01-01".to_string(),
            transactions: vec![Transaction::new(vec![
                Action::Patch {
                    patch_file: "app.bin.patch".into(),
                    patch_source: "app.bin".into(),
                    base_version: "v1.0.0".into(),
                    new_version: "v1.1.0".into(),
                },
                Action::PatchAdd {
                    patch_file: "lib.so.patch".into(),
                    patch_source: "lib.so".into(),
                    dest: "lib_new.so".into(),
                    base_version: "v1.0.0".into(),
                    new_version: "v1.1.0".into(),
                },
                Action::Add { source: "new_module.bin".into(), dest: "new_module.bin".into() },
                Action::Rename { source: "old_config.txt".into(), dest: "config.txt".into() },
                Action::Delete { path: "deprecated.txt".into() },
            ])],
        };

        let release_tar = create_release_tar(
            &manifest,
            vec![
                ("app.bin.patch", app_patch),
                ("lib.so.patch", lib_patch),
                ("new_module.bin", new_module_content.to_vec()),
            ],
        );

        let update_path = "updates/release_v1.1.0.tar";
        fs.write_file(update_path, &release_tar, fs::Location::System);

        let releases = guarded_releases(&fs, &[update_path]);
        let mut estimator = analyze_update(&fs, &releases, 0).unwrap();
        let mut percentages = vec![estimator.snapshot().completion_percentage];
        make_firmware_copy(&fs, |bytes| {
            estimator.record_copy(bytes);
            percentages.push(estimator.snapshot().completion_percentage);
        })
        .unwrap();
        let hasher = |path: &str| hash_file(&fs, path);
        let result = apply_update(&fs, &hasher, releases, &mut estimator, |estimator| {
            percentages.push(estimator.snapshot().completion_percentage);
        });

        assert!(result.is_ok(), "Update failed: {:?}", result.err());
        assert_eq!(result.unwrap(), UpdateOutcome::Done { firmware_timestamp: Some(1) });
        assert!(percentages.windows(2).all(|progress| progress[0] <= progress[1]));

        finalize_update(&mut fs).unwrap();

        // After finalize_update, /keyos.update becomes /keyos
        let patched_app = fs.read_file_contents("keyos/app.bin", fs::Location::System).unwrap();
        assert_eq!(patched_app, new_app_content, "Patch action failed");

        let new_lib = fs.read_file_contents("keyos/lib_new.so", fs::Location::System).unwrap();
        assert_eq!(new_lib, new_lib_content, "PatchAdd action failed");

        let original_lib = fs.read_file_contents("keyos/lib.so", fs::Location::System).unwrap();
        assert_eq!(original_lib, old_lib_content, "PatchAdd should not modify source");

        let added_module = fs.read_file_contents("keyos/new_module.bin", fs::Location::System).unwrap();
        assert_eq!(added_module, new_module_content, "Add action failed");

        let renamed_config = fs.read_file_contents("keyos/config.txt", fs::Location::System).unwrap();
        assert_eq!(renamed_config, old_config_content, "Rename action failed");
        assert!(
            fs.open_file("keyos/old_config.txt", fs::Location::System, fs::OpenFlags::READ_ONLY).is_err(),
            "Old file should not exist after rename"
        );

        assert!(
            fs.open_file("keyos/deprecated.txt", fs::Location::System, fs::OpenFlags::READ_ONLY).is_err(),
            "Delete action failed"
        );

        // Verify keyos.update no longer exists (was renamed to keyos)
        assert!(
            fs.open_file("keyos.update/app.bin", fs::Location::System, fs::OpenFlags::READ_ONLY).is_err(),
            "keyos.update should no longer exist after finalize"
        );

        assert!(fs.open_file("/release", fs::Location::System, fs::OpenFlags::READ_ONLY).is_err());
        assert!(fs
            .open_file("/updates/release_v1.1.0.tar", fs::Location::System, fs::OpenFlags::READ_ONLY)
            .is_err());
    }

    #[test]
    fn finalize_update_recovers_after_first_rename() {
        let mut fs = FsTest::default();
        fs.write_file("keyos/app.bin", b"old firmware", fs::Location::System);
        fs.write_file("keyos.update/app.bin", b"new firmware", fs::Location::System);

        fs.rename(KEYOS_DIR_PATH, KEYOS_OLD_DIR_PATH, fs::Location::System).unwrap();
        finalize_update(&mut fs).unwrap();

        assert_eq!(fs.read_file_contents(FIRMWARE_FILE_PATH, fs::Location::System).unwrap(), b"new firmware");
        assert!(matches!(
            fs.metadata(KEYOS_OLD_DIR_PATH, fs::Location::System),
            Err(fs::Error::FileNotFound)
        ));
    }

    #[test]
    fn finalize_update_is_idempotent_after_success() {
        let mut fs = FsTest::default();
        fs.write_file("keyos/app.bin", b"old firmware", fs::Location::System);
        fs.write_file("keyos.update/app.bin", b"new firmware", fs::Location::System);

        finalize_update(&mut fs).unwrap();
        finalize_update(&mut fs).unwrap();

        assert_eq!(fs.read_file_contents(FIRMWARE_FILE_PATH, fs::Location::System).unwrap(), b"new firmware");
    }

    #[test]
    fn analyze_releases() {
        let fs = FsTest::default();
        fs.write_file("keyos/app.bin", b"base firmware", fs::Location::System);

        let manifest1 = ReleaseManifest {
            label: "v1.1.0".to_string(),
            mandatory: false,
            reboot_required: true,
            date: "2025-01-01".to_string(),
            transactions: vec![Transaction::new(vec![
                Action::Add { source: "file1".into(), dest: "file1".into() },
                Action::Add { source: "file2".into(), dest: "file2".into() },
            ])],
        };

        let manifest2 = ReleaseManifest {
            label: "v1.2.0".to_string(),
            mandatory: false,
            reboot_required: true,
            date: "2025-01-02".to_string(),
            transactions: vec![Transaction::new(vec![
                Action::Add { source: "file3".into(), dest: "file3".into() },
                Action::Add { source: "file4".into(), dest: "file4".into() },
                Action::Add { source: "file5".into(), dest: "file5".into() },
            ])],
        };

        let release1_tar =
            create_release_tar(&manifest1, vec![("file1", vec![0; 10]), ("file2", vec![0; 20])]);
        let release2_tar = create_release_tar(
            &manifest2,
            vec![("file3", vec![0; 30]), ("file4", vec![0; 40]), ("file5", vec![0; 50])],
        );

        let path1 = "updates/release1.tar";
        let path2 = "updates/release2.tar";
        fs.write_file(path1, &release1_tar, fs::Location::System);
        fs.write_file(path2, &release2_tar, fs::Location::System);

        let releases = guarded_releases(&fs, &[path1, path2]);
        let estimator = analyze_update(&fs, &releases, 60).unwrap();

        assert_eq!(estimator.snapshot().completion_percentage, 60);
        assert!(estimator.total_work > 0);
    }

    #[test]
    fn add_creates_missing_parent_directories() {
        let mut fs = FsTest::default();
        fs.write_file("keyos/app.bin", b"base firmware", fs::Location::System);

        let new_app_content = b"new playground app";
        let manifest = ReleaseManifest {
            label: "v1.1.0".to_string(),
            mandatory: false,
            reboot_required: false,
            date: "2025-01-01".to_string(),
            transactions: vec![Transaction::new(vec![Action::Add {
                source: "keyos/apps/gui-app-playground/app.elf".into(),
                dest: "apps/gui-app-playground/app.elf".into(),
            }])],
        };

        let release_tar = create_release_tar(
            &manifest,
            vec![("keyos/apps/gui-app-playground/app.elf", new_app_content.to_vec())],
        );

        let update_path = "updates/release_v1.1.0.tar";
        fs.write_file(update_path, &release_tar, fs::Location::System);

        apply_release(&fs, update_path);
        finalize_update(&mut fs).unwrap();

        let added_app =
            fs.read_file_contents("keyos/apps/gui-app-playground/app.elf", fs::Location::System).unwrap();
        assert_eq!(added_app, new_app_content);
    }

    #[test]
    fn add_truncates_existing_destination_file() {
        let mut fs = FsTest::default();
        let old_content = b"this old file is longer";
        let new_content = b"short";

        fs.write_file("keyos/app.bin", b"base firmware", fs::Location::System);
        fs.write_file("keyos/common/config.bin", old_content, fs::Location::System);

        let manifest = ReleaseManifest {
            label: "v1.1.0".to_string(),
            mandatory: false,
            reboot_required: false,
            date: "2025-01-01".to_string(),
            transactions: vec![Transaction::new(vec![Action::Add {
                source: "keyos/common/config.bin".into(),
                dest: "common/config.bin".into(),
            }])],
        };

        let release_tar =
            create_release_tar(&manifest, vec![("keyos/common/config.bin", new_content.to_vec())]);

        let update_path = "updates/release_v1.1.0.tar";
        fs.write_file(update_path, &release_tar, fs::Location::System);

        apply_release(&fs, update_path);
        finalize_update(&mut fs).unwrap();

        let updated = fs.read_file_contents("keyos/common/config.bin", fs::Location::System).unwrap();
        assert_eq!(updated, new_content);
    }

    #[test]
    fn patch_add_creates_missing_parent_directories() {
        let mut fs = FsTest::default();
        let old_content = b"old library";
        let new_content = b"patched library";
        let patch = create_updiff_patch(old_content, new_content, "v1.0.0", "v1.1.0");

        fs.write_file("keyos/app.bin", b"base firmware", fs::Location::System);
        fs.write_file("keyos/lib.so", old_content, fs::Location::System);

        let manifest = ReleaseManifest {
            label: "v1.1.0".to_string(),
            mandatory: false,
            reboot_required: false,
            date: "2025-01-01".to_string(),
            transactions: vec![Transaction::new(vec![Action::PatchAdd {
                patch_file: "keyos/apps/gui-app-playground/app.elf.patch".into(),
                patch_source: "lib.so".into(),
                dest: "apps/gui-app-playground/app.elf".into(),
                base_version: "v1.0.0".into(),
                new_version: "v1.1.0".into(),
            }])],
        };

        let release_tar =
            create_release_tar(&manifest, vec![("keyos/apps/gui-app-playground/app.elf.patch", patch)]);

        let update_path = "updates/release_v1.1.0.tar";
        fs.write_file(update_path, &release_tar, fs::Location::System);

        apply_release(&fs, update_path);
        finalize_update(&mut fs).unwrap();

        let added_app =
            fs.read_file_contents("keyos/apps/gui-app-playground/app.elf", fs::Location::System).unwrap();
        assert_eq!(added_app, new_content);
    }

    #[test]
    fn rename_creates_missing_parent_directories() {
        let mut fs = FsTest::default();
        let old_content = b"rename me";

        fs.write_file("keyos/app.bin", b"base firmware", fs::Location::System);
        fs.write_file("keyos/old_config.txt", old_content, fs::Location::System);

        let manifest = ReleaseManifest {
            label: "v1.1.0".to_string(),
            mandatory: false,
            reboot_required: false,
            date: "2025-01-01".to_string(),
            transactions: vec![Transaction::new(vec![Action::Rename {
                source: "old_config.txt".into(),
                dest: "apps/gui-app-playground/config.txt".into(),
            }])],
        };

        let release_tar = create_release_tar(&manifest, vec![]);

        let update_path = "updates/release_v1.1.0.tar";
        fs.write_file(update_path, &release_tar, fs::Location::System);

        apply_release(&fs, update_path);
        finalize_update(&mut fs).unwrap();

        let renamed =
            fs.read_file_contents("keyos/apps/gui-app-playground/config.txt", fs::Location::System).unwrap();
        assert_eq!(renamed, old_content);
        assert!(
            fs.open_file("keyos/old_config.txt", fs::Location::System, fs::OpenFlags::READ_ONLY).is_err(),
            "Old file should not exist after rename"
        );
    }
}
