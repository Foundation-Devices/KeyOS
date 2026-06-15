// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{collections::HashMap, io::Read};

use app_manager::{AppQrMatchRules, InstalledAppInfo, ThirdPartyCertificateInfo};
use app_manifest::{Locale, Manifest};
#[cfg(any(keyos, test))]
use fs::messages::AppResourcesRoot;
use log::error;
use regex::Regex;
use serde_json::to_vec;
use xous::{AppId, PID};

use crate::launch::list_apps;

#[cfg(keyos)]
#[path = "registry/hw.rs"]
mod platform;
#[cfg(not(keyos))]
#[path = "registry/hosted.rs"]
mod platform;

use platform::{
    app_binary_metadata, app_binary_size, app_icon_exists, read_app_bytes, read_app_header,
    read_app_icon_bytes,
};

const FOUNDATION_PUBLISHER: &str = "Foundation Devices, Inc.";
const BUNDLED_ICON_FILE: &str = "icon.bin";
const BUILT_IN_APPS_DIR: &str = "/keyos/apps";
const SIDELOADED_APPS_DIR: &str = "/keyos/sideloaded-apps";
const MAX_APP_ICON_SIZE_BYTES: u64 = 256 * 1024;
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

#[derive(Debug, Clone, Default)]
struct AppBinaryMetadata {
    version: String,
    size_bytes: u64,
}

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
                        sub_rule_id,
                        rule.id,
                        hex::encode(app_id.0),
                        e
                    );
                    false
                }
            }
        });
        if rule.sub_rules.is_empty() {
            error!(
                "Rule {:?} for app 0x{} has no sub-rules and will never match",
                rule.id,
                hex::encode(app_id.0)
            );
            false
        } else {
            true
        }
    });
}

#[cfg(keyos)]
const FLUX_APPS_DIR: &str = "/keyos/apps/gui-app-emu-flux/apps";

#[derive(Debug, Clone)]
pub(crate) struct AppInfo {
    id: AppId,
    elf_path: Option<String>,
    manifest: Manifest,
    source: AppSource,
    #[cfg_attr(not(keyos), allow(dead_code))]
    is_flux: bool,
    binary_metadata: Option<AppBinaryMetadata>,
    third_party_signature_cache: ThirdPartySignatureCache,
}

#[cfg(any(keyos, test))]
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

        #[cfg(keyos)]
        {
            // App location is the source of truth for trust classification:
            // firmware-shipped apps live under /keyos/apps, while sideloaded
            // apps live under /keyos/sideloaded-apps.
            Self::scan_apps_dir(&mut installed_apps, BUILT_IN_APPS_DIR, AppSource::BuiltIn, false)?;
            Self::scan_apps_dir(&mut installed_apps, FLUX_APPS_DIR, AppSource::BuiltIn, true)?;
            Self::scan_apps_dir(&mut installed_apps, SIDELOADED_APPS_DIR, AppSource::ThirdParty, false)?;
        }

        #[cfg(not(keyos))]
        {
            Self::scan_hosted_apps(&mut installed_apps)?;
        }

        self.installed_apps = installed_apps;
        log::info!("scan_installed_apps: registry tracks {} installed apps", self.installed_apps.len());

        Ok(())
    }

    #[cfg(keyos)]
    fn scan_apps_dir(
        installed_apps: &mut HashMap<AppId, AppInfo>,
        apps_dir: &str,
        source: AppSource,
        is_flux: bool,
    ) -> anyhow::Result<()> {
        match list_apps(apps_dir) {
            Ok(apps_list) => {
                for app in apps_list {
                    let app_label = app.elf_path.as_deref().unwrap_or(apps_dir).to_string();
                    if source == AppSource::ThirdParty
                        && !sideloaded_app_has_cosign2_header(app.elf_path.as_deref(), &app.manifest.app_id)
                    {
                        continue;
                    }
                    if Self::insert_app(installed_apps, app.elf_path, app.manifest, source, is_flux) {
                        register_manifest_with_names(&app.manifest_bytes, &app_label);
                    }
                }
                Ok(())
            }

            Err(e) => {
                if source == AppSource::ThirdParty {
                    log::debug!("Sideloaded apps directory {apps_dir} is not available: {e:?}");
                    Ok(())
                } else {
                    log::error!("Error listing apps in {apps_dir}: {e:?}");
                    Err(anyhow::anyhow!("Error listing apps in {apps_dir}: {e:?}"))
                }
            }
        }
    }

    #[cfg(not(keyos))]
    fn scan_hosted_apps(installed_apps: &mut HashMap<AppId, AppInfo>) -> anyhow::Result<()> {
        let apps_list = list_apps(BUILT_IN_APPS_DIR).map_err(|e| {
            log::error!("Error listing hosted apps: {e:?}");
            anyhow::anyhow!("Error listing hosted apps: {e:?}")
        })?;

        for app in apps_list {
            let app_label = app
                .elf_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "system manifest".to_string());
            let elf_path = app.elf_path.map(|p| p.to_string_lossy().to_string());
            let source = if elf_path.is_some() { AppSource::ThirdParty } else { AppSource::BuiltIn };
            if Self::insert_app(installed_apps, elf_path, app.manifest, source, false) {
                register_manifest_with_names(&app.manifest_bytes, &app_label);
            }
        }

        Ok(())
    }

    fn insert_app(
        installed_apps: &mut HashMap<AppId, AppInfo>,
        elf_path: Option<String>,
        mut manifest: Manifest,
        source: AppSource,
        is_flux: bool,
    ) -> bool {
        let app_id = AppId(manifest.app_id);

        if installed_apps.contains_key(&app_id) {
            log::warn!(
                "scan_installed_apps: skipping duplicate app_id=0x{} from {:?}",
                hex::encode(app_id.0),
                source
            );
            return false;
        }

        if !sideloaded_app_dir_matches_app_id(elf_path.as_deref(), &app_id, source) {
            return false;
        }

        prune_qr_match_rules(&mut manifest.qr_match_rules, &app_id);

        installed_apps.insert(
            app_id,
            AppInfo {
                id: app_id,
                elf_path,
                manifest,
                source,
                is_flux,
                binary_metadata: None,
                third_party_signature_cache: ThirdPartySignatureCache::Unknown,
            },
        );
        true
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
                    Some(AppQrMatchRules { id: (&app_info.id).into(), rules_json })
                }
                Ok(_) => None,
                Err(_) => {
                    log::warn!(
                        "qr_match_rules: failed to serialize qr_match_rules for app_id=0x{}",
                        hex::encode(app_info.id.0)
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
                let binary_metadata = app_info.binary_metadata();
                let (publisher, can_launch) = app_info.publisher_and_launchable(trusted_publishers);
                let mut installed_app = InstalledAppInfo {
                    app_id: format!("0x{}", hex::encode(app_info.id.0)),
                    bundled_icon_path: app_info.bundled_icon_file_path(),
                    publisher,
                    can_launch,
                    version: binary_metadata.version.clone(),
                    size_bytes: binary_metadata.size_bytes,
                    description: app_info.description(),
                    permissions: app_info.permission_lines(),
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
        let public_key = crate::third_party_certs::decode_public_key_hex(public_key)?;

        self.installed_apps.values_mut().filter(|app_info| app_info.is_third_party()).find_map(|app_info| {
            app_info.has_verified_third_party_signature(public_key).then(|| app_info.localized_name(locale))
        })
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
                app_manager::AppEntry {
                    app_id: format!("0x{}", hex::encode(info.id.0)),
                    name,
                    is_flux: info.is_flux,
                }
            })
            .collect()
    }

    pub(crate) fn elf_path(&self, app_id: AppId) -> Option<String> {
        self.installed_apps.get(&app_id).and_then(|app_info| app_info.elf_path.clone())
    }

    pub(crate) fn app_icon_bytes(&self, app_id: AppId) -> Option<Vec<u8>> {
        let path = self.installed_apps.get(&app_id)?.bundled_icon_file_path()?;
        match read_app_icon_bytes(&path, MAX_APP_ICON_SIZE_BYTES) {
            Ok(data) => Some(data),
            Err(e) => {
                log::warn!("failed to read bundled app icon for app_id=0x{}: {e:?}", hex::encode(app_id.0));
                None
            }
        }
    }

    #[cfg(any(keyos, test))]
    pub(crate) fn app_resources_location(&self, app_id: AppId) -> Option<AppResourcesLocation> {
        self.installed_apps.get(&app_id).and_then(AppInfo::app_resources_location)
    }

    pub(crate) fn requires_debug_signature_trust(&self, app_id: AppId) -> bool {
        self.installed_apps.get(&app_id).map(|app_info| !app_info.is_built_in()).unwrap_or(true)
    }

    pub(crate) fn is_built_in_app(&self, app_id: AppId) -> bool {
        self.installed_apps.get(&app_id).is_some_and(AppInfo::is_built_in)
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

impl AppInfo {
    fn localized_name(&self, locale: &str) -> String {
        self.manifest
            .app_name
            .get(&Locale(locale.to_string()))
            .or_else(|| self.manifest.app_name.get(&Locale("en".to_string())))
            .cloned()
            .unwrap_or_else(|| format!("0x{}", hex::encode(self.id.0)))
    }

    fn permission_lines(&self) -> Vec<String> {
        self.manifest
            .permissions
            .iter()
            .flat_map(|(server, messages)| {
                messages.iter().map(move |message| format!("{server} - {message}"))
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

        match self.verified_third_party_publisher(trusted_publishers) {
            Some(publisher) => (publisher.name.clone(), true),
            None => (String::new(), false),
        }
    }

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

        let elf_path = self.elf_path.as_deref()?;
        let header = read_third_party_app_header(elf_path)?;
        let (publisher, public_key) = trusted_publisher_matching_header(&header, trusted_publishers)?;

        let verified = app_verified_third_party_header_after_prefilter(elf_path, public_key).is_some();
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
        let elf_path = self.elf_path.as_deref()?;
        let (app_dir, _) = elf_path.rsplit_once('/')?;
        Some(format!("{app_dir}/{BUNDLED_ICON_FILE}"))
    }

    #[cfg(any(keyos, test))]
    fn app_resources_location(&self) -> Option<AppResourcesLocation> {
        let elf_path = self.elf_path.as_deref()?;
        let (app_dir, _) = elf_path.rsplit_once('/')?;
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
            AppSource::ThirdParty => hex::encode(self.id.0),
        };

        Some(AppResourcesLocation { root, app_dir })
    }

    fn app_bundle_icon_path(&self, icon: &str) -> Option<String> {
        let elf_path = self.elf_path.as_deref()?;
        let (app_dir, _) = elf_path.rsplit_once('/')?;
        let icon = icon.trim_start_matches('/');
        if icon.split('/').any(|segment| segment.is_empty() || segment == "." || segment == "..") {
            return None;
        }
        Some(format!("{app_dir}/{icon}"))
    }

    fn binary_metadata(&mut self) -> AppBinaryMetadata {
        if self.binary_metadata.is_none() {
            self.binary_metadata =
                Some(self.elf_path.as_deref().map(app_binary_metadata).unwrap_or_default());
        }
        self.binary_metadata.clone().unwrap_or_default()
    }

    fn is_built_in(&self) -> bool { self.source == AppSource::BuiltIn }

    fn is_third_party(&self) -> bool { self.source == AppSource::ThirdParty }

    fn has_verified_third_party_signature(&mut self, public_key: [u8; 33]) -> bool {
        match self.third_party_signature_cache {
            ThirdPartySignatureCache::Verified(cached_key) => return cached_key == public_key,
            ThirdPartySignatureCache::Invalid(cached_key) if cached_key == public_key => return false,
            ThirdPartySignatureCache::Invalid(_) => {}
            ThirdPartySignatureCache::Unknown => {}
        }

        let Some(elf_path) = self.elf_path.as_deref() else {
            return false;
        };
        let Some(header) = read_third_party_app_header(elf_path) else {
            return false;
        };
        if !third_party_header_uses_key(&header, public_key) {
            return false;
        }

        let verified = app_verified_third_party_header_after_prefilter(elf_path, public_key).is_some();
        self.third_party_signature_cache = if verified {
            ThirdPartySignatureCache::Verified(public_key)
        } else {
            ThirdPartySignatureCache::Invalid(public_key)
        };
        verified
    }
}

fn read_cosign2_version_from_reader(reader: &mut impl Read) -> Option<String> {
    read_cosign2_header_from_reader(reader).map(|header| header.version().to_string())
}

fn read_cosign2_header_from_reader(reader: &mut impl Read) -> Option<cosign2::Header> {
    let mut header_bytes = vec![0; cosign2::Header::DEFAULT_SIZE];
    reader.read_exact(&mut header_bytes).ok()?;

    cosign2::Header::parse_unverified(&header_bytes, cosign2::Header::DEFAULT_SIZE, false)
        .inspect_err(|e| log::warn!("failed to parse app cosign2 header: {e:?}"))
        .ok()
        .flatten()
}

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

#[cfg(any(keyos, test))]
fn sideloaded_app_has_cosign2_header(elf_path: Option<&str>, app_id: &[u8; 16]) -> bool {
    let app_id = hex::encode(app_id);
    let Some(elf_path) = elf_path else {
        log::warn!("scan_installed_apps: skipping sideloaded app 0x{app_id}: missing app.elf path");
        return false;
    };

    match read_app_header(elf_path) {
        Ok(Some(_)) => true,
        Ok(None) => {
            log::warn!("scan_installed_apps: skipping sideloaded app 0x{app_id}: missing cosign2 header");
            false
        }
        Err(e) => {
            log::warn!("scan_installed_apps: skipping sideloaded app 0x{app_id}: cannot read app.elf: {e:?}");
            false
        }
    }
}

fn trusted_publisher_by_key(
    trusted_publishers: &[ThirdPartyCertificateInfo],
    public_key: [u8; 33],
) -> Option<&ThirdPartyCertificateInfo> {
    trusted_publishers.iter().find(|publisher| {
        crate::third_party_certs::decode_public_key_hex(&publisher.public_key) == Some(public_key)
    })
}

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

#[cfg(not(test))]
fn verify_third_party_app_header(bytes: &[u8], public_key: [u8; 33]) -> anyhow::Result<cosign2::Header> {
    Ok(fw_utils::hash::verify_cosign2_mem_with_third_party_keys(
        &crate::CryptoApi::default(),
        bytes,
        &[public_key],
        true,
    )?)
}

#[cfg(test)]
fn verify_third_party_app_header(bytes: &[u8], public_key: [u8; 33]) -> anyhow::Result<cosign2::Header> {
    use sha2::{Digest, Sha256};

    struct TestSha256;

    impl cosign2::Sha256 for TestSha256 {
        fn hash(&self, data: &[u8]) -> [u8; 32] { Sha256::digest(data).into() }
    }

    struct TestSecp256k1Verify;

    impl cosign2::Secp256k1Verify for TestSecp256k1Verify {
        fn verify_ecdsa(
            &self,
            msg: [u8; 32],
            signature: [u8; 64],
            pubkey: [u8; 33],
        ) -> cosign2::VerificationResult {
            let Ok(public_key) = secp256k1::PublicKey::from_slice(&pubkey) else {
                return cosign2::VerificationResult::Invalid;
            };
            let Ok(signature) = secp256k1::ecdsa::Signature::from_compact(&signature) else {
                return cosign2::VerificationResult::Invalid;
            };

            let secp = secp256k1::Secp256k1::verification_only();
            if secp.verify_ecdsa(&secp256k1::Message::from_digest(msg), &signature, &public_key).is_ok() {
                cosign2::VerificationResult::Valid
            } else {
                cosign2::VerificationResult::Invalid
            }
        }
    }

    let Some(header) =
        cosign2::Header::parse(bytes, &[], &TestSha256, &TestSecp256k1Verify, cosign2::Header::DEFAULT_SIZE)
            .map_err(|e| anyhow::anyhow!("cosign2 parse error: {e:?}"))?
    else {
        anyhow::bail!("missing cosign2 header");
    };

    if third_party_header_uses_key(&header, public_key) {
        Ok(header)
    } else {
        anyhow::bail!("third-party header does not use expected key");
    }
}

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

    app.permissions.truncate(INSTALLED_APP_PERMISSION_LINES_MAX);
    for permission in &mut app.permissions {
        truncate_string_bytes(permission, INSTALLED_APP_PERMISSION_LINE_MAX_BYTES);
    }
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

fn sideloaded_app_dir_matches_app_id(elf_path: Option<&str>, app_id: &AppId, source: AppSource) -> bool {
    if source != AppSource::ThirdParty {
        return true;
    }

    let Some(elf_path) = elf_path else {
        return true;
    };

    if !elf_path.starts_with(SIDELOADED_APPS_DIR) {
        return true;
    }

    let Some(app_dir) = sideloaded_app_dir_from_elf_path(elf_path) else {
        log::warn!("scan_installed_apps: skipping sideloaded app with invalid bundle path {elf_path:?}");
        return false;
    };

    let expected_app_dir = hex::encode(app_id.0);
    if app_dir != expected_app_dir {
        log::warn!(
            "scan_installed_apps: skipping sideloaded app 0x{} from directory {:?}; expected {:?}",
            expected_app_dir,
            app_dir,
            expected_app_dir
        );
        return false;
    }

    true
}

fn sideloaded_app_dir_from_elf_path(elf_path: &str) -> Option<&str> {
    let relative_path = elf_path.strip_prefix(SIDELOADED_APPS_DIR)?.strip_prefix('/')?;
    let (app_dir, file_name) = relative_path.split_once('/')?;
    (file_name == "app.elf" && !app_dir.is_empty()).then_some(app_dir)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet, HashMap},
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use app_manager::decode_app_id_str;
    use sha2::{Digest, Sha256};

    use super::*;

    const THIRD_PARTY_APP_ID: &str = "0x00112233445566778899aabbccddeeff";
    const THIRD_PARTY_APP_DIR: &str = "00112233445566778899aabbccddeeff";
    const THIRD_PARTY_ELF_PATH: &str = "/keyos/sideloaded-apps/00112233445566778899aabbccddeeff/app.elf";
    const THIRD_PARTY_SECRET_KEY: [u8; 32] = [
        165, 173, 224, 118, 144, 45, 243, 15, 146, 156, 153, 56, 237, 13, 159, 208, 66, 208, 36, 143, 167,
        240, 247, 115, 34, 1, 44, 218, 27, 221, 123, 244,
    ];

    fn app_info(app_id: &str, name: &str, elf_path: Option<&str>) -> AppInfo {
        let source = if elf_path.is_some() { AppSource::ThirdParty } else { AppSource::BuiltIn };
        app_info_with_source_and_icon(app_id, name, elf_path, source, None)
    }

    fn app_info_with_icon(app_id: &str, name: &str, elf_path: Option<&str>, icon: Option<&str>) -> AppInfo {
        let source = if elf_path.is_some() { AppSource::ThirdParty } else { AppSource::BuiltIn };
        app_info_with_source_and_icon(app_id, name, elf_path, source, icon)
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
            elf_path: elf_path.map(ToOwned::to_owned),
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
            source,
            is_flux: false,
            binary_metadata: None,
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
    fn insert_app_rejects_duplicate_app_ids() {
        let app_id = "0x426974636f696e2057616c6c65740000";
        let mut installed_apps = HashMap::new();
        let built_in = built_in_app_info(app_id, "Bitcoin Wallet", Some("/keyos/apps/bitcoin/app.elf"));
        let sideloaded = app_info(
            app_id,
            "Bitcoin Wallet Copy",
            Some("/keyos/sideloaded-apps/426974636f696e2057616c6c65740000/app.elf"),
        );

        assert!(AppRegistry::insert_app(
            &mut installed_apps,
            built_in.elf_path,
            built_in.manifest,
            AppSource::BuiltIn,
            false
        ));
        assert!(!AppRegistry::insert_app(
            &mut installed_apps,
            sideloaded.elf_path,
            sideloaded.manifest,
            AppSource::ThirdParty,
            false
        ));

        let app = installed_apps.get(&decode_app_id_str(app_id).unwrap()).unwrap();
        assert_eq!(app.source, AppSource::BuiltIn);
        assert_eq!(app.localized_name("en"), "Bitcoin Wallet");
    }

    #[test]
    fn insert_app_accepts_sideloaded_app_id_directory() {
        let mut installed_apps = HashMap::new();
        let app = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));

        assert!(AppRegistry::insert_app(
            &mut installed_apps,
            app.elf_path,
            app.manifest,
            AppSource::ThirdParty,
            false
        ));
        assert!(installed_apps.contains_key(&decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()));
    }

    #[test]
    fn insert_app_rejects_sideloaded_app_dir_that_differs_from_app_id() {
        let mut installed_apps = HashMap::new();
        let app = app_info(THIRD_PARTY_APP_ID, "Example App", Some("/keyos/sideloaded-apps/example/app.elf"));

        assert!(!AppRegistry::insert_app(
            &mut installed_apps,
            app.elf_path,
            app.manifest,
            AppSource::ThirdParty,
            false
        ));
        assert!(installed_apps.is_empty());
    }

    struct TestSha256;

    impl cosign2::Sha256 for TestSha256 {
        fn hash(&self, data: &[u8]) -> [u8; 32] { Sha256::digest(data).into() }
    }

    struct TestSigner {
        secret_key: secp256k1::SecretKey,
    }

    impl TestSigner {
        fn new(secret_key: [u8; 32]) -> Self {
            Self { secret_key: secp256k1::SecretKey::from_slice(&secret_key).unwrap() }
        }

        fn public_key(&self) -> [u8; 33] {
            let secp = secp256k1::Secp256k1::signing_only();
            secp256k1::PublicKey::from_secret_key(&secp, &self.secret_key).serialize()
        }
    }

    impl cosign2::Secp256k1Sign for TestSigner {
        fn sign_ecdsa(&self, msg: [u8; 32]) -> [u8; 64] {
            let secp = secp256k1::Secp256k1::signing_only();
            let signature = secp.sign_ecdsa(&secp256k1::Message::from_digest(msg), &self.secret_key);
            signature.serialize_compact()
        }

        fn pubkey(&self) -> [u8; 33] { self.public_key() }
    }

    struct InvalidSignatureSigner {
        public_key: [u8; 33],
    }

    impl cosign2::Secp256k1Sign for InvalidSignatureSigner {
        fn sign_ecdsa(&self, _msg: [u8; 32]) -> [u8; 64] { [0xAB; 64] }

        fn pubkey(&self) -> [u8; 33] { self.public_key }
    }

    fn third_party_app_bytes(signer: &impl cosign2::Secp256k1Sign) -> Vec<u8> {
        let binary = b"example third-party app";
        let header = cosign2::Header::sign_new(
            cosign2::Magic::Atsama5d27KeyOs,
            "1.2.3",
            1,
            cosign2::Signer::Developer,
            binary,
            &TestSha256,
            signer,
            cosign2::Header::DEFAULT_SIZE,
        )
        .unwrap();

        let mut bytes = vec![0; cosign2::Header::DEFAULT_SIZE + binary.len()];
        header.serialize(&mut bytes).unwrap();
        bytes[cosign2::Header::DEFAULT_SIZE..].copy_from_slice(binary);
        bytes
    }

    fn trusted_publisher(public_key: [u8; 33], name: &str) -> ThirdPartyCertificateInfo {
        ThirdPartyCertificateInfo {
            name: name.to_string(),
            company: "Example Company".to_string(),
            contact_email: "hello@example.com".to_string(),
            support_url: "https://example.com".to_string(),
            public_key: hex::encode(public_key),
            not_before_unix_seconds: Some(0),
            not_after_unix_seconds: Some(u64::MAX),
            serial_number: "1".to_string(),
            issuer: String::new(),
            subject: String::new(),
            basic_constraints: String::new(),
            key_usage: String::new(),
            extended_key_usage: String::new(),
        }
    }

    #[test]
    fn sideloaded_app_header_check_accepts_signed_app() {
        let root = make_temp_dir("signed-header-check");
        let elf_path = root.join("app.elf");
        let signer = TestSigner::new(THIRD_PARTY_SECRET_KEY);
        fs::write(&elf_path, third_party_app_bytes(&signer)).unwrap();

        assert!(sideloaded_app_has_cosign2_header(
            Some(elf_path.to_str().unwrap()),
            &app_manifest::parse_app_id_bytes(THIRD_PARTY_APP_ID).unwrap()
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sideloaded_app_header_check_rejects_unsigned_app() {
        let root = make_temp_dir("unsigned-header-check");
        let elf_path = root.join("app.elf");
        fs::write(&elf_path, b"unsigned app").unwrap();

        assert!(!sideloaded_app_has_cosign2_header(
            Some(elf_path.to_str().unwrap()),
            &app_manifest::parse_app_id_bytes(THIRD_PARTY_APP_ID).unwrap()
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installed_apps_excludes_built_in_apps() {
        let mut registry = registry_with(vec![
            built_in_app_info(
                "0x426974636f696e2057616c6c65740000",
                "Bitcoin Wallet",
                Some("/keyos/apps/bitcoin/app.elf"),
            ),
            app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH)),
        ]);

        let apps = registry.installed_apps("en", &[]);

        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].app_id, THIRD_PARTY_APP_ID);
    }

    #[test]
    fn installed_apps_excludes_system_manifests_without_app_file() {
        let mut registry = registry_with(vec![app_info(THIRD_PARTY_APP_ID, "System Manifest", None)]);

        assert!(registry.installed_apps("en", &[]).is_empty());
    }

    #[test]
    fn debug_firmware_requires_signature_trust_for_third_party_apps() {
        let built_in_id = "0x426974636f696e2057616c6c65740000";
        let unknown_id = decode_app_id_str("0xffffffffffffffffffffffffffffffff").unwrap();
        let registry = registry_with(vec![
            built_in_app_info(built_in_id, "Bitcoin Wallet", Some("/keyos/apps/bitcoin/app.elf")),
            app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH)),
        ]);

        assert!(!registry.requires_debug_signature_trust(decode_app_id_str(built_in_id).unwrap()));
        assert!(registry.requires_debug_signature_trust(decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()));
        assert!(registry.requires_debug_signature_trust(unknown_id));
    }

    #[test]
    fn debug_signature_trust_uses_app_source_not_app_id() {
        let built_in_id = "0x426974636f696e2057616c6c65740000";
        let registry = registry_with(vec![app_info(
            built_in_id,
            "Bitcoin Wallet Copy",
            Some("/keyos/sideloaded-apps/426974636f696e2057616c6c65740000/app.elf"),
        )]);

        assert!(registry.requires_debug_signature_trust(decode_app_id_str(built_in_id).unwrap()));
        assert!(!registry.is_built_in_app(decode_app_id_str(built_in_id).unwrap()));
    }

    #[test]
    fn app_requiring_third_party_key_blocks_removal_for_valid_signature() {
        let root = make_temp_dir("valid-third-party-signature");
        let elf_path = root.join("app.elf");
        let signer = TestSigner::new(THIRD_PARTY_SECRET_KEY);
        let public_key = signer.public_key();
        fs::write(&elf_path, third_party_app_bytes(&signer)).unwrap();

        let mut registry = registry_with(vec![app_info(
            THIRD_PARTY_APP_ID,
            "Example App",
            Some(elf_path.to_str().unwrap()),
        )]);

        assert_eq!(
            registry.app_name_requiring_third_party_key(&hex::encode(public_key), "en").as_deref(),
            Some("Example App")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn app_requiring_third_party_key_ignores_unverified_matching_header() {
        let root = make_temp_dir("invalid-third-party-signature");
        let elf_path = root.join("app.elf");
        let signer = TestSigner::new(THIRD_PARTY_SECRET_KEY);
        let public_key = signer.public_key();
        fs::write(&elf_path, third_party_app_bytes(&InvalidSignatureSigner { public_key })).unwrap();

        let mut registry = registry_with(vec![app_info(
            THIRD_PARTY_APP_ID,
            "Example App",
            Some(elf_path.to_str().unwrap()),
        )]);

        assert_eq!(registry.app_name_requiring_third_party_key(&hex::encode(public_key), "en"), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn app_requiring_third_party_key_skips_oversized_binaries() {
        let root = make_temp_dir("oversized-third-party-signature");
        let elf_path = root.join("app.elf");
        let signer = TestSigner::new(THIRD_PARTY_SECRET_KEY);
        let public_key = signer.public_key();
        fs::write(&elf_path, third_party_app_bytes(&signer)).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&elf_path)
            .unwrap()
            .set_len(MAX_THIRD_PARTY_KEY_CHECK_APP_SIZE + 1)
            .unwrap();

        let mut registry = registry_with(vec![app_info(
            THIRD_PARTY_APP_ID,
            "Example App",
            Some(elf_path.to_str().unwrap()),
        )]);

        assert_eq!(registry.app_name_requiring_third_party_key(&hex::encode(public_key), "en"), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installed_apps_include_manifest_app_resource_icon_when_present() {
        let root = make_temp_dir("manifest-resource-icon");
        let elf_path = root.join("app.elf");
        let icon_path = root.join("resources").join(".foundation").join("icon.raw");
        fs::create_dir_all(icon_path.parent().unwrap()).unwrap();
        fs::write(&elf_path, b"elf").unwrap();
        fs::write(&icon_path, b"icon").unwrap();

        let mut registry = registry_with(vec![app_info_with_icon(
            THIRD_PARTY_APP_ID,
            "Example App",
            Some(elf_path.to_str().unwrap()),
            Some("resources/.foundation/icon.raw"),
        )]);

        let apps = registry.installed_apps("en", &[]);

        assert_eq!(apps[0].bundled_icon_path.as_deref(), Some(icon_path.to_str().unwrap()));
        assert_eq!(
            registry.app_icon_bytes(decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()),
            Some(b"icon".to_vec())
        );
        let _ = fs::remove_dir_all(root);
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
    fn installed_apps_prefer_bundled_raw_icon_when_present() {
        let root = make_temp_dir("bundled-icon");
        let elf_path = root.join("app.elf");
        let icon_path = root.join("icon.bin");
        fs::write(&elf_path, b"elf").unwrap();
        fs::write(&icon_path, b"icon").unwrap();

        let mut registry = registry_with(vec![app_info_with_icon(
            THIRD_PARTY_APP_ID,
            "Example App",
            Some(elf_path.to_str().unwrap()),
            Some("images/apps/example/icon.raw"),
        )]);

        let apps = registry.installed_apps("en", &[]);

        assert_eq!(apps[0].bundled_icon_path.as_deref(), Some(icon_path.to_str().unwrap()));
        assert_eq!(
            registry.app_icon_bytes(decode_app_id_str(THIRD_PARTY_APP_ID).unwrap()),
            Some(b"icon".to_vec())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installed_apps_include_manifest_description_without_trusting_publisher_or_version() {
        let mut app = app_info(THIRD_PARTY_APP_ID, "Example App", Some(THIRD_PARTY_ELF_PATH));
        app.manifest.publisher = Some("Example Publisher".to_string());
        app.manifest.description = Some("Example description".to_string());
        app.manifest.version = Some("1.2.3".to_string());
        let mut registry = registry_with(vec![app]);

        let apps = registry.installed_apps("en", &[]);

        assert!(apps[0].publisher.is_empty());
        assert!(!apps[0].can_launch);
        assert_eq!(apps[0].description, "Example description");
        assert!(apps[0].version.is_empty());
    }

    #[test]
    fn installed_apps_caps_user_controlled_metadata() {
        let mut app = app_info(
            THIRD_PARTY_APP_ID,
            &"é".repeat(INSTALLED_APP_NAME_MAX_BYTES),
            Some(THIRD_PARTY_ELF_PATH),
        );
        app.manifest.description = Some("é".repeat(INSTALLED_APP_DESCRIPTION_MAX_BYTES));
        app.manifest.permissions = BTreeMap::from([(
            "server".to_string(),
            (0..INSTALLED_APP_PERMISSION_LINES_MAX + 1)
                .map(|index| format!("{index:02}-{}", "é".repeat(INSTALLED_APP_PERMISSION_LINE_MAX_BYTES)))
                .collect::<BTreeSet<_>>(),
        )]);
        let mut registry = registry_with(vec![app]);

        let apps = registry.installed_apps("en", &[]);

        assert_eq!(apps[0].name.len(), INSTALLED_APP_NAME_MAX_BYTES);
        assert_eq!(apps[0].description.len(), INSTALLED_APP_DESCRIPTION_MAX_BYTES);
        assert_eq!(apps[0].permissions.len(), INSTALLED_APP_PERMISSION_LINES_MAX);
        assert!(apps[0]
            .permissions
            .iter()
            .all(|permission| permission.len() <= INSTALLED_APP_PERMISSION_LINE_MAX_BYTES));
    }

    #[test]
    fn installed_apps_use_verified_trusted_publisher_cert_name() {
        let root = make_temp_dir("verified-publisher");
        let elf_path = root.join("app.elf");
        let signer = TestSigner::new(THIRD_PARTY_SECRET_KEY);
        let public_key = signer.public_key();
        fs::write(&elf_path, third_party_app_bytes(&signer)).unwrap();

        let mut app = app_info(THIRD_PARTY_APP_ID, "Example App", Some(elf_path.to_str().unwrap()));
        app.manifest.publisher = Some("Spoofed Manifest Publisher".to_string());
        let mut registry = registry_with(vec![app]);

        let apps = registry.installed_apps("en", &[trusted_publisher(public_key, "Verified Publisher")]);

        assert_eq!(apps[0].publisher, "Verified Publisher");
        assert!(apps[0].can_launch);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installed_apps_verifies_after_matching_publisher_becomes_trusted() {
        let root = make_temp_dir("deferred-publisher");
        let elf_path = root.join("app.elf");
        let signer = TestSigner::new(THIRD_PARTY_SECRET_KEY);
        let public_key = signer.public_key();
        fs::write(&elf_path, third_party_app_bytes(&signer)).unwrap();

        let mut registry = registry_with(vec![app_info(
            THIRD_PARTY_APP_ID,
            "Example App",
            Some(elf_path.to_str().unwrap()),
        )]);

        let apps = registry.installed_apps("en", &[]);
        assert!(apps[0].publisher.is_empty());
        assert!(!apps[0].can_launch);

        let apps = registry.installed_apps("en", &[trusted_publisher(public_key, "Verified Publisher")]);
        assert_eq!(apps[0].publisher, "Verified Publisher");
        assert!(apps[0].can_launch);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installed_apps_reuses_verified_publisher_until_registry_refresh() {
        let root = make_temp_dir("cached-publisher");
        let elf_path = root.join("app.elf");
        let signer = TestSigner::new(THIRD_PARTY_SECRET_KEY);
        let public_key = signer.public_key();
        fs::write(&elf_path, third_party_app_bytes(&signer)).unwrap();

        let mut registry = registry_with(vec![app_info(
            THIRD_PARTY_APP_ID,
            "Example App",
            Some(elf_path.to_str().unwrap()),
        )]);

        let apps = registry.installed_apps("en", &[trusted_publisher(public_key, "Verified Publisher")]);
        assert_eq!(apps[0].publisher, "Verified Publisher");
        assert!(apps[0].can_launch);

        // The Settings-list cache is invalidated by a registry refresh after
        // sideload writes. App launch still performs full verification.
        fs::write(&elf_path, b"not a signed app").unwrap();
        let apps = registry.installed_apps("en", &[trusted_publisher(public_key, "Renamed Publisher")]);
        assert_eq!(apps[0].publisher, "Renamed Publisher");
        let _ = fs::remove_dir_all(root);
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

    fn make_temp_dir(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("app-manager-registry-{label}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
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
