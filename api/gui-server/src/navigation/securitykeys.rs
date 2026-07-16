// SPDX-FileCopyrightText: 2024-2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Security Keys navigation request and response formats.
//!
//! Currently the only navigation request is the user-presence prompt. Operation outcome
//! notifications used to be a navigation request too, but were moved to a fido subscription
//! event (`SubscribeOperationOutcomes`) since the Security Keys app is guaranteed to be
//! running by the time an outcome fires.

/// Unified navigation request enum for the Security Keys app.
///
/// Kept as an enum (rather than a bare struct) so future nav variants can be added
/// without breaking the wire format.
#[derive(Debug, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub enum SecurityKeysNavRequest {
    UserPresence(UserPresenceOptions),
}

impl SecurityKeysNavRequest {
    pub fn from_slice(data: &[u8]) -> Option<Self> {
        let Ok(archived) = rkyv::access::<ArchivedSecurityKeysNavRequest, rkyv::rancor::Error>(data) else {
            return None;
        };
        rkyv::deserialize::<Self, rkyv::rancor::Error>(archived).ok()
    }

    pub fn serialize(&self) -> Vec<u8> {
        rkyv::to_bytes::<rkyv::rancor::Error>(self).map(|b| b.to_vec()).unwrap_or_default()
    }
}

/// Options for the User Presence navigation request.
///
/// ```rust,ignore
/// # use gui_server_api::navigation::securitykeys::{UserPresenceOptions};
/// let options = UserPresenceOptions::authentication(Some(0)).with_rp_id("foundation.xyz".to_string());
/// ```
#[derive(Debug, Clone, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct UserPresenceOptions {
    /// The security key index to use, or None to allow the user to select one.
    pub security_key_index: Option<usize>,
    pub authentication: bool,
    pub rp_id: Option<String>,
    pub rp_name: Option<String>,
    pub user_name: Option<String>,
    pub user_display_name: Option<String>,
}

impl UserPresenceOptions {
    pub fn registration(security_key_index: Option<usize>) -> Self {
        Self {
            security_key_index,
            authentication: false,
            rp_id: None,
            rp_name: None,
            user_name: None,
            user_display_name: None,
        }
    }

    pub fn authentication(security_key_index: Option<usize>) -> Self {
        Self {
            security_key_index,
            authentication: true,
            rp_id: None,
            rp_name: None,
            user_name: None,
            user_display_name: None,
        }
    }

    pub fn with_rp_id(self, rp_id: String) -> Self { Self { rp_id: Some(rp_id), ..self } }

    pub fn with_rp_name(self, rp_name: String) -> Self { Self { rp_name: Some(rp_name), ..self } }

    pub fn with_user_name(self, user_name: String) -> Self { Self { user_name: Some(user_name), ..self } }

    pub fn with_user_display_name(self, user_display_name: String) -> Self {
        Self { user_display_name: Some(user_display_name), ..self }
    }

    pub fn from_slice(data: &[u8]) -> Option<Self> {
        let Ok(archived) = rkyv::access::<ArchivedUserPresenceOptions, rkyv::rancor::Error>(data) else {
            return None;
        };
        rkyv::deserialize::<Self, rkyv::rancor::Error>(archived).ok()
    }

    pub fn serialize(&self) -> Vec<u8> { rkyv::to_bytes::<rkyv::rancor::Error>(self).unwrap().to_vec() }
}

#[derive(Debug, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct UserPresenceResult {
    present: bool,
    /// The selected key index when user confirms presence.
    /// This is used when the initial security_key_index was None, allowing
    /// the user to select a key during the user presence check.
    selected_key_index: Option<usize>,
}

impl UserPresenceResult {
    pub fn new_checked(selected_key_index: Option<usize>) -> Self {
        UserPresenceResult { present: true, selected_key_index }
    }

    pub fn new_cancelled() -> Self { UserPresenceResult { present: false, selected_key_index: None } }

    pub fn present(&self) -> bool { self.present }

    pub fn selected_key_index(&self) -> Option<usize> { self.selected_key_index }

    pub fn from_slice(data: &[u8]) -> Option<Self> {
        let Ok(archived) = rkyv::access::<ArchivedUserPresenceResult, rkyv::rancor::Error>(data) else {
            return None;
        };
        rkyv::deserialize::<Self, rkyv::rancor::Error>(archived).ok()
    }

    pub fn serialize(&self) -> Vec<u8> { rkyv::to_bytes::<rkyv::rancor::Error>(self).unwrap().to_vec() }
}
