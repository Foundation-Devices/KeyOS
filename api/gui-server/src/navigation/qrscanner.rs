// SPDX-FileCopyrightText: 2024-2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! QR code scanner navigation request and response formats.

use app_manifest::QrPriority;
use server::rkyv_with::WithAppId;
use xous::AppId;

/// Options for the QR Scanner navigation request.
///
/// Example with a left back arrow and a simple message:
///
/// ```rust,ignore
/// # use navigation::api::qrscanner::{ScanQrOptions};
/// let options = ScanQrOptions::default()
///     .with_start_location(Location::External)
///     .with_allowed_locations(AllowedLocations::specific(&[Location::External]))
///     .with_allowed_extensions(AllowedExtensions::specific(&["bin"]));
/// ```
/// A single rule (and the first sub-rule that triggered it) that matched a scanned QR code.
#[derive(Debug, Clone, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct ScanQrMatchedRule {
    pub rule_id: String,
    pub priority: QrPriority,
    /// The ID of the first sub-rule that matched — used for dispatch hints without bloating the
    /// message with every matching sub-rule.
    pub sub_rule_id: String,
}

/// One entry per app that has at least one matching rule for the scanned QR code.
/// All matched rules for that app are collected here so the same app never appears
/// more than once in the disambiguation list.
#[derive(Debug, Clone, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct ScanQrMatchingApp {
    #[rkyv(with = WithAppId)]
    pub id: AppId,
    pub matched_rules: Vec<ScanQrMatchedRule>,
}

#[derive(Debug, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct ScanQrOptions {
    pub header_title: String,
    pub header_left_icon: String,
    pub header_left_text: String,
    pub header_right_icon: String,
    pub header_right_text: String,
    pub message: String,
    pub button_icon: String,
    pub button_text: String,
    pub request_matching_apps: bool,
}

impl Default for ScanQrOptions {
    fn default() -> Self {
        Self {
            header_title: String::new(),
            header_left_icon: String::from("chevron-left"),
            header_left_text: String::new(),
            header_right_icon: String::new(),
            header_right_text: String::new(),
            message: String::new(),
            button_icon: String::new(),
            button_text: String::new(),
            request_matching_apps: false,
        }
    }
}

impl ScanQrOptions {
    pub fn new() -> Self {
        Self {
            header_title: String::new(),
            header_left_icon: String::new(),
            header_left_text: String::new(),
            header_right_icon: String::new(),
            header_right_text: String::new(),
            message: String::new(),
            button_icon: String::new(),
            button_text: String::new(),
            request_matching_apps: false,
        }
    }

    pub fn from_slice(data: &[u8]) -> Option<Self> {
        let Ok(archived) = rkyv::access::<ArchivedScanQrOptions, rkyv::rancor::Error>(data) else {
            return None;
        };
        rkyv::deserialize::<Self, rkyv::rancor::Error>(archived).ok()
    }

    pub fn serialize(&self) -> Vec<u8> { rkyv::to_bytes::<rkyv::rancor::Error>(self).unwrap().to_vec() }
}

#[derive(Debug, Clone, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct MatchedQrResult {
    pub scan_result: ScanQrResult,
    pub matched_rules: Vec<ScanQrMatchedRule>,
}

impl MatchedQrResult {
    pub fn from_slice(data: &[u8]) -> Option<Self> {
        let Ok(archived) = rkyv::access::<ArchivedMatchedQrResult, rkyv::rancor::Error>(data) else {
            return None;
        };
        rkyv::deserialize::<Self, rkyv::rancor::Error>(archived).ok()
    }

    pub fn serialize(&self) -> Vec<u8> { rkyv::to_bytes::<rkyv::rancor::Error>(self).unwrap().to_vec() }
}

#[derive(Debug, Clone, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub enum ScanQrResult {
    Qr { data: Vec<u8>, matching_apps: Option<Vec<ScanQrMatchingApp>> },
    Ur2 { ur_type: String, data: Vec<u8>, matching_apps: Option<Vec<ScanQrMatchingApp>> },
    LeftClicked,
    RightClicked,
    ButtonClicked,
}

impl ScanQrResult {
    pub fn new_qr(data: &[u8]) -> Self { Self::Qr { data: data.to_vec(), matching_apps: None } }

    pub fn new_ur2(ur_type: String, data: &[u8]) -> Self {
        Self::Ur2 { ur_type, data: data.to_vec(), matching_apps: None }
    }

    pub fn with_matching_apps(self, matching_apps: Vec<ScanQrMatchingApp>) -> Self {
        match self {
            Self::Qr { data, .. } => Self::Qr { data, matching_apps: Some(matching_apps) },
            Self::Ur2 { ur_type, data, .. } => {
                Self::Ur2 { ur_type, data, matching_apps: Some(matching_apps) }
            }
            other => other,
        }
    }

    pub fn new_cancelled() -> Self { Self::LeftClicked }

    pub fn from_slice(data: &[u8]) -> Option<Self> {
        let Ok(archived) = rkyv::access::<ArchivedScanQrResult, rkyv::rancor::Error>(data) else {
            return None;
        };
        rkyv::deserialize::<Self, rkyv::rancor::Error>(archived).ok()
    }

    pub fn serialize(&self) -> Vec<u8> { rkyv::to_bytes::<rkyv::rancor::Error>(self).unwrap().to_vec() }
}
