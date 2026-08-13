// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
pub use error::*;
pub use messages::*;

pub mod error;
pub mod messages;

use server::{CheckedConn, CheckedPermissions, MessageAllowed};
use xous::{AppId, PID};

/// The sideload bundle directory, relative to the system volume root. Every sideloaded app,
/// Flux children included, installs into `<SIDELOADED_APPS_DIR>/<hex app id>`.
pub const SIDELOADED_APPS_DIR: &str = "keyos/sideloaded-apps";

#[macro_export]
macro_rules! use_api {
    () => {
        mod app_manager_permissions {
            use app_manager::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/app-manager"]
            pub struct AppManagerPermissions;
        }
        type AppManagerApi = app_manager::AppManagerApi<app_manager_permissions::AppManagerPermissions>;
    };
}

#[derive(Clone, Default)]
pub struct AppManagerApi<P: CheckedPermissions>(pub(crate) CheckedConn<P>);

impl<P: CheckedPermissions> AppManagerApi<P> {
    pub fn launch_app_blocking(&self, app_id: &AppId) -> Result<PID, AppManagerError>
    where
        P: MessageAllowed<LaunchAppBlocking>,
    {
        self.0
            .try_send_blocking_scalar(LaunchAppBlocking(*app_id))
            .map_err(|_| AppManagerError::InternalError)?
    }

    pub fn launch_app(&self, app_id: &AppId) -> Result<(), xous::Error>
    where
        P: MessageAllowed<LaunchApp>,
    {
        self.0.try_send_scalar(LaunchApp(*app_id))?;
        Ok(())
    }

    pub fn refresh_installed_apps(&self) -> Result<(), AppManagerError>
    where
        P: MessageAllowed<RefreshInstalledApps>,
    {
        self.0.try_send_blocking_scalar(RefreshInstalledApps).map_err(|_| AppManagerError::InternalError)?
    }

    /// Rescan after an app bundle has been completely replaced, forcing subscribers to refresh
    /// that app's resources even when its manifest is unchanged
    pub fn refresh_installed_app(&self, app_id: AppId) -> Result<(), AppManagerError>
    where
        P: MessageAllowed<RefreshInstalledApp>,
    {
        self.0
            .try_send_blocking_scalar(RefreshInstalledApp(app_id))
            .map_err(|_| AppManagerError::InternalError)?
    }

    pub fn app_name_by_app_id(&self, id: &AppId, locale: &str) -> Option<String>
    where
        P: MessageAllowed<GetAppName>,
    {
        self.0.send_blocking_archive(GetAppName::new_by_app_id(id, locale))
    }

    pub fn app_name_by_pid(&self, pid: PID, locale: &str) -> Option<String>
    where
        P: MessageAllowed<GetAppName>,
    {
        self.0.send_blocking_archive(GetAppName::new_by_pid(pid, locale))
    }

    pub fn get_qr_match_rules(&self) -> Vec<AppQrMatchRules>
    where
        P: MessageAllowed<GetQrMatchRules>,
    {
        self.0.send_blocking_archive(GetQrMatchRules { app_ids: Vec::new() })
    }

    /// List installed apps, optionally narrowed by `filter`. Pass `AppFilter::default()`
    /// for everything, `AppFilter::sideloaded_only()` for sideloaded apps, etc.
    pub fn list_apps(&self, locale: &str, filter: AppFilter) -> Vec<InstalledAppInfo>
    where
        P: MessageAllowed<ListApps>,
    {
        self.0.send_blocking_archive(ListApps { locale: locale.to_string(), filter })
    }

    pub fn get_app_icon(&self, app_id: &str, variant: IconVariant) -> Option<Vec<u8>>
    where
        P: MessageAllowed<GetAppIcon>,
    {
        self.0.send_blocking_archive(GetAppIcon { app_id: app_id.to_string(), variant })
    }

    pub fn set_app_permission_grant(
        &self,
        app_id: &str,
        subgroup: &str,
        decision: PermissionGrantDecision,
    ) -> SetAppPermissionGrantResult
    where
        P: MessageAllowed<SetAppPermissionGrant>,
    {
        self.0.send_blocking_archive(SetAppPermissionGrant {
            app_id: app_id.to_string(),
            subgroup: subgroup.to_string(),
            decision,
        })
    }

    pub fn get_permission_request_info(
        &self,
        sender_app_id: [u8; 16],
        server_sid: [u32; 4],
        message_id: usize,
        locale: &str,
    ) -> PermissionRequestInfoResult
    where
        P: MessageAllowed<GetPermissionRequestInfo>,
    {
        self.0.send_blocking_archive(GetPermissionRequestInfo {
            sender_app_id,
            server_sid,
            message_id,
            locale: locale.to_string(),
        })
    }

    pub fn get_third_party_certificates(&self) -> Vec<ThirdPartyCertificateInfo>
    where
        P: MessageAllowed<GetThirdPartyCertificates>,
    {
        self.0.send_blocking_archive(GetThirdPartyCertificates)
    }

    pub fn preview_third_party_certificate(
        &self,
        certificate_pem: Vec<u8>,
    ) -> Result<Result<ThirdPartyCertificateInfo, ThirdPartyCertificateError>, xous::Error>
    where
        P: MessageAllowed<PreviewThirdPartyCertificate>,
    {
        self.0.try_send_blocking_archive(PreviewThirdPartyCertificate { certificate_pem })
    }

    pub fn import_third_party_certificate(
        &self,
        certificate_pem: Vec<u8>,
        expected_fingerprint: impl Into<String>,
    ) -> Result<Result<ThirdPartyCertificateInfo, ThirdPartyCertificateError>, xous::Error>
    where
        P: MessageAllowed<ImportThirdPartyCertificate>,
    {
        self.0.try_send_blocking_archive(ImportThirdPartyCertificate {
            certificate_pem,
            expected_fingerprint: expected_fingerprint.into(),
        })
    }

    pub fn remove_third_party_certificate(
        &self,
        fingerprint: impl Into<String>,
        locale: &str,
    ) -> Result<RemoveThirdPartyCertificateResult, xous::Error>
    where
        P: MessageAllowed<RemoveThirdPartyCertificate>,
    {
        self.0.try_send_blocking_archive(RemoveThirdPartyCertificate {
            fingerprint: fingerprint.into(),
            locale: locale.to_string(),
        })
    }

    /// Sends a non-blocking app removal request
    pub fn remove_app(&self, app_id: &AppId) -> Result<(), xous::Error>
    where
        P: MessageAllowed<RemoveApp>,
    {
        self.0.try_send_scalar(RemoveApp(*app_id))
    }

    pub fn install_app_archive(
        &self,
        path: impl Into<String>,
        location: ArchiveLocation,
        locale: &str,
    ) -> Result<Result<InstallAppArchiveResult, InstallError>, xous::Error>
    where
        P: MessageAllowed<InstallAppArchive>,
    {
        self.0.try_send_blocking_archive(InstallAppArchive {
            path: path.into(),
            location,
            locale: locale.to_string(),
        })
    }

    /// Subscribe the calling server to app lifecycle events (launch/crash).
    ///
    /// The subscriber must implement `server::ArchiveEventHandler<AppEvent>`.
    pub fn server_subscribe_app_events<S>(&self, context: &mut server::ServerContext<S>)
    where
        P: 'static,
        P: MessageAllowed<SubscribeAppEvents>,
        S: server::Server + server::ArchiveEventHandler<AppEvent>,
    {
        self.0.subscribe_archive_infallible(SubscribeAppEvents, context)
    }
}

pub fn decode_app_id_str(id: &str) -> anyhow::Result<AppId> {
    Ok(AppId(app_manifest::parse_app_id_bytes(id)?))
}
