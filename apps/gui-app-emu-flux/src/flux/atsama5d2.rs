// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! ARM (atsama5d2) target draw shims. The HID transport, USB descriptor,
//! and Legacy-Mode VID/PID toggle now live in `os/legacy-hid` — see that
//! crate for everything that used to be in this file.

use super::{display, NbglArea};

pub(super) fn draw_rect(x0: u16, y0: u16, width: u16, height: u16, color: u32) {
    display::draw_rect(x0, y0, width, height, color);
}

pub(super) fn draw_image(area: NbglArea, bpp: u8, transformation: u8, buffer: &[u8], color_map: u8) {
    display::draw_image(area, bpp, transformation, buffer, color_map);
}

pub(super) fn draw_image_rle(area: NbglArea, bpp: u8, fore_color: u8, buffer: &[u8], nb_skipped_bytes: u8) {
    display::draw_image_rle(area, bpp, fore_color, buffer, nb_skipped_bytes);
}
