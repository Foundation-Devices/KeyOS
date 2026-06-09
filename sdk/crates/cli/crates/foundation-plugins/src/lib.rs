// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Plugin system for Foundation CLI

pub mod cache;
pub mod external;
pub mod install;
pub mod registry;

// Re-export main types
pub use cache::PluginCache;
pub use external::exec_plugin;
pub use install::{InstallError, PluginInstaller};
pub use registry::{CommandRegistry, ResolvedCommand};
