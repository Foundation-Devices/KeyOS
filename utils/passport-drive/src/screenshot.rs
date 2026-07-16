// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared BGRA → PNG pixel conversion for screenshots.

use crate::{FB_SIZE, SCREEN_HEIGHT, SCREEN_WIDTH};

/// Convert BGRA8888 pixel data to a PNG byte buffer.
pub fn bgra_to_png(bgra: &[u8]) -> Result<Vec<u8>, String> {
    if bgra.len() < FB_SIZE {
        return Err(format!("Screenshot payload too small: {} bytes, expected {}", bgra.len(), FB_SIZE));
    }

    let pixel_count = FB_SIZE / 4;
    let mut rgb = vec![0u8; pixel_count * 3];
    for i in (0..FB_SIZE).step_by(4) {
        let j = (i / 4) * 3;
        rgb[j] = bgra[i + 2]; // R
        rgb[j + 1] = bgra[i + 1]; // G
        rgb[j + 2] = bgra[i]; // B
    }

    let mut png_buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_buf, SCREEN_WIDTH, SCREEN_HEIGHT);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| format!("PNG header: {e}"))?;
        writer.write_image_data(&rgb).map_err(|e| format!("PNG data: {e}"))?;
    }
    Ok(png_buf)
}
