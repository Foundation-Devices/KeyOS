// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Terminal UI abstractions for Foundation CLI

mod pipeline;
mod progress;
mod prompts;
mod spinner;

use indicatif::MultiProgress;
pub use pipeline::Pipeline;
pub use progress::Progress;
pub use prompts::Prompts;
pub use spinner::Spinner;

pub struct TerminalUI {
    multi: MultiProgress,
}

impl TerminalUI {
    pub fn new() -> Self { Self { multi: MultiProgress::new() } }

    /// Create a spinner for indeterminate progress
    pub fn spinner(&self, message: &str) -> Spinner { Spinner::new(&self.multi, message) }

    /// Create a progress bar for known-length operations
    pub fn progress(&self, total: u64, message: &str) -> Progress {
        Progress::new(&self.multi, total, message)
    }

    /// Create a multi-step pipeline that updates in place
    pub fn pipeline(&self, steps: &[&str]) -> Pipeline { Pipeline::new(&self.multi, steps) }

    /// Print a success message with checkmark
    pub fn success(&self, message: &str) { self.multi.println(format!("  ✓ {}", message)).ok(); }

    /// Print a warning message
    pub fn warn(&self, message: &str) { self.multi.println(format!("  ⚠ {}", message)).ok(); }

    /// Print an error message
    pub fn error(&self, message: &str) { self.multi.println(format!("  ✗ {}", message)).ok(); }

    /// Print an info message
    pub fn info(&self, message: &str) { self.multi.println(format!("  ℹ {}", message)).ok(); }
}

impl Default for TerminalUI {
    fn default() -> Self { Self::new() }
}
