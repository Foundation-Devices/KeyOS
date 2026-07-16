// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use crate::error::NfcError;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(Vec<u8>, Vec<u8>), NfcError>)]
pub struct ReadNdefRawMsg(pub Duration);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), NfcError>)]
pub struct WriteNdefRawMsg(pub (Vec<u8>, Vec<u8>, Duration));

#[derive(Debug, server::Message)]
#[response(())]
pub struct SetEnabled(pub bool);

#[derive(Debug, server::Message)]
#[response(bool)]
pub struct IsEnabled;

#[derive(Debug, server::Message)]
#[response(bool)]
pub struct IsActive;
