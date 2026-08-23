// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Microsoft OS 2.0 descriptors, which let Windows bind a driver and write registry properties for
//! a function without a third-party INF file.
//!
//! Interfaces contribute only their own feature descriptors. The set header, function subsets and
//! BOS platform capability are built here, because a function subset names the interface number its
//! function currently carries and only the USB server knows that.

/// Vendor request code the host reads from the platform capability and fetches the set with.
pub const MS_VENDOR_CODE: u8 = 0x01;

/// MS_OS_20_DESCRIPTOR_INDEX, the wIndex the host fetches the descriptor set with.
pub const DESCRIPTOR_SET_INDEX: u16 = 0x0007;

/// Bump when the feature descriptors change. Windows keeps the registry properties it wrote during
/// a device's first enumeration unless the vendor revision differs from the one it recorded.
const DESCRIPTOR_REV: u16 = 1;

const SET_TOTAL_LENGTH_OFFSET: usize = 8;
const SUBSET_LENGTH_OFFSET: usize = 6;

const PLATFORM_CAPABILITY_LEN: u8 = 28;

/// Build the descriptor set covering `functions`, each an interface number paired with the feature
/// descriptors that apply to it. `enabled_priority_bits` has one bit set per enabled interface;
/// folding it into the vendor revision makes the revision change whenever the layout does, which
/// is what gets Windows to rewrite registry properties rather than reuse the recorded ones.
///
/// Returns an empty set when no function has features, which is the signal not to advertise the
/// capability at all.
pub fn descriptor_set(functions: &[(u8, &[u8])], enabled_priority_bits: u8) -> Vec<u8> {
    if functions.is_empty() {
        return Vec::new();
    }

    let revision = (DESCRIPTOR_REV << 8) | enabled_priority_bits as u16;

    #[rustfmt::skip]
    let mut set = vec![
        // Descriptor Set Header
        0x0A, 0x00,                                 // wLength
        0x00, 0x00,                                 // wDescriptorType: MS_OS_20_SET_HEADER_DESCRIPTOR
        0x00, 0x00, 0x03, 0x06,                     // dwWindowsVersion: 0x06030000 (Win 8.1)
        0x00, 0x00,                                 // wTotalLength, patched below
    ];

    for (interface_number, features) in functions {
        let subset_start = set.len();

        #[rustfmt::skip]
        set.extend_from_slice(&[
            // Function Subset Header
            0x08, 0x00,                             // wLength
            0x02, 0x00,                             // wDescriptorType: MS_OS_20_SUBSET_HEADER_FUNCTION
            *interface_number,                      // bFirstInterface
            0x00,                                   // bReserved
            0x00, 0x00,                             // wSubsetLength, patched below
        ]);
        set.extend_from_slice(features);
        #[rustfmt::skip]
        set.extend_from_slice(&[
            // Feature: Vendor Revision
            0x06, 0x00,                             // wLength
            0x08, 0x00,                             // wDescriptorType: MS_OS_20_FEATURE_VENDOR_REVISION
            revision as u8, (revision >> 8) as u8,  // VendorRevision
        ]);

        let subset_length = set.len() - subset_start;
        set[subset_start + SUBSET_LENGTH_OFFSET] = subset_length as u8;
        set[subset_start + SUBSET_LENGTH_OFFSET + 1] = (subset_length >> 8) as u8;
    }

    set[SET_TOTAL_LENGTH_OFFSET] = set.len() as u8;
    set[SET_TOTAL_LENGTH_OFFSET + 1] = (set.len() >> 8) as u8;
    set
}

/// Build the BOS platform capability pointing the host at a descriptor set of
/// `descriptor_set_len` bytes, or nothing when there is no set to advertise.
pub fn platform_capability(descriptor_set_len: usize) -> Vec<u8> {
    if descriptor_set_len == 0 {
        return Vec::new();
    }

    let set_length = descriptor_set_len as u16;

    #[rustfmt::skip]
    let capability: [u8; PLATFORM_CAPABILITY_LEN as usize] = [
        // Platform Device Capability
        PLATFORM_CAPABILITY_LEN,                        // bLength
        0x10,                                           // bDescriptorType: DEVICE_CAPABILITY
        0x05,                                           // bDevCapabilityType: PLATFORM
        0x00,                                           // bReserved
        // PlatformCapabilityUUID: D8DD60DF-4589-4CC7-9CD2-659D9E648A9F, little endian
        0xDF, 0x60, 0xDD, 0xD8, 0x89, 0x45, 0xC7, 0x4C,
        0x9C, 0xD2, 0x65, 0x9D, 0x9E, 0x64, 0x8A, 0x9F,
        0x00, 0x00, 0x03, 0x06,                         // dwWindowsVersion: 0x06030000 (Win 8.1)
        set_length as u8, (set_length >> 8) as u8,      // wMSOSDescriptorSetTotalLength
        MS_VENDOR_CODE,                                 // bMS_VendorCode
        0x00,                                           // bAltEnumCode
    ];
    capability.to_vec()
}
