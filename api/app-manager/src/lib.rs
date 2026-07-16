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
}

pub fn decode_app_id_str(id: &str) -> anyhow::Result<AppId> {
    Ok(AppId(app_manifest::parse_app_id_bytes(id)?))
}
