// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
pub use error::*;
pub use messages::*;

pub mod error;
pub mod messages;

use server::{CheckedConn, CheckedPermissions, MessageAllowed};
use xous::{AppId, PID};

const GET_INSTALLED_APPS_BUFFER_BYTES: usize = 256 * 1024;
const GET_INSTALLED_APPS_PAGE_ITEMS: usize = 8;
const GET_THIRD_PARTY_CERTIFICATES_BUFFER_BYTES: usize = 64 * 1024;
const GET_THIRD_PARTY_CERTIFICATES_PAGE_ITEMS: usize = 8;
// Carries a single icon, whose bytes are capped at 256 KiB server-side. Sized
// above that cap so one icon plus rkyv framing always fits in the buffer.
const GET_APP_ICON_BUFFER_BYTES: usize = 320 * 1024;

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

    pub fn get_installed_apps(&self, locale: &str) -> Vec<InstalledAppInfo>
    where
        P: MessageAllowed<GetInstalledApps>,
    {
        self.try_get_installed_apps(locale).unwrap()
    }

    pub fn try_get_installed_apps(&self, locale: &str) -> Result<Vec<InstalledAppInfo>, xous::Error>
    where
        P: MessageAllowed<GetInstalledApps>,
    {
        let mut apps = Vec::new();
        let mut offset = 0;

        loop {
            let page = self.try_get_installed_apps_page(locale, offset, GET_INSTALLED_APPS_PAGE_ITEMS)?;
            let next_offset = page.next_offset;
            apps.extend(page.apps);
            match next_offset {
                Some(next_offset) if next_offset > offset => offset = next_offset,
                _ => break,
            }
        }

        Ok(apps)
    }

    fn try_get_installed_apps_page(
        &self,
        locale: &str,
        offset: usize,
        limit: usize,
    ) -> Result<InstalledAppsPage, xous::Error>
    where
        P: MessageAllowed<GetInstalledApps>,
    {
        let mut buf = xous_ipc::Buffer::new(GET_INSTALLED_APPS_BUFFER_BYTES);
        self.0.try_send_blocking_archive_buf(
            &mut buf,
            GetInstalledApps { locale: locale.to_string(), offset, limit },
        )
    }

    pub fn get_app_icon(&self, app_id: &str) -> Option<Vec<u8>>
    where
        P: MessageAllowed<GetAppIcon>,
    {
        self.try_get_app_icon(app_id).ok().flatten()
    }

    pub fn try_get_app_icon(&self, app_id: &str) -> Result<Option<Vec<u8>>, xous::Error>
    where
        P: MessageAllowed<GetAppIcon>,
    {
        let mut buf = xous_ipc::Buffer::new(GET_APP_ICON_BUFFER_BYTES);
        self.0.try_send_blocking_archive_buf(&mut buf, GetAppIcon { app_id: app_id.to_string() })
    }

    pub fn get_third_party_certificates(&self) -> Vec<ThirdPartyCertificateInfo>
    where
        P: MessageAllowed<GetThirdPartyCertificates>,
    {
        self.try_get_third_party_certificates().unwrap()
    }

    pub fn try_get_third_party_certificates(&self) -> Result<Vec<ThirdPartyCertificateInfo>, xous::Error>
    where
        P: MessageAllowed<GetThirdPartyCertificates>,
    {
        let mut certificates = Vec::new();
        let mut offset = 0;

        loop {
            let page =
                self.try_get_third_party_certificates_page(offset, GET_THIRD_PARTY_CERTIFICATES_PAGE_ITEMS)?;
            let next_offset = page.next_offset;
            certificates.extend(page.certificates);
            match next_offset {
                Some(next_offset) if next_offset > offset => offset = next_offset,
                _ => break,
            }
        }

        Ok(certificates)
    }

    fn try_get_third_party_certificates_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<ThirdPartyCertificatesPage, xous::Error>
    where
        P: MessageAllowed<GetThirdPartyCertificates>,
    {
        let mut buf = xous_ipc::Buffer::new(GET_THIRD_PARTY_CERTIFICATES_BUFFER_BYTES);
        self.0.try_send_blocking_archive_buf(&mut buf, GetThirdPartyCertificates { offset, limit })
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
}

pub fn decode_app_id_str(id: &str) -> anyhow::Result<AppId> {
    Ok(AppId(app_manifest::parse_app_id_bytes(id)?))
}
