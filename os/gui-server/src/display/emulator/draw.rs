// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    super::consts::{
        VIRT_HOME_BUTTON_HEIGHT, VIRT_HOME_BUTTON_WIDTH, VIRT_HOME_BUTTON_X, VIRT_HOME_BUTTON_Y,
    },
    crate::{display::PlatformDisplay, layers::LayerPixelFormat},
    gui_server_api::consts::{LCD_X, LCD_Y, SCREEN_HEIGHT, SCREEN_WIDTH},
    image::{GenericImage, GenericImageView, ImageBuffer, ImageReader, Rgba},
    rgb_led::RgbColor,
    std::sync::LazyLock,
};

static DEVICE_IMG: LazyLock<ImageBuffer<Rgba<u8>, Vec<u8>>> = LazyLock::new(|| {
    let mut reader = ImageReader::new(std::io::Cursor::new(include_bytes!("../../../assets/device.png")));
    reader.set_format(image::ImageFormat::Png);
    reader.decode().unwrap().to_rgba8()
});

/// A shadow gradient to give more realistic appearance when device is turned off
static BLANK_BUF: LazyLock<[u8; SCREEN_WIDTH * SCREEN_HEIGHT]> = LazyLock::new(|| {
    const COEF: u32 = 5;
    const CENTER_X: usize = SCREEN_WIDTH / 5;
    const DIST_OFFSET: u32 = 325;
    const GRAD_OFFSET: u32 = 25;
    let max_grad = ((SCREEN_WIDTH - CENTER_X) as f32 * (SCREEN_WIDTH - CENTER_X) as f32
        + SCREEN_HEIGHT as f32 * SCREEN_HEIGHT as f32)
        .sqrt() as u32
        / COEF;
    let mut result = [0u8; SCREEN_WIDTH * SCREEN_HEIGHT];

    for x in 0..SCREEN_WIDTH {
        for y in 0..SCREEN_HEIGHT {
            let dx = x.abs_diff(CENTER_X);
            let distance = DIST_OFFSET + ((dx * dx + y * y) as f32).sqrt() as u32;
            let grad = (distance / COEF) & 0xff;
            let grad = max_grad.saturating_sub(grad).saturating_sub(GRAD_OFFSET);
            result[y * SCREEN_WIDTH + x] = grad as u8;
        }
    }
    result
});

pub fn draw_lcd_contents(gfx: &mut impl GenericImage<Pixel = Rgba<u8>>) {
    let backlight_level = PlatformDisplay::backlight_level();
    if backlight_level != 0 {
        PlatformDisplay::with_layer_stack(|layers| {
            for (layer_idx, layer) in layers.layers.iter().enumerate() {
                let Some(layer) = layer else { continue };
                assert_eq!(layer.pixel_format(), LayerPixelFormat::Argb8888);
                let bytes_per_pixel = layer.pixel_format().bytes_per_pixel();
                let alpha = layer.alpha();

                let img = match layer.src() {
                    crate::layers::SourceType::Dma { range, .. } => {
                        let (src_w, src_h) = layer.src_dimensions();
                        let src_len = src_w
                            .checked_mul(src_h)
                            .and_then(|px| px.checked_mul(bytes_per_pixel))
                            .expect("validated layer dimensions");
                        debug_assert!(range.len() >= src_len);
                        // SAFETY: validated by Layer::new_with_pixel_format
                        let buf_slice =
                            unsafe { core::slice::from_raw_parts(range.as_ptr() as *const u8, src_len) };
                        let src_img: ImageBuffer<image::Rgba<u8>, &[u8]> =
                            ImageBuffer::from_raw(src_w as u32, src_h as u32, buf_slice).unwrap();
                        let (crop_x, crop_y) = layer.crop_pos();
                        let (crop_w, crop_h) = layer.crop_dimensions();
                        let mut img = src_img
                            .view(crop_x as u32, crop_y as u32, crop_w as u32, crop_h as u32)
                            .to_image();
                        if alpha != 255 {
                            for pixel in img.pixels_mut() {
                                pixel[3] = alpha;
                            }
                        }
                        img
                    }
                    crate::layers::SourceType::Color { r, g, b } => {
                        let (crop_w, crop_h) = layer.crop_dimensions();
                        let mut buf_vec = Vec::with_capacity(crop_w * crop_h * 4);
                        for _ in 0..crop_w * crop_h {
                            buf_vec.push(r);
                            buf_vec.push(g);
                            buf_vec.push(b);
                            buf_vec.push(alpha);
                        }
                        ImageBuffer::from_raw(crop_w as u32, crop_h as u32, buf_vec).unwrap()
                    }
                };

                let (x, y) = layer.dst_pos();
                let img = if layer.is_scaled() {
                    let (dst_w, dst_h) = layer.dst_dimensions();
                    image::imageops::resize(
                        &img,
                        dst_w as u32,
                        dst_h as u32,
                        image::imageops::FilterType::Nearest,
                    )
                } else {
                    img
                };

                if layer_idx == 0 {
                    gfx.copy_from(&img, x as u32, y as u32).ok();
                } else {
                    image::imageops::overlay(gfx, &img, x as i64, y as i64);
                }
            }
        });
    }

    if backlight_level != 0xff {
        let darken = ImageBuffer::from_fn(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32, |x, y| {
            let value = BLANK_BUF[y as usize * SCREEN_WIDTH + x as usize];
            image::Rgba::<u8>([value, value, value, 0xff - backlight_level])
        });
        image::imageops::overlay(gfx, &darken, 0, 0);
    }
}

pub fn draw_whole_device(gfx: &mut impl GenericImage<Pixel = Rgba<u8>>) {
    draw_lcd_contents(&mut *gfx.sub_image(LCD_X, LCD_Y, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32));
    image::imageops::overlay(gfx, &*DEVICE_IMG, 0, 0);
    let color = PlatformDisplay::rgb_led_color().map(RgbColor::from).unwrap_or(RgbColor::BLACK);
    colorize_home_button(gfx, color);
}

/// Neutralizes the baked-in LED highlight and blends color back in by intensity
fn colorize_home_button(gfx: &mut impl GenericImage<Pixel = Rgba<u8>>, color: RgbColor) {
    let x0 = VIRT_HOME_BUTTON_X as u32;
    let y0 = VIRT_HOME_BUTTON_Y as u32;
    let x1 = x0 + VIRT_HOME_BUTTON_WIDTH as u32;
    let y1 = y0 + VIRT_HOME_BUTTON_HEIGHT as u32;
    let intensity = color.r.max(color.g).max(color.b);
    let normalized = if intensity == 0 {
        RgbColor::BLACK
    } else {
        RgbColor {
            r: ((color.r as u32 * 255) / intensity as u32) as u8,
            g: ((color.g as u32 * 255) / intensity as u32) as u8,
            b: ((color.b as u32 * 255) / intensity as u32) as u8,
        }
    };

    for y in y0..y1 {
        for x in x0..x1 {
            let p = gfx.get_pixel(x, y);
            let luminance = (p[0] as u32 * 54 + p[1] as u32 * 183 + p[2] as u32 * 19) / 256;
            let led_mask = luminance.saturating_sub(32).min(160) * 255 / 160;

            let unlit = [
                blend_channel(p[0], p[0] / 8, led_mask as u8),
                blend_channel(p[1], p[1] / 8, led_mask as u8),
                blend_channel(p[2], p[2] / 8, led_mask as u8),
            ];
            let lit = [
                ((p[0] as u32 * normalized.r as u32) / 255) as u8,
                ((p[1] as u32 * normalized.g as u32) / 255) as u8,
                ((p[2] as u32 * normalized.b as u32) / 255) as u8,
            ];
            let light_alpha = ((intensity as u32 * led_mask) / 255) as u8;
            gfx.put_pixel(
                x,
                y,
                Rgba([
                    blend_channel(unlit[0], lit[0], light_alpha),
                    blend_channel(unlit[1], lit[1], light_alpha),
                    blend_channel(unlit[2], lit[2], light_alpha),
                    p[3],
                ]),
            );
        }
    }
}

fn blend_channel(from: u8, to: u8, alpha: u8) -> u8 {
    let from = from as u32;
    let to = to as u32;
    let alpha = alpha as u32;
    ((from * (255 - alpha) + to * alpha) / 255) as u8
}
