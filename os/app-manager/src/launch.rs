// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(keyos))]
use std::{
    env, fs,
    path::{Path, PathBuf},
};

use app_manifest::Manifest;
use xous::{AppId, PID};

use crate::LaunchError;
#[cfg(keyos)]
use crate::{CryptoApi, FileSystem};

#[cfg(not(keyos))]
const HOSTED_APPS_DIR_ENV: &str = "FOUNDATION_SIMULATOR_APPS_DIR";

pub struct ListedApp<P> {
    pub elf_path: Option<P>,
    pub manifest_bytes: Vec<u8>,
    pub manifest: Manifest,
}

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

#[cfg(keyos)]
pub fn list_apps(apps_dir: &str) -> Result<Vec<ListedApp<String>>, LaunchError> {
    use std::io::Read;

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
                        let elf_file = format!("{app_dir_path}/app.elf");
                        apps.push(ListedApp { elf_path: Some(elf_file), manifest_bytes, manifest });
                    }
                }
            }
        }
    }

    Ok(apps)
}

#[cfg(not(keyos))]
pub fn list_apps(apps_dir: &str) -> Result<Vec<ListedApp<PathBuf>>, LaunchError> {
    let mut apps = vec![];
    if let Some(hosted_apps_dir) = hosted_apps_dir(apps_dir) {
        apps.extend(list_hosted_apps(&hosted_apps_dir));
    }

    Ok(apps)
}

#[cfg(not(keyos))]
fn hosted_apps_dir(apps_dir: &str) -> Option<PathBuf> {
    if let Some(path) = env::var_os(HOSTED_APPS_DIR_ENV).filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }

    let staged_apps_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("target")
        .join("hosted")
        .join(apps_dir.trim_start_matches('/'));
    if staged_apps_dir.is_dir() {
        return Some(staged_apps_dir);
    }

    let apps_dir = PathBuf::from(apps_dir);
    apps_dir.is_dir().then_some(apps_dir)
}

#[cfg(not(keyos))]
fn list_hosted_apps(apps_dir: &Path) -> Vec<ListedApp<PathBuf>> {
    let Ok(entries) = fs::read_dir(apps_dir) else {
        log::warn!("Failed to read hosted apps directory: {}", apps_dir.display());
        return vec![];
    };

    entries
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(error) => {
                log::warn!("Failed to read hosted app directory entry: {error:?}");
                None
            }
        })
        .filter(|entry| entry.file_type().map(|file_type| file_type.is_dir()).unwrap_or(false))
        .filter_map(|entry| read_hosted_app(&entry.path()))
        .collect()
}

#[cfg(not(keyos))]
fn read_hosted_app(app_dir: &Path) -> Option<ListedApp<PathBuf>> {
    let manifest_path = app_dir.join("manifest.json");
    let elf_path = app_dir.join("app.elf");

    let manifest_bytes = match fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            log::error!("Error reading hosted app manifest {}: {error:?}", manifest_path.display());
            return None;
        }
    };

    let manifest = match app_manifest::try_from_bytes(&manifest_bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            log::error!("Error parsing hosted app manifest {}: {error:?}", manifest_path.display());
            return None;
        }
    };

    if !elf_path.is_file() {
        log::warn!("Skipping hosted app without app.elf: {}", app_dir.display());
        return None;
    }

    Some(ListedApp { elf_path: Some(elf_path), manifest_bytes, manifest })
}

#[cfg(all(test, not(keyos)))]
mod tests {
    use std::{
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn list_apps_includes_hosted_apps_from_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = temp_dir("hosted-apps");
        let apps_dir = root.join("apps");
        let app_dir = apps_dir.join("Example");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(app_dir.join("app.elf"), b"hosted-app").unwrap();

        let app_id = "0x00112233445566778899aabbccddeeff";
        fs::write(
            app_dir.join("manifest.json"),
            format!(
                r#"{{
  "appName": {{"en": "Hosted Example"}},
  "appId": "{app_id}",
  "permissions": {{}}
}}"#
            ),
        )
        .unwrap();

        env::set_var(HOSTED_APPS_DIR_ENV, &apps_dir);
        env::remove_var("XOUS_PROCESS_KEY");

        let apps = list_apps("/keyos/apps").unwrap();

        env::remove_var(HOSTED_APPS_DIR_ENV);

        let app_id_bytes = app_manifest::parse_app_id_bytes(app_id).unwrap();
        let staged_app = apps.iter().find(|app| app.manifest.app_id == app_id_bytes).unwrap();
        assert_eq!(staged_app.elf_path.as_ref().unwrap(), &app_dir.join("app.elf"));

        cleanup(&root);
    }

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = env::temp_dir().join(format!("foundation-app-manager-{label}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn cleanup(path: &Path) { let _ = fs::remove_dir_all(path); }
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
