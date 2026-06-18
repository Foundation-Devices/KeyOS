// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(keyos))]
use std::{
    env,
    path::{Path, PathBuf},
};

use xous::{AppId, PID};

#[cfg(keyos)]
use crate::CryptoApi;
#[cfg(keyos)]
use crate::FileSystem;
use crate::LaunchError;

#[cfg(not(keyos))]
const APP_ELF_ROOT_ENV: &str = "FOUNDATION_SIMULATOR_APP_ELF_ROOT";

#[cfg(not(keyos))]
pub fn launch_app(
    app_id: &AppId,
    elf_file: &Path,
    _trusted_third_party_pubkeys: &[[u8; 33]],
    _check_trust: bool,
) -> Result<PID, LaunchError> {
    if let Some(pid) = xous::app_id_to_pid(app_id)? {
        log::debug!("App 0x{app_id} already running with pid {pid}");

        return Ok(pid);
    }

    let app_name = app_name_from_path(elf_file).map_err(|_| LaunchError::InternalError)?;
    let args =
        xous::ProcessArgs::new(*app_id, &app_name, elf_file.to_str().ok_or(LaunchError::InternalError)?);
    let (pid, _) = xous::create_process(args)?;
    log::info!("launched app {} with pid {}", app_name, pid);

    Ok(pid)
}

#[cfg(not(keyos))]
fn app_name_from_path(path: &Path) -> anyhow::Result<String> {
    let app_name = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("no parent"))?
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("no filename"))?
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("can't convert to str"))?;
    Ok(app_name.to_string())
}

#[cfg(keyos)]
pub struct VerifiedApp {
    app_id: AppId,
    process_name: String,
    elf_bytes: xous::DropDeallocate,
}

#[cfg(keyos)]
impl VerifiedApp {
    pub fn launch(self) -> Result<PID, LaunchError> {
        use xous::{create_process, ProcessArgs};

        let Self { app_id, process_name, elf_bytes } = self;

        log::trace!("Launching the elf file");
        log::trace!("process name: {}", process_name);
        log::trace!("app id: {:?}", app_id);
        let new_pid = create_process(ProcessArgs::new(app_id, &process_name, *elf_bytes))?.0;
        elf_bytes.leak();

        Ok(new_pid)
    }
}

#[cfg(keyos)]
pub fn verify_app(
    app_id: &AppId,
    elf_path: &str,
    trusted_third_party_pubkeys: &[[u8; 33]],
    check_trust: bool,
) -> Result<VerifiedApp, LaunchError> {
    use std::io::Read;

    use xous::DropDeallocate;

    log::trace!("Verifying elf file: {}", elf_path);

    let fs = FileSystem::default();
    let metadata = fs.metadata(elf_path, fs::Location::System).map_err(|_| LaunchError::InternalError)?;
    log::trace!("ELF file metadata: {:?}", metadata);

    let mut elf_file = fs
        .open_file(elf_path, fs::Location::System, fs::OpenFlags { read: true, write: false, create: false })
        .map_err(|_| LaunchError::InternalError)?;
    let size = metadata.size as usize;
    let size_aligned = size.next_multiple_of(4096);

    log::trace!("Allocating {} ({size_aligned} bytes aligned) buffer", metadata.size);
    let mut elf_bytes =
        DropDeallocate::new(xous::map_memory(None, None, size_aligned, xous::MemoryFlags::W)?);

    // Read the entire file into memory
    log::trace!("Reading {} ({size_aligned} aligned) bytes from the file", size);
    elf_file.read_exact(&mut elf_bytes.as_slice_mut()[..size]).map_err(|_| LaunchError::InternalError)?;

    // Verify the app integrity
    fw_utils::hash::verify_cosign2_mem_with_third_party_keys(
        &CryptoApi::default(),
        &elf_bytes.as_slice::<u8>()[..size],
        trusted_third_party_pubkeys,
        check_trust,
    )
    .inspect_err(|e| log::error!("failed to verify app integrity {e:?}"))
    .map_err(|e| hash_error_to_launch_error(e))?;

    // Skip over the cosign2 header so that the memory begins with the ELF data
    elf_bytes.as_slice_mut::<u8>().copy_within(cosign2::Header::DEFAULT_SIZE.., 0);

    let process_name = elf_path.split('/').rev().nth(1).ok_or(LaunchError::InternalError)?.to_string();
    Ok(VerifiedApp { app_id: *app_id, process_name, elf_bytes })
}

/// Host mirror of the image's `/keyos` directory, holding the launchable app
/// binaries (`apps/<name>/app.elf`, `sideloaded-apps/<hex>/app.elf`). The
/// simulator launcher exports the env; without it we resolve against the current
/// directory (a baked build path would not be relocatable). See
/// [`warn_if_app_elf_root_unset`] for the startup diagnostic.
#[cfg(not(keyos))]
fn app_elf_root() -> PathBuf {
    env::var_os(APP_ELF_ROOT_ENV)
        .filter(|path| !path.is_empty())
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// Warn if the simulator launcher didn't export the app-elf root, so host app
/// launches silently resolving against the current dir are diagnosable.
#[cfg(not(keyos))]
pub(crate) fn warn_if_app_elf_root_unset() {
    if env::var_os(APP_ELF_ROOT_ENV).filter(|path| !path.is_empty()).is_none() {
        log::warn!("{APP_ELF_ROOT_ENV} not set; resolving host app binaries relative to the current dir");
    }
}

/// Map a bundle's fs `app.elf` path (`/keyos/apps/<name>/app.elf`) to the host
/// binary the simulator execs. The root mirrors the image's `/keyos` dir, so the
/// path resolves under it once that segment is dropped.
#[cfg(not(keyos))]
pub fn host_elf_path(fs_elf_path: &str) -> Option<PathBuf> {
    let rel = fs_elf_path.trim_start_matches('/').strip_prefix("keyos/")?;
    let elf_path = app_elf_root().join(rel);
    elf_path.is_file().then_some(elf_path)
}

#[cfg(keyos)]
pub fn hash_error_to_launch_error(err: fw_utils::hash::HashError) -> app_manager::LaunchError {
    use app_manager::VerificationError;
    app_manager::LaunchError::Verification(match err {
        fw_utils::hash::HashError::Cosign2Error(_) => VerificationError::Unverified,
        fw_utils::hash::HashError::MissingCosign2Header => VerificationError::MissingCosign2Header,
        _ => VerificationError::InternalError,
    })
}
