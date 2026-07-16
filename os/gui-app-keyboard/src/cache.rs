// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Mutex;

use gui_server_api::consts::{DEFAULT_KEYBOARD_HEIGHT, KEYBOARD_TOP_BAR_MARGIN, SCREEN_WIDTH};
use tiny_skia::{Pixmap, PixmapRef, Transform};

use crate::{
    colors::{SHADOW_OFFSET_Y, SHADOW_OPACITY_DARK, SHADOW_OPACITY_LIGHT, SHADOW_RADIUS},
    drawing::drop_shadow,
    keyboard::assets::{BG_DARK_IMAGE, BG_IMAGE_HEIGHT, BG_IMAGE_WIDTH, BG_LIGHT_IMAGE},
    layout::{
        alpha::{
            LAYOUT_ALPHA_LOWER, LAYOUT_ALPHA_NUMERIC, LAYOUT_ALPHA_PUNCTUATION, LAYOUT_ALPHA_UPPER,
            LAYOUT_ALPHA_UPPER_CAPS,
        },
        decimal::LAYOUT_DECIMAL,
        numeric::LAYOUT_NUMERIC,
        Layout,
    },
    IS_DARK,
};

static KEYBOARD_CACHE: Mutex<Vec<(&Layout, Pixmap)>> = Mutex::new(Vec::new());

pub fn refresh_cache() {
    log::info!("Rendering cache");
    *KEYBOARD_CACHE.lock().unwrap() = vec![];
    for layout in [
        &LAYOUT_NUMERIC,
        &LAYOUT_ALPHA_LOWER,
        &LAYOUT_ALPHA_UPPER,
        &LAYOUT_ALPHA_UPPER_CAPS,
        &LAYOUT_ALPHA_NUMERIC,
        &LAYOUT_ALPHA_PUNCTUATION,
        &LAYOUT_DECIMAL,
    ] {
        let pixmap = render_background(&layout);
        KEYBOARD_CACHE.lock().unwrap().push((layout, pixmap));
    }
    log::info!("Done rendering");
}

pub fn with_cached_pixmap<R>(layout: &'static Layout, f: impl FnOnce(PixmapRef) -> R) -> R {
    loop {
        {
            let cache = KEYBOARD_CACHE.lock().unwrap();
            for (cached_layout, pixmap) in &*cache {
                if core::ptr::eq(*cached_layout, layout) {
                    return f(pixmap.as_ref());
                }
            }
        }
        // Lock dropped before sleeping so other threads can populate the
        // cache while we wait.
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn render_background(layout: &'static Layout) -> Pixmap {
    let mut pixmap = Pixmap::new(SCREEN_WIDTH as u32, DEFAULT_KEYBOARD_HEIGHT as u32).unwrap();
    let is_dark = IS_DARK.load(std::sync::atomic::Ordering::SeqCst);
    for (y_range, row) in layout.rows_with_coords() {
        for (x_range, key) in row.keys_with_coords() {
            key.draw(x_range.start, y_range.start, &mut pixmap.as_mut(), false, None, None);
        }
    }
    drop_shadow(
        &mut pixmap.as_mut(),
        SHADOW_RADIUS,
        if is_dark { SHADOW_OPACITY_DARK } else { SHADOW_OPACITY_LIGHT },
        0,
        SHADOW_OFFSET_Y as i32,
        false,
    );

    let bg_image_data = if is_dark { BG_DARK_IMAGE } else { BG_LIGHT_IMAGE };
    let bg_image =
        PixmapRef::from_bytes(bg_image_data, BG_IMAGE_WIDTH as u32, BG_IMAGE_HEIGHT as u32).unwrap();
    pixmap.draw_pixmap(
        0,
        KEYBOARD_TOP_BAR_MARGIN as i32,
        bg_image,
        &tiny_skia::PixmapPaint { blend_mode: tiny_skia::BlendMode::DestinationOver, ..Default::default() },
        Transform::identity(),
        None,
    );
    pixmap
}
