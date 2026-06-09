// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use num_traits::{FromPrimitive, ToPrimitive};
use server::{AsScalar, FromScalar};
use xous::{AppId, PID};

use crate::error::{AppManagerError, LaunchError};

#[derive(Debug, server::Message)]
#[response(Result<PID, AppManagerError>)]
pub struct LaunchAppBlocking(pub AppId);

impl AsScalar<3> for AppManagerError {
    fn as_scalar(&self) -> [u32; 3] { [self.to_u32().unwrap(), 0, 0] }
}

impl FromScalar<3> for AppManagerError {
    fn from_scalar([e, ..]: [u32; 3]) -> Self {
        AppManagerError::from_u32(e).unwrap_or(AppManagerError::InternalError)
    }
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(AppEvent)]
pub struct SubscribeAppEvents;

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum AppEvent {
    AppLaunched { app_id: [u32; 4], pid: PID, launched_by: PID },

    AppCrashed { app_id: [u32; 4], pid: PID, launched_by: PID, exit_code: u32, panic_message: Option<String> },

    LaunchError(LaunchError),
}

#[derive(Debug, server::Message)]
pub struct LaunchApp(pub AppId);
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct AppQrMatchRules {
    pub id: [u32; 4],
    pub rules_json: Vec<u8>,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct InstalledAppInfo {
    pub app_id: String,
    pub name: String,
    /// On-disk path/reference for the app's bundled icon when it exists. Icon
    /// bytes are fetched separately via [`GetAppIcon`] on device so a single
    /// large icon (or many apps) cannot overflow the listing's IPC buffer.
    pub bundled_icon_path: Option<String>,
    pub publisher: String,
    pub version: String,
    pub size_bytes: u64,
    pub description: String,
    pub permissions: Vec<String>,
}

#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct ThirdPartyCertificateInfo {
    pub name: String,
    pub company: String,
    pub contact_email: String,
    pub support_url: String,
    pub public_key: String,
    #[serde(default)]
    pub not_before_unix_seconds: Option<u64>,
    #[serde(default)]
    pub not_after_unix_seconds: Option<u64>,
    pub serial_number: String,
    pub issuer: String,
    pub subject: String,
    pub basic_constraints: String,
    pub key_usage: String,
    pub extended_key_usage: String,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum ImportThirdPartyCertificateResult {
    Imported(ThirdPartyCertificateInfo),
    Invalid,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum RemoveThirdPartyCertificateResult {
    Removed,
    NotFound,
    AppRequiresKey(String),
    InternalError,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Option<String>)]
pub enum GetAppName {
    ByAppId { id: [u32; 4], locale: String },

    ByPid { pid: PID, locale: String },
}

impl GetAppName {
    pub fn new_by_app_id(id: &AppId, locale: &str) -> Self {
        Self::ByAppId { id: id.into(), locale: locale.to_string() }
    }

    pub fn new_by_pid(pid: PID, locale: &str) -> Self { Self::ByPid { pid, locale: locale.to_string() } }
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Vec<AppQrMatchRules>)]
pub struct GetQrMatchRules;

/// Catalogue entry for a single installed app, returned by [`ListApps`].
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct AppEntry {
    pub app_id: String,
    pub name: String,
    /// `true` when the app lives under `/keyos/apps/gui-app-emu-flux/apps`.
    pub is_flux: bool,
}

impl AppEntry {
    pub fn new(app_id: &str, name: &str, is_flux: bool) -> Self {
        AppEntry { app_id: app_id.to_string(), name: name.to_string(), is_flux }
    }
}

/// Filter applied by [`ListApps`]. `None` on a field means "either"; `Some(v)`
/// restricts the result to entries matching `v`.
#[derive(Debug, Clone, Default, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct AppFilter {
    pub is_flux: Option<bool>,
}

impl AppFilter {
    /// Convenience: filter to Flux apps only.
    pub fn flux_only() -> Self { Self { is_flux: Some(true) } }

    /// Convenience: filter to non-Flux apps only.
    pub fn standard_only() -> Self { Self { is_flux: Some(false) } }
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Vec<AppEntry>)]
pub struct ListApps {
    pub locale: String,
    pub filter: AppFilter,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(InstalledAppsPage)]
pub struct GetInstalledApps {
    pub locale: String,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct InstalledAppsPage {
    pub apps: Vec<InstalledAppInfo>,
    pub next_offset: Option<usize>,
}

/// Fetch the raw bytes of a single app's bundled icon, keyed by its hex app id
/// (as returned in [`InstalledAppInfo::app_id`]). Returns `None` when the app
/// is unknown, has no bundled icon, or the icon cannot be read.
#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Option<Vec<u8>>)]
pub struct GetAppIcon {
    pub app_id: String,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(ThirdPartyCertificatesPage)]
pub struct GetThirdPartyCertificates {
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ThirdPartyCertificatesPage {
    pub certificates: Vec<ThirdPartyCertificateInfo>,
    pub next_offset: Option<usize>,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(ImportThirdPartyCertificateResult)]
pub struct ImportThirdPartyCertificate {
    pub certificate_pem: Vec<u8>,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(RemoveThirdPartyCertificateResult)]
pub struct RemoveThirdPartyCertificate {
    pub public_key: String,
    pub locale: String,
}
