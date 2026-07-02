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

const BUNDLED_ICON_FILE: &str = "icon.bin";
const BUILT_IN_APPS_DIR: &str = "/keyos/apps";
pub const SIDELOADED_APPS_DIR: &str = "/keyos/sideloaded-apps";
const MAX_APP_ICON_SIZE_BYTES: u64 = 300 * 1024;
const MAX_MANIFEST_SIZE_BYTES: u64 = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppSource {
    BuiltIn,
    ThirdParty,
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
    /// The verified manifest JSON as scanned, handed verbatim to the name server at launch.
    manifest_bytes: Vec<u8>,
    source: AppSource,
    #[cfg_attr(not(keyos), allow(dead_code))]
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

#[derive(Debug, Default)]
pub(crate) struct AppRegistry {
    installed_apps: HashMap<AppId, AppInfo>,
    running_apps: HashMap<PID, RunningAppInfo>,
}

impl AppRegistry {
    pub(crate) fn scan_installed_apps(&mut self) -> anyhow::Result<()> {
        let mut installed_apps = HashMap::new();

        // App location is the source of truth for trust classification: firmware-shipped apps
        // live under /keyos/apps and verify against the official keys, while sideloaded apps
        // live under /keyos/sideloaded-apps and only need a valid developer signature here.
        // The simulator reads the same dirs through fs and signs nothing.
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

    /// Load one app bundle, on hardware only after its signed manifest verifies, or else a forged
    /// manifest's contents (app id, permissions, QR rules) would be trusted the moment it enters
    /// the registry, with no launch required. Returns `None` when a sideloaded bundle's dir name
    /// doesn't match its app id (already logged).
    fn load_app(app_dir: &str, source: AppSource, is_flux: bool) -> anyhow::Result<Option<AppInfo>> {
        let manifest_raw = read_capped_file(&format!("{app_dir}/manifest.json"), MAX_MANIFEST_SIZE_BYTES)?;
        let (manifest_json, third_party_signer) = check_manifest_signature(&manifest_raw, source)?;
        let mut manifest = app_manifest::try_from_bytes(manifest_json)
            .map_err(|e| anyhow::anyhow!("invalid manifest: {e}"))?;

        let app_id = AppId(manifest.app_id);
        if !sideloaded_app_dir_matches_app_id(Some(app_dir), &app_id, source) {
            return Ok(None);
        }
        prune_qr_match_rules(&mut manifest.qr_match_rules, &app_id);

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

    pub(crate) fn qr_match_rules(&self, publishers: &[ThirdPartyCertificateInfo]) -> Vec<AppQrMatchRules> {
        self.installed_apps
            .values()
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
    ) -> Vec<InstalledAppInfo> {
        let mut apps = self
            .installed_apps
            .values_mut()
            .filter(|app_info| filter.is_flux.map_or(true, |want| app_info.is_flux == want))
            .filter(|app_info| filter.third_party.map_or(true, |want| app_info.is_third_party() == want))
            .map(|app_info| {
                let name = app_info.localized_name(locale);
                let size_bytes = app_info.binary_size();
                let version = app_info.manifest.version.clone().unwrap_or_default();
                let (publisher, can_launch) = app_info.publisher_and_launchable(trusted_publishers);
                InstalledAppInfo {
                    app_id: format!("0x{}", app_info.id),
                    publisher,
                    can_launch,
                    version,
                    size_bytes,
                    description: app_info.description(),
                    permissions: app_info.permission_groups(),
                    name,
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

        self.installed_apps.values().filter(|app_info| app_info.is_third_party()).find_map(|app_info| {
            (app_info.third_party_signer == Some(public_key)).then(|| app_info.localized_name(locale))
        })
    }

    pub(crate) fn elf_path(&self, app_id: AppId) -> Option<String> {
        self.installed_apps.get(&app_id).and_then(AppInfo::elf_path)
    }

    /// Blind-read the app's bundled `icon.bin`, returning `None` when the app ships
    /// no icon (the common case) or it can't be read.
    pub(crate) fn app_icon_bytes(&self, app_id: AppId) -> Option<Vec<u8>> {
        let app_dir = self.installed_apps.get(&app_id)?.app_dir.as_deref()?;
        let path = format!("{app_dir}/{BUNDLED_ICON_FILE}");
        read_capped_file(&path, MAX_APP_ICON_SIZE_BYTES).ok()
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

    /// The app's verified manifest JSON, for handing to the name server at launch.
    pub(crate) fn manifest_bytes(&self, app_id: AppId) -> Option<&[u8]> {
        self.installed_apps.get(&app_id).map(|app_info| app_info.manifest_bytes.as_slice())
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
}

/// Read a bundle file through `fs`, refusing anything larger than `max_size_bytes` before
/// allocating, so a malformed bundle can't make us read an unbounded amount into memory.
fn read_capped_file(path: &str, max_size_bytes: u64) -> anyhow::Result<Vec<u8>> {
    let fs = FileSystem::default();
    let metadata = fs.metadata(path, fs::Location::System)?;
    if metadata.size > max_size_bytes {
        anyhow::bail!("{path} exceeds the {max_size_bytes}-byte cap: {} bytes", metadata.size);
    }

    let mut file = fs.open_file(path, fs::Location::System, fs::OpenFlags::READ_ONLY)?;
    let mut bytes = Vec::with_capacity(metadata.size as usize);
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
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
    use std::collections::{BTreeMap, HashMap};

    use app_manager::decode_app_id_str;

    use super::*;

    const THIRD_PARTY_APP_ID: &str = "0x00112233445566778899aabbccddeeff";
    const THIRD_PARTY_APP_DIR: &str = "00112233445566778899aabbccddeeff";
    const THIRD_PARTY_ELF_PATH: &str = "/keyos/sideloaded-apps/00112233445566778899aabbccddeeff/app.elf";

    fn app_info(app_id: &str, name: &str, elf_path: Option<&str>) -> AppInfo {
        let source = if elf_path.is_some() { AppSource::ThirdParty } else { AppSource::BuiltIn };
        app_info_with_source(app_id, name, elf_path, source)
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

    fn registry_with(apps: Vec<AppInfo>) -> AppRegistry {
        AppRegistry {
            installed_apps: apps.into_iter().map(|app| (app.id, app)).collect::<HashMap<_, _>>(),
            running_apps: HashMap::new(),
        }
    }

    #[test]
    fn installed_apps_excludes_system_manifests_without_app_file() {
        let mut registry = registry_with(vec![app_info(THIRD_PARTY_APP_ID, "System Manifest", None)]);

        assert!(registry.list_apps("en", &[], &app_manager::AppFilter::third_party_only()).is_empty());
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
    fn built_in_app_launches_without_a_publisher_name() {
        let mut app = built_in_app_info(
            "0x426974636f696e2057616c6c65740000",
            "Bitcoin Wallet",
            Some("/keyos/apps/bitcoin/app.elf"),
        );
        app.manifest.publisher = Some("Different Publisher".to_string());

        assert_eq!(app.publisher_and_launchable(&[]), (String::new(), true));
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
