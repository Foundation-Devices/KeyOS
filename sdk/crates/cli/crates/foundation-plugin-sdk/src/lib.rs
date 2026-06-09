// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! SDK for building Foundation CLI plugins

pub mod command;
pub mod context;
pub mod describe;
pub mod i18n;

// Re-export main types
pub use command::{Command, CommandError};
pub use context::CommandContext;
pub use describe::{ArgSpec, PluginSpec};
pub use i18n::PluginI18n;
