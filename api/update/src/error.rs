// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[derive(Clone, Debug, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum Error {
    #[error("failed to reboot")]
    Reboot,
    #[error("failed to start a new update, must continue previous")]
    StartButShouldContinue,
    #[error("no update downloaded")]
    NoUpdateDownloaded,
    #[error("io error: {0}")]
    Io(String),
    #[error(transparent)]
    Fs(#[from] fs::Error),
    #[error("failed to parse version string {0}")]
    ParseVersion(String),
    #[error("patch version mismatch")]
    PatchVersionMismatch,
    #[error("patch file ({file_name}) size mismatch, expected = {expected_size}, actual = {actual_size}")]
    PatchSizeMismatch { file_name: String, expected_size: u64, actual_size: u64 },
    #[error("patch file hash mismatch")]
    PatchHashMismatch,
    #[error("bsdiff patch failed: {0}")]
    Bsdiff(String),
    #[error("cosign2 error: {0}")]
    Cosign2(String),
    #[error("cosign2 header missing")]
    Cosign2HeaderMissing,
    #[error("crypto error")]
    CryptoError(#[from] crypto::error::CryptoError),
    #[error("security error")]
    SecurityError,
    #[error("invalid manifest")]
    InvalidManifest,
    #[error("unexpected error")]
    Unexpected(String),
    #[error("insufficient battery level for update")]
    InsufficientBattery,
    #[error("firmware rollback prevented: current timestamp = {current}, update timestamp = {update}")]
    RollbackPrevented { current: u32, update: u32 },
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self { Error::Io(e.to_string()) }
}

#[derive(Clone, Debug, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum DownloadError {
    #[error("io error: {0}")]
    Io(String),
    #[error(transparent)]
    Fs(#[from] fs::Error),
    #[error("envoy error: {0}")]
    EnvoyError(String),
    #[error("retry request failed: {0}")]
    RetryFailed(String),
    #[error("invalid state")]
    InvalidState,
    #[error("invalid chunk")]
    InvalidChunk,
    #[error("download stalled")]
    Stalled,
}

impl From<std::io::Error> for DownloadError {
    fn from(e: std::io::Error) -> Self { DownloadError::Io(e.to_string()) }
}
