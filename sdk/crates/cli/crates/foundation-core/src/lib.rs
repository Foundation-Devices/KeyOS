// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Core types and utilities for Foundation CLI

pub mod config;
pub mod context;
pub mod errors;
pub mod manifest;
pub mod sdk;
pub mod signing;

// Re-export main types
pub use config::{
    AppConfig, AppId, AppIdError, ConfigError, PermissionEntries, PermissionsConfig, PublisherConfig,
    APP_CONFIG_FILE, PERMISSION_TEMPLATES_FILE,
};
pub use context::{ContextError, ProjectContext};
pub use errors::{FoundationCoreError, FoundationCoreResult};
pub use manifest::AppManifest;
pub use sdk::{SdkError, SdkLayout, SdkRoot};
pub use signing::{
    configured_signing_identities, foundation_dir, list_signing_identities, resolve_identity_cosign2_config,
    signing_identity_paths, signing_root_dir, SigningError, SigningIdentityPaths, COSIGN2_CONFIG_FILE,
};
