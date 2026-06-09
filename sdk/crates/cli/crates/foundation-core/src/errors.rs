// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Umbrella error type for `foundation-core`.
//!
//! Each module already exposes its own typed error (`ConfigError`,
//! `ContextError`, `SdkError`, `SigningError`, `AppIdError`). This module
//! defines a `FoundationCoreError` that wraps all of them so callers can:
//!
//! - return a single `Result<T, FoundationCoreError>` from helpers that touch
//!   several modules
//! - match on a single enum at the binary boundary instead of plumbing five
//!   different error types
//! - convert into `anyhow::Error` cheaply (`?` works thanks to `From`)
//!
//! The per-module enums remain the canonical surface — they carry the
//! finest-grained detail. `FoundationCoreError` is purely additive.

use thiserror::Error;

use crate::config::{AppIdError, ConfigError};
use crate::context::ContextError;
use crate::sdk::SdkError;
use crate::signing::SigningError;

/// Umbrella error for `foundation-core` operations.
#[derive(Debug, Error)]
pub enum FoundationCoreError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Context(#[from] ContextError),

    #[error(transparent)]
    Sdk(#[from] SdkError),

    #[error(transparent)]
    Signing(#[from] SigningError),

    #[error(transparent)]
    AppId(#[from] AppIdError),
}

/// Convenience alias for the umbrella result.
pub type FoundationCoreResult<T> = std::result::Result<T, FoundationCoreError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppIdError;
    use crate::sdk::SdkError;

    #[test]
    fn wraps_app_id_error() {
        let err: FoundationCoreError = AppIdError::Empty.into();
        assert!(matches!(err, FoundationCoreError::AppId(AppIdError::Empty)));
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn wraps_sdk_error() {
        let err: FoundationCoreError = SdkError::NotFound.into();
        assert!(matches!(err, FoundationCoreError::Sdk(SdkError::NotFound)));
    }

    /// `?` should compose every per-module error type into the umbrella.
    #[test]
    fn question_mark_composes_module_errors() {
        fn produce_app_id() -> Result<(), AppIdError> { Err(AppIdError::MissingPrefix) }
        fn touch_several() -> FoundationCoreResult<()> {
            produce_app_id()?; // From<AppIdError> for FoundationCoreError
            Ok(())
        }
        let err = touch_several().unwrap_err();
        assert!(matches!(err, FoundationCoreError::AppId(AppIdError::MissingPrefix)));
    }
}
