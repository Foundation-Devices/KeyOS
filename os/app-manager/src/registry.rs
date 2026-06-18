// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{collections::HashMap, io::Read};

use app_manager::{
    AppQrMatchRules, InstalledAppInfo, InstalledAppPermissionGroup, ThirdPartyCertificateInfo,
};
use app_manifest::{Locale, Manifest};
use fs::messages::AppResourcesRoot;
use log::error;
use regex::Regex;
use serde_json::to_vec;
use xous::{AppId, PID};

use crate::FileSystem;

#[cfg(keyos)]
mod hw;

#[cfg(keyos)]
use hw::{app_binary_size, read_app_bytes, read_app_header};

const FOUNDATION_PUBLISHER: &str = "Foundation Devices, Inc.";
const BUNDLED_ICON_FILE: &str = "icon.bin";
const BUILT_IN_APPS_DIR: &str = "/keyos/apps";
pub const SIDELOADED_APPS_DIR: &str = "/keyos/sideloaded-apps";
const MAX_APP_ICON_SIZE_BYTES: u64 = 256 * 1024;
#[cfg(keyos)]
const MAX_THIRD_PARTY_KEY_CHECK_APP_SIZE: u64 = 16 * 1024 * 1024;
const INSTALLED_APP_NAME_MAX_BYTES: usize = 128;
const INSTALLED_APP_PUBLISHER_MAX_BYTES: usize = 128;
const INSTALLED_APP_VERSION_MAX_BYTES: usize = 64;
const INSTALLED_APP_DESCRIPTION_MAX_BYTES: usize = 4 * 1024;
const INSTALLED_APP_PERMISSION_LINES_MAX: usize = 64;
const INSTALLED_APP_PERMISSION_LINE_MAX_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppSource {
    BuiltIn,
    ThirdParty,
}

#[cfg(keyos)]
#[derive(Debug, Clone, Copy, Default)]
enum ThirdPartySignatureCache {
    #[default]
    Unknown,
    Verified([u8; 33]),
    Invalid([u8; 33]),
}

/// Removes sub-rules with invalid regex patterns, then removes rules that become empty.
/// Logs errors for each dropped sub-rule or empty rule.
pub(crate) fn prune_qr_match_rules(rules: &mut Vec<app_manifest::QrMatchRule>, app_id: &AppId) {
    rules.retain_mut(|rule| {
        rule.sub_rules.retain(|sub_rule_id, sub_rule| {
            let app_manifest::QrMatchSubRule::QR { regex_pattern: Some(pattern), .. } = sub_rule else {
                return true;
            };
            match Regex::new(pattern) {
                Ok(_) => true,
                Err(e) => {
                    error!(
                        "Dropping sub-rule {:?} in rule {:?} for app 0x{} due to invalid regex: {}",
                        sub_rule_id, rule.id, app_id, e
                    );
                    false
                }
            }
        });
        if rule.sub_rules.is_empty() {
            error!("Rule {:?} for app 0x{} has no sub-rules and will never match", rule.id, app_id);
            false
        } else {
            true
        }
    });
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
    /// Raw manifest bytes, kept so the name server can be told about the app at
    /// launch (see [`AppRegistry::register_app_names`]).
    manifest_bytes: Vec<u8>,
    source: AppSource,
    #[cfg_attr(not(keyos), allow(dead_code))]
    is_flux: bool,
    binary_size: Option<u64>,
    #[cfg(keyos)]
    third_party_signature_cache: ThirdPartySignatureCache,
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

#[derive(Debug, Default)]
pub(crate) struct AppRegistry {
    installed_apps: HashMap<AppId, AppInfo>,
    running_apps: HashMap<PID, RunningAppInfo>,
}

impl AppRegistry {
    pub(crate) fn scan_installed_apps(&mut self) -> anyhow::Result<()> {
        let mut installed_apps = HashMap::new();

        // App location is the source of truth for trust classification:
        // firmware-shipped apps live under /keyos/apps, sideloaded apps under
        // /keyos/sideloaded-apps. The simulator reads the same dirs through fs.
        // Trust is enforced at launch (verify_app), so enumeration doesn't filter.
        Self::scan_apps_dir(&mut installed_apps, BUILT_IN_APPS_DIR, AppSource::BuiltIn, false);
        Self::scan_apps_dir(&mut installed_apps, FLUX_APPS_DIR, AppSource::BuiltIn, true);
        Self::scan_apps_dir(&mut installed_apps, SIDELOADED_APPS_DIR, AppSource::ThirdParty, false);

        self.installed_apps = installed_apps;
        log::info!("scan_installed_apps: registry tracks {} installed apps", self.installed_apps.len());

        Ok(())
    }

    /// Read every app bundle under `apps_dir` (a `Location::System` path) through
    /// fs and register it. A missing or unreadable dir is just logged and skipped;
    /// a real loading problem then shows up as a missing app.
    fn scan_apps_dir(
        installed_apps: &mut HashMap<AppId, AppInfo>,
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
                    installed_apps.insert(app.id, app);
                }
                Ok(None) => {}
                Err(e) => log::warn!("Skipping app bundle {app_dir}: {e:?}"),
            }
        }
    }

    /// Load and validate one app bundle's manifest. Returns `None` when a
    /// sideloaded bundle's dir name doesn't match its app id (already logged).
    fn load_app(app_dir: &str, source: AppSource, is_flux: bool) -> anyhow::Result<Option<AppInfo>> {
        let fs = FileSystem::default();
        let mut manifest_file =
            fs.open_file(format!("{app_dir}/manifest.json"), fs::Location::System, fs::OpenFlags::READ_ONLY)?;
        let mut manifest_bytes = vec![];
        manifest_file.read_to_end(&mut manifest_bytes)?;
        let mut manifest = app_manifest::try_from_bytes(&manifest_bytes)?;

        let app_id = AppId(manifest.app_id);
        if !sideloaded_app_dir_matches_app_id(Some(app_dir), &app_id, source) {
            return Ok(None);
        }
        prune_qr_match_rules(&mut manifest.qr_match_rules, &app_id);

        Ok(Some(AppInfo {
            id: app_id,
            app_dir: Some(app_dir.to_string()),
            manifest,
            manifest_bytes,
            source,
            is_flux,
            binary_size: None,
            #[cfg(keyos)]
            third_party_signature_cache: ThirdPartySignatureCache::Unknown,
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

    pub(crate) fn qr_match_rules(&self) -> Vec<AppQrMatchRules> {
        self.installed_apps
            .values()
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

    pub(crate) fn installed_apps(
        &mut self,
        locale: &str,
        trusted_publishers: &[ThirdPartyCertificateInfo],
    ) -> Vec<InstalledAppInfo> {
        let mut apps = self
            .installed_apps
            .values_mut()
            .filter(|app_info| app_info.is_third_party())
            .map(|app_info| {
                let name = app_info.localized_name(locale);
                let size_bytes = app_info.binary_size();
                let version = app_info.manifest.version.clone().unwrap_or_default();
                let (publisher, can_launch) = app_info.publisher_and_launchable(trusted_publishers);
                let mut installed_app = InstalledAppInfo {
                    app_id: format!("0x{}", app_info.id),
                    bundled_icon_path: app_info.bundled_icon_file_path(),
                    publisher,
                    can_launch,
                    version,
                    size_bytes,
                    description: app_info.description(),
                    permissions: app_info.permission_groups(),
                    name,
                };
                limit_installed_app_metadata(&mut installed_app);
                installed_app
            })
            .collect::<Vec<_>>();

        apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        apps
    }

    pub(crate) fn app_name_requiring_third_party_key(
        &mut self,
        public_key: &str,
        locale: &str,
    ) -> Option<String> {
        // Third-party signature verification is device-only; the simulator has
        // no signed apps to block on.
        #[cfg(not(keyos))]
        {
            let _ = (public_key, locale);
            return None;
        }
        #[cfg(keyos)]
        {
            let public_key = crate::third_party_certs::decode_public_key_hex(public_key)?;
            self.installed_apps.values_mut().filter(|app_info| app_info.is_third_party()).find_map(
                |app_info| {
                    app_info
                        .has_verified_third_party_signature(public_key)
                        .then(|| app_info.localized_name(locale))
                },
            )
        }
    }

    pub(crate) fn list_apps(
        &self,
        locale: &str,
        filter: &app_manager::AppFilter,
    ) -> Vec<app_manager::AppEntry> {
        self.installed_apps
            .values()
            .filter(|info| filter.is_flux.map_or(true, |want| info.is_flux == want))
            .map(|info| {
                let name = info
                    .manifest
                    .app_name
                    .get(&locale.to_string().into())
                    .cloned()
                    .unwrap_or_else(|| info.manifest.app_name_en());
                app_manager::AppEntry { app_id: format!("0x{}", info.id), name, is_flux: info.is_flux }
            })
            .collect()
    }

    pub(crate) fn elf_path(&self, app_id: AppId) -> Option<String> {
        self.installed_apps.get(&app_id).and_then(AppInfo::elf_path)
    }

    pub(crate) fn app_icon_bytes(&self, app_id: AppId) -> Option<Vec<u8>> {
        let path = self.installed_apps.get(&app_id)?.bundled_icon_file_path()?;
        match read_app_icon_bytes(&path, MAX_APP_ICON_SIZE_BYTES) {
            Ok(data) => Some(data),
            Err(e) => {
                log::warn!("failed to read bundled app icon for app_id=0x{app_id}: {e:?}");
                None
            }
        }
    }

    pub(crate) fn app_resources_location(&self, app_id: AppId) -> Option<AppResourcesLocation> {
        self.installed_apps.get(&app_id).and_then(AppInfo::app_resources_location)
    }

    pub(crate) fn requires_debug_signature_trust(&self, app_id: AppId) -> bool {
        self.installed_apps.get(&app_id).map(|app_info| !app_info.is_built_in()).unwrap_or(true)
    }

    pub(crate) fn is_built_in_app(&self, app_id: AppId) -> bool {
        self.installed_apps.get(&app_id).is_some_and(AppInfo::is_built_in)
    }

    pub(crate) fn contains_app(&self, app_id: AppId) -> bool { self.installed_apps.contains_key(&app_id) }

    pub(crate) fn is_running(&self, app_id: &AppId) -> bool {
        self.running_apps.values().any(|running_app| running_app.info.id == *app_id)
    }

    pub(crate) fn sideloaded_bundle_dir(&self, app_id: AppId) -> Option<String> {
        let app_info = self.installed_apps.get(&app_id)?;
        if app_info.source != AppSource::ThirdParty {
            return None;
        }

        let app_dir = app_info.app_dir.as_deref()?;
        if app_dir.rsplit('/').next()? != hex::encode(app_id.0) {
            return None;
        }

        Some(app_dir.to_string())
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

    /// Register the app's manifest names with the name server.
    pub(crate) fn register_app_names(&self, app_id: AppId) {
        if let Some(info) = self.installed_apps.get(&app_id) {
            register_manifest_with_names(&info.manifest_bytes, info.app_dir.as_deref().unwrap_or("app"));
        }
    }
}

#[cfg(any(keyos, all(not(test), not(keyos))))]
fn register_manifest_with_names(manifest_bytes: &[u8], app_label: &str) {
    let names =
        server::xous_names::XousNames::new().expect("xous-names should be available during app scanning");

    if let Err(error) = names.add_manifest(manifest_bytes) {
        log::error!("Could not send the manifest of {app_label} to the name server: {error:?}");
    }
}

#[cfg(all(test, not(keyos)))]
fn register_manifest_with_names(_manifest_bytes: &[u8], _app_label: &str) {
    // Plain Rust unit tests run outside the hosted Xous kernel.
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

    fn permission_groups(&self) -> Vec<InstalledAppPermissionGroup> {
        self.manifest
            .permissions
            .iter()
            .map(|(server, messages)| InstalledAppPermissionGroup {
                server: server.clone(),
                messages: messages.iter().cloned().collect(),
            })
            .collect()
    }

    fn publisher_and_launchable(
        &mut self,
        trusted_publishers: &[ThirdPartyCertificateInfo],
    ) -> (String, bool) {
        if self.is_built_in() {
            return (FOUNDATION_PUBLISHER.to_string(), true);
        }

        // Cosign2 publisher verification is device-only; the simulator can't
        // verify signatures, so a sideloaded app shows no verified publisher.
        #[cfg(keyos)]
        match self.verified_third_party_publisher(trusted_publishers) {
            Some(publisher) => (publisher.name.clone(), true),
            None => (String::new(), false),
        }
        #[cfg(not(keyos))]
        {
            let _ = trusted_publishers;
            (String::new(), false)
        }
    }

    #[cfg(keyos)]
    fn verified_third_party_publisher<'a>(
        &mut self,
        trusted_publishers: &'a [ThirdPartyCertificateInfo],
    ) -> Option<&'a ThirdPartyCertificateInfo> {
        match self.third_party_signature_cache {
            ThirdPartySignatureCache::Verified(public_key) => {
                return trusted_publisher_by_key(trusted_publishers, public_key);
            }
            ThirdPartySignatureCache::Invalid(_) => return None,
            ThirdPartySignatureCache::Unknown => {}
        }

        let elf_path = self.elf_path()?;
        let header = read_third_party_app_header(&elf_path)?;
        let (publisher, public_key) = trusted_publisher_matching_header(&header, trusted_publishers)?;

        let verified = app_verified_third_party_header_after_prefilter(&elf_path, public_key).is_some();
        self.third_party_signature_cache = if verified {
            ThirdPartySignatureCache::Verified(public_key)
        } else {
            ThirdPartySignatureCache::Invalid(public_key)
        };

        verified.then_some(publisher)
    }

    fn description(&self) -> String { self.manifest.description.clone().unwrap_or_default() }

    /// On-disk path of the app's bundled raw icon, if one exists. The
    /// auto-discovered `icon.bin` takes precedence over a manifest-declared icon
    /// that resolves into the app bundle. Only existence is checked here; the
    /// bytes are read lazily via [`AppRegistry::app_icon_bytes`].
    fn bundled_icon_file_path(&self) -> Option<String> {
        if let Some(path) = self.bundled_icon_path().filter(|path| app_icon_exists(path)) {
            return Some(path);
        }

        let icon = self.manifest.icon.as_deref().map(str::trim).filter(|icon| !icon.is_empty())?;
        self.app_bundle_icon_path(icon).filter(|path| app_icon_exists(path))
    }

    fn bundled_icon_path(&self) -> Option<String> {
        let app_dir = self.app_dir.as_deref()?;
        Some(format!("{app_dir}/{BUNDLED_ICON_FILE}"))
    }

    fn app_resources_location(&self) -> Option<AppResourcesLocation> {
        let app_dir = self.app_dir.as_deref()?;
        let app_dir = app_dir.rsplit('/').next()?;
        if app_dir.is_empty() || app_dir == "." || app_dir == ".." {
            return None;
        }

        let root = match self.source {
            AppSource::BuiltIn => AppResourcesRoot::BuiltIn,
            AppSource::ThirdParty => AppResourcesRoot::Sideloaded,
        };
        let app_dir = match self.source {
            AppSource::BuiltIn => app_dir.to_string(),
            AppSource::ThirdParty => self.id.to_string(),
        };

        Some(AppResourcesLocation { root, app_dir })
    }

    fn app_bundle_icon_path(&self, icon: &str) -> Option<String> {
        let app_dir = self.app_dir.as_deref()?;
        let icon = icon.trim_start_matches('/');
        if icon.split('/').any(|segment| segment.is_empty() || segment == "." || segment == "..") {
            return None;
        }
        Some(format!("{app_dir}/{icon}"))
    }

    fn binary_size(&mut self) -> u64 {
        if self.binary_size.is_none() {
            // The simulator can't read the app size: the elf isn't in the image.
            #[cfg(keyos)]
            {
                self.binary_size =
                    Some(self.elf_path().as_deref().and_then(|path| app_binary_size(path).ok()).unwrap_or(0));
            }
            #[cfg(not(keyos))]
            {
                self.binary_size = Some(0);
            }
        }
        self.binary_size.unwrap_or(0)
    }

    fn is_built_in(&self) -> bool { self.source == AppSource::BuiltIn }

    fn is_third_party(&self) -> bool { self.source == AppSource::ThirdParty }

    #[cfg(keyos)]
    fn has_verified_third_party_signature(&mut self, public_key: [u8; 33]) -> bool {
        match self.third_party_signature_cache {
            ThirdPartySignatureCache::Verified(cached_key) => return cached_key == public_key,
            ThirdPartySignatureCache::Invalid(cached_key) if cached_key == public_key => return false,
            ThirdPartySignatureCache::Invalid(_) => {}
            ThirdPartySignatureCache::Unknown => {}
        }

        let Some(elf_path) = self.elf_path() else {
            return false;
        };
        let Some(header) = read_third_party_app_header(&elf_path) else {
            return false;
        };
        if !third_party_header_uses_key(&header, public_key) {
            return false;
        }

        let verified = app_verified_third_party_header_after_prefilter(&elf_path, public_key).is_some();
        self.third_party_signature_cache = if verified {
            ThirdPartySignatureCache::Verified(public_key)
        } else {
            ThirdPartySignatureCache::Invalid(public_key)
        };
        verified
    }
}

/// The bundled icon lives in the image on both targets, so it is read through
/// `fs` regardless of where the app's elf runs from.
fn app_icon_exists(path: &str) -> bool { FileSystem::default().metadata(path, fs::Location::System).is_ok() }

fn read_app_icon_bytes(path: &str, max_size_bytes: u64) -> anyhow::Result<Vec<u8>> {
    let fs = FileSystem::default();
    let metadata = fs.metadata(path, fs::Location::System)?;
    if metadata.size > max_size_bytes {
        anyhow::bail!("app icon {path} is too large: {} bytes", metadata.size);
    }

    let mut file = fs.open_file(path, fs::Location::System, fs::OpenFlags::READ_ONLY)?;
    let mut bytes = Vec::with_capacity(metadata.size as usize);
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(keyos)]
fn read_cosign2_header_from_reader(reader: &mut impl Read) -> Option<cosign2::Header> {
    let mut header_bytes = vec![0; cosign2::Header::DEFAULT_SIZE];
    reader.read_exact(&mut header_bytes).ok()?;

    cosign2::Header::parse_unverified(&header_bytes, cosign2::Header::DEFAULT_SIZE, false)
        .inspect_err(|e| log::warn!("failed to parse app cosign2 header: {e:?}"))
        .ok()
        .flatten()
}

#[cfg(keyos)]
fn read_third_party_app_header(elf_path: &str) -> Option<cosign2::Header> {
    match read_app_header(elf_path) {
        Ok(Some(header)) => Some(header),
        Ok(None) => None,
        Err(e) => {
            log::warn!("failed to read app header for third-party key check {elf_path}: {e:?}");
            None
        }
    }
}

#[cfg(keyos)]
fn trusted_publisher_by_key(
    trusted_publishers: &[ThirdPartyCertificateInfo],
    public_key: [u8; 33],
) -> Option<&ThirdPartyCertificateInfo> {
    trusted_publishers.iter().find(|publisher| {
        crate::third_party_certs::decode_public_key_hex(&publisher.public_key) == Some(public_key)
    })
}

#[cfg(keyos)]
fn trusted_publisher_matching_header<'a>(
    header: &cosign2::Header,
    trusted_publishers: &'a [ThirdPartyCertificateInfo],
) -> Option<(&'a ThirdPartyCertificateInfo, [u8; 33])> {
    trusted_publishers.iter().find_map(|publisher| {
        crate::third_party_certs::decode_public_key_hex(&publisher.public_key)
            .filter(|public_key| third_party_header_uses_key(header, *public_key))
            .map(|public_key| (publisher, public_key))
    })
}

#[cfg(keyos)]
fn verify_third_party_app_header(bytes: &[u8], public_key: [u8; 33]) -> anyhow::Result<cosign2::Header> {
    Ok(fw_utils::hash::verify_cosign2_mem_with_third_party_keys(
        &crate::CryptoApi::default(),
        bytes,
        &[public_key],
        true,
    )?)
}

#[cfg(keyos)]
fn app_verified_third_party_header_after_prefilter(
    elf_path: &str,
    public_key: [u8; 33],
) -> Option<cosign2::Header> {
    let size = match app_binary_size(elf_path) {
        Ok(size) => size,
        Err(e) => {
            log::warn!("failed to read app file size for third-party key check {elf_path}: {e:?}");
            return None;
        }
    };
    if size > MAX_THIRD_PARTY_KEY_CHECK_APP_SIZE {
        log::warn!(
            "skipping oversized app file for third-party key check {elf_path}: {size} bytes exceeds {MAX_THIRD_PARTY_KEY_CHECK_APP_SIZE}"
        );
        return None;
    }

    let bytes = match read_app_bytes(elf_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            log::warn!("failed to read app file for third-party key check {elf_path}: {e:?}");
            return None;
        }
    };

    let header = match verify_third_party_app_header(&bytes, public_key) {
        Ok(header) => header,
        Err(e) => {
            log::warn!("failed to verify app file for third-party key check {elf_path}: {e:?}");
            return None;
        }
    };

    third_party_header_uses_key(&header, public_key).then_some(header)
}

#[cfg(keyos)]
fn third_party_header_uses_key(header: &cosign2::Header, public_key: [u8; 33]) -> bool {
    header.pubkey1() == [0; 33]
        && header.signature1() == [0; 64]
        && header.pubkey2() == public_key
        && header.signature2() != [0; 64]
}

fn limit_installed_app_metadata(app: &mut InstalledAppInfo) {
    truncate_string_bytes(&mut app.name, INSTALLED_APP_NAME_MAX_BYTES);
    truncate_string_bytes(&mut app.publisher, INSTALLED_APP_PUBLISHER_MAX_BYTES);
    truncate_string_bytes(&mut app.version, INSTALLED_APP_VERSION_MAX_BYTES);
    truncate_string_bytes(&mut app.description, INSTALLED_APP_DESCRIPTION_MAX_BYTES);

    let mut remaining_permissions = INSTALLED_APP_PERMISSION_LINES_MAX;
    app.permissions.retain_mut(|group| {
        truncate_string_bytes(&mut group.server, INSTALLED_APP_PERMISSION_LINE_MAX_BYTES);

        if remaining_permissions == 0 {
            return false;
        }

        group.messages.truncate(remaining_permissions);
        remaining_permissions -= group.messages.len();

        for message in &mut group.messages {
            truncate_string_bytes(message, INSTALLED_APP_PERMISSION_LINE_MAX_BYTES);
        }

        !group.messages.is_empty()
    });
}

fn truncate_string_bytes(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }

    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

fn sideloaded_app_dir_matches_app_id(app_dir: Option<&str>, app_id: &AppId, source: AppSource) -> bool {
    if source != AppSource::ThirdParty {
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
    let name = app_dir.strip_prefix(SIDELOADED_APPS_DIR)?.strip_prefix('/')?;
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    use app_manager::decode_app_id_str;

    use super::*;

    const THIRD_PARTY_APP_ID: &str = "0x00112233445566778899aabbccddeeff";
    const THIRD_PARTY_APP_DIR: &str = "00112233445566778899aabbccddeeff";
    const THIRD_PARTY_ELF_PATH: &str = "/keyos/sideloaded-apps/00112233445566778899aabbccddeeff/app.elf";

    fn app_info(app_id: &str, name: &str, elf_path: Option<&str>) -> AppInfo {
        let source = if elf_path.is_some() { AppSource::ThirdParty } else { AppSource::BuiltIn };
        app_info_with_source_and_icon(app_id, name, elf_path, source, None)
    }

    fn built_in_app_info(app_id: &str, name: &str, elf_path: Option<&str>) -> AppInfo {
        app_info_with_source_and_icon(app_id, name, elf_path, AppSource::BuiltIn, None)
    }

    fn app_info_with_source_and_icon(
        app_id: &str,
        name: &str,
        elf_path: Option<&str>,
        source: AppSource,
        icon: Option<&str>,
    ) -> AppInfo {
        AppInfo {
            id: decode_app_id_str(app_id).unwrap(),
            app_dir: elf_path.map(|path| path.strip_suffix("/app.elf").unwrap_or(path).to_owned()),
            manifest: Manifest {
                app_name: BTreeMap::from([(Locale("en".to_string()), name.to_string())]),
                app_id: app_manifest::parse_app_id_bytes(app_id).unwrap(),
                icon: icon.map(ToOwned::to_owned),
                publisher: None,
                description: None,
                version: None,
                servers: BTreeMap::new(),
                fixed_sids: BTreeMap::new(),
                permissions: BTreeMap::new(),
                memory: Vec::new(),
                syscall: Vec::new(),
                qr_match_rules: Vec::new(),
            },
            manifest_bytes: Vec::new(),
            source,
            is_flux: false,
            binary_size: None,
            #[cfg(keyos)]
            third_party_signature_cache: ThirdPartySignatureCache::Unknown,
        }
    }

    fn registry_with(apps: Vec<AppInfo>) -> AppRegistry {
        AppRegistry {
            installed_apps: apps.into_iter().map(|app| (app.id, app)).collect::<HashMap<_, _>>(),
            running_apps: HashMap::new(),
        }
    }

    #[test]
    fn sideloaded_bundle_dir_returns_validated_app_directory() {
        let registry =
            registry_with(vec![app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH))]);

        assert_eq!(
            registry.sideloaded_bundle_dir(decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()),
            Some(format!("{SIDELOADED_APPS_DIR}/{THIRD_PARTY_APP_DIR}"))
        );
    }

    #[test]
    fn sideloaded_bundle_dir_rejects_non_sideloaded_apps() {
        let registry = registry_with(vec![
            built_in_app_info(THIRD_PARTY_APP_ID, "Built In App", Some("/keyos/apps/example/app.elf")),
            app_info_with_source_and_icon(
                "0xffeeddccbbaa99887766554433221100",
                "Hosted App",
                Some("/tmp/hosted-app/app.elf"),
                AppSource::ThirdParty,
                None,
            ),
        ]);

        assert_eq!(registry.sideloaded_bundle_dir(decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()), None);
        assert_eq!(
            registry.sideloaded_bundle_dir(decode_app_id_str("0xffeeddccbbaa99887766554433221100").unwrap()),
            None
        );
    }

    #[test]
    fn sideloaded_bundle_dir_rejects_path_that_does_not_match_app_id() {
        let registry = registry_with(vec![app_info_with_source_and_icon(
            THIRD_PARTY_APP_ID,
            "Example App",
            Some("/keyos/sideloaded-apps/ffeeddccbbaa99887766554433221100/app.elf"),
            AppSource::ThirdParty,
            None,
        )]);

        assert_eq!(registry.sideloaded_bundle_dir(decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()), None);
    }

    #[test]
    fn installed_apps_excludes_system_manifests_without_app_file() {
        let mut registry = registry_with(vec![app_info(THIRD_PARTY_APP_ID, "System Manifest", None)]);

        assert!(registry.installed_apps("en", &[]).is_empty());
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
    fn built_in_app_publisher_comes_from_source() {
        let mut app = built_in_app_info(
            "0x426974636f696e2057616c6c65740000",
            "Bitcoin Wallet",
            Some("/keyos/apps/bitcoin/app.elf"),
        );
        app.manifest.publisher = Some("Different Publisher".to_string());

        assert_eq!(app.publisher_and_launchable(&[]), (FOUNDATION_PUBLISHER.to_string(), true));
    }

    // ---------------------------------------------------------------------------------------------
    // QR match-rule pruning tests (`prune_qr_match_rules`).
    // ---------------------------------------------------------------------------------------------

    use app_manifest::{QrMatchRule, QrMatchSubRule, QrPriority};

    fn qr_app_id() -> AppId { AppId([0u8; app_manifest::APP_ID_BYTE_LEN]) }

    fn qr_sub_rule(pattern: Option<&str>) -> QrMatchSubRule {
        QrMatchSubRule::QR { min_len: None, max_len: None, regex_pattern: pattern.map(str::to_string) }
    }

    fn make_rule(id: &str, sub_rules: BTreeMap<String, QrMatchSubRule>) -> QrMatchRule {
        QrMatchRule {
            id: id.to_string(),
            id_localizations: BTreeMap::new(),
            sub_rules,
            priority: QrPriority::default(),
        }
    }

    #[test]
    fn valid_regex_sub_rules_survive() {
        let mut rules = vec![make_rule("r", [("s".to_string(), qr_sub_rule(Some("^abc")))].into())];
        prune_qr_match_rules(&mut rules, &qr_app_id());
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].sub_rules.len(), 1);
    }

    #[test]
    fn invalid_regex_sub_rule_dropped_rule_survives_if_others_valid() {
        let mut rules = vec![make_rule(
            "r",
            [
                ("good".to_string(), qr_sub_rule(Some("^abc"))),
                ("bad".to_string(), qr_sub_rule(Some("[invalid"))),
            ]
            .into(),
        )];
        prune_qr_match_rules(&mut rules, &qr_app_id());
        assert_eq!(rules.len(), 1);
        assert!(rules[0].sub_rules.contains_key("good"));
        assert!(!rules[0].sub_rules.contains_key("bad"));
    }

    #[test]
    fn all_sub_rules_invalid_drops_entire_rule() {
        let mut rules = vec![make_rule("r", [("bad".to_string(), qr_sub_rule(Some("[invalid")))].into())];
        prune_qr_match_rules(&mut rules, &qr_app_id());
        assert!(rules.is_empty());
    }

    #[test]
    fn ur_sub_rules_always_survive_pruning() {
        let mut rules = vec![make_rule(
            "r",
            [("ur".to_string(), QrMatchSubRule::UR { ur_type: "psbt".to_string() })].into(),
        )];
        prune_qr_match_rules(&mut rules, &qr_app_id());
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn no_regex_qr_sub_rule_survives_pruning() {
        let mut rules = vec![make_rule("r", [("any-qr".to_string(), qr_sub_rule(None))].into())];
        prune_qr_match_rules(&mut rules, &qr_app_id());
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn rule_with_no_sub_rules_is_dropped() {
        let mut rules = vec![make_rule("empty", BTreeMap::new())];
        prune_qr_match_rules(&mut rules, &qr_app_id());
        assert!(rules.is_empty(), "empty-sub-rules rule should be dropped");
    }
}
