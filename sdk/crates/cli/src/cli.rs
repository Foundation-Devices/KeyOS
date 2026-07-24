// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Top-level CLI definition.
//!
//! The command tree is derived; each command owns its argument struct in its
//! own module (see `commands::*`), so this file only wires the variants
//! together and carries the top-level/long-form help text.

use clap::{Parser, Subcommand};

use crate::commands;

#[derive(Parser)]
#[command(
    name = "foundation",
    version,
    about = "Foundation CLI for KeyOS app development",
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a new KeyOS application project
    #[command(
        long_about = "Scaffolds a new KeyOS application with the necessary structure and configuration files"
    )]
    New(commands::new::NewArgs),

    /// Enter the KeyOS development environment
    #[command(
        long_about = "Starts a Nix development shell with all required tools and dependencies for KeyOS app development"
    )]
    Develop,

    /// Clean up the development environment
    #[command(
        long_about = "Removes SDK-related Nix state, clears cache directories, and runs garbage collection to free up disk space"
    )]
    Exit,

    /// Build the application
    #[command(long_about = "Compiles the KeyOS application and prepares it for deployment")]
    Build(commands::build::BuildArgs),

    /// Remove generated build and theme files
    #[command(
        long_about = "Removes the app's generated build and theme artifacts: the target/ directory (cargo output, the generated target/foundation UI/resources/theme files, and target/keyos hardware bundles), the generated manifest.toml, and the ui/ui SDK UI mapping. Authored source is left untouched"
    )]
    Clean,

    /// Build, sign, upload, and launch on hardware over usb-debug
    #[command(
        long_about = "Builds and signs the application, uploads it to a connected Passport Prime over usb-debug, and launches it through passport-drive MCP by default"
    )]
    Sideload(commands::sideload::SideloadArgs),

    /// Build and run in the simulator
    #[command(long_about = "Builds the KeyOS application and runs it in the KeyOS simulator")]
    Sim,

    /// Manage signing certificates
    #[command(
        long_about = "Generate, inspect, and install publisher certificates used for Foundation app signing",
        arg_required_else_help = true
    )]
    Cert(commands::cert::CertArgs),

    /// Open the app theme editor
    #[command(
        long_about = "Opens the visual theme editor for a specified theme JSON, or for the current app's configured theme JSON when no file is given"
    )]
    Theme(commands::theme::ThemeArgs),

    /// Manage app themes
    #[command(
        long_about = "Seed, generate, list, and scaffold KeyOS app themes in ~/.foundation/themes",
        arg_required_else_help = true
    )]
    Themes(commands::themes::ThemesArgs),

    /// Check development environment
    #[command(
        long_about = "Verifies that all required tools and dependencies are properly installed and configured"
    )]
    Doctor,

    /// Preview UI in foundation-slint-viewer
    #[command(
        long_about = "Opens the application UI in foundation-slint-viewer for quick preview without a full hardware build"
    )]
    Preview(commands::preview::PreviewArgs),

    /// Open the Passport USB log viewer
    #[command(
        long_about = "Launches foundation-keyos-log-viewer and automatically connects it to the Passport currently attached over USB"
    )]
    Logs(commands::logs::LogsArgs),

    /// Manage Foundation CLI plugins
    #[command(
        long_about = "Search, install, and uninstall Foundation CLI plugins",
        arg_required_else_help = true
    )]
    Plugin(commands::plugin::PluginArgs),

    /// Generate shell completions
    #[command(long_about = "Generates shell completion scripts for bash, zsh, fish, or PowerShell")]
    Completions(commands::completions::CompletionsArgs),
}
