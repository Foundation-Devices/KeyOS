// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use num_traits::{FromPrimitive, ToPrimitive};
use server::{AsScalar, FromScalar, WithAppId};
use xous::{AppId, PID};

use crate::error::{AppManagerError, LaunchError};

#[derive(Debug, server::Message)]
#[response(Result<PID, AppManagerError>)]
pub struct LaunchAppBlocking(pub AppId);

#[derive(Debug, server::Message)]
#[response(Result<(), AppManagerError>)]
pub struct RefreshInstalledApps;

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
    AppLaunched {
        #[rkyv(with = WithAppId)]
        app_id: AppId,
        pid: PID,
        launched_by: PID,
    },

    AppCrashed {
        #[rkyv(with = WithAppId)]
        app_id: AppId,
        pid: PID,
        launched_by: PID,
        exit_code: u32,
        panic_message: Option<String>,
    },

    LaunchError {
        #[rkyv(with = WithAppId)]
        app_id: AppId,
        error: LaunchError,
    },

    /// A rescan (triggered by `RefreshInstalledApps` or `RemoveInstalledApp`) added, removed, or
    /// updated apps
    ///
    /// `installed`: covers both app ids that weren't in the registry before and app
    /// ids that were already registered but whose manifest changed
    /// `removed` covers app ids no longer found
    AppSetChanged {
        #[rkyv(with = rkyv::with::Map<WithAppId>)]
        installed: Vec<AppId>,
        #[rkyv(with = rkyv::with::Map<WithAppId>)]
        removed: Vec<AppId>,
    },

    TrustedPublishersChanged,
}

#[derive(Debug, server::Message)]
pub struct LaunchApp(pub AppId);
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct AppQrMatchRules {
    #[rkyv(with = WithAppId)]
    pub id: AppId,
    pub rules_json: Vec<u8>,
}

/// One permission subgroup of an app, the unit the user sees, approves, and denies. The
/// individual messages behind it stay internal to the OS.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct InstalledAppPermissionSubgroup {
    pub key: String,
    pub label: String,
    pub approved: bool,
}

/// A top-level permission group (the part of a subgroup key before the first `.`), under
/// which the permission UI collapses its subgroups.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct InstalledAppPermissionGroup {
    pub key: String,
    pub label: String,
    pub subgroups: Vec<InstalledAppPermissionSubgroup>,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct InstalledAppInfo {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub can_launch: bool,
    pub can_remove: bool,
    /// Whether this is a Flux child app: it runs inside the Flux emulator, so
    /// direct-launch affordances (e.g. an Open App button) don't apply to it.
    pub is_flux: bool,
    pub version: String,
    pub size_bytes: u64,
    pub description: String,
    /// Auto-granted permissions (shown but not user-toggleable).
    pub basic_permissions: Vec<InstalledAppPermissionGroup>,
    /// Permissions the user can allow or deny.
    pub approvable_permissions: Vec<InstalledAppPermissionGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum SetAppPermissionGrantResult {
    Updated,
    AppNotFound,
    PermissionNotFound,
    NotUserGrantable,
    Unauthorized,
    StorageUnavailable,
    InternalError,
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct PermissionRequestInfo {
    #[rkyv(with = WithAppId)]
    pub app_id: AppId,
    pub app_name: String,
    /// Subgroup key the grant is recorded under (e.g. `peripherals.camera-use`).
    pub subgroup: String,
    /// The subgroup's localized display name, ready to show in the prompt as-is.
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum PermissionRequestInfoResult {
    Prompt(PermissionRequestInfo),
    AlreadyApproved,
    Denied,
    NotGrantable,
    AppNotFound,
    Unauthorized,
    InternalError,
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

impl ThirdPartyCertificateInfo {
    /// Whether the current time falls within the certificate's validity window. A missing bound or
    /// an unreadable clock counts as invalid.
    pub fn is_currently_valid(&self) -> bool {
        let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
            return false;
        };
        let now = elapsed.as_secs();
        matches!(
            (self.not_before_unix_seconds, self.not_after_unix_seconds),
            (Some(not_before), Some(not_after)) if not_before <= now && now <= not_after
        )
    }
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

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum RemoveInstalledAppResult {
    Removed,
    NotFound,
    NotSideloaded,
    Running,
    InternalError,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Option<String>)]
pub enum GetAppName {
    ByAppId {
        #[rkyv(with = WithAppId)]
        id: AppId,
        locale: String,
    },

    ByPid {
        pid: PID,
        locale: String,
    },
}

impl GetAppName {
    pub fn new_by_app_id(id: &AppId, locale: &str) -> Self {
        Self::ByAppId { id: *id, locale: locale.to_string() }
    }

    pub fn new_by_pid(pid: PID, locale: &str) -> Self { Self::ByPid { pid, locale: locale.to_string() } }
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Vec<AppQrMatchRules>)]
/// limits the response to the listed apps; an empty list returns all apps.
pub struct GetQrMatchRules {
    #[rkyv(with = rkyv::with::Map<WithAppId>)]
    pub app_ids: Vec<AppId>,
}

/// Filter applied by [`ListApps`]. A `None` axis matches either value; the axes are
/// independent, so a Flux app may be built-in or sideloaded.
#[derive(Debug, Clone, Default, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct AppFilter {
    pub is_flux: Option<bool>,
    pub third_party: Option<bool>,
}

impl AppFilter {
    /// Filter to non-Flux apps.
    pub fn standard_only() -> Self { Self { is_flux: Some(false), ..Default::default() } }

    /// Filter to Flux child apps only.
    pub fn flux_only() -> Self { Self { is_flux: Some(true), ..Default::default() } }

    /// Filter to sideloaded third-party apps only.
    pub fn third_party_only() -> Self { Self { third_party: Some(true), ..Default::default() } }
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Vec<InstalledAppInfo>)]
pub struct ListApps {
    pub locale: String,
    pub filter: AppFilter,
}

/// Fetch the raw bytes of a single app's bundled icon, keyed by its hex app id
/// (as returned in [`InstalledAppInfo::app_id`]). Returns `None` when the app
/// is unknown, has no bundled icon, or the icon cannot be read.
#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Option<Vec<u8>>)]
pub struct GetAppIcon {
    pub app_id: String,
}

/// How the user answered a permission prompt (or moved a Settings toggle) for one
/// permission subgroup of an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum PermissionGrantDecision {
    /// Persist an approval ("Allow Always").
    Allow,
    /// Persist a denial ("Never Allow").
    Deny,
    /// Deny for the current run only ("Not Now"): the broker auto-denies further requests
    /// for the same subgroup without re-prompting until the app is relaunched.
    /// Not persisted; cleared when the app next launches.
    DenyForRun,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(SetAppPermissionGrantResult)]
pub struct SetAppPermissionGrant {
    pub app_id: String,
    /// Subgroup key (e.g. `peripherals.camera-use`); the grant covers every message in it.
    pub subgroup: String,
    pub decision: PermissionGrantDecision,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(PermissionRequestInfoResult)]
pub struct GetPermissionRequestInfo {
    /// The requesting app's id, captured by the kernel when the request was parked, so it is
    /// stable even if the sender exits and its pid is recycled before the broker asks.
    pub sender_app_id: [u8; 16],
    /// The target server's SID as captured by the kernel when the request was parked; it
    /// identifies the exact server even when one process hosts several.
    pub server_sid: [u32; 4],
    pub message_id: usize,
    pub locale: String,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Vec<ThirdPartyCertificateInfo>)]
pub struct GetThirdPartyCertificates;

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

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(RemoveInstalledAppResult)]
pub struct RemoveInstalledApp {
    #[rkyv(with = WithAppId)]
    pub app_id: AppId,
}
