// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use tiny_skia::{
    ColorU8, FillRule, GradientStop, IntRect, LinearGradient, Paint, Path, PathBuilder, Pixmap, PixmapMut,
    Point, Rect, Stroke, Transform,
};

use crate::{
    colors::{to_color, KeyStyle, UI_BLUE_600},
    drawing::{drop_shadow, round_rect_path},
    font::draw_text,
    keyboard::{KEY_BORDER_RADIUS, KEY_HEIGHT, ROW_HEIGHT},
};

const PADDING: f32 = 8.0;
const MARGIN: f32 = 2.0;

const FONT_SCALE: f32 = 48.0;
const BUBBLE_SLOT_PLUS_WIDTH: f32 = 10.0;
const BUBBLE_HEIGHT: f32 = 80.0;
const BUBBLE_BACKGROUND: ColorU8 = UI_BLUE_600;
const BUBBLE_BORDER_RADIUS: f32 = 18.0;
const BUBBLE_Y_OFFSET: f32 = 96.0;

const SHADOW_RADIUS: u32 = 4;
const SHADOW_OFFSET_Y: usize = 2;
const SHADOW_OPACITY: u16 = 200;

const KEY_SHADOW_RADIUS: u32 = 18;
const KEY_SHADOW_OFFSET: usize = 8;
const KEY_SHADOW_OPACITY: u16 = 76;
const SELECTED_KEY_HEIGHT: f32 = 63.0;
const SELECTED_KEY_BORDER_RADIUS: f32 = 16.0;

pub struct OverlayCache {
    bubble_pixmap: Pixmap,
    selected_key_pixmap: Pixmap,
    offset_x: i32,
    offset_y: i32,
    cache_key: OverlayCacheKey,
}

#[derive(Default, PartialEq, Eq)]
struct OverlayCacheKey {
    blob_width: u32,
    key_width: u32,
    // Blob X offset versus the key it's above
    blob_offset: i32,
}

impl Default for OverlayCache {
    fn default() -> Self {
        Self {
            bubble_pixmap: Pixmap::new(1, 1).unwrap(),
            selected_key_pixmap: Pixmap::new(1, 1).unwrap(),
            offset_x: 0,
            offset_y: 0,
            cache_key: Default::default(),
        }
    }
}

fn overlay_shape_path(d: &Rect, k_x: f32, k_y: f32, k_w: f32) -> Path {
    let mut pb = PathBuilder::new();

    let x = d.x();
    let y = d.y();
    let width = d.width();
    let height = d.height();
    let b_bubble = 0.448 * BUBBLE_BORDER_RADIUS;
    let b_key = 0.448 * KEY_BORDER_RADIUS;

    /*
        0_______________
       8/               \1
        | A Á Á A Á Á Á |
       7\__       ______/2
          6\     /3
            | A |
           5\___/4

    */

    // 0
    pb.move_to(x + BUBBLE_BORDER_RADIUS, y);
    pb.line_to(x + width - BUBBLE_BORDER_RADIUS, y);
    // 1
    pb.cubic_to(x + width - b_bubble, y, x + width, y + b_bubble, x + width, y + BUBBLE_BORDER_RADIUS);
    pb.line_to(x + width, y + height - BUBBLE_BORDER_RADIUS);
    let dx = (x + width) - (k_x + k_w);

    if dx > 2.0 * KEY_BORDER_RADIUS {
        // 2
        pb.cubic_to(
            x + width,
            y + height - b_bubble,
            x + width - b_bubble,
            y + height,
            x + width - BUBBLE_BORDER_RADIUS,
            y + height,
        );
        pb.line_to(k_x + k_w + BUBBLE_BORDER_RADIUS, y + height);
        // 3
        pb.cubic_to(
            k_x + k_w,
            y + height,
            k_x + k_w,
            y + height + BUBBLE_BORDER_RADIUS,
            k_x + k_w,
            y + height + BUBBLE_BORDER_RADIUS,
        );
    } else {
        // 2 + 3
        pb.cubic_to(
            x + width,
            y + height,
            k_x + k_w,
            y + height,
            k_x + k_w,
            y + height + BUBBLE_BORDER_RADIUS,
        );
    }
    pb.line_to(k_x + k_w, k_y + KEY_HEIGHT - KEY_BORDER_RADIUS);
    // 4
    pb.cubic_to(
        k_x + k_w,
        k_y + KEY_HEIGHT - b_key,
        k_x + k_w - b_key,
        k_y + KEY_HEIGHT,
        k_x + k_w - KEY_BORDER_RADIUS,
        k_y + KEY_HEIGHT,
    );
    pb.line_to(k_x + KEY_BORDER_RADIUS, k_y + KEY_HEIGHT);
    // 5
    pb.cubic_to(
        k_x + KEY_BORDER_RADIUS - b_key,
        k_y + KEY_HEIGHT,
        k_x,
        k_y + KEY_HEIGHT - b_key,
        k_x,
        k_y + KEY_HEIGHT - KEY_BORDER_RADIUS,
    );
    pb.line_to(k_x, y + height + BUBBLE_BORDER_RADIUS);
    let dx = k_x - x;
    if dx > 2.0 * KEY_BORDER_RADIUS {
        // 6
        pb.cubic_to(
            k_x,
            y + height + b_bubble,
            k_x - b_bubble,
            y + height,
            k_x - BUBBLE_BORDER_RADIUS,
            y + height,
        );
        pb.line_to(x + BUBBLE_BORDER_RADIUS, y + height);
        // 7
        pb.cubic_to(x + b_bubble, y + height, x, y + height - b_bubble, x, y + height - BUBBLE_BORDER_RADIUS);
    } else {
        // 6 + 7
        pb.cubic_to(k_x, y + height, x, y + height, x, y + height - BUBBLE_BORDER_RADIUS);
    }
    pb.line_to(x, y + BUBBLE_BORDER_RADIUS);
    // 8
    pb.cubic_to(x, y + b_bubble, x + b_bubble, y, x + BUBBLE_BORDER_RADIUS, y);
    pb.close();
    pb.finish().unwrap()
}

pub fn draw(
    cache: &mut OverlayCache,
    text: &str,
    pixmap: &mut PixmapMut,
    key_rect: &Rect,
    mouse_x: i32,
) -> (IntRect, Option<char>) {
    let text_len = text.chars().count();
    let slot_width = key_rect.width() + BUBBLE_SLOT_PLUS_WIDTH;
    let blob_width: f32 = slot_width * (text_len as f32) + MARGIN * 2.0; // n chars + margins for left and right
    let x = (key_rect.x() - (blob_width - key_rect.width()) * 0.5)
        .clamp(PADDING, pixmap.width() as f32 - blob_width - PADDING);
    let y = (key_rect.y() - BUBBLE_Y_OFFSET).clamp(0.0, pixmap.height() as f32);

    let cta_colors = KeyStyle::Cta.colors();

    let cache_key = OverlayCacheKey {
        blob_width: blob_width as u32,
        key_width: key_rect.width() as u32,
        blob_offset: (x - key_rect.x()) as i32,
    };

    if cache.cache_key != cache_key {
        let offset_rect =
            Rect::from_xywh(x - key_rect.x(), y - key_rect.y(), blob_width, BUBBLE_HEIGHT).unwrap();
        let path = overlay_shape_path(&offset_rect, 0.0, 0.0, key_rect.width());
        let mut paint = Paint::default();

        cache.bubble_pixmap = Pixmap::new(
            path.bounds().width() as u32 + SHADOW_RADIUS * 2,
            path.bounds().height() as u32 + SHADOW_RADIUS * 2,
        )
        .unwrap();
        cache.offset_x = SHADOW_RADIUS as i32 - path.bounds().x() as i32;
        cache.offset_y = SHADOW_RADIUS as i32 - path.bounds().y() as i32;
        let transform = Transform::from_translate(cache.offset_x as f32, cache.offset_y as f32);
        // background
        paint.set_color(to_color(BUBBLE_BACKGROUND));
        cache.bubble_pixmap.fill_path(&path, &paint, FillRule::EvenOdd, transform, None);

        // border
        cache.bubble_pixmap.stroke_path(
            &path,
            &Paint {
                shader: LinearGradient::new(
                    Point::from_xy(0.0, -ROW_HEIGHT),
                    Point::from_xy(0.0, 0.0),
                    vec![
                        GradientStop::new(0.0, to_color(cta_colors.gradient_top)),
                        GradientStop::new(1.0, to_color(cta_colors.gradient_bottom)),
                    ],
                    tiny_skia::SpreadMode::Pad,
                    Transform::identity(),
                )
                .unwrap(),
                ..Default::default()
            },
            &Stroke { width: 2.0, ..Default::default() },
            transform,
            None,
        );

        drop_shadow(
            &mut cache.bubble_pixmap.as_mut(),
            SHADOW_RADIUS,
            SHADOW_OPACITY,
            0,
            SHADOW_OFFSET_Y as i32,
            false,
        );

        cache.selected_key_pixmap = Pixmap::new(
            slot_width as u32 + KEY_SHADOW_RADIUS * 2,
            SELECTED_KEY_HEIGHT as u32 + KEY_SHADOW_RADIUS * 2,
        )
        .unwrap();

        let mut bg = Paint::default();
        bg.set_color(to_color(cta_colors.background));
        let ch_rect = Rect::from_xywh(
            KEY_SHADOW_RADIUS as f32,
            KEY_SHADOW_RADIUS as f32 - KEY_SHADOW_OFFSET as f32,
            slot_width,
            SELECTED_KEY_HEIGHT,
        )
        .unwrap();
        let rr = round_rect_path(&ch_rect, SELECTED_KEY_BORDER_RADIUS);
        cache.selected_key_pixmap.fill_path(
            &rr,
            &bg,
            tiny_skia::FillRule::EvenOdd,
            Transform::identity(),
            None,
        );
        drop_shadow(
            &mut cache.selected_key_pixmap.as_mut(),
            KEY_SHADOW_RADIUS,
            KEY_SHADOW_OPACITY,
            0,
            KEY_SHADOW_OFFSET as i32,
            false,
        );

        cache.cache_key = cache_key;
    }

    let bubble_left = key_rect.x() as i32 - cache.offset_x;
    let bubble_top = key_rect.y() as i32 - cache.offset_y;
    let bubble_right = bubble_left + cache.bubble_pixmap.width() as i32;
    let bubble_bottom = bubble_top + cache.bubble_pixmap.height() as i32;
    pixmap.draw_pixmap(
        bubble_left,
        bubble_top,
        cache.bubble_pixmap.as_ref(),
        &Default::default(),
        Transform::identity(),
        None,
    );

    let key_index = ((mouse_x as f32 - x).clamp(0.0, blob_width - MARGIN * 2.0 - 1.0) / slot_width) as usize;

    let key_shadow_left =
        (x + MARGIN - KEY_SHADOW_RADIUS as f32 + slot_width * key_index as f32).round() as i32;
    let key_shadow_top = (y + MARGIN - KEY_SHADOW_RADIUS as f32 + KEY_SHADOW_OFFSET as f32).round() as i32;
    let key_shadow_right = key_shadow_left + cache.selected_key_pixmap.width() as i32;
    let key_shadow_bottom = key_shadow_top + cache.selected_key_pixmap.height() as i32;
    pixmap.draw_pixmap(
        key_shadow_left,
        key_shadow_top,
        cache.selected_key_pixmap.as_ref(),
        &Default::default(),
        Transform::identity(),
        None,
    );

    for (index, ch) in text.chars().enumerate() {
        let ch_rect = Rect::from_xywh(
            x + MARGIN + slot_width * index as f32,
            y + MARGIN,
            slot_width,
            SELECTED_KEY_HEIGHT,
        )
        .unwrap();
        draw_text(&ch.to_string(), FONT_SCALE, pixmap, &ch_rect, cta_colors.text);
    }
    let selected = text.chars().skip(key_index).next();
    let dirty = IntRect::from_ltrb(
        bubble_left.min(key_shadow_left),
        bubble_top.min(key_shadow_top),
        bubble_right.max(key_shadow_right),
        bubble_bottom.max(key_shadow_bottom),
    )
    .unwrap();
    (dirty, selected)
}
