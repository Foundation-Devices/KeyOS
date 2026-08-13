// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Installing an app from an archive the user picked on local storage.
//!
//! The archive is read in one pass: its signed manifest comes first and decides everything that
//! follows, and nothing else in it is trusted. The bundle is written to a staging directory and
//! swapped in once the whole archive has been accepted, so a damaged archive leaves an installed
//! app untouched.

use std::io::Read;

use anyhow::Context;
use app_manager::{ArchiveLocation, InstallError, SIDELOADED_APPS_DIR};
use app_manifest::Manifest;
use fs::adapter::FsAdapter;
use fs::messages::{
    CloseDir, CloseFile, CreateDirMessage, Flush, GetMetadata, NextEntry, OpenDirMessage, OpenFileMessage,
    ReadFile, Remove, Rename, SeekFile, WriteFile,
};
use server::permission_set;
use xous::AppId;

use crate::registry::{AppRegistry, FLUX_EMULATOR_SERVER};

/// Appended to a bundle directory's name while its archive is being written; the registry never
/// scans such a name as an app.
const STAGING_SUFFIX: &str = ".part";

permission_set!(
    /// The fs messages an install sends. Narrower than `BasicFsPermissions`, which would oblige
    /// app-manager to hold permissions it has no reason to.
    pub(crate) trait InstallFsPermissions {
        OpenFileMessage, CloseFile, CreateDirMessage, OpenDirMessage, NextEntry, CloseDir, ReadFile,
        SeekFile, WriteFile, Flush, GetMetadata, Remove, Rename
    }
);

/// Everything an install needs of the filesystem, so a signature naming it stays one line.
pub(crate) trait InstallFs: FsAdapter<Permissions: InstallFsPermissions, File: 'static> {}

impl<F> InstallFs for F where F: FsAdapter<Permissions: InstallFsPermissions, File: 'static> {}

/// What an install consults about the apps already installed, so tests can supply the facts
/// without a registry built from a real scan.
pub(crate) trait InstalledApps {
    fn is_built_in(&self, app_id: &AppId) -> bool;
    fn is_running(&self, app_id: &AppId) -> bool;
    fn bundle_signer(&self, app_id: &AppId) -> Option<Option<[u8; 33]>>;
    fn flux_emulator_installed(&self) -> bool;
}

impl InstalledApps for AppRegistry {
    fn is_built_in(&self, app_id: &AppId) -> bool { AppRegistry::is_built_in(self, app_id) }

    fn is_running(&self, app_id: &AppId) -> bool { AppRegistry::is_running(self, app_id) }

    fn bundle_signer(&self, app_id: &AppId) -> Option<Option<[u8; 33]>> {
        AppRegistry::bundle_signer(self, app_id)
    }

    fn flux_emulator_installed(&self) -> bool { AppRegistry::flux_emulator_installed(self) }
}

/// Where an install put an app, so a caller that has to undo it does not have to guess.
pub(crate) struct Installed {
    pub(crate) app_id: AppId,
    pub(crate) app_dir: String,
}

/// Log why an install is being refused, and hand back the reason the user is shown. The variants
/// carry no payload, so this is the only place the detail exists.
fn install_error(error: InstallError, detail: String) -> InstallError {
    log::error!("install refused: {detail}");
    error
}

/// Unpack an archive into the sideload bundle directory its manifest calls for, replacing any
/// bundle already installed under that app id. The caller must rescan for it to enter the
/// registry.
pub(crate) fn install_archive(
    fs: &impl InstallFs,
    path: &str,
    location: ArchiveLocation,
    installed: &impl InstalledApps,
) -> Result<Installed, InstallError> {
    let file = fs
        .open_file(path, fs_location(location), fs::OpenFlags::READ_ONLY)
        .map_err(|e| install_error(InstallError::NotAnApp, format!("cannot open {path}: {e:?}")))?;
    let mut archive = tar::Archive::new(app_archive::decode(file));
    let mut entries = archive
        .entries()
        .map_err(|e| install_error(InstallError::NotAnApp, format!("{path} is not a tar archive: {e:?}")))?;

    let mut first = entries
        .next()
        .ok_or_else(|| install_error(InstallError::NotAnApp, format!("{path} holds no entries")))?
        .map_err(|e| install_error(InstallError::NotAnApp, format!("{path} is not a tar archive: {e:?}")))?;
    let (name, size) =
        entry_name_and_size(&first).map_err(|e| install_error(InstallError::NotAnApp, format!("{e:#}")))?;
    // The signed manifest decides everything that follows, so it has to arrive before any of it,
    // and its declared size is the archive's word: hold it to what the scan will accept later,
    // or an unverified header picks the size of an allocation.
    if name != app_archive::MANIFEST_FILE || size > crate::registry::MAX_MANIFEST_SIZE_BYTES {
        return Err(install_error(
            InstallError::NotAnApp,
            format!(
                "{path} starts with {name} of {size} bytes, not a plausible {}",
                app_archive::MANIFEST_FILE
            ),
        ));
    }

    let mut manifest_raw = Vec::new();
    first.by_ref().take(size).read_to_end(&mut manifest_raw).map_err(|e| {
        install_error(InstallError::NotAnApp, format!("cannot read the archive manifest: {e:?}"))
    })?;
    let (manifest, signer) = verify_manifest(&manifest_raw)
        .map_err(|e| install_error(InstallError::InvalidSignature, format!("{e:#}")))?;

    let app_id = AppId(manifest.app_id);
    let app_dir = format!("{SIDELOADED_APPS_DIR}/{}", hex::encode(app_id.0));
    // A Flux child is run by the emulator, so it installs only while the emulator itself is
    // installed; the emulator need not be running.
    if manifest.permissions.contains_key(FLUX_EMULATOR_SERVER) && !installed.flux_emulator_installed() {
        return Err(install_error(
            InstallError::FluxEmulatorMissing,
            format!("app 0x{} is a Flux child and the emulator is not installed", hex::encode(app_id.0)),
        ));
    }
    if installed.is_built_in(&app_id) {
        return Err(install_error(
            InstallError::BuiltInApp,
            format!("app 0x{} ships with the firmware", hex::encode(app_id.0)),
        ));
    }
    if installed.is_running(&app_id) {
        return Err(install_error(
            InstallError::AppRunning,
            format!("app 0x{} is running", hex::encode(app_id.0)),
        ));
    }

    // An update only when the same publisher signed it: permission grants and AppData are keyed by
    // app id alone, so anything else hands this id's approvals and stored data to a different app.
    if let Some(previous) = installed.bundle_signer(&app_id) {
        if previous != signer {
            return Err(install_error(
                InstallError::PublisherMismatch,
                format!("app 0x{} is installed from another publisher", hex::encode(app_id.0)),
            ));
        }
    } else {
        // A bundle the scan skipped is not in the registry but still owns this id's grants and
        // AppData, so its directory alone blocks the id.
        if fs.metadata(&app_dir, fs::Location::System).is_ok() {
            return Err(install_error(
                InstallError::PublisherMismatch,
                format!("app 0x{} is installed from another publisher", hex::encode(app_id.0)),
            ));
        }
    }

    log::info!("installing app 0x{} from {path} into {app_dir}", hex::encode(app_id.0));
    // A leftover staging directory must go first: the fs opens existing files without truncating,
    // so writing into one merges the two bundles, and an entry landing on a file left there would
    // read as an archive carrying the same name twice.
    let staging_dir = format!("{app_dir}{STAGING_SUFFIX}");
    fs.remove_if_exists(&staging_dir, fs::Location::System).map_err(InstallError::Fs)?;
    if let Err(e) = write_bundle(fs, &staging_dir, &manifest_raw, entries, &manifest) {
        let _ = fs.remove_if_exists(&staging_dir, fs::Location::System);
        return Err(e);
    }

    fs.remove_if_exists(&app_dir, fs::Location::System).map_err(InstallError::Fs)?;
    fs.rename(&staging_dir, &app_dir, fs::Location::System).map_err(|e| {
        install_error(InstallError::Internal, format!("cannot move {staging_dir} to {app_dir}: {e:?}"))
    })?;

    Ok(Installed { app_id, app_dir })
}

/// Write the manifest and every remaining entry into `app_dir`, then confirm the bundle holds
/// exactly the files its manifest hashes.
fn write_bundle<R: Read>(
    fs: &impl InstallFs,
    app_dir: &str,
    manifest_raw: &[u8],
    entries: tar::Entries<'_, R>,
    manifest: &Manifest,
) -> Result<(), InstallError> {
    write_file(fs, &format!("{app_dir}/{}", app_archive::MANIFEST_FILE), &mut &manifest_raw[..])?;

    for entry in entries {
        let mut entry = entry.map_err(|e| {
            install_error(InstallError::NotAnApp, format!("cannot read an archive entry: {e:?}"))
        })?;
        let (name, size) = entry_name_and_size(&entry)
            .map_err(|e| install_error(InstallError::NotAnApp, format!("{e:#}")))?;

        if !manifest.file_hashes.contains_key(&name) {
            return Err(install_error(
                InstallError::NotAnApp,
                format!("archive carries {name}, which its manifest does not hash"),
            ));
        }
        // The directory was emptied before this, and a create opens an existing file rather than
        // replacing it, so anything already here is a second entry about to land on the first.
        // Only the filesystem knows which names it cannot tell apart: it matches an entry by its
        // long name folded to upper case or by its 8.3 short name, so MANIFE~1.JSO reaches the
        // manifest every decision above was made from.
        let path = format!("{app_dir}/{name}");
        if fs.metadata(&path, fs::Location::System).is_ok() {
            return Err(install_error(InstallError::NotAnApp, format!("archive carries {name} twice")));
        }

        // Cap the copy at the size the entry declared: the header is part of the unverified
        // archive, so a lying one would otherwise write past what the checker accounted for.
        write_file(fs, &path, &mut entry.by_ref().take(size))?;
    }

    // A bundle missing a file the manifest hashes would otherwise install and only fail at launch,
    // where the hashes are checked, and one missing its binary would install however little the
    // manifest hashes: fileHashes defaults to empty.
    let required =
        std::iter::once(app_archive::ELF_FILE).chain(manifest.file_hashes.keys().map(String::as_str));
    for name in required {
        if fs.metadata(&format!("{app_dir}/{name}"), fs::Location::System).is_err() {
            return Err(install_error(InstallError::NotAnApp, format!("archive does not carry {name}")));
        }
    }

    Ok(())
}

/// Drop the staging directories a power loss left behind; nothing else reclaims them.
pub(crate) fn sweep_staged_bundles(fs: &impl InstallFs) {
    let Ok(dir) = fs.open_dir(SIDELOADED_APPS_DIR, fs::Location::System) else { return };
    // Collect before removing: fatfs iterates entries in place.
    let staged: Vec<String> = dir
        .filter_map(Result::ok)
        .filter(|entry| entry.is_dir && entry.name.ends_with(STAGING_SUFFIX))
        .map(|entry| entry.name)
        .collect();
    for name in staged {
        log::info!("removing {SIDELOADED_APPS_DIR}/{name}, left by an interrupted install");
        let _ = fs.remove_if_exists(&format!("{SIDELOADED_APPS_DIR}/{name}"), fs::Location::System);
    }
}

/// Verify an archive manifest as a sideloaded bundle, returning it with its signer (`None` for a
/// Foundation-signed archive).
fn verify_manifest(manifest_raw: &[u8]) -> anyhow::Result<(Manifest, Option<[u8; 33]>)> {
    let (manifest_json, signature) = crate::registry::verified_sideload_manifest(manifest_raw)?;
    let manifest =
        app_manifest::try_from_bytes(manifest_json).map_err(|e| anyhow::anyhow!("invalid manifest: {e}"))?;
    Ok((manifest, signature.signer()))
}

fn entry_name_and_size<R: Read>(entry: &tar::Entry<'_, R>) -> anyhow::Result<(String, u64)> {
    let path = entry.path().context("archive entry has an unreadable path")?;
    let name = path.to_str().context("archive entry path is not valid UTF-8")?.to_string();
    // Plain components only. os/fs drops empty, "." and ".." segments before resolving, so
    // `./manifest.json` and `manifest.json/` name the same file as `manifest.json` while reading
    // as different keys here: two entries would alias onto one file, and the name checks below,
    // which compare the raw string, would not see it.
    anyhow::ensure!(
        !name.is_empty()
            && !name.starts_with('/')
            && !name.ends_with('/')
            && name.split('/').all(|part| !part.is_empty() && part != "." && part != ".."),
        "archive entry {name} is not a plain bundle path"
    );
    // A symlink, hardlink or directory entry reports no size, so one named app.elf would install
    // a 0-byte binary. A bundle is regular files and nothing else.
    let entry_type = entry.header().entry_type();
    anyhow::ensure!(entry_type.is_file(), "archive entry {name} is a {entry_type:?}, not a file");
    Ok((name, entry.size()))
}

/// Write `data` to a bundle file, creating the directories above it.
///
/// Everything here is a write to System, which does not fail short of the filesystem itself being
/// broken, so there is nothing for the user to do about one but be told the install did not work.
fn write_file(fs: &impl InstallFs, path: &str, data: &mut impl Read) -> Result<(), InstallError> {
    use std::io::Write;

    let failed = |detail: String| install_error(InstallError::Internal, detail);

    fs.ensure_parent_dir_exists(path, fs::Location::System)
        .map_err(|e| failed(format!("cannot create the parent directory of {path}: {e:?}")))?;
    let mut file = fs
        .open_file(path, fs::Location::System, fs::OpenFlags::CREATE)
        .map_err(|e| failed(format!("cannot create {path}: {e:?}")))?;
    std::io::copy(data, &mut file).map_err(|e| failed(format!("cannot write {path}: {e:?}")))?;
    file.flush().map_err(|e| failed(format!("cannot flush {path}: {e:?}")))?;
    Ok(())
}

fn fs_location(location: ArchiveLocation) -> fs::Location {
    match location {
        ArchiveLocation::Internal => fs::Location::User,
        ArchiveLocation::Usb => fs::Location::Usb,
        ArchiveLocation::Airlock => fs::Location::Airlock,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::Write;

    use fs::adapter::test_utils::FsTest;

    use super::*;

    const APP_ID: &str = "0x00112233445566778899aabbccddeeff";
    const APP_DIR: &str = "keyos/sideloaded-apps/00112233445566778899aabbccddeeff";
    const ARCHIVE: &str = "example.app";
    const ELF: &[u8] = b"signed elf bytes";
    const ICON: &[u8] = b"icon pixels";

    #[derive(Default)]
    struct FakeInstalledApps {
        built_in: bool,
        running: bool,
        signer: Option<Option<[u8; 33]>>,
        emulator_installed: bool,
    }

    impl InstalledApps for FakeInstalledApps {
        fn is_built_in(&self, _app_id: &AppId) -> bool { self.built_in }

        fn is_running(&self, _app_id: &AppId) -> bool { self.running }

        fn bundle_signer(&self, _app_id: &AppId) -> Option<Option<[u8; 33]>> { self.signer }

        fn flux_emulator_installed(&self) -> bool { self.emulator_installed }
    }

    fn app_id() -> AppId { AppId(app_manifest::parse_app_id_bytes(APP_ID).unwrap()) }

    /// A hosted manifest: unsigned JSON, since `check_manifest_signature` verifies nothing off
    /// device. `hashes` is what the manifest claims the bundle holds.
    fn manifest_json(hashes: &[(&str, &[u8])], permissions: &[&str]) -> Vec<u8> {
        use sha2::{Digest, Sha256};

        let file_hashes: HashMap<&str, String> =
            hashes.iter().map(|(name, bytes)| (*name, hex::encode(Sha256::digest(bytes)))).collect();
        let permissions: HashMap<&str, Vec<&str>> =
            permissions.iter().map(|server| (*server, Vec::new())).collect();
        serde_json::to_vec(&serde_json::json!({
            "appName": { "en": "Example App" },
            "appId": APP_ID,
            "fileHashes": file_hashes,
            "permissions": permissions,
        }))
        .unwrap()
    }

    fn header(size: u64) -> tar::Header {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(size);
        header.set_mode(0o644);
        header.set_mtime(0);
        header
    }

    /// Build an archive from entries given in order, so a test can produce the malformed ones a
    /// packer would never write.
    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            builder.append_data(&mut header(data.len() as u64), name, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn valid_archive() -> Vec<u8> {
        let manifest = manifest_json(&[("app.elf", ELF), ("icon.bin", ICON), ("icon-dark.bin", ICON)], &[]);
        archive(&[
            ("manifest.json", &manifest),
            ("app.elf", ELF),
            ("icon.bin", ICON),
            ("icon-dark.bin", ICON),
        ])
    }

    /// Stage an archive on removable storage, gzipped as the format requires.
    fn staged(tar_bytes: &[u8]) -> FsTest {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(tar_bytes).unwrap();
        let bytes = encoder.finish().unwrap();

        let fs = FsTest::default();
        let mut file = fs.open_file(ARCHIVE, fs::Location::Usb, fs::OpenFlags::CREATE).unwrap();
        file.write_all(&bytes).unwrap();
        file.flush().unwrap();
        fs
    }

    fn install(fs: &FsTest, installed: &impl InstalledApps) -> Result<AppId, InstallError> {
        install_archive(fs, ARCHIVE, ArchiveLocation::Usb, installed).map(|done| done.app_id)
    }

    fn read(fs: &FsTest, path: &str) -> Option<Vec<u8>> {
        use std::io::Read as _;

        let mut file = fs.open_file(path, fs::Location::System, fs::OpenFlags::READ_ONLY).ok()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        Some(bytes)
    }

    #[test]
    fn installs_a_bundle_under_its_app_id() {
        let fs = staged(&valid_archive());

        let installed = install(&fs, &FakeInstalledApps::default()).unwrap();

        assert_eq!(installed, app_id());
        assert_eq!(read(&fs, &format!("{APP_DIR}/app.elf")).as_deref(), Some(ELF));
        assert_eq!(read(&fs, &format!("{APP_DIR}/icon.bin")).as_deref(), Some(ICON));
        assert_eq!(read(&fs, &format!("{APP_DIR}/icon-dark.bin")).as_deref(), Some(ICON));
        assert!(read(&fs, &format!("{APP_DIR}/manifest.json")).is_some());
    }

    #[test]
    fn installs_a_flux_app_when_the_emulator_is_installed() {
        let manifest = manifest_json(&[("app.elf", ELF)], &[FLUX_EMULATOR_SERVER]);
        let fs = staged(&archive(&[("manifest.json", &manifest), ("app.elf", ELF)]));
        let installed = FakeInstalledApps { emulator_installed: true, ..Default::default() };

        install(&fs, &installed).unwrap();

        assert_eq!(read(&fs, &format!("{APP_DIR}/app.elf")).as_deref(), Some(ELF));
    }

    #[test]
    fn a_flux_app_without_the_emulator_is_rejected() {
        let manifest = manifest_json(&[("app.elf", ELF)], &[FLUX_EMULATOR_SERVER]);
        let fs = staged(&archive(&[("manifest.json", &manifest), ("app.elf", ELF)]));

        let error = install(&fs, &FakeInstalledApps::default()).unwrap_err();

        assert!(matches!(error, InstallError::FluxEmulatorMissing));
    }

    #[test]
    fn an_archive_not_starting_with_the_manifest_is_rejected() {
        let manifest = manifest_json(&[("app.elf", ELF)], &[]);
        let fs = staged(&archive(&[("app.elf", ELF), ("manifest.json", &manifest)]));

        let error = install(&fs, &FakeInstalledApps::default()).unwrap_err();

        assert!(matches!(error, InstallError::NotAnApp));
    }

    /// A file added to the archive after signing: the manifest cannot vouch for it.
    /// The manifest is the archive's own, so it can list its own name; the create would then open
    /// the verified copy and write over it, leaving the scan a manifest this install never read.
    #[test]
    fn a_second_manifest_entry_cannot_replace_the_verified_one() {
        let manifest = manifest_json(&[("app.elf", ELF), ("manifest.json", b"forged")], &[]);
        let fs =
            staged(&archive(&[("manifest.json", &manifest), ("app.elf", ELF), ("manifest.json", b"forged")]));

        let error = install(&fs, &FakeInstalledApps::default()).unwrap_err();

        assert!(matches!(error, InstallError::NotAnApp));
    }

    /// os/fs drops "." and ".." segments, so `./manifest.json` resolves to the manifest this
    /// install verified: a forged copy under that name would overwrite it and the scan would
    /// build the registry entry from a document nothing here checked.
    #[test]
    fn an_entry_name_that_normalizes_onto_another_is_rejected() {
        let manifest = manifest_json(&[("app.elf", ELF), ("./manifest.json", b"forged")], &[]);
        let mut builder = tar::Builder::new(Vec::new());
        builder.append_data(&mut header(manifest.len() as u64), "manifest.json", &manifest[..]).unwrap();
        builder.append_data(&mut header(ELF.len() as u64), "app.elf", ELF).unwrap();
        // set_path drops a "." component, so the name goes into the header directly.
        let forged: &[u8] = b"forged";
        let mut aliased = header(forged.len() as u64);
        aliased.as_gnu_mut().unwrap().name[.."./manifest.json".len()].copy_from_slice(b"./manifest.json");
        aliased.set_cksum();
        builder.append(&aliased, forged).unwrap();
        let fs = staged(&builder.into_inner().unwrap());

        let error = install(&fs, &FakeInstalledApps::default()).unwrap_err();

        assert!(matches!(error, InstallError::NotAnApp));
    }

    /// fileHashes defaults to empty, so nothing else makes the bundle launchable.
    #[test]
    fn an_archive_without_a_binary_is_rejected() {
        let manifest = manifest_json(&[], &[]);
        let fs = staged(&archive(&[("manifest.json", &manifest)]));

        let error = install(&fs, &FakeInstalledApps::default()).unwrap_err();

        assert!(matches!(error, InstallError::NotAnApp));
        assert_eq!(read(&fs, &format!("{APP_DIR}/manifest.json")), None, "no bundle is left");
    }

    #[test]
    fn an_entry_the_manifest_does_not_hash_is_rejected() {
        let manifest = manifest_json(&[("app.elf", ELF)], &[]);
        let fs = staged(&archive(&[
            ("manifest.json", &manifest),
            ("app.elf", ELF),
            ("resources/extra.bin", b"unsigned"),
        ]));

        let error = install(&fs, &FakeInstalledApps::default()).unwrap_err();

        assert!(matches!(error, InstallError::NotAnApp));
        assert_eq!(read(&fs, &format!("{APP_DIR}/app.elf")), None, "a failed install leaves no bundle");
    }

    /// A truncated copy: the manifest hashes an icon the archive does not carry.
    #[test]
    fn an_archive_missing_a_hashed_file_is_rejected() {
        let manifest = manifest_json(&[("app.elf", ELF), ("icon.bin", ICON)], &[]);
        let fs = staged(&archive(&[("manifest.json", &manifest), ("app.elf", ELF)]));

        let error = install(&fs, &FakeInstalledApps::default()).unwrap_err();

        assert!(matches!(error, InstallError::NotAnApp));
        assert_eq!(read(&fs, &format!("{APP_DIR}/app.elf")), None, "a failed install leaves no bundle");
    }

    #[test]
    fn another_publishers_app_under_the_same_id_is_rejected() {
        let fs = staged(&valid_archive());
        let installed = FakeInstalledApps { signer: Some(Some([9u8; 33])), ..Default::default() };

        let error = install(&fs, &installed).unwrap_err();

        assert!(matches!(error, InstallError::PublisherMismatch));
        assert_eq!(read(&fs, &format!("{APP_DIR}/app.elf")), None, "the installed app is untouched");
    }

    /// A bundle the scan skipped is not in the registry but still owns the AppData and grants
    /// keyed by its app id, so its directory blocks the id.
    #[test]
    fn an_unregistered_bundle_blocks_the_id() {
        let fs = staged(&valid_archive());
        write_file(&fs, &format!("{APP_DIR}/app.elf"), &mut &b"unknown to the registry"[..]).unwrap();

        let error = install(&fs, &FakeInstalledApps::default()).unwrap_err();

        assert!(matches!(error, InstallError::PublisherMismatch));
        assert_eq!(
            read(&fs, &format!("{APP_DIR}/app.elf")).as_deref(),
            Some(&b"unknown to the registry"[..]),
            "the unregistered bundle is untouched"
        );
    }

    #[test]
    fn the_same_publisher_updates_in_place() {
        let fs = staged(&valid_archive());
        write_file(&fs, &format!("{APP_DIR}/app.elf"), &mut &b"the installed app"[..]).unwrap();
        // Hosted manifests are unsigned, so the signer both sides is None; what matters is that a
        // matching identity is an update rather than a takeover.
        let installed = FakeInstalledApps { signer: Some(None), ..Default::default() };

        install(&fs, &installed).unwrap();

        assert_eq!(read(&fs, &format!("{APP_DIR}/app.elf")).as_deref(), Some(ELF));
    }

    /// A directory entry named app.elf reports no size, so it would install a 0-byte binary.
    #[test]
    fn an_entry_that_is_not_a_regular_file_is_rejected() {
        let manifest = manifest_json(&[("app.elf", ELF)], &[]);
        let mut builder = tar::Builder::new(Vec::new());
        builder.append_data(&mut header(manifest.len() as u64), "manifest.json", &manifest[..]).unwrap();
        let mut dir = tar::Header::new_gnu();
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_size(0);
        dir.set_mode(0o755);
        dir.set_cksum();
        builder.append_data(&mut dir, "app.elf", &[][..]).unwrap();
        let fs = staged(&builder.into_inner().unwrap());

        let error = install(&fs, &FakeInstalledApps::default()).unwrap_err();

        assert!(matches!(error, InstallError::NotAnApp));
    }

    #[test]
    fn a_corrupt_update_leaves_the_installed_bundle_in_place() {
        // The archive is cut short after its manifest: the icon it hashes never arrives.
        let manifest = manifest_json(&[("app.elf", ELF), ("icon.bin", ICON)], &[]);
        let fs = staged(&archive(&[("manifest.json", &manifest), ("app.elf", ELF)]));
        write_file(&fs, &format!("{APP_DIR}/app.elf"), &mut &b"the installed app"[..]).unwrap();
        let installed = FakeInstalledApps { signer: Some(None), ..Default::default() };

        let error = install(&fs, &installed).unwrap_err();

        assert!(matches!(error, InstallError::NotAnApp));
        assert_eq!(read(&fs, &format!("{APP_DIR}/app.elf")).as_deref(), Some(&b"the installed app"[..]));
        assert_eq!(read(&fs, &format!("{APP_DIR}{STAGING_SUFFIX}/app.elf")), None);
    }

    /// The fs opens existing files without truncating, so a stale staging directory would merge
    /// into the new bundle: its extra files ride the rename into the installed app, and a shorter
    /// file keeps the previous attempt's tail, neither covered by the signed fileHashes the
    /// launch path checks.
    #[test]
    fn a_stale_staging_directory_does_not_leak_into_the_install() {
        let fs = staged(&valid_archive());
        let staging = format!("{APP_DIR}{STAGING_SUFFIX}");
        write_file(&fs, &format!("{staging}/stale.bin"), &mut &b"previous attempt"[..]).unwrap();
        let longer = [ELF, b" and a tail to keep"].concat();
        write_file(&fs, &format!("{staging}/app.elf"), &mut &longer[..]).unwrap();

        install(&fs, &FakeInstalledApps::default()).unwrap();

        assert_eq!(read(&fs, &format!("{APP_DIR}/stale.bin")), None);
        assert_eq!(read(&fs, &format!("{APP_DIR}/app.elf")).as_deref(), Some(ELF), "no tail survives");
    }

    #[test]
    fn a_sweep_drops_staged_bundles_and_leaves_the_rest() {
        let fs = staged(&valid_archive());
        write_file(&fs, &format!("{APP_DIR}/app.elf"), &mut &b"the installed app"[..]).unwrap();
        write_file(&fs, &format!("{APP_DIR}{STAGING_SUFFIX}/app.elf"), &mut &b"half an install"[..]).unwrap();

        sweep_staged_bundles(&fs);

        assert_eq!(read(&fs, &format!("{APP_DIR}{STAGING_SUFFIX}/app.elf")), None);
        assert_eq!(read(&fs, &format!("{APP_DIR}/app.elf")).as_deref(), Some(&b"the installed app"[..]));
    }
}
