// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugin system for loading component definitions from JSON files.
//!
//! The theme editor uses plugins to dynamically define what components can be themed,
//! what variants/states/sizes they have, and what properties are editable.
//!
//! Plugins are loaded from `~/.foundation/theme-editor/plugins/` as JSON files.
//! Tokens are loaded from `~/.foundation/theme-editor/tokens.json`.

mod loader;
mod schema;
mod storage;
mod tokens;

pub use loader::*;
pub use schema::*;
pub use storage::*;
pub use tokens::*;
