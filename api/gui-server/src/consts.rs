// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub const CONTROL_CENTER_HEIGHT_COLLAPSED_PX: usize = 40;
pub const CONTROL_CENTER_HEIGHT_EXPANDED_PX: usize = 400;

/// The minimal height of the Control Center when dragged down
pub const CONTROL_CENTER_MIN_HEIGHT_PX: usize =
    CONTROL_CENTER_HEIGHT_COLLAPSED_PX + CONTROL_CENTER_DRAG_MARGIN_PX + 12;

/// Number of pixels on the lower part of the Control Center where user can grab and drag
/// it
pub const CONTROL_CENTER_DRAG_MARGIN_PX: usize = 42;

pub const CONTROL_CENTER_BG_COLOR: u32 = 0xd3d2d0;

pub const DEFAULT_KEYBOARD_HEIGHT: usize = 396;
pub const KEYBOARD_TOP_BAR_MARGIN: usize = 90;
pub const KEYBOARD_BG_COLOR: u32 = 0x332824;

pub const SCREEN_WIDTH: usize = 480;
pub const SCREEN_HEIGHT: usize = 800;

/// Size of a full-screen framebuffer in bytes (SCREEN_WIDTH * SCREEN_HEIGHT * 4 bytes per pixel).
pub const FB_SIZE: usize = SCREEN_WIDTH * SCREEN_HEIGHT * 4;

pub const FPS: usize = 40;

/// Max height of the modal (popup) window.
pub const MODAL_HEIGHT: usize = 600;

/// Height of the area around modal top drag bar where the modal will get dragged if touched
pub const MODAL_DRAG_BAR_MARGIN_PX: usize = 32;

// Simulator consts
// Native (1x) size of the device-frame artwork (assets/device.png). The window
// is sized DEVICE_WIDTH/HEIGHT * scale_factor, and the composite buffers derive
// from these, so updating them here covers every scale mode.
pub const DEVICE_WIDTH: u32 = 592;
pub const DEVICE_HEIGHT: u32 = 1114;
// Origin of the inset screen within the device-frame artwork, in native (1x)
// pixels. The whole device composite is scaled by the scale factor afterward,
// so this offset stays aligned at every scale. Both the rendered screen and the
// touch area (TOUCH_AREA_X/Y) derive from these, so they move together.
pub const LCD_X: u32 = 56;
pub const LCD_Y: u32 = 138;

// Physical coordinates of the virtual touch button area (as they come from the hardware)
pub const VIRT_BUTTON_PHYS_ORIGIN_X: usize = 0;
pub const VIRT_BUTTON_PHYS_ORIGIN_Y: usize = 850;
pub const VIRT_BUTTON_PHYS_WIDTH: usize = 200;
pub const VIRT_BUTTON_PHYS_HEIGHT: usize = 121;

pub const CLOSE_TIMEOUT_EXIT_CODE: u32 = 2525;
