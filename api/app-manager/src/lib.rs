// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
pub use error::*;
pub use messages::*;

pub mod error;
pub mod messages;

use server::{CheckedConn, CheckedPermissions, MessageAllowed};
use xous::{AppId, PID};

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

#[derive(Default)]
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
        self.0.send_blocking_archive(GetQrMatchRules)
    }

    /// List installed apps, optionally filtered. Pass `AppFilter::default()`
    /// (or `AppFilter { is_flux: None }`) for "everything", `AppFilter::flux_only()`
    /// for Flux child apps, etc.
    pub fn list_apps(&self, locale: &str, filter: AppFilter) -> Vec<AppEntry>
    where
        P: MessageAllowed<ListApps>,
    {
        self.0.send_blocking_archive(ListApps { locale: locale.to_string(), filter })
    }

    /// Convenience wrapper around [`Self::list_apps`] for callers that only
    /// want Flux-child apps (the original `list_flux_apps` use case).
    pub fn list_flux_apps(&self, locale: &str) -> Vec<AppEntry>
    where
        P: MessageAllowed<ListApps>,
    {
        self.list_apps(locale, AppFilter::flux_only())
    }

    pub fn get_installed_apps(&self, locale: &str) -> Vec<InstalledAppInfo>
    where
        P: MessageAllowed<GetInstalledApps>,
    {
        self.0.send_blocking_archive(GetInstalledApps { locale: locale.to_string() })
    }

    pub fn get_app_icon(&self, app_id: &str) -> Option<Vec<u8>>
    where
        P: MessageAllowed<GetAppIcon>,
    {
        self.0.send_blocking_archive(GetAppIcon { app_id: app_id.to_string() })
    }

    pub fn get_third_party_certificates(&self) -> Vec<ThirdPartyCertificateInfo>
    where
        P: MessageAllowed<GetThirdPartyCertificates>,
    {
        self.0.send_blocking_archive(GetThirdPartyCertificates)
    }

    pub fn import_third_party_certificate(
        &self,
        certificate_pem: Vec<u8>,
    ) -> Result<ImportThirdPartyCertificateResult, xous::Error>
    where
        P: MessageAllowed<ImportThirdPartyCertificate>,
    {
        self.0.try_send_blocking_archive(ImportThirdPartyCertificate { certificate_pem })
    }

    pub fn remove_third_party_certificate(
        &self,
        public_key: impl Into<String>,
        locale: &str,
    ) -> Result<RemoveThirdPartyCertificateResult, xous::Error>
    where
        P: MessageAllowed<RemoveThirdPartyCertificate>,
    {
        self.0.try_send_blocking_archive(RemoveThirdPartyCertificate {
            public_key: public_key.into(),
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
