// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Command trait definition

use async_trait::async_trait;

use crate::context::CommandContext;

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("This command must be run from within a Foundation project")]
    NoProject,

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// Trait that all commands must implement
#[async_trait]
pub trait Command: Send + Sync {
    /// Command name (used in CLI: `foundation <name>`)
    fn name(&self) -> &'static str;

    /// Short description for help text
    fn about(&self) -> String;

    /// Optional long description
    fn long_about(&self) -> Option<String> { None }

    /// Define clap arguments for this command
    fn args(&self) -> Vec<clap::Arg> { vec![] }

    /// Define subcommands (for nested commands like `foundation add page`)
    fn subcommands(&self) -> Vec<Box<dyn Command>> { vec![] }

    /// Commands that must run successfully before this one
    fn requires(&self) -> &[&'static str] { &[] }

    /// Whether this command requires a project context
    fn requires_project(&self) -> bool { true }

    /// Execute the command
    async fn execute(&self, ctx: &CommandContext) -> Result<(), CommandError>;
}
