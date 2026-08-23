// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod api;
pub mod messages;

/// Ordering keys for the configuration descriptor, which numbers the enabled interfaces
/// contiguously from zero in this order. The values are not the numbers the host sees.
pub mod interface_priorities {
    /// Priorities index the low byte of the MS OS 2.0 vendor revision, so they cannot exceed its
    /// width.
    pub const MAX_INTERFACE_PRIORITY: u8 = 7;

    /// Host wallets expect Legacy HID at bInterfaceNumber 0, so nothing may sort before it.
    pub const LEGACY_HID: u8 = 0;
    pub const MASS_STORAGE: u8 = 1;
    pub const CTAP_HID: u8 = 2;
    pub const USB_DEBUG: u8 = 3;
}

pub const MAJ_DEV_VERSION: u8 = 1;
pub const MIN_DEV_VERSION: u8 = 0;
/// Feeds bcdDevice, so changing it makes hosts treat the device as new hardware. MS OS 2.0
/// descriptor changes go through the vendor revision instead.
pub const BLD_DEV_VERSION: u8 = 2;

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl SetupPacket {
    pub fn from_bytes(setup_data: &[u8]) -> Self {
        SetupPacket {
            request_type: setup_data[0],
            request: setup_data[1],
            value: u16::from_le_bytes(setup_data[2..4].try_into().unwrap()),
            index: u16::from_le_bytes(setup_data[4..6].try_into().unwrap()),
            length: u16::from_le_bytes(setup_data[6..8].try_into().unwrap()),
        }
    }
}
