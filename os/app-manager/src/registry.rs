// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet, HashMap};

use app_archive::ELF_FILE;
use app_manager::{
    AppQrMatchRules, CompatibilityError, IconVariant, InstalledAppInfo, InstalledAppPermissionGroup,
    InstalledAppPermissionSubgroup, LaunchError, PermissionRequestInfo, PermissionRequestInfoResult,
    ThirdPartyCertificateInfo, SIDELOADED_APPS_DIR,
};
use app_manifest::{Locale, Manifest, RequiredSignature};
use fs::messages::AppResourcesRoot;
use log::error;
use semver::Version;
use serde_json::to_vec;
use xous::{AppId, PID};

use crate::{
    permission_catalog::{self, ServerPermissionCache},
    permission_grants::{PermissionGrantState, PermissionGrantStore},
    FileSystem,
};

// The key this firmware build was signed with, emitted by build.rs from the repo cosign2.toml.
#[cfg(all(keyos, not(feature = "production")))]
include!(concat!(env!("OUT_DIR"), "/dev_signer.rs"));

const BUILT_IN_APPS_DIR: &str = "/keyos/apps";
/// The server a Flux child declares to reach the emulator; declaring it is what tags an app
/// as a Flux child, and providing it is what tags an app as the emulator.
pub(crate) const FLUX_EMULATOR_SERVER: &str = "os/gui-app-emu-flux";
// Each icon file holds one 110x110 RGBA archived RawImage (~47 KiB of pixels)
// plus rkyv header/alignment overhead. Leave margin for format drift and
// oversized sources.
const MAX_APP_ICON_SIZE_BYTES: u64 = 300 * 1024;
pub(crate) const MAX_MANIFEST_SIZE_BYTES: u64 = 128 * 1024;
/// Filename of a sideloaded app's icon within its bundle, next to `app.elf`. The SDK writes it
/// here, mirroring this name with its own constant (it can't depend on this crate). Built-in
/// icons instead live in CommonAssets (`app-icons/<app-id>.bin`).
const BUNDLED_ICON_FILE: &str = "icon.bin";
/// Filename of the dark-theme icon beside [`BUNDLED_ICON_FILE`], staged only by apps that ship
/// one; the light icon serves both themes otherwise. Built-in dark icons are the
/// `app-icons/<app-id>-dark.bin` sibling in CommonAssets.
const BUNDLED_DARK_ICON_FILE: &str = "icon-dark.bin";

/// How a declared message is available to an app once its signature is taken into account:
/// granted automatically, granted through the user's subgroup decision, or not reachable at
/// all (the signature requirement isn't met, or the message is not user-facing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageAvailability {
    AutoAllow,
    ApprovalBased,
    Unavailable,
}

#[derive(Debug, Clone)]
pub(crate) struct AppInfo {
    id: AppId,
    /// Filesystem path of the app's bundle directory (e.g. `/keyos/apps/<name>`).
    /// Icon, manifest, and resources are read from it via `fs`; the launchable
    /// `app.elf` is derived as `<app_dir>/app.elf`.
    app_dir: Option<String>,
    manifest: Manifest,
    /// The verified manifest JSON as scanned, handed verbatim to the name server at launch.
    manifest_bytes: Vec<u8>,
    /// The root the bundle was scanned from: built-ins ship in the firmware image, sideloads
    /// install under the sideload root and are removable.
    root: AppResourcesRoot,
    /// The signature class the manifest carried at scan.
    signature: AppSignature,
    is_flux: bool,
    /// Size of the bundle's elf, 0 when unknown.
    binary_size: u64,
}

/// The signature class a manifest carried at scan, derived from which key signed it:
/// Foundation for the official signature (or, on a development build, the developer signature
/// of the key this firmware was built with); ThirdParty carries the developer key that signed
/// the bundle, so trust can be decided later against the cert store (`None` on hosted builds,
/// which sign nothing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppSignature {
    Foundation,
    ThirdParty(Option<[u8; 33]>),
}

impl AppSignature {
    /// The signature level this class satisfies when a message's requirement is checked.
    fn required(self) -> RequiredSignature {
        match self {
            AppSignature::Foundation => RequiredSignature::Foundation,
            AppSignature::ThirdParty(_) => RequiredSignature::ThirdParty,
        }
    }

    /// The developer key that signed a third-party bundle; `None` for Foundation-signed apps
    /// and on hosted builds.
    pub(crate) fn signer(self) -> Option<[u8; 33]> {
        match self {
            AppSignature::Foundation => None,
            AppSignature::ThirdParty(signer) => signer,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppResourcesLocation {
    pub(crate) root: AppResourcesRoot,
    pub(crate) app_dir: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RunningAppInfo {
    pub(crate) info: AppInfo,
    pub(crate) launched_by: PID,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AppRegistryDiff {
    pub(crate) installed: Vec<AppId>,
    pub(crate) removed: Vec<AppId>,
}

impl AppRegistryDiff {
    /// Diff the previous and current scan results. Ids are sorted by their byte value so event
    /// emission order is stable across runs instead of following `HashMap`'s iteration order.
    fn new(before: &HashMap<AppId, AppInfo>, after: &HashMap<AppId, AppInfo>) -> Self {
        let mut installed: Vec<AppId> = after
            .iter()
            .filter(|(id, info)| match before.get(id) {
                None => true,
                Some(previous) => previous.manifest_bytes != info.manifest_bytes,
            })
            .map(|(id, _)| *id)
            .collect();
        let mut removed: Vec<AppId> = before.keys().filter(|id| !after.contains_key(id)).copied().collect();

        installed.sort_by_key(|id| id.0);
        removed.sort_by_key(|id| id.0);

        Self { installed, removed }
    }

    pub(crate) fn mark_installed(&mut self, app_id: AppId) {
        if !self.installed.contains(&app_id) {
            self.installed.push(app_id);
            self.installed.sort_by_key(|id| id.0);
        }
    }

    pub(crate) fn mark_removed(&mut self, app_id: AppId) {
        if !self.removed.contains(&app_id) {
            self.removed.push(app_id);
            self.removed.sort_by_key(|id| id.0);
        }
    }
}

#[derive(Debug)]
pub(crate) struct AppRegistry {
    installed_apps: HashMap<AppId, AppInfo>,
    running_apps: HashMap<PID, RunningAppInfo>,
    current_keyos_version: Version,
}

impl Default for AppRegistry {
    fn default() -> Self { Self::new(semver::Version::new(u64::MAX, 0, 0)) }
}

impl AppRegistry {
    pub(crate) fn new(current_keyos_version: Version) -> Self {
        Self { installed_apps: HashMap::new(), running_apps: HashMap::new(), current_keyos_version }
    }

    pub(crate) fn current_keyos_version(&self) -> &Version { &self.current_keyos_version }

    pub(crate) fn scan_installed_apps(
        &mut self,
        fs: &FileSystem,
    ) -> anyhow::Result<(ServerPermissionCache, AppRegistryDiff)> {
        let mut installed_apps = HashMap::new();

        // Build the per-server cache as we scan. Adding a manifest also detects server-name
        // collisions, so an app that declares a server already owned by a system service or an
        // earlier app is rejected. Seed it with the system services first.
        let mut cache = ServerPermissionCache::default();
        for manifest in permission_catalog::system_manifests() {
            cache.add_manifest(manifest).expect("system manifests must not declare colliding servers");
        }

        // Location decides built-in versus sideloaded: firmware-shipped apps live under
        // /keyos/apps and must carry the Foundation signature, while a sideloaded bundle's
        // trust class follows from the key that signed it. The simulator reads the same dirs
        // through fs and signs nothing.
        Self::scan_apps_dir(
            fs,
            &mut installed_apps,
            &mut cache,
            BUILT_IN_APPS_DIR,
            AppResourcesRoot::BuiltIn,
        );
        Self::scan_apps_dir(
            fs,
            &mut installed_apps,
            &mut cache,
            SIDELOADED_APPS_DIR,
            AppResourcesRoot::Sideloaded,
        );

        let diff = AppRegistryDiff::new(&self.installed_apps, &installed_apps);

        self.installed_apps = installed_apps;
        log::info!("scan_installed_apps: registry tracks {} installed apps", self.installed_apps.len());

        Ok((cache, diff))
    }

    /// Read every app bundle under `apps_dir` (a `Location::System` path) through
    /// fs and register it. A missing or unreadable dir is just logged and skipped;
    /// a real loading problem then shows up as a missing app.
    fn scan_apps_dir(
        fs: &FileSystem,
        installed_apps: &mut HashMap<AppId, AppInfo>,
        cache: &mut ServerPermissionCache,
        apps_dir: &str,
        root: AppResourcesRoot,
    ) {
        let dir = match fs.open_dir(apps_dir.to_string(), fs::Location::System) {
            Ok(dir) => dir,
            Err(e) => {
                log::info!("Not scanning apps in {apps_dir}: {e:?}");
                return;
            }
        };

        while let Ok(Some(entry)) = dir.next_entry() {
            if !entry.is_dir || entry.name == "." || entry.name == ".." {
                continue;
            }
            let app_dir = format!("{apps_dir}/{}", entry.name);
            match Self::load_app(fs, &app_dir, root) {
                Ok(Some(app)) if installed_apps.contains_key(&app.id) => {
                    log::warn!("Skipping duplicate app_id=0x{} from {root:?}", hex::encode(app.id.0));
                }
                Ok(Some(app)) => {
                    if let Err(collision) = cache.add_manifest(&app.manifest) {
                        log::warn!(
                            "Skipping app 0x{}: declares server `{}`, already owned by a system service or another app",
                            hex::encode(app.id.0),
                            collision.0
                        );
                        continue;
                    }
                    installed_apps.insert(app.id, app);
                }
                Ok(None) => {}
                Err(e) => log::warn!("Skipping app bundle {app_dir}: {e:?}"),
            }
        }
    }

    /// Load one app bundle, on hardware only after its signed manifest verifies, or else a forged
    /// manifest's contents (app id, permissions, QR rules) would be trusted the moment it enters
    /// the registry, with no launch required. Returns `None` when a sideloaded bundle's dir name
    /// doesn't match its app id (already logged).
    fn load_app(fs: &FileSystem, app_dir: &str, root: AppResourcesRoot) -> anyhow::Result<Option<AppInfo>> {
        let manifest_raw = read_capped_file(
            fs,
            &format!("{app_dir}/manifest.json"),
            fs::Location::System,
            MAX_MANIFEST_SIZE_BYTES,
        )?;
        let (manifest_json, signature) = check_manifest_signature(&manifest_raw, root)?;
        let manifest = app_manifest::try_from_bytes(manifest_json)
            .map_err(|e| anyhow::anyhow!("invalid manifest: {e}"))?;
        if manifest.min_keyos_version.is_none() {
            anyhow::bail!("manifest does not declare minKeyosVersion");
        }

        let app_id = AppId(manifest.app_id);
        if !sideloaded_app_dir_matches_app_id(Some(app_dir), &app_id, root) {
            return Ok(None);
        }
        Ok(Some(AppInfo {
            id: app_id,
            app_dir: Some(app_dir.to_string()),
            is_flux: manifest.permissions.contains_key(FLUX_EMULATOR_SERVER),
            manifest,
            manifest_bytes: manifest_json.to_vec(),
            root,
            signature,
            binary_size: elf_size(fs, &format!("{app_dir}/{ELF_FILE}")),
        }))
    }

    pub(crate) fn app_name_by_id(&self, id: &AppId, locale: &str) -> Option<String> {
        self.installed_apps
            .get(id)
            .and_then(|app_info| app_info.manifest.app_name.get(&locale.to_string().into()).cloned())
    }

    /// An installed app's display name, falling back to English then its app id. `None` only when
    /// the app is not installed.
    pub(crate) fn display_name(&self, app_id: &AppId, locale: &str) -> Option<String> {
        self.installed_apps.get(app_id).map(|app_info| app_info.localized_name(locale))
    }

    pub(crate) fn app_name_by_pid(&self, pid: PID, locale: &str) -> Option<String> {
        self.running_apps
            .get(&pid)
            .and_then(|app_info| app_info.info.manifest.app_name.get(&locale.to_string().into()).cloned())
    }

    pub(crate) fn qr_match_rules(
        &self,
        app_ids: &[AppId],
        publishers: &[ThirdPartyCertificateInfo],
    ) -> Vec<AppQrMatchRules> {
        self.installed_apps
            .values()
            .filter(|app_info| app_ids.is_empty() || app_ids.contains(&app_info.id))
            .filter(|app_info| {
                app_info.publisher_and_launch_error(&self.current_keyos_version, publishers).1.is_none()
            })
            .filter(|app_info| !app_info.manifest.qr_match_rules.is_empty())
            .filter_map(|app_info| match to_vec(&app_info.manifest.qr_match_rules) {
                Ok(rules_json) if !rules_json.is_empty() => {
                    Some(AppQrMatchRules { id: app_info.id, rules_json })
                }
                Ok(_) => None,
                Err(_) => {
                    log::warn!(
                        "qr_match_rules: failed to serialize qr_match_rules for app_id=0x{}",
                        app_info.id
                    );
                    None
                }
            })
            .collect()
    }

    pub(crate) fn list_apps(
        &self,
        locale: &str,
        publishers: &[ThirdPartyCertificateInfo],
        filter: &app_manager::AppFilter,
        permission_grants: &PermissionGrantStore,
    ) -> Vec<InstalledAppInfo> {
        let mut apps = self
            .installed_apps
            .values()
            .filter(|app_info| filter.is_flux.map_or(true, |want| app_info.is_flux == want))
            .filter(|app_info| filter.sideloaded.map_or(true, |want| app_info.is_sideloaded() == want))
            .map(|app_info| {
                let (publisher_fingerprint, launch_error) =
                    app_info.publisher_and_launch_error(&self.current_keyos_version, publishers);
                // The label is an identity, not the bundle's claim: a certified app shows the
                // certificate name the user confirmed at import, a Foundation-signed app its
                // manifest publisher (covered by the Foundation signature), an uncertified
                // bundle nothing.
                let publisher_name = if !publisher_fingerprint.is_empty() {
                    publishers
                        .iter()
                        .find(|p| p.short_fingerprint == publisher_fingerprint)
                        .map(|p| p.name.clone())
                        .unwrap_or_default()
                } else if !app_info.is_third_party() {
                    app_info.manifest.publisher.clone().unwrap_or_default()
                } else {
                    String::new()
                };
                let (basic_permissions, approvable_permissions) =
                    app_info.permission_groups(permission_grants, locale);
                InstalledAppInfo {
                    app_id: format!("0x{}", app_info.id),
                    publisher_fingerprint,
                    publisher_name,
                    is_foundation_signed: !app_info.is_third_party(),
                    launch_error,
                    can_remove: app_info.is_sideloaded(),
                    is_flux: app_info.is_flux,
                    version: app_info.manifest.version.as_ref().map(ToString::to_string).unwrap_or_default(),
                    size_bytes: app_info.binary_size,
                    app_hash: app_info.manifest.file_hashes.get(ELF_FILE).copied().unwrap_or_default(),
                    description: app_info.description(),
                    basic_permissions,
                    approvable_permissions,
                    name: app_info.localized_name(locale),
                }
            })
            .collect::<Vec<_>>();

        apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        apps
    }

    pub(crate) fn app_name_requiring_third_party_key(
        &self,
        public_key: &str,
        locale: &str,
    ) -> Option<String> {
        let public_key = crate::third_party_certs::decode_public_key_hex(public_key)?;

        self.installed_apps.values().find_map(|app_info| {
            (app_info.signature == AppSignature::ThirdParty(Some(public_key)))
                .then(|| app_info.localized_name(locale))
        })
    }

    pub(crate) fn elf_path(&self, app_id: AppId) -> Option<String> {
        self.installed_apps.get(&app_id).and_then(AppInfo::elf_path)
    }

    /// Blind-read the app's icon, returning `None` when the app ships no icon (the common
    /// case) or it can't be read. Most apps ship no dark variant, so a missing one is not an
    /// error: the light icon answers a dark-variant request instead. Built-in icons live in
    /// CommonAssets (keyed by app id); sideloaded icons live in the app bundle.
    pub(crate) fn app_icon_bytes(
        &self,
        fs: &FileSystem,
        app_id: AppId,
        variant: IconVariant,
    ) -> Option<Vec<u8>> {
        let app = self.installed_apps.get(&app_id)?;
        if matches!(variant, IconVariant::Dark) {
            if let Some((path, location)) = app.icon_path(IconVariant::Dark) {
                match read_capped_file(fs, &path, location, MAX_APP_ICON_SIZE_BYTES) {
                    Ok(bytes) if !bytes.is_empty() => return Some(bytes),
                    // An empty file would decode to a blank image, so let the light icon answer.
                    Ok(_) => log::warn!("dark app icon for app_id=0x{app_id} is empty"),
                    Err(e) if is_file_not_found(&e) => {}
                    Err(e) => log::warn!("failed to read the dark app icon for app_id=0x{app_id}: {e:?}"),
                }
            }
        }

        let (path, location) = app.icon_path(IconVariant::Light)?;
        read_capped_file(fs, &path, location, MAX_APP_ICON_SIZE_BYTES)
            .map_err(|e| log::warn!("failed to read app icon for app_id=0x{app_id}: {e:?}"))
            .ok()
    }

    pub(crate) fn app_resources_location(&self, app_id: AppId) -> Option<AppResourcesLocation> {
        self.installed_apps.get(&app_id).and_then(AppInfo::app_resources_location)
    }

    /// Why launching the app would fail right now, or `None` while it would get as far as the
    /// signature check.
    pub(crate) fn launch_error(
        &self,
        app_id: AppId,
        publishers: &[ThirdPartyCertificateInfo],
    ) -> Option<LaunchError> {
        self.installed_apps.get(&app_id).map_or(Some(LaunchError::UnknownAppId), |app| {
            app.publisher_and_launch_error(&self.current_keyos_version, publishers).1
        })
    }

    /// The bundle file hashes from the app's manifest, verified and stored at scan time. Launch
    /// checks the files against these without re-reading or re-verifying the manifest.
    #[cfg(keyos)]
    pub(crate) fn file_hashes(
        &self,
        app_id: AppId,
    ) -> Option<std::collections::BTreeMap<String, [u8; app_manifest::FILE_HASH_BYTE_LEN]>> {
        self.installed_apps.get(&app_id).map(|app_info| app_info.manifest.file_hashes.clone())
    }

    pub(crate) fn contains_app(&self, app_id: AppId) -> bool { self.installed_apps.contains_key(&app_id) }

    /// Whether a freshly scanned app is the sideloaded bundle expected by the uploader. A built-in
    /// that shares the id must not make a sideload completion look successful.
    pub(crate) fn is_sideloaded_app(&self, app_id: AppId) -> bool {
        self.installed_apps.get(&app_id).is_some_and(AppInfo::is_sideloaded)
    }

    /// The key that signed the bundle installed under this app id, `None` when no app is installed
    /// under it. The key is itself optional: a hosted build's manifests are unsigned.
    pub(crate) fn bundle_signer(&self, app_id: &AppId) -> Option<Option<[u8; 33]>> {
        self.installed_apps.get(app_id).map(|app_info| app_info.signature.signer())
    }

    /// AppIds of every installed Flux child. The emulator host itself is not flux, so this
    /// returns the children only.
    pub(crate) fn flux_child_app_ids(&self) -> Vec<AppId> {
        self.installed_apps.values().filter(|a| a.is_flux).map(|a| a.id).collect()
    }

    /// Whether this installed app provides the Flux emulator's server, i.e. is the emulator the
    /// Flux children depend on.
    pub(crate) fn provides_flux_emulator(&self, app_id: &AppId) -> bool {
        self.installed_apps
            .get(app_id)
            .is_some_and(|app| app.manifest.servers.contains_key(FLUX_EMULATOR_SERVER))
    }

    /// Whether any installed app provides the Flux emulator's server.
    pub(crate) fn flux_emulator_installed(&self) -> bool {
        self.installed_apps.values().any(|app| app.manifest.servers.contains_key(FLUX_EMULATOR_SERVER))
    }

    pub(crate) fn is_running(&self, app_id: &AppId) -> bool { self.running_pid(app_id).is_some() }

    pub(crate) fn running_pid(&self, app_id: &AppId) -> Option<PID> {
        self.running_apps
            .iter()
            .find_map(|(pid, running_app)| (running_app.info.id == *app_id).then_some(*pid))
    }

    /// Whether an app id belongs to a firmware-shipped app.
    ///
    /// A scan registers built-ins before sideloads and skips a second bundle claiming an id it
    /// already has, so a sideloaded bundle under a built-in's id can never take effect.
    pub(crate) fn is_built_in(&self, app_id: &AppId) -> bool {
        self.installed_apps.get(app_id).is_some_and(|app_info| !app_info.is_sideloaded())
            || permission_catalog::system_manifests().iter().any(|m| m.app_id == app_id.0)
    }

    pub(crate) fn removable_bundle_dir(&self, app_id: AppId) -> Option<String> {
        let app_info = self.installed_apps.get(&app_id)?;
        if !app_info.is_sideloaded() {
            return None;
        }

        app_info.app_dir.clone()
    }

    pub(crate) fn clear_registered_manifest(&self, app_id: AppId) {
        if let Err(error) = clear_manifest_with_names(app_id) {
            log::error!(
                "Could not remove the manifest of removed app 0x{} from the name server: {error:?}",
                hex::encode(app_id.0)
            );
        }
    }

    pub(crate) fn register_running_app(&mut self, pid: PID, app_id: AppId, launched_by: PID) {
        self.installed_apps.get(&app_id).inspect(|app_info| {
            self.running_apps.insert(pid, RunningAppInfo { info: (*app_info).clone(), launched_by });
        });
    }

    pub(crate) fn app_id_by_pid(&self, pid: PID) -> Option<&AppId> {
        self.running_apps.get(&pid).map(|app_info| &app_info.info.id)
    }

    pub(crate) fn launched_by(&self, app_id: &AppId) -> Option<PID> {
        self.running_apps
            .values()
            .find(|app_info| app_info.info.id == *app_id)
            .map(|app_info| app_info.launched_by)
    }

    pub(crate) fn terminate_app(&mut self, pid: PID) { self.running_apps.remove(&pid); }

    /// Register the app's manifest names with the name server. Fails the
    /// launch when the effective manifest cannot be produced or registered:
    /// launching anyway would leave the app without server access and turn
    /// the failure into silent runtime errors.
    pub(crate) fn register_app_names(
        &self,
        app_id: AppId,
        permission_grants: &PermissionGrantStore,
    ) -> Result<(), LaunchError> {
        let info = self.installed_apps.get(&app_id).ok_or(LaunchError::UnknownAppId)?;
        let manifest_bytes = info.effective_manifest_bytes(permission_grants)?;
        register_manifest_with_names(&manifest_bytes).map_err(|error| {
            error!("could not register manifest names for app 0x{app_id}: {error:?}");
            LaunchError::NameRegistration
        })
    }

    pub(crate) fn set_permission_grant(
        &self,
        app_id: AppId,
        subgroup: &str,
        approved: bool,
        permission_grants: &mut PermissionGrantStore,
    ) -> app_manager::SetAppPermissionGrantResult {
        let Some(app_info) = self.installed_apps.get(&app_id) else {
            return app_manager::SetAppPermissionGrantResult::AppNotFound;
        };
        // Foundation-signed apps' permissions are not user-managed (see effective_manifest_bytes),
        // so there is nothing to grant or revoke for them.
        if !app_info.is_third_party() {
            return app_manager::SetAppPermissionGrantResult::AppNotFound;
        }

        // A grant is recorded per subgroup, so it is valid as soon as the app declares at least
        // one message of that subgroup the app's signature satisfies and the user may decide.
        let mut declared = false;
        let mut grantable = false;
        for (server, messages) in &app_info.manifest.permissions {
            for message in messages {
                let Some(entry) = permission_grants.message_metadata(server, message) else {
                    continue;
                };
                if entry.subgroup() != subgroup {
                    continue;
                }
                declared = true;
                if app_info.message_availability(entry) == MessageAvailability::ApprovalBased {
                    grantable = true;
                }
            }
        }
        if !declared {
            return app_manager::SetAppPermissionGrantResult::PermissionNotFound;
        }
        if !grantable {
            return app_manager::SetAppPermissionGrantResult::NotUserGrantable;
        }

        permission_grants.set_grant(app_id, subgroup, approved)
    }

    pub(crate) fn permission_request_info(
        &self,
        sender_app_id: AppId,
        server_name: &str,
        message_id: usize,
        locale: &str,
        permission_grants: &PermissionGrantStore,
    ) -> PermissionRequestInfoResult {
        let Some(app_info) = self.installed_apps.get(&sender_app_id) else {
            return PermissionRequestInfoResult::AppNotFound;
        };
        // Foundation-signed apps bypass the permission mechanism and are never parked for a
        // prompt; this is defensive so their messages can't be routed through the grant flow.
        if !app_info.is_third_party() {
            return PermissionRequestInfoResult::NotGrantable;
        }

        // Message ids are unique within one named server, so the (name, id) pair resolves
        // without ambiguity even when a process hosts several servers with overlapping ids.
        let Some(message) = permission_grants.message_name_by_id(server_name, message_id) else {
            return PermissionRequestInfoResult::NotGrantable;
        };
        let message = message.to_string();
        if !app_info.manifest.permissions.get(server_name).is_some_and(|messages| messages.contains(&message))
        {
            return PermissionRequestInfoResult::NotGrantable;
        }

        let Some(entry) = permission_grants.message_metadata(server_name, &message) else {
            return PermissionRequestInfoResult::NotGrantable;
        };
        if app_info.message_availability(entry) != MessageAvailability::ApprovalBased {
            return PermissionRequestInfoResult::NotGrantable;
        }

        // The user decides at the subgroup level: the stored subgroup grant answers this message
        // and every other message of the subgroup, so it is prompted at most once.
        match permission_grants.subgroup_grant_state(app_info.id, entry.subgroup()) {
            PermissionGrantState::Approved => PermissionRequestInfoResult::AlreadyApproved,
            PermissionGrantState::Denied => PermissionRequestInfoResult::Denied,
            PermissionGrantState::Unset => PermissionRequestInfoResult::Prompt(PermissionRequestInfo {
                app_id: app_info.id,
                app_name: app_info.localized_name(locale),
                subgroup: entry.subgroup().to_string(),
                label: entry.subgroup_label(locale).to_string(),
            }),
        }
    }
}

#[cfg(any(keyos, all(not(test), not(keyos))))]
fn register_manifest_with_names(manifest_bytes: &[u8]) -> Result<(), xous::Error> {
    let names =
        server::xous_names::XousNames::new().expect("xous-names should be available during app scanning");

    names.add_manifest(manifest_bytes)
}

#[cfg(all(test, not(keyos)))]
fn register_manifest_with_names(_manifest_bytes: &[u8]) -> Result<(), xous::Error> {
    // Plain Rust unit tests run outside the hosted Xous kernel.
    Ok(())
}

#[cfg(any(keyos, all(not(test), not(keyos))))]
fn clear_manifest_with_names(app_id: AppId) -> Result<(), xous::Error> {
    let names =
        server::xous_names::XousNames::new().expect("xous-names should be available during app scanning");
    names.remove_manifest(app_id)
}

#[cfg(all(test, not(keyos)))]
fn clear_manifest_with_names(_app_id: AppId) -> Result<(), xous::Error> {
    // Plain Rust unit tests run outside the hosted Xous kernel.
    Ok(())
}

impl AppInfo {
    /// The launchable `app.elf` lives directly inside the bundle dir.
    fn elf_path(&self) -> Option<String> { self.app_dir.as_deref().map(|dir| format!("{dir}/{ELF_FILE}")) }

    fn localized_name(&self, locale: &str) -> String {
        self.manifest
            .app_name
            .get(&Locale(locale.to_string()))
            .or_else(|| self.manifest.app_name.get(&Locale("en".to_string())))
            .cloned()
            .unwrap_or_else(|| format!("0x{}", self.id))
    }

    fn message_availability(
        &self,
        entry: &crate::permission_catalog::MessageMetadata,
    ) -> MessageAvailability {
        if !entry.signature_satisfied_by(self.signature.required()) {
            MessageAvailability::Unavailable
        } else if entry.is_auto_allow() || !self.is_third_party() {
            // Foundation-signed apps, built-in or sideloaded, get every declared message
            // automatically; only third-party apps go through approvals.
            MessageAvailability::AutoAllow
        } else if entry.is_approval_based() {
            MessageAvailability::ApprovalBased
        } else {
            MessageAvailability::Unavailable
        }
    }

    /// The app's permission subgroups, collapsed under their top-level groups and split by
    /// kind: auto-granted (basic) and user-grantable (approvable). A message the app's
    /// signature can't satisfy is left out of both.
    fn permission_groups(
        &self,
        permission_grants: &PermissionGrantStore,
        locale: &str,
    ) -> (Vec<InstalledAppPermissionGroup>, Vec<InstalledAppPermissionGroup>) {
        let mut basic = Vec::new();
        let mut approvable = Vec::new();

        for (server, messages) in &self.manifest.permissions {
            for message in messages {
                let Some(entry) = permission_grants.message_metadata(server, message) else {
                    continue;
                };
                // Ungrouped messages are internal plumbing granted on signature alone (e.g. the
                // Flux emulator's child channel); they carry no user-facing label, so keep them out
                // of the permission UI. effective_permissions still enforces them.
                if entry.subgroup().is_empty() {
                    continue;
                }
                let (groups, approved) = match self.message_availability(entry) {
                    MessageAvailability::AutoAllow => (&mut basic, true),
                    MessageAvailability::ApprovalBased => (
                        &mut approvable,
                        permission_grants.subgroup_grant_state(self.id, entry.subgroup())
                            == PermissionGrantState::Approved,
                    ),
                    MessageAvailability::Unavailable => continue,
                };
                push_permission_subgroup(groups, entry, approved, locale);
            }
        }

        (basic, approvable)
    }

    fn effective_manifest_bytes(
        &self,
        permission_grants: &PermissionGrantStore,
    ) -> Result<Vec<u8>, LaunchError> {
        // Foundation-signed apps, built-in or sideloaded, run with everything they declare and
        // are not user-managed: trust comes from the Foundation signature, so they bypass
        // filtering, first-use prompts, and Settings entirely. Only third-party apps get their
        // manifest narrowed to the granted set.
        if !self.is_third_party() {
            return Ok(self.manifest_bytes.clone());
        }

        let mut manifest = self.manifest.clone();
        manifest.permissions = self.effective_permissions(permission_grants);
        serde_json::to_vec(&manifest).map_err(|error| {
            error!("failed to serialize effective manifest for app_id=0x{}: {error:?}", self.id);
            LaunchError::InternalError
        })
    }

    fn effective_permissions(
        &self,
        permission_grants: &PermissionGrantStore,
    ) -> BTreeMap<String, BTreeSet<String>> {
        let mut effective = BTreeMap::new();
        for (server, messages) in &self.manifest.permissions {
            let mut connectable = false;
            for message in messages {
                let Some(entry) = permission_grants.message_metadata(server, message) else {
                    continue;
                };
                let allowed = match self.message_availability(entry) {
                    MessageAvailability::AutoAllow => {
                        connectable = true;
                        true
                    }
                    MessageAvailability::ApprovalBased => {
                        connectable = true;
                        permission_grants.is_approved(self.id, server, message)
                    }
                    MessageAvailability::Unavailable => false,
                };
                if allowed {
                    effective.entry(server.clone()).or_insert_with(BTreeSet::new).insert(message.clone());
                }
            }
            if connectable {
                effective.entry(server.clone()).or_insert_with(BTreeSet::new);
            }
        }
        effective
    }

    /// The publisher fingerprint to show and why a launch would fail, if it would. Compatibility
    /// takes precedence over publisher errors. `publishers` is every stored certificate rather
    /// than only the usable ones, so a signer that matches an unusable certificate is reported
    /// under that certificate instead of as a missing one. Neither built-in, Foundation-signed,
    /// nor hosted apps carry a publisher fingerprint; the simulator ignores publisher signatures.
    fn publisher_and_launch_error(
        &self,
        current_keyos_version: &Version,
        publishers: &[ThirdPartyCertificateInfo],
    ) -> (String, Option<LaunchError>) {
        let minimum = self
            .manifest
            .min_keyos_version
            .as_ref()
            .expect("registered app manifests must declare minKeyosVersion");
        let compatibility_error = (minimum > current_keyos_version).then(|| {
            LaunchError::Compatibility(CompatibilityError::KeyOsVersionTooOld {
                minimum: minimum.to_string(),
                current: current_keyos_version.to_string(),
            })
        });
        #[cfg(all(not(keyos), not(test)))]
        {
            let _ = publishers;
            (String::new(), compatibility_error)
        }
        #[cfg(any(keyos, test))]
        {
            let (publisher_fingerprint, publisher_error) = match self.signature {
                AppSignature::Foundation => (String::new(), None),
                AppSignature::ThirdParty(None) => (String::new(), Some(LaunchError::NoCertificate)),
                AppSignature::ThirdParty(Some(signer)) => {
                    let Some(publisher) = publishers.iter().find(|p| {
                        crate::third_party_certs::decode_public_key_hex(&p.public_key) == Some(signer)
                    }) else {
                        return (String::new(), compatibility_error.or(Some(LaunchError::NoCertificate)));
                    };
                    let error = if publisher.has_expired() {
                        Some(LaunchError::PublisherCertificateExpired)
                    } else if publisher.is_not_yet_valid() {
                        Some(LaunchError::PublisherCertificateNotYetActive)
                    } else {
                        None
                    };
                    (publisher.short_fingerprint.clone(), error)
                }
            };
            (publisher_fingerprint, compatibility_error.or(publisher_error))
        }
    }

    fn description(&self) -> String { self.manifest.description.clone().unwrap_or_default() }

    fn icon_path(&self, variant: IconVariant) -> Option<(String, fs::Location)> {
        if self.is_sideloaded() {
            let file = match variant {
                IconVariant::Light => BUNDLED_ICON_FILE,
                IconVariant::Dark => BUNDLED_DARK_ICON_FILE,
            };
            let app_dir = self.app_dir.as_deref()?;
            Some((format!("{app_dir}/{file}"), fs::Location::System))
        } else {
            let suffix = match variant {
                IconVariant::Light => "",
                IconVariant::Dark => "-dark",
            };
            Some((format!("app-icons/{}{suffix}.bin", self.id), fs::Location::CommonAssets))
        }
    }

    fn app_resources_location(&self) -> Option<AppResourcesLocation> {
        let app_dir = self.app_dir.as_deref()?;
        let app_dir = app_dir.rsplit('/').next()?;
        if app_dir.is_empty() || app_dir == "." || app_dir == ".." {
            return None;
        }

        let app_dir = if self.is_sideloaded() { self.id.to_string() } else { app_dir.to_string() };

        Some(AppResourcesLocation { root: self.root, app_dir })
    }

    fn is_third_party(&self) -> bool { matches!(self.signature, AppSignature::ThirdParty(_)) }

    fn is_sideloaded(&self) -> bool { self.root == AppResourcesRoot::Sideloaded }
}

/// File `entry`'s subgroup under its top-level group, creating either level on first sight.
/// Several declared messages resolve to the same subgroup; the user sees one row.
fn push_permission_subgroup(
    groups: &mut Vec<InstalledAppPermissionGroup>,
    entry: &crate::permission_catalog::MessageMetadata,
    approved: bool,
    locale: &str,
) {
    let group = match groups.iter_mut().find(|group| group.key == entry.group()) {
        Some(group) => group,
        None => {
            groups.push(InstalledAppPermissionGroup {
                key: entry.group().to_string(),
                label: entry.group_label(locale).to_string(),
                subgroups: Vec::new(),
            });
            groups.last_mut().expect("group pushed")
        }
    };

    if !group.subgroups.iter().any(|subgroup| subgroup.key == entry.subgroup()) {
        group.subgroups.push(InstalledAppPermissionSubgroup {
            key: entry.subgroup().to_string(),
            label: entry.subgroup_label(locale).to_string(),
            approved,
        });
    }
}

/// Read a bundle file through `fs`, refusing anything larger than `max_size_bytes` before
/// allocating, so a malformed bundle can't make us read an unbounded amount into memory.
fn read_capped_file(
    fs: &FileSystem,
    path: &str,
    location: fs::Location,
    max_size_bytes: u64,
) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;

    let metadata = fs.metadata(path, location)?;
    if metadata.size > max_size_bytes {
        anyhow::bail!("{path} exceeds the {max_size_bytes}-byte cap: {} bytes", metadata.size);
    }

    let mut file = fs.open_file(path, location, fs::OpenFlags::READ_ONLY)?;
    let mut bytes = Vec::with_capacity(metadata.size as usize);
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Whether a read failed only because the file is absent, which for the optional dark icon is
/// the common case rather than a fault.
fn is_file_not_found(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref::<fs::Error>(), Some(fs::Error::FileNotFound))
}

/// Size of the app binary, 0 when there is none to read: a system manifest ships no app file.
#[cfg(keyos)]
fn elf_size(fs: &FileSystem, path: &str) -> u64 {
    fs.metadata(path, fs::Location::System).map(|metadata| metadata.size).unwrap_or(0)
}

/// A host binary's size says nothing about the app that ships, so hosted doesn't report one.
#[cfg(not(keyos))]
fn elf_size(_fs: &FileSystem, _path: &str) -> u64 { 0 }

/// Verify a Foundation-signed binary: a built-in, or a sideload bundle from the same build.
/// Production requires the official double signature; a development build accepts exactly a
/// developer signature by the key this firmware was built with.
#[cfg(all(keyos, feature = "production"))]
pub(crate) fn verify_foundation(
    crypto: &crate::CryptoApi,
    data: &[u8],
) -> Result<cosign2::Header, fw_utils::hash::HashError> {
    fw_utils::hash::verify_cosign2_mem(crypto, data, true)
}

#[cfg(all(keyos, not(feature = "production")))]
pub(crate) fn verify_foundation(
    crypto: &crate::CryptoApi,
    data: &[u8],
) -> Result<cosign2::Header, fw_utils::hash::HashError> {
    let header = fw_utils::hash::verify_cosign2_mem_third_party(crypto, data)?;
    if header.pubkey2() != DEV_SIGNER {
        return Err(fw_utils::hash::HashError::NotTrusted);
    }
    Ok(header)
}

/// Classify a sideloaded bundle by the key that signed it: the build's own signer (or the
/// official roster on production) is Foundation, any other developer key is third-party.
#[cfg(keyos)]
fn classify_sideload(
    crypto: &crate::CryptoApi,
    data: &[u8],
) -> Result<AppSignature, fw_utils::hash::HashError> {
    #[cfg(not(feature = "production"))]
    {
        let header = fw_utils::hash::verify_cosign2_mem_third_party(crypto, data)?;
        Ok(if header.pubkey2() == DEV_SIGNER {
            AppSignature::Foundation
        } else {
            AppSignature::ThirdParty(Some(header.pubkey2()))
        })
    }
    #[cfg(feature = "production")]
    {
        if verify_foundation(crypto, data).is_ok() {
            return Ok(AppSignature::Foundation);
        }
        let header = fw_utils::hash::verify_cosign2_mem_third_party(crypto, data)?;
        Ok(AppSignature::ThirdParty(Some(header.pubkey2())))
    }
}

/// Verify a bundle manifest and return its header-stripped JSON and its signature class,
/// derived from which key signed it. A built-in must carry the Foundation signature; a
/// third-party manifest only needs a valid developer signature here, since whether its key is
/// allowed is decided at launch and listing time against the cert store.
#[cfg(keyos)]
fn check_manifest_signature(
    manifest_raw: &[u8],
    root: AppResourcesRoot,
) -> anyhow::Result<(&[u8], AppSignature)> {
    // Drop the cosign2 header, leaving the JSON it wraps.
    let manifest_json = manifest_raw
        .get(cosign2::Header::DEFAULT_SIZE..)
        .ok_or_else(|| anyhow::anyhow!("manifest is too short to hold a cosign2 header"))?;

    let crypto = crate::CryptoApi::default();
    let signature = match root {
        AppResourcesRoot::BuiltIn => {
            verify_foundation(&crypto, manifest_raw)
                .map_err(|e| anyhow::anyhow!("unverified manifest: {e:?}"))?;
            AppSignature::Foundation
        }
        AppResourcesRoot::Sideloaded => classify_sideload(&crypto, manifest_raw)
            .map_err(|e| anyhow::anyhow!("unverified manifest: {e:?}"))?,
    };
    Ok((manifest_json, signature))
}

/// Hosted manifests are unsigned, so the raw bytes are the JSON, there is no signer, and only
/// the root decides the class: hosted sideloads behave as third-party.
#[cfg(not(keyos))]
fn check_manifest_signature(
    manifest_raw: &[u8],
    root: AppResourcesRoot,
) -> anyhow::Result<(&[u8], AppSignature)> {
    let signature = match root {
        AppResourcesRoot::BuiltIn => AppSignature::Foundation,
        AppResourcesRoot::Sideloaded => AppSignature::ThirdParty(None),
    };
    Ok((manifest_raw, signature))
}

/// Verify a sideloaded bundle's manifest, returning its header-stripped JSON and its signature
/// class.
pub(crate) fn verified_sideload_manifest(manifest_raw: &[u8]) -> anyhow::Result<(&[u8], AppSignature)> {
    check_manifest_signature(manifest_raw, AppResourcesRoot::Sideloaded)
}

fn sideloaded_app_dir_matches_app_id(app_dir: Option<&str>, app_id: &AppId, root: AppResourcesRoot) -> bool {
    if root == AppResourcesRoot::BuiltIn {
        return true;
    }

    let Some(app_dir) = app_dir else {
        return true;
    };

    if !app_dir.starts_with(SIDELOADED_APPS_DIR) {
        return true;
    }

    let Some(name) = sideloaded_app_dir_name(app_dir) else {
        log::warn!("scan_installed_apps: skipping sideloaded app with invalid bundle path {app_dir:?}");
        return false;
    };

    let expected_app_dir = hex::encode(app_id.0);
    if name != expected_app_dir {
        log::warn!(
            "scan_installed_apps: skipping sideloaded app 0x{} from directory {:?}; expected {:?}",
            expected_app_dir,
            name,
            expected_app_dir
        );
        return false;
    }

    true
}

fn sideloaded_app_dir_name(app_dir: &str) -> Option<&str> {
    let name = sideloaded_path_suffix(app_dir)?;
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

fn sideloaded_path_suffix(path: &str) -> Option<&str> {
    path.strip_prefix(SIDELOADED_APPS_DIR).and_then(|path| path.strip_prefix('/'))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    use app_manager::decode_app_id_str;
    use app_manifest::{QrMatchRule, QrMatchSubRule, QrPriority};

    use super::*;

    const THIRD_PARTY_APP_ID: &str = "0x00112233445566778899aabbccddeeff";
    const THIRD_PARTY_APP_DIR: &str = "00112233445566778899aabbccddeeff";
    const THIRD_PARTY_ELF_PATH: &str = "/keyos/sideloaded-apps/00112233445566778899aabbccddeeff/app.elf";
    const TEST_MINIMUM_KEYOS_VERSION: &str = "1.4.0-beta1";

    fn latest_keyos_version() -> Version { Version::new(u64::MAX, 0, 0) }

    fn app_info(app_id: &str, name: &str, elf_path: Option<&str>) -> AppInfo {
        let (root, signature) = if elf_path.is_some() {
            (AppResourcesRoot::Sideloaded, AppSignature::ThirdParty(None))
        } else {
            (AppResourcesRoot::BuiltIn, AppSignature::Foundation)
        };
        app_info_with_trust(app_id, name, elf_path, root, signature)
    }

    fn third_party_app_with_permissions(permissions: &[(&str, &[&str])]) -> AppInfo {
        let mut app = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        app.manifest.permissions = permissions
            .iter()
            .map(|(server, messages)| {
                (
                    (*server).to_string(),
                    messages.iter().map(|message| (*message).to_string()).collect::<BTreeSet<_>>(),
                )
            })
            .collect();
        app
    }

    fn built_in_app_info(app_id: &str, name: &str, elf_path: Option<&str>) -> AppInfo {
        app_info_with_trust(app_id, name, elf_path, AppResourcesRoot::BuiltIn, AppSignature::Foundation)
    }

    fn app_info_with_trust(
        app_id: &str,
        name: &str,
        elf_path: Option<&str>,
        root: AppResourcesRoot,
        signature: AppSignature,
    ) -> AppInfo {
        AppInfo {
            id: decode_app_id_str(app_id).unwrap(),
            app_dir: elf_path.map(|path| path.strip_suffix("/app.elf").unwrap_or(path).to_owned()),
            manifest: Manifest {
                app_name: BTreeMap::from([(Locale("en".to_string()), name.to_string())]),
                app_id: app_manifest::parse_app_id_bytes(app_id).unwrap(),
                publisher: None,
                description: None,
                version: None,
                min_keyos_version: Some(semver::Version::parse(TEST_MINIMUM_KEYOS_VERSION).unwrap()),
                servers: BTreeMap::new(),
                fixed_sids: BTreeMap::new(),
                permissions: BTreeMap::new(),
                memory: Vec::new(),
                syscall: Vec::new(),
                qr_match_rules: Vec::new(),
                file_hashes: BTreeMap::new(),
            },
            manifest_bytes: Vec::new(),
            root,
            signature,
            is_flux: false,
            binary_size: 0,
        }
    }

    #[test]
    fn app_icon_read_cap_allows_raw_256_rgba_icon() {
        let pixel_bytes = 256 * 256 * 4;
        assert!(MAX_APP_ICON_SIZE_BYTES > pixel_bytes);
    }

    #[test]
    fn dark_icon_path_is_the_dark_suffixed_sibling() {
        let built_in = built_in_app_info(THIRD_PARTY_APP_ID, "Example App", None);
        assert_eq!(
            built_in.icon_path(IconVariant::Light),
            Some((format!("app-icons/{THIRD_PARTY_APP_DIR}.bin"), fs::Location::CommonAssets))
        );
        assert_eq!(
            built_in.icon_path(IconVariant::Dark),
            Some((format!("app-icons/{THIRD_PARTY_APP_DIR}-dark.bin"), fs::Location::CommonAssets))
        );

        let third_party = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        let bundle_dir = format!("/keyos/sideloaded-apps/{THIRD_PARTY_APP_DIR}");
        assert_eq!(
            third_party.icon_path(IconVariant::Light),
            Some((format!("{bundle_dir}/icon.bin"), fs::Location::System))
        );
        assert_eq!(
            third_party.icon_path(IconVariant::Dark),
            Some((format!("{bundle_dir}/icon-dark.bin"), fs::Location::System))
        );
    }

    fn registry_with(apps: Vec<AppInfo>) -> AppRegistry {
        AppRegistry {
            installed_apps: apps.into_iter().map(|app| (app.id, app)).collect::<HashMap<_, _>>(),
            ..AppRegistry::default()
        }
    }

    /// A grant store whose server cache is built from the registry's manifests, mirroring what
    /// `scan_installed_apps` does at runtime, so the metadata lookups resolve in tests.
    fn grants_for(registry: &AppRegistry) -> PermissionGrantStore {
        let mut cache = ServerPermissionCache::default();
        for manifest in permission_catalog::system_manifests() {
            cache.add_manifest(manifest).unwrap();
        }
        for app in registry.installed_apps.values() {
            let _ = cache.add_manifest(&app.manifest);
        }
        let mut grants = PermissionGrantStore::default();
        grants.set_server_cache(cache);
        grants
    }

    #[test]
    fn installed_apps_excludes_system_manifests_without_app_file() {
        let registry = registry_with(vec![app_info(THIRD_PARTY_APP_ID, "System Manifest", None)]);

        assert!(registry
            .list_apps(
                "en",
                &[],
                &app_manager::AppFilter::sideloaded_only(),
                &PermissionGrantStore::default()
            )
            .is_empty());
    }

    // A valid compressed-key prefix (0x02) followed by zeroes; decode_public_key_hex only checks
    // the prefix, so it stands in for a developer signer without needing a real curve point.
    const SIGNER_HEX: &str = "020000000000000000000000000000000000000000000000000000000000000000";

    fn signer_bytes() -> [u8; 33] { crate::third_party_certs::decode_public_key_hex(SIGNER_HEX).unwrap() }

    fn publisher_cert(public_key_hex: &str, name: &str) -> ThirdPartyCertificateInfo {
        publisher_cert_valid_until(public_key_hex, name, app_manager::now_unix_seconds() + 3600)
    }

    fn publisher_cert_valid_until(
        public_key_hex: &str,
        name: &str,
        not_after_unix_seconds: u64,
    ) -> ThirdPartyCertificateInfo {
        ThirdPartyCertificateInfo {
            name: name.to_string(),
            company: String::new(),
            contact_email: String::new(),
            support_url: String::new(),
            public_key: public_key_hex.to_string(),
            fingerprint: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            short_fingerprint: "00000000…00000000".to_string(),
            added_unix_seconds: None,
            not_before_unix_seconds: app_manager::now_unix_seconds() - 3600,
            not_after_unix_seconds,
            serial_number: String::new(),
            issuer: String::new(),
            subject: String::new(),
            basic_constraints: String::new(),
            key_usage: String::new(),
            extended_key_usage: String::new(),
        }
    }

    #[test]
    fn sideloaded_app_launchable_only_with_matching_publisher() {
        let mut app = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        app.signature = AppSignature::ThirdParty(Some(signer_bytes()));

        // No matching publisher: not launchable and no publisher fingerprint to show.
        assert_eq!(
            app.publisher_and_launch_error(&latest_keyos_version(), &[]),
            (String::new(), Some(LaunchError::NoCertificate))
        );

        // A publisher whose key matches the stored signer makes it launchable under its fingerprint.
        let publishers = vec![publisher_cert(SIGNER_HEX, "Acme")];
        assert_eq!(
            app.publisher_and_launch_error(&latest_keyos_version(), &publishers),
            (publishers[0].short_fingerprint.clone(), None)
        );
    }

    #[test]
    fn expired_publisher_blocks_launch_under_its_own_fingerprint() {
        let mut app = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        app.signature = AppSignature::ThirdParty(Some(signer_bytes()));
        let publishers =
            vec![publisher_cert_valid_until(SIGNER_HEX, "Acme", app_manager::now_unix_seconds() - 1)];

        assert_eq!(
            app.publisher_and_launch_error(&latest_keyos_version(), &publishers),
            (publishers[0].short_fingerprint.clone(), Some(LaunchError::PublisherCertificateExpired))
        );
    }

    #[test]
    fn launch_error_tracks_signer_and_builtin() {
        let built_in_id = "0x426974636f696e2057616c6c65740000";
        let mut sideloaded = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        sideloaded.signature = AppSignature::ThirdParty(Some(signer_bytes()));
        let registry = registry_with(vec![
            sideloaded,
            built_in_app_info(built_in_id, "Bitcoin Wallet", Some("/keyos/apps/bitcoin/app.elf")),
        ]);

        let third_party = decode_app_id_str(THIRD_PARTY_APP_ID).unwrap();
        assert_eq!(registry.launch_error(third_party, &[publisher_cert(SIGNER_HEX, "Acme")]), None);
        assert_eq!(registry.launch_error(third_party, &[]), Some(LaunchError::NoCertificate));
        // Built-in apps launch regardless of publishers; an unknown id never does.
        assert_eq!(registry.launch_error(decode_app_id_str(built_in_id).unwrap(), &[]), None);
        assert_eq!(
            registry.launch_error(decode_app_id_str("0xffffffffffffffffffffffffffffffff").unwrap(), &[]),
            Some(LaunchError::UnknownAppId)
        );
    }

    #[test]
    fn an_installed_app_is_blocked_after_a_keyos_downgrade() {
        let mut app = app_info_with_trust(
            THIRD_PARTY_APP_ID,
            "Example App",
            Some(THIRD_PARTY_ELF_PATH),
            AppResourcesRoot::Sideloaded,
            AppSignature::Foundation,
        );
        app.manifest.min_keyos_version = Some(semver::Version::parse(TEST_MINIMUM_KEYOS_VERSION).unwrap());
        let app_id = app.id;
        let mut registry = registry_with(vec![app]);
        registry.current_keyos_version = Version::parse("1.3.1").unwrap();

        assert_eq!(
            registry.launch_error(app_id, &[]),
            Some(LaunchError::Compatibility(app_manager::CompatibilityError::KeyOsVersionTooOld {
                minimum: TEST_MINIMUM_KEYOS_VERSION.to_string(),
                current: "1.3.1".to_string(),
            }))
        );
        let apps = registry.list_apps(
            "en",
            &[],
            &app_manager::AppFilter::sideloaded_only(),
            &PermissionGrantStore::default(),
        );
        assert!(matches!(
            apps[0].launch_error,
            Some(LaunchError::Compatibility(app_manager::CompatibilityError::KeyOsVersionTooOld { .. }))
        ));
    }

    /// A built-in app providing the camera server's manifest, so the message-id lookup does not
    /// depend on the xtask-generated SYSTEM_MANIFESTS (empty under plain `cargo test`).
    fn camera_provider() -> AppInfo {
        let camera_app_id_hex = "0x6775692d6170702d63616d6572610000";
        let mut camera = built_in_app_info(camera_app_id_hex, "Camera", Some("/keyos/apps/camera/app.elf"));
        camera.manifest.servers = BTreeMap::from([(
            "os/camera".to_string(),
            BTreeMap::from([(
                "Subscribe".to_string(),
                app_manifest::Message {
                    id: 1,
                    r#type: app_manifest::MessageType::ScalarEvent,
                    description: None,
                    cfg: None,
                    permission_group: Some("peripherals.camera-use".to_string()),
                    required_signature: Some(app_manifest::RequiredSignature::ThirdParty),
                    approval: app_manifest::ApprovalBehavior::GrantOnFirstUse,
                },
            )]),
        )]);
        camera
    }

    #[test]
    fn permission_request_info_prompts_for_requested_approval_based_permission() {
        let registry = registry_with(vec![
            third_party_app_with_permissions(&[("os/camera", &["Subscribe"])]),
            camera_provider(),
        ]);

        let result = registry.permission_request_info(
            decode_app_id_str(THIRD_PARTY_APP_ID).unwrap(),
            "os/camera",
            1,
            "en",
            &grants_for(&registry),
        );

        match result {
            PermissionRequestInfoResult::Prompt(info) => {
                assert_eq!(info.app_id, decode_app_id_str(THIRD_PARTY_APP_ID).unwrap());
                assert_eq!(info.app_name, "Example App");
                assert_eq!(info.subgroup, "peripherals.camera-use");
                assert_eq!(info.label, "Camera use");
            }
            other => panic!("unexpected permission request result: {other:?}"),
        }
    }

    /// A Foundation-signed sideload is trusted like a built-in (launches with no publisher cert,
    /// every declared permission auto-granted, never prompted) but removable like any sideload.
    #[test]
    fn foundation_sideloaded_app_is_trusted_like_a_built_in_but_removable() {
        let mut app = app_info_with_trust(
            THIRD_PARTY_APP_ID,
            "Legacy",
            Some(THIRD_PARTY_ELF_PATH),
            AppResourcesRoot::Sideloaded,
            AppSignature::Foundation,
        );
        app.manifest.permissions =
            BTreeMap::from([("os/camera".to_string(), BTreeSet::from(["Subscribe".to_string()]))]);
        app.manifest.publisher = Some("Foundation Devices".to_string());
        let registry = registry_with(vec![app, camera_provider()]);
        let grants = grants_for(&registry);

        let apps = registry.list_apps("en", &[], &app_manager::AppFilter::sideloaded_only(), &grants);
        assert_eq!(apps.len(), 1, "Settings lists the Foundation sideload");
        assert_eq!(apps[0].launch_error, None, "launchable with no publisher cert");
        assert!(apps[0].can_remove);
        assert!(apps[0].publisher_fingerprint.is_empty());
        assert_eq!(apps[0].publisher_name, "Foundation Devices");
        assert!(apps[0].is_foundation_signed);
        assert!(apps[0].approvable_permissions.is_empty(), "nothing is left to approve");
        assert_eq!(apps[0].basic_permissions.len(), 1, "declared permissions are auto-granted");

        assert_eq!(
            registry.permission_request_info(
                decode_app_id_str(THIRD_PARTY_APP_ID).unwrap(),
                "os/camera",
                1,
                "en",
                &grants,
            ),
            PermissionRequestInfoResult::NotGrantable
        );
    }

    #[test]
    fn permission_request_info_rejects_built_in_sender() {
        let built_in_id = "0x426974636f696e2057616c6c65740000";
        let registry = registry_with(vec![built_in_app_info(
            built_in_id,
            "Bitcoin Wallet",
            Some("/keyos/apps/bitcoin/app.elf"),
        )]);
        assert_eq!(
            registry.permission_request_info(
                decode_app_id_str(built_in_id).unwrap(),
                "os/camera",
                1,
                "en",
                &PermissionGrantStore::default(),
            ),
            PermissionRequestInfoResult::NotGrantable
        );
    }

    #[test]
    fn permission_request_info_rejects_basic_only_permissions() {
        let registry =
            registry_with(vec![third_party_app_with_permissions(&[("os/app-manager", &["GetAppName"])])]);

        assert_eq!(
            registry.permission_request_info(
                decode_app_id_str(THIRD_PARTY_APP_ID).unwrap(),
                "os/app-manager",
                3,
                "en",
                &PermissionGrantStore::default(),
            ),
            PermissionRequestInfoResult::NotGrantable
        );
    }

    #[test]
    fn permission_request_info_rejects_unrequested_permission() {
        let registry =
            registry_with(vec![third_party_app_with_permissions(&[("os/app-manager", &["GetAppName"])])]);

        assert_eq!(
            registry.permission_request_info(
                decode_app_id_str(THIRD_PARTY_APP_ID).unwrap(),
                "os/camera",
                1,
                "en",
                &PermissionGrantStore::default(),
            ),
            PermissionRequestInfoResult::NotGrantable
        );
    }

    #[test]
    fn app_resources_location_uses_app_id_for_sideloaded_bundle() {
        let registry =
            registry_with(vec![app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH))]);
        let third_party_location = registry
            .app_resources_location(decode_app_id_str(THIRD_PARTY_APP_ID).unwrap())
            .expect("app resources location");

        assert_eq!(third_party_location.root, AppResourcesRoot::Sideloaded);
        assert_eq!(third_party_location.app_dir, THIRD_PARTY_APP_DIR);

        let built_in_id = "0x426974636f696e2057616c6c65740000";
        let registry = registry_with(vec![built_in_app_info(
            built_in_id,
            "Bitcoin Wallet",
            Some("/keyos/apps/bitcoin/app.elf"),
        )]);
        let built_in_location = registry
            .app_resources_location(decode_app_id_str(built_in_id).unwrap())
            .expect("app resources location");

        assert_eq!(built_in_location.root, AppResourcesRoot::BuiltIn);
        assert_eq!(built_in_location.app_dir, "bitcoin");
    }

    #[test]
    fn installed_apps_include_manifest_description_and_version_without_allowed_publisher() {
        let mut app = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        app.manifest.publisher = Some("Example Publisher".to_string());
        app.manifest.description = Some("Example description".to_string());
        app.manifest.version = Some(semver::Version::parse("1.2.3").unwrap());
        app.manifest.file_hashes.insert(ELF_FILE.to_string(), [0xab; 32]);
        let registry = registry_with(vec![app]);

        let apps = registry.list_apps(
            "en",
            &[],
            &app_manager::AppFilter::sideloaded_only(),
            &PermissionGrantStore::default(),
        );

        assert!(apps[0].publisher_fingerprint.is_empty());
        assert!(apps[0].publisher_name.is_empty(), "an uncertified bundle's claim is not shown");
        assert!(!apps[0].is_foundation_signed, "a third-party bundle never gets the Foundation badge");
        assert_eq!(apps[0].launch_error, Some(LaunchError::NoCertificate));
        assert_eq!(apps[0].description, "Example description");
        assert_eq!(apps[0].version, "1.2.3");
        assert_eq!(apps[0].app_hash, [0xab; 32]);
    }

    #[test]
    fn sideloaded_dir_name_must_match_app_id() {
        let app_id = decode_app_id_str(THIRD_PARTY_APP_ID).unwrap();

        let matching = format!("{SIDELOADED_APPS_DIR}/{THIRD_PARTY_APP_DIR}");
        assert!(sideloaded_app_dir_matches_app_id(Some(&matching), &app_id, AppResourcesRoot::Sideloaded));

        let mismatched = format!("{SIDELOADED_APPS_DIR}/ffffffffffffffffffffffffffffffff");
        assert!(!sideloaded_app_dir_matches_app_id(Some(&mismatched), &app_id, AppResourcesRoot::Sideloaded));
    }

    /// An archive claiming a built-in's app id is refused at install time, because a scan would
    /// register the built-in first and skip the sideloaded bundle as a duplicate.
    #[test]
    fn built_in_app_ids_are_recognized() {
        let built_in_id = "0x426974636f696e2057616c6c65740000";
        let registry = registry_with(vec![
            built_in_app_info(built_in_id, "Bitcoin Wallet", Some("/keyos/apps/bitcoin/app.elf")),
            app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH)),
        ]);

        assert!(registry.is_built_in(&decode_app_id_str(built_in_id).unwrap()));
        assert!(!registry.is_built_in(&decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()));
        assert!(!registry.is_built_in(&decode_app_id_str("0xffffffffffffffffffffffffffffffff").unwrap()));
    }

    #[test]
    fn display_name_falls_back_when_the_manifest_lacks_the_locale() {
        let app_id = decode_app_id_str(THIRD_PARTY_APP_ID).unwrap();
        let registry =
            registry_with(vec![app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH))]);

        assert_eq!(registry.display_name(&app_id, "en").as_deref(), Some("Example App"));
        assert_eq!(registry.display_name(&app_id, "es").as_deref(), Some("Example App"));

        let unknown = decode_app_id_str("0xffffffffffffffffffffffffffffffff").unwrap();
        assert_eq!(registry.display_name(&unknown, "en"), None, "only an absent app has no name");
    }

    #[test]
    fn list_apps_filters_and_tags_sideloaded_flux_apps() {
        let flux_elf = format!("{SIDELOADED_APPS_DIR}/{THIRD_PARTY_APP_DIR}/app.elf");
        let mut flux_app = app_info(THIRD_PARTY_APP_ID, "Monero", Some(&flux_elf));
        flux_app.is_flux = true;
        let built_in_id = "0x426974636f696e2057616c6c65740000";
        let built_in = built_in_app_info(built_in_id, "Bitcoin Wallet", Some("/keyos/apps/bitcoin/app.elf"));
        let registry = registry_with(vec![flux_app, built_in]);
        let grants = PermissionGrantStore::default();

        let third_party = registry.list_apps("en", &[], &app_manager::AppFilter::sideloaded_only(), &grants);
        assert_eq!(third_party.len(), 1);
        assert!(third_party[0].is_flux, "Settings sees the sideloaded flux app tagged as flux");
        assert!(third_party[0].can_remove);

        let flux = registry.list_apps("en", &[], &app_manager::AppFilter::flux_only(), &grants);
        assert_eq!(flux.len(), 1, "the emulator grid sees the sideloaded flux app");
        assert_eq!(flux[0].name, "Monero");

        let standard = registry.list_apps("en", &[], &app_manager::AppFilter::standard_only(), &grants);
        assert!(standard.iter().all(|app| app.name != "Monero"), "the launcher never sees flux apps");
    }

    #[test]
    fn built_in_app_launches_without_a_publisher_fingerprint() {
        let mut app = built_in_app_info(
            "0x426974636f696e2057616c6c65740000",
            "Bitcoin Wallet",
            Some("/keyos/apps/bitcoin/app.elf"),
        );
        app.manifest.publisher = Some("Different Publisher".to_string());

        assert_eq!(app.publisher_and_launch_error(&latest_keyos_version(), &[]), (String::new(), None));
    }

    #[test]
    fn qr_match_rules_filter_by_app_id_and_empty_filter_returns_all() {
        let requested_id = "0x426974636f696e2057616c6c65740000";
        let other_id = "0x53656564205661756c74000000000000";
        let rule = QrMatchRule {
            id: "test".to_string(),
            priority: QrPriority::default(),
            id_localizations: BTreeMap::new(),
            sub_rules: BTreeMap::from([(
                "qr".to_string(),
                QrMatchSubRule::QR { min_len: None, max_len: None, regex_pattern: None },
            )]),
        };
        let mut requested = built_in_app_info(requested_id, "Bitcoin Wallet", None);
        requested.manifest.qr_match_rules.push(rule.clone());
        let mut other = built_in_app_info(other_id, "Seed Vault", None);
        other.manifest.qr_match_rules.push(rule);
        let registry = registry_with(vec![requested, other]);
        let requested_id = decode_app_id_str(requested_id).unwrap();

        let filtered = registry.qr_match_rules(&[requested_id], &[]);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, requested_id);
        assert_eq!(registry.qr_match_rules(&[], &[]).len(), 2);
    }

    #[test]
    fn qr_match_rules_exclude_apps_requiring_newer_keyos() {
        let rule = QrMatchRule {
            id: "test".to_string(),
            priority: QrPriority::default(),
            id_localizations: BTreeMap::new(),
            sub_rules: BTreeMap::from([(
                "qr".to_string(),
                QrMatchSubRule::QR { min_len: None, max_len: None, regex_pattern: None },
            )]),
        };
        let mut app = app_info_with_trust(
            THIRD_PARTY_APP_ID,
            "Legacy App",
            Some(THIRD_PARTY_ELF_PATH),
            AppResourcesRoot::Sideloaded,
            AppSignature::Foundation,
        );
        app.manifest.min_keyos_version = Some(semver::Version::parse("1.5.0").unwrap());
        app.manifest.qr_match_rules.push(rule);
        let mut registry = registry_with(vec![app]);
        registry.current_keyos_version = Version::parse("1.4.0").unwrap();

        assert!(registry.qr_match_rules(&[], &[]).is_empty());
    }

    // ---------------------------------------------------------------------------------------------
    // AppRegistryDiff tests.
    // ---------------------------------------------------------------------------------------------

    const OTHER_APP_ID: &str = "0x426974636f696e2057616c6c65740000";

    fn as_map(apps: Vec<AppInfo>) -> HashMap<AppId, AppInfo> {
        apps.into_iter().map(|app| (app.id, app)).collect()
    }

    #[test]
    fn diff_reports_new_and_removed_apps() {
        let before = as_map(vec![app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH))]);
        let after = as_map(vec![built_in_app_info(OTHER_APP_ID, "Bitcoin Wallet", None)]);

        let diff = AppRegistryDiff::new(&before, &after);

        assert_eq!(diff.installed, vec![decode_app_id_str(OTHER_APP_ID).unwrap()]);
        assert_eq!(diff.removed, vec![decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()]);
    }

    #[test]
    fn diff_ignores_an_id_with_an_unchanged_manifest() {
        let app = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        let before = as_map(vec![app.clone()]);
        let after = as_map(vec![app]);

        let diff = AppRegistryDiff::new(&before, &after);

        assert!(diff.installed.is_empty());
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn diff_can_report_a_successful_same_manifest_reinstall() {
        let app = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        let before = as_map(vec![app.clone()]);
        let after = as_map(vec![app]);

        let mut diff = AppRegistryDiff::new(&before, &after);
        diff.mark_installed(decode_app_id_str(THIRD_PARTY_APP_ID).unwrap());
        diff.mark_installed(decode_app_id_str(THIRD_PARTY_APP_ID).unwrap());

        assert_eq!(diff.installed, vec![decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()]);
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn diff_can_report_an_already_absent_removal() {
        let app_id = decode_app_id_str(THIRD_PARTY_APP_ID).unwrap();
        let mut diff = AppRegistryDiff::default();

        diff.mark_removed(app_id);
        diff.mark_removed(app_id);

        assert!(diff.installed.is_empty());
        assert_eq!(diff.removed, vec![app_id]);
    }

    /// A sideload that overwrites an existing bundle in place (an app update) keeps the same app
    /// id in both scans; only the manifest content changes. Subscribers still need to hear about
    /// it, so it must surface as `installed`, not be treated as unchanged.
    #[test]
    fn diff_reports_a_same_id_manifest_change_as_installed() {
        let mut before_app = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        before_app.manifest_bytes = b"v1".to_vec();
        let mut after_app = before_app.clone();
        after_app.manifest_bytes = b"v2".to_vec();

        let before = as_map(vec![before_app]);
        let after = as_map(vec![after_app]);

        let diff = AppRegistryDiff::new(&before, &after);

        assert_eq!(diff.installed, vec![decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()]);
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn diff_orders_ids_by_byte_value_regardless_of_hashmap_iteration_order() {
        let before = HashMap::new();
        let after = as_map(vec![
            built_in_app_info(OTHER_APP_ID, "Bitcoin Wallet", None),
            app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH)),
        ]);

        let diff = AppRegistryDiff::new(&before, &after);

        let mut expected =
            vec![decode_app_id_str(OTHER_APP_ID).unwrap(), decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()];
        expected.sort_by_key(|id| id.0);
        assert_eq!(diff.installed, expected);
    }
}
