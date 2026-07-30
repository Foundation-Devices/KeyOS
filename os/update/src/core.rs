// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{self, Read, Seek, Write};

use fs::adapter::{BasicFsPermissions, FileAdapter, FsAdapter};
use qbsdiff::Bspatch;
use release_manifest::{Action, ReleaseManifest};
use sha2::Digest;
use update::messages::PatchProgress;
use update::Error;
use whence::WhenceExt;

/// Delta events that represent state changes during the update process
#[derive(Debug, Clone)]
pub enum UpdateEvent {
    ActionCompleted,
    PatchCompleted,
}

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
    Done,
    /// Some releases were applied, but a reboot is required before applying the remaining ones.
    Partial(Vec<String>),
}

/// extract update patch metadata without applying them.
/// DOES NOT verify signatures
pub fn analyze_patches<F>(fs: &F, release_paths: &[String]) -> whence::Result<Vec<PatchProgress>, Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    let mut patches = Vec::new();

    for release_path in release_paths {
        let file = open_release_file(fs, release_path)?;
        let file_size = file.metadata().whence()?.size;

        let mut archive = tar::Archive::new(file);
        let manifest = extract_manifest_from_tar(&mut archive)?;

        let total_actions = manifest.transactions.iter().fold(0, |acc, tx| acc + tx.actions().len() as u32);

        patches.push(PatchProgress {
            file_size,
            total_actions,
            completed_actions: 0,
            requires_reboot: manifest.reboot_required,
        });
    }

    Ok(patches)
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
    let mut completed_work = 0u64;

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
                let block_size = remaining.min(1024 * 1024);
                let written = src.copy_block_to(&mut dst, block_size).whence()?;
                // copy_block_to returns 0 at EOF; stop, or else an over-long entry.len loops forever.
                if written == 0 {
                    break;
                }
                remaining = remaining.saturating_sub(written);
                completed_work += written as u64;
                progress(completed_work);
            }
        }
    }

    Ok(())
}

pub fn measure_fw_size<F>(fs: &F) -> whence::Result<u64, Error>
where
    F: FsAdapter + Clone,
    F::Permissions: BasicFsPermissions,
{
    let walker = fs.walk_dir(KEYOS_DIR_PATH, fs::Location::System).whence()?;
    let mut work = 0;
    for entry in walker {
        let (_path, entry) = entry.whence()?;
        if !entry.is_dir {
            work += entry.len;
        }
    }
    Ok(work)
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
/// * `release_paths` - Paths to release files to apply
/// * `verify_signature` - Function to verify the signature of a release file (can be no-op if already
///   verified)
/// * `progress` - Callback invoked for all events
pub fn apply_update<F>(
    fs: &F,
    mut verify_signature: impl FnMut(&str) -> whence::Result<(), Error>,
    release_paths: Vec<String>,
    mut progress: impl FnMut(UpdateEvent),
) -> whence::Result<UpdateOutcome, Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    let mut release_paths = release_paths.into_iter();

    while let Some(release_path) = release_paths.next() {
        log::info!("applying release from {release_path}");

        // Verify if the caller requires it (may be no-op if already verified during analysis)
        verify_signature(&release_path)?;

        // Open the release file (reusing helper)
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
                io::copy(&mut entry, &mut dest_file).whence()?;
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
            execute_transaction(fs, tx.actions(), &mut progress)?;
        }

        log::info!("cleaning up update files");

        fs.remove(RELEASE_DIR_PATH, fs::Location::System).whence()?;
        drop(release_tar);
        fs.remove(&release_path, fs::Location::System).whence()?;

        log::info!("release applied successfully");

        progress(UpdateEvent::PatchCompleted);

        if manifest.reboot_required {
            let remaining_releases = release_paths.collect::<Vec<_>>();
            if !remaining_releases.is_empty() {
                log::info!("release requires a reboot, returning partial outcome");
                return Ok(UpdateOutcome::Partial(remaining_releases));
            } else {
                log::info!("last release requires reboot, returning done outcome");
                return Ok(UpdateOutcome::Done);
            }
        }
    }

    Ok(UpdateOutcome::Done)
}

/// Execute the actions from a single transaction on a copy of the OS firmware.
fn execute_transaction<F>(
    fs: &F,
    actions: &[Action],
    progress: &mut impl FnMut(UpdateEvent),
) -> whence::Result<(), Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    for action in actions {
        match action {
            Action::Patch { patch_file, patch_source, base_version, new_version } => {
                log::debug!("patch file {patch_source}");
                patch_to(fs, patch_file, patch_source, patch_source, base_version, new_version)?;
            }
            Action::PatchAdd { patch_file, patch_source, dest, base_version, new_version } => {
                log::debug!("patch-add file {patch_source}");
                patch_to(fs, patch_file, patch_source, dest, base_version, new_version)?;
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

                io::copy(&mut source_file, &mut dest_file).whence()?;
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

        // Emit event to update progress state
        progress(UpdateEvent::ActionCompleted);
    }

    Ok(())
}

fn patch_to<F>(
    fs: &F,
    patch_file: &str,
    patch_source: &str,
    patch_dest: &str,
    base_version: &str,
    new_version: &str,
) -> whence::Result<(), Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    let base_version =
        parse_version(base_version).map_err(|_| Error::ParseVersion(base_version.to_string())).whence()?;
    let new_version =
        parse_version(new_version).map_err(|_| Error::ParseVersion(new_version.to_string())).whence()?;
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

    // File cursor will be past the updiff header after this.
    let header = UpdiffHeader::read_from(&mut patch).whence()?;
    let versions_match = header.old_version == base_version && header.new_version == new_version;
    if !versions_match {
        return Err(Error::PatchVersionMismatch).whence();
    }

    let old_file_path = format!("{KEYOS_UPDATE_DIR_PATH}/{patch_source}");
    let new_file_path = format!("{KEYOS_UPDATE_DIR_PATH}/{patch_dest}");

    check_patch_file_integrity(fs, &old_file_path, header.old_file_size, &header.old_file_hash)?;

    let mut old_file =
        fs.open_file(&old_file_path, fs::Location::System, fs::OpenFlags::READ_ONLY).whence()?;

    // File cursor is past (uncompressed) updiff header, decompress the patch content and apply it.
    let mut patch_buf = Vec::with_capacity(patch_file_size - UpdiffHeader::SIZE);
    let mut decoder = bzip2::read::BzDecoder::new(patch);
    decoder.read_to_end(&mut patch_buf).whence()?;
    let bspatch = Bspatch::new(&patch_buf).map_err(|e| Error::Bsdiff(e.to_string())).whence()?;
    if patch_source == patch_dest {
        // Patch to a temporary file because we cannot have two files with the same name then
        // rename it to `patch_dest`.
        let tempfile_path = format!("{KEYOS_UPDATE_DIR_PATH}/tempfile");
        let mut tempfile =
            fs.open_file(&tempfile_path, fs::Location::System, fs::OpenFlags::CREATE).whence()?;
        tempfile.truncate().whence()?;
        bspatch.apply(&mut old_file, &mut tempfile).map_err(|e| Error::Bsdiff(e.to_string())).whence()?;
        tempfile.flush().whence()?;
        drop(old_file);
        drop(tempfile);
        fs.remove(&old_file_path, fs::Location::System).whence()?;
        fs.ensure_parent_dir_exists(&new_file_path, fs::Location::System).whence()?;
        fs.rename(&tempfile_path, &new_file_path, fs::Location::System).whence()?;
    } else {
        fs.ensure_parent_dir_exists(&new_file_path, fs::Location::System).whence()?;
        let mut new_file =
            fs.open_file(&new_file_path, fs::Location::System, fs::OpenFlags::CREATE).whence()?;
        new_file.truncate().whence()?;
        bspatch.apply(&mut old_file, &mut new_file).map_err(|e| Error::Bsdiff(e.to_string())).whence()?;
        new_file.flush().whence()?;
    };
    check_patch_file_integrity(fs, &new_file_path, header.new_file_size, &header.new_file_hash)?;

    Ok(())
}

/// Checks whether the source/target files of the patching process are valid,
/// based on the data about them in the [UpdiffHeader].
fn check_patch_file_integrity<F>(
    fs: &F,
    file_path: &str,
    expected_file_size: u64,
    expected_file_hash: &[u8; 32],
) -> whence::Result<(), Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    let file_size = fs.metadata(file_path, fs::Location::System).whence()?.size;
    let mut file = fs.open_file(file_path, fs::Location::System, fs::OpenFlags::READ_ONLY).whence()?;

    if file_size != expected_file_size {
        return Err(Error::PatchSizeMismatch {
            file_name: file_path.to_string(),
            expected_size: expected_file_size,
            actual_size: file_size,
        })
        .whence();
    }

    let mut hasher = sha2::Sha256::new();
    let mut block = [0; 4096];
    let file_hash: [u8; 32] = loop {
        let n = file.read(&mut block).whence()?;
        if n == 0 {
            break hasher.finalize().into();
        }
        hasher.update(&block[..n]);
    };
    if &file_hash != expected_file_hash {
        return Err(Error::PatchHashMismatch).whence();
    }

    file.seek(io::SeekFrom::Start(0)).whence()?;

    Ok(())
}

fn parse_version(s: &str) -> Result<[u8; 4], &'static str> {
    if !s.starts_with('v') {
        return Err("version must start with 'v'");
    }
    let s = &s[1..];
    let (major, rest) = s.split_once('.').ok_or("missing major version")?;
    let (minor, patch_and_beta) = rest.split_once('.').ok_or("missing minor version")?;
    let (patch, beta) = patch_and_beta.split_once('b').unwrap_or((patch_and_beta, ""));
    let major = major.parse().map_err(|_| "major version invalid or out of range")?;
    let minor = minor.parse().map_err(|_| "minor version invalid or out of range")?;
    let patch = patch.parse().map_err(|_| "patch version invalid or out of range")?;
    let beta = if beta.is_empty() {
        0xFF
    } else {
        let beta = beta.parse().map_err(|_| "beta version invalid or out of range")?;
        if beta == 0xFF {
            return Err("beta version may not be 0xFF");
        }
        beta
    };
    Ok([major, minor, patch, beta])
}

struct UpdiffHeader {
    old_version: [u8; 4],
    old_file_size: u64,
    old_file_hash: [u8; 32],
    new_version: [u8; 4],
    new_file_size: u64,
    new_file_hash: [u8; 32],
    _reserved: [u8; 128],
}

impl UpdiffHeader {
    const SIZE: usize = 4 + 8 + 32 + 4 + 8 + 32 + 128;

    /// Read the updiff header from the given reader. The file cursor will be
    /// advanced by the size of the header.
    fn read_from<T: Read>(reader: &mut T) -> io::Result<Self> {
        let mut old_version = [0; 4];
        reader.read_exact(&mut old_version)?;
        let mut old_file_size = [0; 8];
        reader.read_exact(&mut old_file_size)?;
        let mut old_file_hash = [0; 32];
        reader.read_exact(&mut old_file_hash)?;
        let mut new_version = [0; 4];
        reader.read_exact(&mut new_version)?;
        let mut new_file_size = [0; 8];
        reader.read_exact(&mut new_file_size)?;
        let mut new_file_hash = [0; 32];
        reader.read_exact(&mut new_file_hash)?;
        let mut reserved = [0; 128];
        reader.read_exact(&mut reserved)?;
        Ok(Self {
            old_version,
            old_file_size: u64::from_le_bytes(old_file_size),
            old_file_hash,
            new_version,
            new_file_size: u64::from_le_bytes(new_file_size),
            new_file_hash,
            _reserved: reserved,
        })
    }
}

/// Opens a release file and seeks past the cosign2 header.
fn open_release_file<F>(fs: &F, release_path: &str) -> whence::Result<F::File, Error>
where
    F: FsAdapter,
    F::Permissions: BasicFsPermissions,
{
    let mut file = fs.open_file(release_path, fs::Location::System, fs::OpenFlags::READ_ONLY).whence()?;

    // Skip cosign2 header
    let cosign2_header_size: u64 = cosign2::Header::DEFAULT_SIZE.try_into().unwrap();
    file.seek(io::SeekFrom::Start(cosign2_header_size)).whence()?;

    Ok(file)
}

/// Extracts just the manifest from a tar archive without extracting other files.
fn extract_manifest_from_tar<R: Read>(
    archive: &mut tar::Archive<R>,
) -> whence::Result<ReleaseManifest, Error> {
    let entries = archive.entries().whence()?;
    for entry in entries {
        let mut entry = entry.whence()?;
        let entry_path = entry.path().whence()?.to_str().ok_or(Error::InvalidManifest).whence()?.to_string();

        if entry_path == "manifest.json" {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).whence()?;
            let manifest = serde_json::from_slice::<ReleaseManifest>(&buf).map_err(|e| {
                log::error!("failed to parse manifest: {e:?}");
                Error::InvalidManifest
            })?;
            return Ok(manifest);
        }
    }

    Err(Error::InvalidManifest).whence()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io::Write;

    use fs::adapter::test_utils::FsTest;
    use qbsdiff::Bsdiff;
    use release_manifest::{Action, ReleaseManifest, Transaction};
    use sha2::Digest;

    use super::*;

    /// create an updiff patch file
    fn create_updiff_patch(
        old_content: &[u8],
        new_content: &[u8],
        old_version: &str,
        new_version: &str,
    ) -> Vec<u8> {
        let mut bsdiff_patch = Vec::new();
        Bsdiff::new(old_content, new_content).compare(&mut bsdiff_patch).unwrap();

        let mut compressed_patch = Vec::new();
        let mut encoder = bzip2::write::BzEncoder::new(&mut compressed_patch, bzip2::Compression::best());
        encoder.write_all(&bsdiff_patch).unwrap();
        encoder.finish().unwrap();

        let old_hash: [u8; 32] = sha2::Sha256::digest(old_content).into();
        let new_hash: [u8; 32] = sha2::Sha256::digest(new_content).into();

        let mut updiff = Vec::with_capacity(UpdiffHeader::SIZE + compressed_patch.len());
        updiff.extend_from_slice(&parse_version(old_version).unwrap());
        updiff.extend_from_slice(&(old_content.len() as u64).to_le_bytes());
        updiff.extend_from_slice(&old_hash);
        updiff.extend_from_slice(&parse_version(new_version).unwrap());
        updiff.extend_from_slice(&(new_content.len() as u64).to_le_bytes());
        updiff.extend_from_slice(&new_hash);
        updiff.extend_from_slice(&[0u8; 128]); // reserved
        updiff.extend_from_slice(&compressed_patch);

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

        make_firmware_copy(&fs, |_| ()).unwrap();

        let mut action_count = 0;
        let mut patch_completed_count = 0;

        let result = apply_update(
            &fs,
            |_path| Ok(()), // noop
            vec![update_path.to_string()],
            |event| match event {
                UpdateEvent::ActionCompleted => action_count += 1,
                UpdateEvent::PatchCompleted => patch_completed_count += 1,
            },
        );

        assert!(result.is_ok(), "Update failed: {:?}", result.err());
        assert_eq!(result.unwrap(), UpdateOutcome::Done);

        assert_eq!(action_count, 5);
        assert_eq!(patch_completed_count, 1);

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

        let manifest1 = ReleaseManifest {
            label: "v1.1.0".to_string(),
            mandatory: false,
            reboot_required: false,
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

        let release1_tar = create_release_tar(&manifest1, vec![]);
        let release2_tar = create_release_tar(&manifest2, vec![]);

        let path1 = "updates/release1.tar";
        let path2 = "updates/release2.tar";
        fs.write_file(path1, &release1_tar, fs::Location::System);
        fs.write_file(path2, &release2_tar, fs::Location::System);

        let patches = analyze_patches(&fs, &[path1.to_string(), path2.to_string()]).unwrap();

        assert_eq!(patches.len(), 2);

        assert_eq!(patches[0].file_size, release1_tar.len() as u64);
        assert_eq!(patches[0].total_actions, 2);
        assert_eq!(patches[0].completed_actions, 0);
        assert_eq!(patches[0].requires_reboot, false);

        assert_eq!(patches[1].file_size, release2_tar.len() as u64);
        assert_eq!(patches[1].total_actions, 3);
        assert_eq!(patches[1].completed_actions, 0);
        assert_eq!(patches[1].requires_reboot, true);
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

        make_firmware_copy(&fs, |_| ()).unwrap();
        apply_update(&fs, |_path| Ok(()), vec![update_path.to_string()], |_| {}).unwrap();
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

        make_firmware_copy(&fs, |_| ()).unwrap();
        apply_update(&fs, |_path| Ok(()), vec![update_path.to_string()], |_| {}).unwrap();
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

        make_firmware_copy(&fs, |_| ()).unwrap();
        apply_update(&fs, |_path| Ok(()), vec![update_path.to_string()], |_| {}).unwrap();
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

        make_firmware_copy(&fs, |_| ()).unwrap();
        apply_update(&fs, |_path| Ok(()), vec![update_path.to_string()], |_| {}).unwrap();
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
