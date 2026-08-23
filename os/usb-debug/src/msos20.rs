// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Microsoft OS 2.0 feature descriptors that make Windows auto-bind WinUSB to the vendor-specific
//! debug interface without a third-party driver.
//!
//! The USB server wraps these in a function subset naming whichever interface number the debug
//! interface currently carries, and advertises the result through the BOS.

/// Feature descriptors for the debug function. 152 bytes:
///
///   20  Feature: Compatible ID ("WINUSB")
/// +132  Feature: Registry Property (DeviceInterfaceGUIDs)
///
/// The Registry Property writes DeviceInterfaceGUIDs into the Windows registry; host-side tools
/// (rusb/nusb/libusb's WinUSB backend) read this to construct the path needed to open the
/// interface. Without it, claim_interface fails even when winusb.sys is correctly bound.
#[rustfmt::skip]
pub const FEATURES: [u8; 152] = [
    // ---- Feature: Compatible ID (20 bytes) ----
    0x14, 0x00,                                 // wLength = 20
    0x03, 0x00,                                 // wDescriptorType = MS_OS_20_FEATURE_COMPATIBLE_ID
    b'W', b'I', b'N', b'U', b'S', b'B', 0, 0,   // CompatibleID
    0, 0, 0, 0, 0, 0, 0, 0,                     // SubCompatibleID

    // ---- Feature: Registry Property (132 bytes) ----
    // Writes DeviceInterfaceGUIDs = {C0F1A6F8-2D7A-4E83-9F8B-7D5E0E9C1234}
    0x84, 0x00,                                 // wLength = 132
    0x04, 0x00,                                 // wDescriptorType = MS_OS_20_FEATURE_REG_PROPERTY
    0x07, 0x00,                                 // wPropertyDataType = REG_MULTI_SZ
    0x2A, 0x00,                                 // wPropertyNameLength = 42
    // PropertyName: "DeviceInterfaceGUIDs\0" in UTF-16LE (42 bytes)
    b'D', 0, b'e', 0, b'v', 0, b'i', 0, b'c', 0, b'e', 0, b'I', 0, b'n', 0, b't', 0, b'e', 0, b'r', 0, b'f', 0, b'a', 0, b'c', 0, b'e', 0, b'G', 0, b'U', 0, b'I', 0, b'D', 0, b's', 0, 0, 0,
    0x50, 0x00,                                 // wPropertyDataLength = 80
    // PropertyData: "{C0F1A6F8-2D7A-4E83-9F8B-7D5E0E9C1234}\0\0" in UTF-16LE (80 bytes).
    // REG_MULTI_SZ requires double-NUL termination.
    b'{', 0, b'C', 0, b'0', 0, b'F', 0, b'1', 0, b'A', 0, b'6', 0, b'F', 0, b'8', 0, b'-', 0, b'2', 0, b'D', 0, b'7', 0, b'A', 0, b'-', 0, b'4', 0, b'E', 0, b'8', 0, b'3', 0, b'-', 0, b'9', 0, b'F', 0, b'8', 0, b'B', 0, b'-', 0, b'7', 0, b'D', 0, b'5', 0, b'E', 0, b'0', 0, b'E', 0, b'9', 0, b'C', 0, b'1', 0, b'2', 0, b'3', 0, b'4', 0, b'}', 0,
    0, 0, 0, 0,                                 // string NUL + MULTI_SZ extra NUL
];
