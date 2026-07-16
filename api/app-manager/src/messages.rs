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

    LaunchError { app_id: [u32; 4], error: LaunchError },
}

#[derive(Debug, server::Message)]
pub struct LaunchApp(pub AppId);
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct AppQrMatchRules {
    pub id: [u32; 4],
    pub rules_json: Vec<u8>,
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
