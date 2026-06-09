// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Progress bar for determinate progress

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub struct Progress {
    pub(crate) pb: ProgressBar,
}

impl Progress {
    pub(crate) fn new(multi: &MultiProgress, total: u64, message: &str) -> Self {
        let pb = multi.add(ProgressBar::new(total));
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{msg} [{bar:30.cyan/dim}] {pos}/{len}")
                .expect("valid template")
                .progress_chars("━━╸"),
        );
        pb.set_message(message.to_string());
        Self { pb }
    }

    /// Increment progress by 1
    pub fn inc(&self) { self.pb.inc(1); }

    /// Increment progress by n
    pub fn inc_by(&self, n: u64) { self.pb.inc(n); }

    /// Set the current position
    pub fn set_position(&self, pos: u64) { self.pb.set_position(pos); }

    /// Update the message
    pub fn set_message(&self, message: &str) { self.pb.set_message(message.to_string()); }

    /// Finish the progress bar
    pub fn finish(&self) { self.pb.finish_with_message("✓"); }

    /// Finish with a custom message
    pub fn finish_with_message(&self, message: &str) { self.pb.finish_with_message(message.to_string()); }
}
