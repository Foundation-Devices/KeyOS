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
    validate_display_app_name, validate_icon_file, AppConfig, AppId, AppIdError, ConfigError, IconDimensions,
    PermissionEntries, PermissionsConfig, PublisherConfig, APP_CONFIG_FILE, APP_ICON_SIZE_PX,
    DISPLAY_APP_NAME_ALLOWED_CHARS, PERMISSION_TEMPLATES_FILE,
};
pub use context::{ContextError, ProjectContext};
pub use errors::{FoundationCoreError, FoundationCoreResult};
pub use manifest::{app_manifest_from_config, AppManifest};
pub use sdk::{SdkError, SdkLayout, SdkRoot};
pub use signing::{
    configured_signing_identities, foundation_dir, list_signing_identities, resolve_identity_cosign2_config,
    signing_identity_paths, signing_root_dir, SigningError, SigningIdentityPaths, COSIGN2_CONFIG_FILE,
};
