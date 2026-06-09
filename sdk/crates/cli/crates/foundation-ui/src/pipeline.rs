// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Multi-step pipeline progress

use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub struct Pipeline {
    pb: ProgressBar,
    steps: Vec<String>,
    current: usize,
    total: usize,
}

impl Pipeline {
    pub fn new(multi: &MultiProgress, steps: &[&str]) -> Self {
        let total = steps.len();
        let pb = multi.add(ProgressBar::new_spinner());
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.cyan} [{pos}/{len}] {msg}")
                .expect("valid template"),
        );
        pb.set_length(total as u64);
        pb.set_position(0);
        pb.enable_steady_tick(Duration::from_millis(80));

        Self { pb, steps: steps.iter().map(|s| s.to_string()).collect(), current: 0, total }
    }

    /// Advance to the next step
    pub fn advance(&mut self) {
        if self.current < self.total {
            self.pb.set_position(self.current as u64 + 1);
            self.pb.set_message(self.steps[self.current].clone());
            self.current += 1;
        }
    }

    /// Advance with a custom message (overrides step name)
    pub fn advance_with_message(&mut self, message: &str) {
        if self.current < self.total {
            self.pb.set_position(self.current as u64 + 1);
            self.pb.set_message(message.to_string());
            self.current += 1;
        }
    }

    /// Update the current step's message without advancing
    pub fn set_message(&self, message: &str) { self.pb.set_message(message.to_string()); }

    /// Mark the pipeline as complete
    pub fn finish(&self) { self.pb.finish_with_message("✓ Complete"); }

    /// Mark the pipeline as failed
    pub fn finish_error(&self, message: &str) { self.pb.finish_with_message(format!("✗ {}", message)); }
}
