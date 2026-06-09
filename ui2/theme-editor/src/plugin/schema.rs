// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! JSON schema definitions for plugin files (button.json, etc.).
//!
//! These types now live in the shared `components::theme_gen` module so the
//! `foundation build` theme-compile step can reuse the exact same schema +
//! emitter for per-app component themes. They're re-exported here to keep the
//! editor's `plugin::*` surface (and `loader`/`storage`) unchanged.

pub use components::theme_gen::{DefaultValue, PluginDefinition, PropDefaults, PropDefinition, TokenOrValue};
