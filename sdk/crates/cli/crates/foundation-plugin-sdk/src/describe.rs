// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Plugin specification for --describe output

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSpec {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long_about: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<ArgSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcommands: Vec<PluginSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgSpec {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short: Option<char>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default)]
    pub takes_value: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub possible_values: Vec<String>,
    #[serde(default)]
    pub required: bool,
}

impl PluginSpec {
    /// Generate spec from a clap Command
    pub fn from_clap(cmd: &clap::Command) -> Self {
        Self {
            name: cmd.get_name().to_string(),
            about: cmd.get_about().map(|s| s.to_string()),
            long_about: cmd.get_long_about().map(|s| s.to_string()),
            args: cmd
                .get_arguments()
                .filter(|a| {
                    let id = a.get_id().as_str();
                    id != "help" && id != "version"
                })
                .map(ArgSpec::from_clap)
                .collect(),
            subcommands: cmd.get_subcommands().map(Self::from_clap).collect(),
        }
    }

    /// Output as JSON for --describe
    pub fn to_json(&self) -> String { serde_json::to_string_pretty(self).unwrap_or_default() }
}

impl ArgSpec {
    pub fn from_clap(arg: &clap::Arg) -> Self {
        Self {
            name: arg.get_id().to_string(),
            short: arg.get_short(),
            long: arg.get_long().map(|s| s.to_string()),
            help: arg.get_help().map(|s| s.to_string()),
            takes_value: arg.get_num_args().map(|n| n.takes_values()).unwrap_or(false),
            value_name: arg.get_value_names().and_then(|v| v.first()).map(|s| s.to_string()),
            possible_values: arg.get_possible_values().iter().map(|v| v.get_name().to_string()).collect(),
            required: arg.is_required_set(),
        }
    }
}
