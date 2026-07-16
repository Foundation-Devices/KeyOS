// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Microsoft OS 2.0 descriptors that make Windows auto-bind WinUSB to
//! the vendor-specific debug interface without a third-party driver.
//!
//! - [`PLATFORM_CAPABILITY`] is the body of the BOS Platform Capability descriptor (the 20-byte header is
//!   prepended by `register_capability`).
//! - [`descriptor_set`] returns the full MS OS 2.0 descriptor set that the host fetches via the vendor
//!   control request advertised in the platform capability.

use server::{handle_blocking_archive_message, BlockingArchiveHandler, MessageId, Server, ServerMessages};
use usb::device::messages::SetupPacketCallback;
use uuid::{uuid, Uuid};

pub const PLATFORM_CAPABILITY_UUID: Uuid = uuid!("D8DD60DF-4589-4CC7-9CD2-659D9E648A9F");

/// Arbitrary vendor request code; the host reads it from the platform
/// capability descriptor and uses it in the control request.
const MS_VENDOR_CODE: u8 = 0x01;

/// MS_OS_20_DESCRIPTOR_INDEX -- wIndex value the host uses when
/// fetching the descriptor set.
const DESCRIPTOR_SET_INDEX: u16 = 0x0007;

/// Body of the BOS Platform Capability functional descriptors. The 20-byte
/// MS OS 2.0 platform capability header is prepended by `register_capability`.
/// Layout: dwWindowsVersion (4) | wMSOSDescriptorSetTotalLength (2) | bMS_VendorCode (1) | bAltEnumCode (1).
#[rustfmt::skip]
pub const PLATFORM_CAPABILITY: [u8; 8] = [
    0x00, 0x00, 0x03, 0x06, // dwWindowsVersion = 0x06030000 (Win 8.1)
    0xAA, 0x00,             // wMSOSDescriptorSetTotalLength = 170
    MS_VENDOR_CODE,
    0,                      // bAltEnumCode
];

/// Offset of `bFirstInterface` inside [`DESCRIPTOR_SET_TEMPLATE`].
const INTERFACE_NUM_OFFSET: usize = 14;

/// MS OS 2.0 descriptor set template. Everything is fixed except for
/// `bFirstInterface` at byte [`INTERFACE_NUM_OFFSET`], which is patched at
/// runtime by [`descriptor_set`]. Total length 170 bytes:
///
///   10  Set Header
/// +  8  Function Subset Header
/// + 20  Feature: Compatible ID ("WINUSB")
/// +132  Feature: Registry Property (DeviceInterfaceGUIDs)
///
/// The Registry Property writes DeviceInterfaceGUIDs into the Windows
/// registry; host-side tools (rusb/nusb/libusb's WinUSB backend) read
/// this to construct the path needed to open the interface. Without it,
/// claim_interface fails even when winusb.sys is correctly bound.
#[rustfmt::skip]
const DESCRIPTOR_SET_TEMPLATE: [u8; 170] = [
    // ---- Set Header (10 bytes) ----
    0x0A, 0x00,                                 // wLength = 10
    0x00, 0x00,                                 // wDescriptorType = MS_OS_20_SET_HEADER_DESCRIPTOR
    0x00, 0x00, 0x03, 0x06,                     // dwWindowsVersion = 0x06030000 (Win 8.1)
    0xAA, 0x00,                                 // wTotalLength = 170

    // ---- Function Subset Header (8 bytes) ----
    0x08, 0x00,                                 // wLength = 8
    0x02, 0x00,                                 // wDescriptorType = MS_OS_20_SUBSET_HEADER_FUNCTION
    0,                                          // bFirstInterface (patched at runtime)
    0,                                          // bReserved
    0xA0, 0x00,                                 // wSubsetLength = 160

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

/// Return the MS OS 2.0 descriptor set with `bFirstInterface` patched.
pub fn descriptor_set(interface_num: u8) -> Vec<u8> {
    let mut set = DESCRIPTOR_SET_TEMPLATE.to_vec();
    set[INTERFACE_NUM_OFFSET] = interface_num;
    set
}

/// Setup-packet responder that answers the vendor "Get MS OS 2.0
/// Descriptor Set" request with a prebuilt descriptor set.
pub struct SetupResponder {
    pub descriptor_set: Vec<u8>,
}

impl ServerMessages for SetupResponder {
    const NAME: &'static str = "";

    fn messages() -> &'static [server::MessageDef<Self>]
    where
        Self: Sized,
    {
        &[(SetupPacketCallback::ID, handle_blocking_archive_message::<SetupPacketCallback, _>)]
    }
}

impl Server for SetupResponder {}

impl BlockingArchiveHandler<SetupPacketCallback> for SetupResponder {
    fn handle(
        &mut self,
        SetupPacketCallback(msg): SetupPacketCallback,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Option<Vec<u8>> {
        // Vendor IN to Device, MS_VENDOR_CODE, wIndex = 7 -> return set.
        if msg.request_type == 0xC0
            && msg.request == MS_VENDOR_CODE
            && msg.value == 0
            && msg.index == DESCRIPTOR_SET_INDEX
        {
            Some(self.descriptor_set.clone())
        } else {
            None
        }
    }
}
