// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[derive(
    Debug,
    Clone,
    Copy,
    thiserror::Error,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
    num_derive::FromPrimitive,
    num_derive::ToPrimitive,
)]
pub enum AppManagerError {
    #[error("Unknown AppId")]
    UnknownAppId = 0,

    #[error("Verification Failed")]
    VerificationFailed,

    #[error("Internal Error")]
    InternalError,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum VerificationError {
    Unverified,
    MissingCosign2Header,
    InternalError,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum LaunchError {
    UnknownAppId,
    Verification(VerificationError),
    NameRegistration,
    UntrustedPublisher,
    OutOfMemory,
    InternalError,
}

impl From<xous::Error> for LaunchError {
    fn from(value: xous::Error) -> Self {
        match value {
            xous::Error::OutOfMemory => LaunchError::OutOfMemory,
            _ => LaunchError::InternalError,
        }
    }
}

impl From<std::str::Utf8Error> for LaunchError {
    fn from(_: std::str::Utf8Error) -> Self { LaunchError::InternalError }
}

impl From<LaunchError> for AppManagerError {
    fn from(value: LaunchError) -> Self {
        match value {
            LaunchError::UnknownAppId => AppManagerError::UnknownAppId,
            LaunchError::Verification(_) | LaunchError::UntrustedPublisher => {
                AppManagerError::VerificationFailed
            }
            _ => AppManagerError::InternalError,
        }
    }
}

#[cfg(not(keyos))]
impl From<std::io::Error> for LaunchError {
    fn from(_value: std::io::Error) -> Self { LaunchError::InternalError }
}

#[cfg(not(keyos))]
impl From<serde_json::Error> for LaunchError {
    fn from(_value: serde_json::Error) -> Self { LaunchError::InternalError }
}
