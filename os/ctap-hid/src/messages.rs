// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

// === External messages ===

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(())]
pub struct ProcessHidPacket(pub Vec<u8>);

// === Internal messages ===

// === Test messages ===

#[cfg(feature = "test-app")]
#[derive(Debug, server::Message)]
#[response(Result<(), crate::error::CtapHidError>)]
pub struct RegisterSimuUsbReceiver(pub xous::CID);

#[cfg(feature = "test-app")]
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct SimuUsbReceiveCallback(pub Vec<u8>);

#[cfg(feature = "test-app")]
impl server::MessageId for SimuUsbReceiveCallback {
    const ID: xous::MessageId = 65;
    const SERVER: &'static str = "";
}

#[cfg(feature = "test-app")]
impl server::BlockingArchive for SimuUsbReceiveCallback {
    type Response = ();
}
