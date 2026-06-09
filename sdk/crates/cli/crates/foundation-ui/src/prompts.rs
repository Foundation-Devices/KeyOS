// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! User input prompts

use console::Term;
use dialoguer::{Confirm, Input, Select};

pub struct Prompts {
    term: Term,
}

impl Prompts {
    pub fn new() -> Self { Self { term: Term::stderr() } }

    /// Ask for text input
    pub fn input(&self, prompt: &str) -> Result<String, dialoguer::Error> {
        Input::new().with_prompt(prompt).interact_text_on(&self.term)
    }

    /// Ask for text input with a default value
    pub fn input_with_default(&self, prompt: &str, default: &str) -> Result<String, dialoguer::Error> {
        Input::new().with_prompt(prompt).default(default.to_string()).interact_text_on(&self.term)
    }

    /// Ask for confirmation (yes/no)
    pub fn confirm(&self, prompt: &str, default: bool) -> Result<bool, dialoguer::Error> {
        Confirm::new().with_prompt(prompt).default(default).interact_on(&self.term)
    }

    /// Select from a list of options
    pub fn select(&self, prompt: &str, options: &[&str]) -> Result<usize, dialoguer::Error> {
        Select::new().with_prompt(prompt).items(options).interact_on(&self.term)
    }
}

impl Default for Prompts {
    fn default() -> Self { Self::new() }
}
