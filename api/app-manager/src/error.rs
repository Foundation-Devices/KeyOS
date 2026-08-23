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
    VerificationFailed = 1,

    #[error("Internal Error")]
    InternalError = 2,

    #[error("No Certificate")]
    NoCertificate = 3,

    #[error("Publisher Certificate Expired")]
    PublisherCertificateExpired = 4,

    #[error("Publisher Certificate Not Active Yet")]
    PublisherCertificateNotYetActive = 5,

    #[error("KeyOS Version Too Old")]
    KeyOsVersionTooOld = 6,
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum VerificationError {
    Unverified,
    MissingCosign2Header,
    InternalError,
}

/// A manifest is well-formed but cannot run on the current KeyOS release.
#[derive(
    Debug, Clone, PartialEq, Eq, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub enum CompatibilityError {
    #[error("app requires KeyOS {minimum}, but this device is running {current}")]
    KeyOsVersionTooOld { minimum: String, current: String },
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum LaunchError {
    UnknownAppId,
    Verification(VerificationError),
    NameRegistration,
    NoCertificate,
    PublisherCertificateExpired,
    PublisherCertificateNotYetActive,
    Compatibility(CompatibilityError),
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
            LaunchError::Verification(_) => AppManagerError::VerificationFailed,
            LaunchError::NoCertificate => AppManagerError::NoCertificate,
            LaunchError::PublisherCertificateExpired => AppManagerError::PublisherCertificateExpired,
            LaunchError::PublisherCertificateNotYetActive => {
                AppManagerError::PublisherCertificateNotYetActive
            }
            LaunchError::Compatibility(CompatibilityError::KeyOsVersionTooOld { .. }) => {
                AppManagerError::KeyOsVersionTooOld
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
