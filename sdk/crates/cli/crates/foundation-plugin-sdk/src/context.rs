// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Command execution context

use crate::command::CommandError;

/// Context provided to commands during execution
pub struct CommandContext {
    /// Project context (None for commands that don't require a project)
    pub project: Option<foundation_core::ProjectContext>,

    /// Terminal UI for progress and output
    pub ui: foundation_ui::TerminalUI,

    /// Raw command-line arguments (after the command name)
    pub args: Vec<String>,
}

impl CommandContext {
    /// Create a new context, discovering project if possible
    pub fn new(args: Vec<String>) -> Self {
        Self {
            project: foundation_core::ProjectContext::discover_optional(),
            ui: foundation_ui::TerminalUI::new(),
            args,
        }
    }

    /// Create a context requiring a project (errors if not found)
    pub fn require_project(args: Vec<String>) -> Result<Self, foundation_core::ContextError> {
        Ok(Self {
            project: Some(foundation_core::ProjectContext::discover()?),
            ui: foundation_ui::TerminalUI::new(),
            args,
        })
    }

    /// Get project or return an error
    pub fn project(&self) -> Result<&foundation_core::ProjectContext, CommandError> {
        self.project.as_ref().ok_or(CommandError::NoProject)
    }
}
