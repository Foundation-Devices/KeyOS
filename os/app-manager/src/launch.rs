// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(keyos))]
use std::{
    env,
    path::{Path, PathBuf},
};

#[cfg(keyos)]
use app_archive::ELF_FILE;
use xous::{AppId, PID};

#[cfg(keyos)]
use crate::CryptoApi;
#[cfg(keyos)]
use crate::FileSystem;
use crate::LaunchError;

#[cfg(not(keyos))]
const APP_ELF_ROOT_ENV: &str = "FOUNDATION_SIMULATOR_APP_ELF_ROOT";

#[cfg(not(keyos))]
pub fn launch_app(app_id: &AppId, elf_file: &Path) -> Result<PID, LaunchError> {
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

/// Verify the bundle, then launch the ELF. `third_party_signer` is the developer key a sideloaded
/// app.elf must be signed by; `None` requires an official signature.
#[cfg(keyos)]
pub fn verify_and_launch(
    fs: &FileSystem,
    app_id: &AppId,
    elf_path: &str,
    file_hashes: &std::collections::BTreeMap<String, [u8; app_manifest::FILE_HASH_BYTE_LEN]>,
    third_party_signer: Option<[u8; 33]>,
) -> Result<PID, LaunchError> {
    use std::io::Read;

    use app_manager::VerificationError;
    use xous::DropDeallocate;

    let (app_dir, _) = elf_path.rsplit_once('/').ok_or(LaunchError::InternalError)?;

    let crypto = CryptoApi::default();

    let metadata = fs.metadata(elf_path, fs::Location::System).map_err(|_| LaunchError::InternalError)?;
    let mut elf_file = fs
        .open_file(elf_path, fs::Location::System, fs::OpenFlags::READ_ONLY)
        .map_err(|_| LaunchError::InternalError)?;
    let size = metadata.size as usize;
    let size_aligned = size.next_multiple_of(4096);
    let mut elf_bytes =
        DropDeallocate::new(xous::map_memory(None, None, size_aligned, xous::MemoryFlags::W)?);
    elf_file.read_exact(&mut elf_bytes.as_slice_mut()[..size]).map_err(|_| LaunchError::InternalError)?;

    let header = match third_party_signer {
        None => fw_utils::hash::verify_cosign2_mem(
            &crypto,
            &elf_bytes.as_slice::<u8>()[..size],
            cfg!(feature = "production"),
        )
        .map_err(hash_error_to_launch_error)?,
        Some(signer) => {
            let header =
                fw_utils::hash::verify_cosign2_mem_third_party(&crypto, &elf_bytes.as_slice::<u8>()[..size])
                    .map_err(hash_error_to_launch_error)?;
            // Reject an elf signed by a different developer than the manifest, valid signature or not.
            if header.pubkey2() != signer {
                log::error!("app.elf signer does not match the manifest signer");
                return Err(LaunchError::Verification(VerificationError::Unverified));
            }
            header
        }
    };

    // A verified header already binds binary_hash to the payload, so cross-check it against the
    // manifest instead of re-hashing.
    let expected_elf = file_hashes.get(ELF_FILE).ok_or_else(|| {
        log::error!("manifest does not hash {ELF_FILE}");
        LaunchError::Verification(VerificationError::Unverified)
    })?;
    if expected_elf != header.binary_hash() {
        log::error!("manifest hash mismatch for {ELF_FILE}");
        return Err(LaunchError::Verification(VerificationError::Unverified));
    }

    for (rel, expected) in file_hashes {
        if rel == ELF_FILE {
            continue;
        }
        // Reject keys that could escape the bundle directory.
        let safe = !rel.starts_with('/')
            && !rel.contains('\\')
            && rel.split('/').all(|part| !part.is_empty() && part != "." && part != "..");
        if !safe {
            log::error!("manifest lists unsafe bundle path: {rel}");
            return Err(LaunchError::Verification(VerificationError::Unverified));
        }

        // Stream off disk so a large resource never lands in one buffer.
        let path = format!("{app_dir}/{rel}");
        let size =
            fs.metadata(&path, fs::Location::System).map_err(|_| LaunchError::InternalError)?.size as usize;
        let file = fs
            .open_file(&path, fs::Location::System, fs::OpenFlags::READ_ONLY)
            .map_err(|_| LaunchError::InternalError)?;
        let actual = fw_utils::hash::sha256_streaming(&crypto, size, file, |_| {})
            .map_err(|_| LaunchError::InternalError)?;

        if expected != &actual {
            log::error!("manifest hash mismatch for bundle file: {rel}");
            return Err(LaunchError::Verification(VerificationError::Unverified));
        }
    }

    // Drop the cosign2 header so the mapping starts at the ELF image.
    elf_bytes.as_slice_mut::<u8>().copy_within(cosign2::Header::DEFAULT_SIZE.., 0);

    let process_name = elf_path.split('/').rev().nth(1).ok_or(LaunchError::InternalError)?.to_string();
    let new_pid = xous::create_process(xous::ProcessArgs::new(*app_id, &process_name, *elf_bytes))?.0;
    elf_bytes.leak();

    Ok(new_pid)
}

#[cfg(keyos)]
fn hash_error_to_launch_error(err: fw_utils::hash::HashError) -> LaunchError {
    use app_manager::VerificationError;
    LaunchError::Verification(match err {
        fw_utils::hash::HashError::Cosign2Error(_) => VerificationError::Unverified,
        fw_utils::hash::HashError::MissingCosign2Header => VerificationError::MissingCosign2Header,
        _ => VerificationError::InternalError,
    })
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
