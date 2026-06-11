// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Screen capture logic — composites the current layer stack into a raw pixel buffer.

use gui_server_api::consts::{SCREEN_HEIGHT, SCREEN_WIDTH};

use crate::Gui;

#[cfg(keyos)]
const BPP: usize = 4; // BGRA8888 / RGBA8888
#[cfg(keyos)]
const STRIDE: usize = SCREEN_WIDTH * BPP;

impl Gui {
    /// Composites the current display into `out` as raw pixel data.
    ///
    /// Iterates the layer stack built by `update_layers()`, reading pixel
    /// data from virtual addresses. This mirrors the simulator's
    /// `draw_lcd_contents` but without the `image` crate.
    pub(crate) fn capture_screen_into(&self, out: &mut [u8]) {
        #[cfg(not(keyos))]
        {
            use image::{ImageBuffer, Rgba};

            if let Some(mut image_buffer) =
                ImageBuffer::<Rgba<u8>, _>::from_raw(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32, out)
            {
                crate::display::draw::draw_lcd_contents(&mut image_buffer);
            }
        }

        #[cfg(keyos)]
        {
            use crate::layers::{LayerPixelFormat, SourceType};

            out[..SCREEN_HEIGHT * STRIDE].fill(0);

            for (i, layer) in self.layers.layers.iter().enumerate() {
                let Some(layer) = layer else { continue };

                let (crop_x, crop_y) = layer.crop_pos();
                let (crop_w, crop_h) = layer.crop_dimensions();
                let (dst_x, dst_y) = layer.dst_pos();
                let alpha = layer.alpha();

                match layer.src() {
                    SourceType::Dma { range, .. } => {
                        // TODO: compositing assumes 4-byte BPP, so the camera (Rgb565) layer
                        // won't appear in the capture until that's handled.
                        if layer.pixel_format() != LayerPixelFormat::Argb8888 {
                            continue;
                        }
                        let (src_w, src_h) = layer.src_dimensions();
                        let src_bytes = src_w * src_h * BPP;
                        debug_assert!(range.len() >= src_bytes);
                        // SAFETY: validated by Layer::new_with_pixel_format
                        let src =
                            unsafe { core::slice::from_raw_parts(range.as_ptr() as *const u8, src_bytes) };
                        if i == 0 && alpha == 255 && crop_x == 0 && dst_x == 0 && dst_y == 0 {
                            // Fast path: base layer, full opacity, no offset — memcpy rows.
                            let copy_h = crop_h.min(SCREEN_HEIGHT);
                            for row in 0..copy_h {
                                let src_off = (crop_y + row) * src_w * BPP;
                                let dst_off = row * STRIDE;
                                let copy_len = (crop_w * BPP).min(STRIDE);
                                if src_off + copy_len <= src.len() && dst_off + copy_len <= out.len() {
                                    out[dst_off..dst_off + copy_len]
                                        .copy_from_slice(&src[src_off..src_off + copy_len]);
                                }
                            }
                        } else {
                            composite_layer(
                                out, src, src_w, crop_x, crop_y, crop_w, crop_h, dst_x, dst_y, alpha,
                            );
                        }
                    }
                    SourceType::Color { r, g, b } => {
                        composite_color(out, r, g, b, alpha, crop_w, crop_h, dst_x, dst_y);
                    }
                }
            }
        }
    }
}

/// Alpha-composite a cropped region of `src` (BGRA8888) onto `dst` at (dst_x, dst_y).
#[cfg(keyos)]
fn composite_layer(
    dst: &mut [u8],
    src: &[u8],
    src_width: usize,
    crop_x: usize,
    crop_y: usize,
    crop_w: usize,
    crop_h: usize,
    dst_x: usize,
    dst_y: usize,
    alpha: u8,
) {
    for row in 0..crop_h {
        let dy = dst_y + row;
        if dy >= SCREEN_HEIGHT {
            break;
        }
        let src_row_off = ((crop_y + row) * src_width + crop_x) * BPP;
        let dst_row_off = dy * STRIDE + dst_x * BPP;
        for col in 0..crop_w {
            if dst_x + col >= SCREEN_WIDTH {
                break;
            }
            let si = src_row_off + col * BPP;
            let di = dst_row_off + col * BPP;
            if si + 3 >= src.len() || di + 3 >= dst.len() {
                break;
            }
            // Premultiplied alpha compositing (matches KeyosPixel::blend).
            // Source RGB is already premultiplied by source alpha.
            let la = alpha as u32;
            let sa = src[si + 3] as u32 * la / 255;
            if sa == 0 {
                continue;
            }
            let inv = 255 - sa;
            dst[di] = (dst[di] as u32 * inv / 255 + src[si] as u32 * la / 255) as u8;
            dst[di + 1] = (dst[di + 1] as u32 * inv / 255 + src[si + 1] as u32 * la / 255) as u8;
            dst[di + 2] = (dst[di + 2] as u32 * inv / 255 + src[si + 2] as u32 * la / 255) as u8;
            dst[di + 3] = (dst[di + 3] as u32 + sa - dst[di + 3] as u32 * sa / 255) as u8;
        }
    }
}

/// Fill a rectangle with a solid color, alpha-composited onto `dst`.
#[cfg(keyos)]
fn composite_color(
    dst: &mut [u8],
    r: u8,
    g: u8,
    b: u8,
    alpha: u8,
    width: usize,
    height: usize,
    dst_x: usize,
    dst_y: usize,
) {
    for row in 0..height {
        let dy = dst_y + row;
        if dy >= SCREEN_HEIGHT {
            break;
        }
        let dst_row_off = dy * STRIDE + dst_x * BPP;
        for col in 0..width {
            if dst_x + col >= SCREEN_WIDTH {
                break;
            }
            let di = dst_row_off + col * BPP;
            if di + 3 >= dst.len() {
                break;
            }
            // Premultiplied alpha compositing for a solid color.
            // Premultiply the color by alpha first.
            let sa = alpha as u32;
            if sa == 0 {
                continue;
            }
            let inv = 255 - sa;
            let pb = b as u32 * sa / 255;
            let pg = g as u32 * sa / 255;
            let pr = r as u32 * sa / 255;
            // BGRA order
            dst[di] = (dst[di] as u32 * inv / 255 + pb) as u8;
            dst[di + 1] = (dst[di + 1] as u32 * inv / 255 + pg) as u8;
            dst[di + 2] = (dst[di + 2] as u32 * inv / 255 + pr) as u8;
            dst[di + 3] = (dst[di + 3] as u32 + sa - dst[di + 3] as u32 * sa / 255) as u8;
        }
    }
}
