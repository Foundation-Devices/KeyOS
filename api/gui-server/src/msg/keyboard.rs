// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::KeyboardKind;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub struct UpdateKeyboard {
    pub kind: KeyboardKind,
    pub request_caps: bool,
    pub accept_button_text: String,
    pub accept_button_enabled: bool,
    pub delete_button_enabled: bool,
}

#[derive(Debug, server::Message)]
pub struct HideKeyboard;

#[derive(Debug, server::Message)]
pub struct KeyPressed(pub Option<crate::Key>);

#[derive(Debug, server::Message)]
pub struct KeyReleased(pub Option<crate::Key>);
