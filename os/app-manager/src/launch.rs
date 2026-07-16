// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(keyos))]
use std::path::Path;

use app_manifest::Manifest;
use xous::{AppId, PID};

use crate::LaunchError;

#[cfg(keyos)]
crypto::use_api!();
#[cfg(keyos)]
fs::use_api!();

#[cfg(not(keyos))]
pub fn launch_app(app_id: &AppId, elf_file: &Path) -> Result<PID, LaunchError> {
    if let Some(pid) = xous::app_id_to_pid(app_id)? {
        log::debug!("App {:02x?} already running with pid {}", app_id.0, pid);

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
pub fn launch_app(app_id: &AppId, elf_path: &str) -> Result<PID, LaunchError> {
    use std::io::Read;

    use xous::{create_process, DropDeallocate, ProcessArgs};

    if let Some(pid) = xous::app_id_to_pid(app_id)? {
        log::debug!("App {:02x?} already running with pid {}", app_id, pid);
        return Ok(pid);
    }

    log::trace!("Launching elf file: {}", elf_path);

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
    fw_utils::hash::verify_cosign2_mem(
        &CryptoApi::default(),
        &elf_bytes.as_slice::<u8>()[..size],
        cfg!(feature = "production"),
    )
    .inspect_err(|e| log::error!("failed to verify app integrity {e:?}"))
    .map_err(|e| hash_error_to_launch_error(e))?;

    // Skip over the cosign2 header so that the memory begins with the ELF data
    elf_bytes.as_slice_mut::<u8>().copy_within(cosign2::Header::DEFAULT_SIZE.., 0);

    log::trace!("Launching the elf file");
    let dir_name = elf_path.split('/').rev().nth(1).ok_or(LaunchError::InternalError)?;
    log::trace!("process name: {}", dir_name);
    log::trace!("app id: {:?}", app_id);
    let new_pid = create_process(ProcessArgs::new(*app_id, dir_name, *elf_bytes))?.0;
    elf_bytes.leak();

    Ok(new_pid)
}

#[cfg(keyos)]
pub fn list_apps(apps_dir: &str) -> Result<Vec<(Option<String>, Manifest)>, LaunchError> {
    use std::io::Read;

    let names = server::xous_names::XousNames::new().unwrap();

    let fs = FileSystem::default();
    let mut apps = vec![];

    log::trace!("Listing FS apps...");
    let apps_dir_path = apps_dir.to_string();
    log::trace!("apps_dir_path: {}", apps_dir_path);

    let dir = fs.open_dir(apps_dir_path.clone(), fs::Location::System).map_err(fs_error_to_launch_error)?;

    while let Ok(Some(entry)) = dir.next_entry() {
        log::trace!("entry: {:?}", entry);

        if entry.is_dir {
            if entry.name == "." || entry.name == ".." {
                continue;
            }

            let app_dir_path = format!("{apps_dir_path}/{}", entry.name);
            log::trace!("app_dir_path: {}", app_dir_path);

            let manifest_path = format!("{app_dir_path}/manifest.json");
            log::trace!("manifest_path: {}", manifest_path);

            log::debug!("Reading manifest file: {}", manifest_path);
            if let Ok(mut manifest_file) = fs
                .open_file(
                    manifest_path,
                    fs::Location::System,
                    fs::OpenFlags { read: true, write: false, create: false },
                )
                .map_err(|e| log::error!("Error opening manifest file: {:?}", e))
            {
                log::debug!("Reading manifest file");
                let mut manifest_bytes = vec![];
                if manifest_file
                    .read_to_end(&mut manifest_bytes)
                    .map_err(|e| log::error!("Error reading manifest file: {:?}", e))
                    .is_ok()
                {
                    log::debug!("Deserializing manifest");
                    if let Ok(manifest) = app_manifest::try_from_bytes(&manifest_bytes)
                        .map_err(|e| log::error!("Error parsing the app manifest: {:?}", e))
                    {
                        if let Err(e) = names.add_manifest(&manifest_bytes) {
                            log::error!(
                                "Could not send the manifest of {app_dir_path} to the name server: {e:?}"
                            );
                        }
                        let elf_file = format!("{app_dir_path}/app.elf");
                        apps.push((Some(elf_file), manifest));
                    }
                }
            }
        }
    }

    Ok(apps)
}

#[cfg(not(keyos))]
pub fn list_apps(_apps_dir: &str) -> Result<Vec<(Option<std::path::PathBuf>, Manifest)>, LaunchError> {
    let mut apps = vec![];
    for manifest_json in system_manifests::SYSTEM_MANIFESTS {
        match app_manifest::try_from_bytes(manifest_json.as_bytes()) {
            Ok(manifest) => {
                apps.push((None, manifest));
            }
            Err(e) => {
                log::error!("Failed to parse hosted SYSTEM_MANIFEST entry: {:?}", e);
            }
        }
    }

    Ok(apps)
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

#[cfg(keyos)]
pub fn fs_error_to_launch_error(err: fs::Error) -> app_manager::LaunchError {
    if let fs::Error::OutOfMemory = err {
        app_manager::LaunchError::OutOfMemory
    } else {
        app_manager::LaunchError::InternalError
    }
}
