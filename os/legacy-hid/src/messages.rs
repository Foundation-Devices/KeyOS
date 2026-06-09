// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

/// Outgoing APDU from the GUI app (a child Flux app's Rapdu) → write to the
/// HID IN endpoint, fragmented per the Flux HID transport.
#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct WriteHidApdu {
    pub channel_id: u16,
    pub data: Vec<u8>,
}

/// Toggle the Legacy Mode USB identity (0x2C97:0x0007) and trigger a
/// host-visible re-enumeration. Blocking so the caller knows when the
/// controller reset has completed.
#[derive(Debug, server::Message)]
#[response(())]
pub struct SetLegacyMode(pub bool);

/// One reassembled APDU received over USB HID. The channel_id matches the
/// host's current HID channel and is what `WriteHidApdu` must echo back.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct IncomingApdu {
    pub channel_id: u16,
    pub data: Vec<u8>,
}

/// Subscribe to the inbound APDU stream. The GUI app subscribes here on
/// startup and feeds events into its `SEPH_FIFO`.
#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(IncomingApdu)]
pub struct SubscribeIncomingApdu;

/// Internal-only message: the OUT thread delivers reassembled APDUs by
/// sending this to the server's main loop. Permission is only granted to
/// `os/legacy-hid` itself, mirroring `os/ctap-hid::ProcessHidPacket`.
#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DeliverHidApdu {
    pub channel_id: u16,
    pub data: Vec<u8>,
}
