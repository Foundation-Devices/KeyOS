// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReleaseManifest {
    pub label: String,
    pub mandatory: bool,
    pub reboot_required: bool,
    pub date: String,
    pub transactions: Vec<Transaction>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Transaction(Vec<Action>);

impl Transaction {
    pub fn new(actions: Vec<Action>) -> Self { Self(actions) }

    pub fn actions(&self) -> &[Action] { &self.0 }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    #[serde(rename_all = "kebab-case")]
    Patch {
        patch_file: String,
        patch_source: String,
        base_version: String,
        new_version: String,
    },
    #[serde(rename_all = "kebab-case")]
    PatchAdd {
        patch_file: String,
        patch_source: String,
        dest: String,
        base_version: String,
        new_version: String,
    },
    Add {
        source: String,
        dest: String,
    },
    #[serde(rename_all = "kebab-case")]
    Replace {
        source: String,
        dest: String,
        new_version: String,
    },
    UpdateBt,
    Delete {
        path: String,
    },
    Rename {
        source: String,
        dest: String,
    },
    Move {
        source: String,
        dest: String,
    },
    Copy {
        source: String,
        dest: String,
    },
    Set {
        setting: String,
        value: String,
    },
    #[serde(rename_all = "kebab-case")]
    OpenApp {
        app_id: String,
        route: String,
    },
}
