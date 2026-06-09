// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Spinner for indeterminate progress

use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub struct Spinner {
    pub(crate) pb: ProgressBar,
}

impl Spinner {
    pub(crate) fn new(multi: &MultiProgress, message: &str) -> Self {
        let pb = multi.add(ProgressBar::new_spinner());
        pb.set_style(
            ProgressStyle::default_spinner().template("  {spinner:.cyan} {msg}").expect("valid template"),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        pb.set_message(message.to_string());
        Self { pb }
    }

    /// Update the spinner message
    pub fn set_message(&self, message: &str) { self.pb.set_message(message.to_string()); }

    /// Finish with success (shows checkmark)
    pub fn finish_success(&self, message: &str) {
        // Change template to remove spinner placeholder before finishing
        self.pb.set_style(ProgressStyle::default_bar().template("  {msg}").expect("valid template"));
        self.pb.finish_with_message(format!("✓ {}", message));
    }

    /// Finish with error (shows X)
    pub fn finish_error(&self, message: &str) {
        // Change template to remove spinner placeholder before finishing
        self.pb.set_style(ProgressStyle::default_bar().template("  {msg}").expect("valid template"));
        self.pb.finish_with_message(format!("✗ {}", message));
    }

    /// Finish and clear the line
    pub fn finish_clear(&self) { self.pb.finish_and_clear(); }
}
