// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{Locale, Message};

const DEFAULT_QR_PRIORITY_VALUE_V0: u8 = 3;
const MIN_QR_PRIORITY_V0: u8 = 1;
const MAX_QR_PRIORITY_V0: u8 = 5;

/// Frozen v0 manifest schema. Add non-breaking fields (Option<T> or #[serde(default)])
/// here directly. For breaking changes: freeze this struct, define ManifestV1, and
/// update the `Manifest` type alias in lib.rs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestV0 {
    pub app_name: BTreeMap<Locale, String>,
    #[serde(with = "crate::app_id_hex")]
    pub app_id: [u8; crate::APP_ID_BYTE_LEN],
    #[serde(default)]
    pub servers: BTreeMap<String, BTreeMap<String, Message>>,
    #[serde(default)]
    pub fixed_sids: BTreeMap<String, String>,
    #[serde(default)]
    pub permissions: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub memory: Vec<String>,
    #[serde(default)]
    pub syscall: Vec<String>,
    #[serde(default)]
    pub qr_match_rules: Vec<QrMatchRuleV0>,
}

/// Frozen v0 API manifest schema. Same versioning rules as ManifestV0.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiManifestV0 {
    #[serde(default)]
    pub extends: Option<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, BTreeMap<String, Message>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QrMatchRuleV0 {
    pub id: String,
    #[serde(default)]
    pub priority: QrPriorityV0,
    #[serde(default)]
    pub id_localizations: BTreeMap<Locale, String>,
    pub sub_rules: BTreeMap<String, QrMatchSubRuleV0>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(try_from = "u8", into = "u8")]
pub struct QrPriorityV0(u8);

impl QrPriorityV0 {
    pub const DEFAULT: Self = Self(DEFAULT_QR_PRIORITY_VALUE_V0);

    pub const fn new(value: u8) -> Option<Self> {
        if value >= MIN_QR_PRIORITY_V0 && value <= MAX_QR_PRIORITY_V0 {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn get(self) -> u8 { self.0 }
}

impl Default for QrPriorityV0 {
    fn default() -> Self { Self::DEFAULT }
}

impl TryFrom<u8> for QrPriorityV0 {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or_else(|| {
            format!("QR priority must be between {MIN_QR_PRIORITY_V0} and {MAX_QR_PRIORITY_V0}")
        })
    }
}

impl From<QrPriorityV0> for u8 {
    fn from(value: QrPriorityV0) -> Self { value.get() }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QrMatchSubRuleV0 {
    QR {
        min_len: Option<usize>,
        max_len: Option<usize>,
        #[serde(default)]
        regex_pattern: Option<String>,
    },
    UR {
        ur_type: String,
    },
}
