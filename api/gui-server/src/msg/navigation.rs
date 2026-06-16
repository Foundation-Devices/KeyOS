// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::rkyv_with::WithAppId;
use xous::AppId;

use crate::{error::NavigationError, ModalStyle};

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(NavigationResult)]
pub struct ShowModal {
    pub modal_style: ModalStyle,
    #[rkyv(with = WithAppId)]
    pub app_id: AppId,
    pub args: Vec<u8>,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(NavigationResult)]
pub struct NavigateTo {
    #[rkyv(with = WithAppId)]
    pub app_id: AppId,
    pub args: Vec<u8>,
}

/// Reason an app launch failed. Carries enough information for the SDK / UI to
/// pick a useful follow-up (import a cert, enable developer mode, restart, …)
/// instead of just printing a generic "launch failed" message.
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum LaunchFailureReason {
    /// App is signed by a key not in the trusted set.
    SignatureRejected,
    /// Required permission not granted to the launching context.
    MissingPermission,
    /// Anything else — app-manager-internal failure, IPC failure, etc.
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum RunAppResponse {
    Launched { pid: usize },
    AlreadyRunning { pid: usize },
    Locked,
    NotReady,
    AppIdNotFound,
    LaunchFailed { reason: LaunchFailureReason },
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(RunAppResponse)]
pub struct RunApp {
    #[rkyv(with = WithAppId)]
    pub app_id: AppId,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(())]
pub struct FinishResponse {
    pub response: Vec<u8>,
}

impl FinishResponse {
    pub fn as_slice(&self) -> &[u8] { &self.response }
}

pub type NavigationResult = Result<FinishResponse, NavigationError>;

#[derive(Debug, server::Message)]
pub struct NavigationCancel;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Option<Vec<u8>>)]
pub struct GetPendingNavRequest;

#[derive(Debug, Copy, Clone, server::Message)]
pub struct LoginSuccess;
