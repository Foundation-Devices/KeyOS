// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{display::PlatformDisplay, layers::LayerPixelFormat},
    gui_server_api::consts::{LCD_X, LCD_Y, SCREEN_HEIGHT, SCREEN_WIDTH},
    image::{GenericImage, GenericImageView, ImageBuffer, ImageReader, Rgba},
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
}
