// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet, HashMap};

use app_manager::{
    AppQrMatchRules, IconVariant, InstalledAppInfo, InstalledAppPermissionGroup,
    InstalledAppPermissionSubgroup, LaunchError, PermissionRequestInfo, PermissionRequestInfoResult,
    ThirdPartyCertificateInfo, SIDELOADED_APPS_DIR, SIDELOADED_FLUX_APPS_DIR,
};
use app_manifest::{Locale, Manifest, RequiredSignature};
use fs::messages::AppResourcesRoot;
use log::error;
use serde_json::to_vec;
use xous::{AppId, PID};

use crate::{
    permission_catalog::{self, ServerPermissionCache},
    permission_grants::{PermissionGrantState, PermissionGrantStore},
    FileSystem,
};

const BUILT_IN_APPS_DIR: &str = "/keyos/apps";
/// The sideload bundle roots (shared with usb-debug via `app_manager`) whose dir names must
/// equal the app id (see `sideloaded_app_dir_matches_app_id`).
const SIDELOAD_ROOTS: &[&str] = &[SIDELOADED_APPS_DIR, SIDELOADED_FLUX_APPS_DIR];
/// The Flux emulator host's AppId: the 16 ASCII bytes of `gui-app-emu-flux`.
pub(crate) const FLUX_EMULATOR_APP_ID: AppId = AppId(*b"gui-app-emu-flux");
/// Built-in apps the OS permits the user to permanently delete, as a deliberate exception
/// to the "only sideloaded apps are removable" rule. The trusted OS binary decides this,
/// never the app's own (signed-but-self-asserted) manifest.
const REMOVABLE_BUILTIN: &[AppId] = &[FLUX_EMULATOR_APP_ID];
// Each icon file holds one 110x110 RGBA archived RawImage (~47 KiB of pixels)
// plus rkyv header/alignment overhead. Leave margin for format drift and
// oversized sources.
const MAX_APP_ICON_SIZE_BYTES: u64 = 300 * 1024;
const MAX_MANIFEST_SIZE_BYTES: u64 = 128 * 1024;
/// Filename of a sideloaded app's icon within its bundle, next to `app.elf`. The SDK writes it
/// here, mirroring this name with its own constant (it can't depend on this crate). Built-in
/// icons instead live in CommonAssets (`app-icons/<app-id>.bin`).
const BUNDLED_ICON_FILE: &str = "icon.bin";
/// Filename of the dark-theme icon beside [`BUNDLED_ICON_FILE`], staged only by apps that ship
/// one; the light icon serves both themes otherwise. Built-in dark icons are the
/// `app-icons/<app-id>-dark.bin` sibling in CommonAssets.
const BUNDLED_DARK_ICON_FILE: &str = "icon-dark.bin";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppSource {
    BuiltIn,
    ThirdParty,
}

impl AppSource {
    /// The signature level an app from this source carries: built-ins are Foundation-signed,
    /// sideloads are third-party-signed. (Foundation-signed sideloads are not supported yet; the
    /// load directory stands in for the signer until a later PR.)
    fn signature(self) -> RequiredSignature {
        match self {
            AppSource::BuiltIn => RequiredSignature::Foundation,
            AppSource::ThirdParty => RequiredSignature::ThirdParty,
        }
    }
}

/// How a declared message is available to an app once its signature is taken into account:
/// granted automatically, granted through the user's subgroup decision, or not reachable at
/// all (the signature requirement isn't met, or the message is not user-facing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageAvailability {
    AutoAllow,
    ApprovalBased,
    Unavailable,
}

const FLUX_APPS_DIR: &str = "/keyos/apps/gui-app-emu-flux/apps";

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
    source: AppSource,
    is_flux: bool,
    binary_size: Option<u64>,
    /// The developer key that signed this sideloaded app's manifest, captured at scan; trust is
    /// decided later by matching it against the cert store. `None` for built-in apps and on hosted.
    third_party_signer: Option<[u8; 33]>,
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
}

#[derive(Debug, Default)]
pub(crate) struct AppRegistry {
    installed_apps: HashMap<AppId, AppInfo>,
    running_apps: HashMap<PID, RunningAppInfo>,
}

impl AppRegistry {
    pub(crate) fn scan_installed_apps(&mut self) -> anyhow::Result<(ServerPermissionCache, AppRegistryDiff)> {
        let mut installed_apps = HashMap::new();

        // Build the per-server cache as we scan. Adding a manifest also detects server-name
        // collisions, so an app that declares a server already owned by a system service or an
        // earlier app is rejected. Seed it with the system services first.
        let mut cache = ServerPermissionCache::default();
        for manifest in permission_catalog::system_manifests() {
            cache.add_manifest(manifest).expect("system manifests must not declare colliding servers");
        }

        // App location is the source of truth for both trust classification and the Flux
        // tag: firmware-shipped apps live under /keyos/apps and verify against the official
        // keys, while sideloaded apps live under the sideload roots and only need a valid
        // developer signature here. The simulator reads the same dirs through fs and signs
        // nothing.
        Self::scan_apps_dir(&mut installed_apps, &mut cache, BUILT_IN_APPS_DIR, AppSource::BuiltIn, false);
        Self::scan_apps_dir(&mut installed_apps, &mut cache, FLUX_APPS_DIR, AppSource::BuiltIn, true);
        Self::scan_apps_dir(
            &mut installed_apps,
            &mut cache,
            SIDELOADED_APPS_DIR,
            AppSource::ThirdParty,
            false,
        );
        Self::scan_apps_dir(
            &mut installed_apps,
            &mut cache,
            SIDELOADED_FLUX_APPS_DIR,
            AppSource::ThirdParty,
            true,
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
        installed_apps: &mut HashMap<AppId, AppInfo>,
        cache: &mut ServerPermissionCache,
        apps_dir: &str,
        source: AppSource,
        is_flux: bool,
    ) {
        let dir = match FileSystem::default().open_dir(apps_dir.to_string(), fs::Location::System) {
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
            match Self::load_app(&app_dir, source, is_flux) {
                Ok(Some(app)) if installed_apps.contains_key(&app.id) => {
                    log::warn!("Skipping duplicate app_id=0x{} from {source:?}", hex::encode(app.id.0));
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
    fn load_app(app_dir: &str, source: AppSource, is_flux: bool) -> anyhow::Result<Option<AppInfo>> {
        let manifest_raw = read_capped_file(
            &format!("{app_dir}/manifest.json"),
            fs::Location::System,
            MAX_MANIFEST_SIZE_BYTES,
        )?;
        let (manifest_json, third_party_signer) = check_manifest_signature(&manifest_raw, source)?;
        let manifest = app_manifest::try_from_bytes(manifest_json)
            .map_err(|e| anyhow::anyhow!("invalid manifest: {e}"))?;

        let app_id = AppId(manifest.app_id);
        if !sideloaded_app_dir_matches_app_id(Some(app_dir), &app_id, source) {
            return Ok(None);
        }
        Ok(Some(AppInfo {
            id: app_id,
            app_dir: Some(app_dir.to_string()),
            manifest,
            manifest_bytes: manifest_json.to_vec(),
            source,
            is_flux,
            binary_size: None,
            third_party_signer,
        }))
    }

    pub(crate) fn app_name_by_id(&self, id: &AppId, locale: &str) -> Option<String> {
        self.installed_apps
            .get(id)
            .and_then(|app_info| app_info.manifest.app_name.get(&locale.to_string().into()).cloned())
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
            .filter(|app_info| app_info.publisher_and_launchable(publishers).1)
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
        &mut self,
        locale: &str,
        trusted_publishers: &[ThirdPartyCertificateInfo],
        filter: &app_manager::AppFilter,
        permission_grants: &PermissionGrantStore,
    ) -> Vec<InstalledAppInfo> {
        let ids = self
            .installed_apps
            .values()
            .filter(|app_info| filter.is_flux.map_or(true, |want| app_info.is_flux == want))
            .filter(|app_info| filter.third_party.map_or(true, |want| app_info.is_third_party() == want))
            .map(|app_info| app_info.id)
            .collect::<Vec<_>>();

        let mut apps = Vec::with_capacity(ids.len());
        for id in ids {
            // The binary size is cached lazily, so it takes the only mutable borrow; the
            // permission sections then resolve policy across all installed manifests immutably.
            let Some(app_info) = self.installed_apps.get_mut(&id) else { continue };
            let size_bytes = app_info.binary_size();
            let app_info = &self.installed_apps[&id];
            let name = app_info.localized_name(locale);
            let (publisher, can_launch) = app_info.publisher_and_launchable(trusted_publishers);
            let (basic_permissions, approvable_permissions) =
                app_info.permission_groups(permission_grants, locale);
            apps.push(InstalledAppInfo {
                app_id: format!("0x{}", app_info.id),
                publisher,
                can_launch,
                can_remove: app_info.is_removable(),
                is_flux: app_info.is_flux,
                version: app_info.manifest.version.clone().unwrap_or_default(),
                size_bytes,
                description: app_info.description(),
                basic_permissions,
                approvable_permissions,
                name,
            });
        }

        apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        apps
    }

    pub(crate) fn app_name_requiring_third_party_key(
        &self,
        public_key: &str,
        locale: &str,
    ) -> Option<String> {
        let public_key = crate::third_party_certs::decode_public_key_hex(public_key)?;

        self.installed_apps.values().filter(|app_info| app_info.is_third_party()).find_map(|app_info| {
            (app_info.third_party_signer == Some(public_key)).then(|| app_info.localized_name(locale))
        })
    }

    pub(crate) fn elf_path(&self, app_id: AppId) -> Option<String> {
        self.installed_apps.get(&app_id).and_then(AppInfo::elf_path)
    }

    /// Blind-read the app's icon, returning `None` when the app ships no icon (the common
    /// case) or it can't be read. Most apps ship no dark variant, so a missing one is not an
    /// error: the light icon answers a dark-variant request instead. Built-in icons live in
    /// CommonAssets (keyed by app id); sideloaded icons live in the app bundle.
    pub(crate) fn app_icon_bytes(&self, app_id: AppId, variant: IconVariant) -> Option<Vec<u8>> {
        let app = self.installed_apps.get(&app_id)?;
        if matches!(variant, IconVariant::Dark) {
            if let Some((path, location)) = app.icon_path(IconVariant::Dark) {
                match read_capped_file(&path, location, MAX_APP_ICON_SIZE_BYTES) {
                    Ok(bytes) if !bytes.is_empty() => return Some(bytes),
                    // An empty file would decode to a blank image, so let the light icon answer.
                    Ok(_) => log::warn!("dark app icon for app_id=0x{app_id} is empty"),
                    Err(e) if is_file_not_found(&e) => {}
                    Err(e) => log::warn!("failed to read the dark app icon for app_id=0x{app_id}: {e:?}"),
                }
            }
        }

        let (path, location) = app.icon_path(IconVariant::Light)?;
        read_capped_file(&path, location, MAX_APP_ICON_SIZE_BYTES)
            .map_err(|e| log::warn!("failed to read app icon for app_id=0x{app_id}: {e:?}"))
            .ok()
    }

    pub(crate) fn app_resources_location(&self, app_id: AppId) -> Option<AppResourcesLocation> {
        self.installed_apps.get(&app_id).and_then(AppInfo::app_resources_location)
    }

    /// Whether the app may launch: a sideloaded app's signer must match a currently-valid cert.
    pub(crate) fn is_launchable(&self, app_id: AppId, publishers: &[ThirdPartyCertificateInfo]) -> bool {
        self.installed_apps.get(&app_id).is_some_and(|app| app.publisher_and_launchable(publishers).1)
    }

    /// The bundle file hashes from the app's manifest, verified and stored at scan time. Launch
    /// checks the files against these without re-reading or re-verifying the manifest.
    #[cfg(keyos)]
    pub(crate) fn file_hashes(&self, app_id: AppId) -> Option<std::collections::BTreeMap<String, String>> {
        self.installed_apps.get(&app_id).map(|app_info| app_info.manifest.file_hashes.clone())
    }

    /// The developer key that signed a sideloaded app, captured at scan; `None` for a built-in app
    /// (signed with the official key).
    #[cfg(keyos)]
    pub(crate) fn elf_signer(&self, app_id: AppId) -> Option<[u8; 33]> {
        self.installed_apps.get(&app_id).and_then(|app_info| app_info.third_party_signer)
    }

    pub(crate) fn contains_app(&self, app_id: AppId) -> bool { self.installed_apps.contains_key(&app_id) }

    /// AppIds of every installed Flux child (built-in or sideloaded). The emulator host
    /// itself is not flux, so this returns the children only. Each child persists its NVM
    /// to its own AppData, a tree that removing the emulator does not otherwise touch.
    pub(crate) fn flux_child_app_ids(&self) -> Vec<AppId> {
        self.installed_apps.values().filter(|a| a.is_flux).map(|a| a.id).collect()
    }

    pub(crate) fn is_running(&self, app_id: &AppId) -> bool {
        self.running_apps.values().any(|running_app| running_app.info.id == *app_id)
    }

    pub(crate) fn removable_bundle_dir(&self, app_id: AppId) -> Option<String> {
        let app_info = self.installed_apps.get(&app_id)?;
        if !app_info.is_removable() {
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
        // Built-in permissions are not user-managed (see effective_manifest_bytes), so there is
        // nothing to grant or revoke for them.
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
        // Built-ins bypass the permission mechanism and are never parked for a prompt; this is
        // defensive so a built-in message can't be routed through the grant flow.
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
    fn elf_path(&self) -> Option<String> { self.app_dir.as_deref().map(|dir| format!("{dir}/app.elf")) }

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
        // A message restricted to Flux children is reachable only by an app the OS classified as
        // one (from its install directory, not a manifest claim); any other app is refused.
        if entry.requires_flux() && !self.is_flux {
            MessageAvailability::Unavailable
        } else if !entry.signature_satisfied_by(self.source.signature()) {
            MessageAvailability::Unavailable
        } else if entry.is_auto_allow() {
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
        // Built-ins run with everything they declare and are not user-managed: trust comes from
        // the /keyos/apps directory itself, so they bypass filtering, first-use prompts, and
        // Settings entirely. Only sideloaded apps get their manifest narrowed to the granted set.
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

    /// The publisher name to show and whether the app may launch. A sideloaded app is launchable
    /// only while its signer matches one of the currently-valid `publishers`; neither built-in nor
    /// hosted apps carry a publisher name. The simulator signs nothing, so it launches everything.
    fn publisher_and_launchable(&self, publishers: &[ThirdPartyCertificateInfo]) -> (String, bool) {
        #[cfg(all(not(keyos), not(test)))]
        {
            let _ = publishers;
            (String::new(), true)
        }
        #[cfg(any(keyos, test))]
        {
            let Some(signer) = self.third_party_signer else {
                return (String::new(), self.source == AppSource::BuiltIn);
            };
            match publishers
                .iter()
                .find(|p| crate::third_party_certs::decode_public_key_hex(&p.public_key) == Some(signer))
            {
                Some(publisher) => (publisher.name.clone(), true),
                None => (String::new(), false),
            }
        }
    }

    fn description(&self) -> String { self.manifest.description.clone().unwrap_or_default() }

    fn icon_path(&self, variant: IconVariant) -> Option<(String, fs::Location)> {
        match self.source {
            AppSource::BuiltIn => {
                let suffix = match variant {
                    IconVariant::Light => "",
                    IconVariant::Dark => "-dark",
                };
                Some((format!("app-icons/{}{suffix}.bin", self.id), fs::Location::CommonAssets))
            }
            AppSource::ThirdParty => {
                let file = match variant {
                    IconVariant::Light => BUNDLED_ICON_FILE,
                    IconVariant::Dark => BUNDLED_DARK_ICON_FILE,
                };
                let app_dir = self.app_dir.as_deref()?;
                Some((format!("{app_dir}/{file}"), fs::Location::System))
            }
        }
    }

    fn app_resources_location(&self) -> Option<AppResourcesLocation> {
        let app_dir = self.app_dir.as_deref()?;
        let app_dir = app_dir.rsplit('/').next()?;
        if app_dir.is_empty() || app_dir == "." || app_dir == ".." {
            return None;
        }

        let root = match (self.source, self.is_flux) {
            (AppSource::BuiltIn, _) => AppResourcesRoot::BuiltIn,
            (AppSource::ThirdParty, true) => AppResourcesRoot::SideloadedFlux,
            (AppSource::ThirdParty, false) => AppResourcesRoot::Sideloaded,
        };
        let app_dir = match self.source {
            AppSource::BuiltIn => app_dir.to_string(),
            AppSource::ThirdParty => self.id.to_string(),
        };

        Some(AppResourcesLocation { root, app_dir })
    }

    fn binary_size(&mut self) -> u64 {
        if self.binary_size.is_none() {
            // The simulator can't read the app size: the elf isn't in the image.
            #[cfg(keyos)]
            {
                self.binary_size =
                    Some(self.elf_path().as_deref().and_then(|path| file_size(path).ok()).unwrap_or(0));
            }
            #[cfg(not(keyos))]
            {
                self.binary_size = Some(0);
            }
        }
        self.binary_size.unwrap_or(0)
    }

    fn is_third_party(&self) -> bool { self.source == AppSource::ThirdParty }

    fn is_removable(&self) -> bool { self.is_third_party() || REMOVABLE_BUILTIN.contains(&self.id) }
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
fn read_capped_file(path: &str, location: fs::Location, max_size_bytes: u64) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;

    let fs = FileSystem::default();
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

#[cfg(keyos)]
fn file_size(path: &str) -> anyhow::Result<u64> {
    Ok(FileSystem::default().metadata(path, fs::Location::System)?.size)
}

/// Verify a bundle manifest and return its header-stripped JSON together with the developer key
/// that signed a sideloaded one (`None` for a built-in app). A built-in manifest must carry a
/// valid official signature; production requires it trusted. A sideloaded manifest only needs a
/// valid developer signature here, since whether its key is trusted is decided at launch and
/// listing time against the cert store.
#[cfg(keyos)]
fn check_manifest_signature(
    manifest_raw: &[u8],
    source: AppSource,
) -> anyhow::Result<(&[u8], Option<[u8; 33]>)> {
    // Drop the cosign2 header, leaving the JSON it wraps.
    let manifest_json = manifest_raw
        .get(cosign2::Header::DEFAULT_SIZE..)
        .ok_or_else(|| anyhow::anyhow!("manifest is too short to hold a cosign2 header"))?;

    let crypto = crate::CryptoApi::default();
    let signer = match source {
        AppSource::BuiltIn => {
            fw_utils::hash::verify_cosign2_mem(&crypto, manifest_raw, cfg!(feature = "production"))
                .map_err(|e| anyhow::anyhow!("unverified manifest: {e:?}"))?;
            None
        }
        AppSource::ThirdParty => {
            let header = fw_utils::hash::verify_cosign2_mem_third_party(&crypto, manifest_raw)
                .map_err(|e| anyhow::anyhow!("unverified manifest: {e:?}"))?;
            Some(header.pubkey2())
        }
    };
    Ok((manifest_json, signer))
}

/// Hosted manifests are unsigned, so the raw bytes are the JSON and there is no signer.
#[cfg(not(keyos))]
fn check_manifest_signature(
    manifest_raw: &[u8],
    _source: AppSource,
) -> anyhow::Result<(&[u8], Option<[u8; 33]>)> {
    Ok((manifest_raw, None))
}

fn sideloaded_app_dir_matches_app_id(app_dir: Option<&str>, app_id: &AppId, source: AppSource) -> bool {
    if source != AppSource::ThirdParty {
        return true;
    }

    let Some(app_dir) = app_dir else {
        return true;
    };

    if !SIDELOAD_ROOTS.iter().any(|root| app_dir.starts_with(root)) {
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
    SIDELOAD_ROOTS.iter().find_map(|root| path.strip_prefix(root).and_then(|path| path.strip_prefix('/')))
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

    fn app_info(app_id: &str, name: &str, elf_path: Option<&str>) -> AppInfo {
        let source = if elf_path.is_some() { AppSource::ThirdParty } else { AppSource::BuiltIn };
        app_info_with_source(app_id, name, elf_path, source)
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
        app_info_with_source(app_id, name, elf_path, AppSource::BuiltIn)
    }

    fn app_info_with_source(app_id: &str, name: &str, elf_path: Option<&str>, source: AppSource) -> AppInfo {
        AppInfo {
            id: decode_app_id_str(app_id).unwrap(),
            app_dir: elf_path.map(|path| path.strip_suffix("/app.elf").unwrap_or(path).to_owned()),
            manifest: Manifest {
                app_name: BTreeMap::from([(Locale("en".to_string()), name.to_string())]),
                app_id: app_manifest::parse_app_id_bytes(app_id).unwrap(),
                publisher: None,
                description: None,
                version: None,
                servers: BTreeMap::new(),
                fixed_sids: BTreeMap::new(),
                permissions: BTreeMap::new(),
                memory: Vec::new(),
                syscall: Vec::new(),
                qr_match_rules: Vec::new(),
                file_hashes: BTreeMap::new(),
            },
            manifest_bytes: Vec::new(),
            source,
            is_flux: false,
            binary_size: None,
            third_party_signer: None,
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
            running_apps: HashMap::new(),
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
        let mut registry = registry_with(vec![app_info(THIRD_PARTY_APP_ID, "System Manifest", None)]);

        assert!(registry
            .list_apps(
                "en",
                &[],
                &app_manager::AppFilter::third_party_only(),
                &PermissionGrantStore::default()
            )
            .is_empty());
    }

    // A valid compressed-key prefix (0x02) followed by zeroes; decode_public_key_hex only checks
    // the prefix, so it stands in for a developer signer without needing a real curve point.
    const SIGNER_HEX: &str = "020000000000000000000000000000000000000000000000000000000000000000";

    fn signer_bytes() -> [u8; 33] { crate::third_party_certs::decode_public_key_hex(SIGNER_HEX).unwrap() }

    fn publisher_cert(public_key_hex: &str, name: &str) -> ThirdPartyCertificateInfo {
        ThirdPartyCertificateInfo {
            name: name.to_string(),
            company: String::new(),
            contact_email: String::new(),
            support_url: String::new(),
            public_key: public_key_hex.to_string(),
            not_before_unix_seconds: None,
            not_after_unix_seconds: None,
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
        app.third_party_signer = Some(signer_bytes());

        // No matching publisher: not launchable and no publisher name to show.
        assert_eq!(app.publisher_and_launchable(&[]), (String::new(), false));

        // A publisher whose key matches the stored signer makes it launchable under that name.
        let publishers = vec![publisher_cert(SIGNER_HEX, "Acme")];
        assert_eq!(app.publisher_and_launchable(&publishers), ("Acme".to_string(), true));
    }

    #[test]
    fn is_launchable_tracks_signer_and_builtin() {
        let built_in_id = "0x426974636f696e2057616c6c65740000";
        let mut sideloaded = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        sideloaded.third_party_signer = Some(signer_bytes());
        let registry = registry_with(vec![
            sideloaded,
            built_in_app_info(built_in_id, "Bitcoin Wallet", Some("/keyos/apps/bitcoin/app.elf")),
        ]);

        let third_party = decode_app_id_str(THIRD_PARTY_APP_ID).unwrap();
        assert!(registry.is_launchable(third_party, &[publisher_cert(SIGNER_HEX, "Acme")]));
        assert!(!registry.is_launchable(third_party, &[]));
        // Built-in apps launch regardless of publishers; an unknown id never does.
        assert!(registry.is_launchable(decode_app_id_str(built_in_id).unwrap(), &[]));
        assert!(
            !registry.is_launchable(decode_app_id_str("0xffffffffffffffffffffffffffffffff").unwrap(), &[])
        );
    }

    #[test]
    fn permission_request_info_prompts_for_requested_approval_based_permission() {
        // Provide the camera server's manifest through an installed app so the message-id
        // lookup does not depend on the xtask-generated SYSTEM_MANIFESTS (empty under plain
        // `cargo test`).
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
                    required_type: None,
                    approval: app_manifest::ApprovalBehavior::GrantOnFirstUse,
                },
            )]),
        )]);
        let registry =
            registry_with(vec![third_party_app_with_permissions(&[("os/camera", &["Subscribe"])]), camera]);

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
    fn installed_apps_include_manifest_description_and_version_without_trusting_publisher() {
        let mut app = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        app.manifest.publisher = Some("Example Publisher".to_string());
        app.manifest.description = Some("Example description".to_string());
        app.manifest.version = Some("1.2.3".to_string());
        let mut registry = registry_with(vec![app]);

        let apps = registry.list_apps(
            "en",
            &[],
            &app_manager::AppFilter::third_party_only(),
            &PermissionGrantStore::default(),
        );

        assert!(apps[0].publisher.is_empty());
        assert!(!apps[0].can_launch);
        assert_eq!(apps[0].description, "Example description");
        assert_eq!(apps[0].version, "1.2.3");
    }

    #[test]
    fn sideloaded_flux_dir_name_must_match_app_id() {
        let app_id = decode_app_id_str(THIRD_PARTY_APP_ID).unwrap();

        let matching = format!("{SIDELOADED_FLUX_APPS_DIR}/{THIRD_PARTY_APP_DIR}");
        assert!(sideloaded_app_dir_matches_app_id(Some(&matching), &app_id, AppSource::ThirdParty));

        let mismatched = format!("{SIDELOADED_FLUX_APPS_DIR}/ffffffffffffffffffffffffffffffff");
        assert!(!sideloaded_app_dir_matches_app_id(Some(&mismatched), &app_id, AppSource::ThirdParty));
    }

    #[test]
    fn list_apps_filters_and_tags_sideloaded_flux_apps() {
        let flux_elf = format!("{SIDELOADED_FLUX_APPS_DIR}/{THIRD_PARTY_APP_DIR}/app.elf");
        let mut flux_app = app_info(THIRD_PARTY_APP_ID, "Monero", Some(&flux_elf));
        flux_app.is_flux = true;
        let built_in_id = "0x426974636f696e2057616c6c65740000";
        let built_in = built_in_app_info(built_in_id, "Bitcoin Wallet", Some("/keyos/apps/bitcoin/app.elf"));
        let mut registry = registry_with(vec![flux_app, built_in]);
        let grants = PermissionGrantStore::default();

        let third_party = registry.list_apps("en", &[], &app_manager::AppFilter::third_party_only(), &grants);
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
    fn built_in_app_launches_without_a_publisher_name() {
        let mut app = built_in_app_info(
            "0x426974636f696e2057616c6c65740000",
            "Bitcoin Wallet",
            Some("/keyos/apps/bitcoin/app.elf"),
        );
        app.manifest.publisher = Some("Different Publisher".to_string());

        assert_eq!(app.publisher_and_launchable(&[]), (String::new(), true));
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
