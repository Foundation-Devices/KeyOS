// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::OnceLock;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Mask, Paint, PathBuilder, Pattern, Pixmap, Point, Rect,
    SpreadMode, Stroke, StrokeDash, Transform,
};

use super::card::LINE_PATTERN;

// Cached stripe background
static STRIPE_CACHE: OnceLock<Pixmap> = OnceLock::new();
const BOTTOM_VERTICAL_MARGIN: u32 = 6;

/// Get or create a cached stripe background pixmap
fn get_stripe_background(w: u32, h: u32, offset_x: f32, offset_y: f32) -> &'static Pixmap {
    STRIPE_CACHE.get_or_init(|| {
        let mut stripe_pixmap = Pixmap::new(w, h).unwrap();
        let mut line_paint = Paint::default();
        let line_pattern = &*LINE_PATTERN;
        line_paint.shader = Pattern::new(
            line_pattern.as_ref(),
            SpreadMode::Repeat,
            tiny_skia::FilterQuality::Nearest,
            1.0,
            Transform::from_translate(-offset_x, -offset_y),
        );
        stripe_pixmap.fill_rect(
            Rect::from_xywh(0.0, 0.0, w as f32, h as f32).unwrap(),
            &line_paint,
            Transform::identity(),
            None,
        );
        stripe_pixmap
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PricePoint {
    pub price: u32,
    pub timestamp: u64,
    #[serde(default)]
    pub is_pad: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct GraphPoint {
    pub x: f32,
    pub y: f32,
    pub price: f32,
    pub timestamp: u64,
}

pub fn graph_points(data: &[PricePoint], w: u32, h: u32, max_height: u32) -> Vec<GraphPoint> {
    if data.len() < 2 {
        return Vec::new();
    }

    let prices: Vec<u32> = data.iter().map(|point| point.price).collect();
    let max_value = *prices.iter().max().unwrap_or(&0);
    let min_value = *prices.iter().min().unwrap_or(&0);
    let is_constant = max_value == min_value;
    let value_range = (max_value - min_value) as f32;
    let max_y = h.saturating_sub(BOTTOM_VERTICAL_MARGIN) as f32;

    let min_timestamp = data.first().map(|point| point.timestamp).unwrap_or_default();
    let max_timestamp = data.last().map(|point| point.timestamp).unwrap_or(min_timestamp);
    let time_range = max_timestamp.saturating_sub(min_timestamp);
    let num_points = data.len() as f32;

    prices
        .iter()
        .enumerate()
        .map(|(index, price)| {
            let point = data[index];
            let time_offset = point.timestamp.saturating_sub(min_timestamp);
            let x = if time_range > 0 {
                (time_offset as f32 / time_range as f32) * w as f32
            } else {
                (index as f32 / (num_points - 1.0)) * w as f32
            };
            let scaled = if is_constant {
                0.0
            } else {
                ((*price - min_value) as f32 / value_range) * max_height as f32
            };
            let adjusted_value = if is_constant { max_y - h as f32 * 0.25 } else { scaled };
            let y = max_y - adjusted_value;

            GraphPoint { x, y, price: point.price as f32, timestamp: point.timestamp }
        })
        .collect()
}

pub fn draw_graph(
    data: &[PricePoint],
    w: u32,
    h: u32,
    max_height: u32,
    is_dark_mode: bool,
    offset_x: f32,
    offset_y: f32,
) -> Image {
    let mut pixel_buffer = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let mut pixmap = tiny_skia::PixmapMut::from_bytes(pixel_buffer.make_mut_bytes(), w, h).unwrap();
    pixmap.fill(Color::TRANSPARENT);

    if data.len() < 2 {
        return Image::from_rgba8_premultiplied(pixel_buffer);
    }

    let is_flatline = data.len() == 2 && data[0].price == data[1].price && data[0].is_pad;

    if is_flatline {
        draw_flatline_graph(&mut pixmap, data, w, h, max_height, is_dark_mode);
    } else {
        // Round offsets and apply modulo for proper tiling alignment
        let pattern_width = LINE_PATTERN.width() as f32;
        let pattern_height = LINE_PATTERN.height() as f32;
        let offset_x = offset_x.round() % pattern_width;
        let offset_y = offset_y.round() % pattern_height;
        draw_normal_graph(&mut pixmap, data, w, h, max_height, is_dark_mode, offset_x, offset_y);
    }

    Image::from_rgba8_premultiplied(pixel_buffer)
}

fn draw_flatline_graph(
    pixmap: &mut tiny_skia::PixmapMut,
    data: &[PricePoint],
    w: u32,
    h: u32,
    max_height: u32,
    is_dark_mode: bool,
) {
    const SHADOW_HEIGHT: usize = 6;

    let bg_color = if is_dark_mode {
        Color::from_rgba8(0x11, 0x11, 0x11, 0xff)
    } else {
        Color::from_rgba8(0xf6, 0xf6, 0xf6, 0xff)
    };
    let stale_fg_color = if is_dark_mode {
        Color::from_rgba8(0x5a, 0x59, 0x5a, 0xff)
    } else {
        Color::from_rgba8(0x95, 0x93, 0x94, 0xff)
    };
    let shadow = Color::from_rgba8(0x11, 0x11, 0x11, 0x0d);

    let max_y = h.saturating_sub(BOTTOM_VERTICAL_MARGIN);
    let adjusted_value = max_y as f32 - h as f32 * 0.25;
    let shadow_offset = SHADOW_HEIGHT.saturating_sub(1) as f32;
    let y = max_y as f32 - adjusted_value;
    let y_shadow = max_y as f32 - (adjusted_value - shadow_offset).max(0.0);

    let mut pb_line_stale = PathBuilder::new();
    let mut pb_line_shadow_stale = PathBuilder::new();
    let mut pb_fill_stale = PathBuilder::new();

    let min_timestamp = data[0].timestamp;
    let max_timestamp = data[1].timestamp;
    let time_range = max_timestamp.saturating_sub(min_timestamp);

    let x0 = 0.0;
    let x1 = if time_range > 0 { w as f32 } else { w as f32 };

    pb_fill_stale.move_to(x0, h as f32);
    pb_fill_stale.line_to(x0, y);
    pb_fill_stale.line_to(x1, y);
    pb_fill_stale.line_to(x1, h as f32);
    pb_fill_stale.close();

    pb_line_stale.move_to(x0, y);
    pb_line_stale.line_to(x1, y);
    pb_line_shadow_stale.move_to(x0, y_shadow);
    pb_line_shadow_stale.line_to(x1, y_shadow);

    let mut border_path = PathBuilder::default();
    border_path.push_rect(Rect::from_xywh(0., 0., w as f32, h as f32).unwrap());
    let border_path = border_path.finish().unwrap();

    let graph_stale_path = pb_line_stale.finish().unwrap();
    let shadow_stale_path = pb_line_shadow_stale.finish().unwrap();
    let fill_path_stale = pb_fill_stale.finish().unwrap();

    let mut paint_line_stale = Paint::default();
    paint_line_stale.set_color(stale_fg_color);
    let mut paint_line_shadow_stale = Paint::default();
    paint_line_shadow_stale.set_color(shadow);

    let (start, end) =
        (Point::from_xy(w as f32 / 2.0, h as f32), Point::from_xy(w as f32 / 2.0, (h - max_height) as f32));

    let stale_stops = if is_dark_mode {
        [(0.0, Color::from_rgba8(0x86, 0x83, 0x85, 0x33)), (1.0, Color::from_rgba8(0x45, 0x44, 0x44, 0xff))]
    } else {
        [(0.0, Color::from_rgba8(0x95, 0x93, 0x94, 0x00)), (1.0, Color::from_rgba8(0x95, 0x93, 0x94, 0x66))]
    };
    let stale_stops: Vec<_> =
        stale_stops.into_iter().map(|(pos, color)| GradientStop::new(pos, color)).collect();

    let paint_fill_stale = Paint {
        shader: LinearGradient::new(start, end, stale_stops, SpreadMode::Pad, Transform::identity()).unwrap(),
        ..Default::default()
    };

    let graph_stroke = Stroke {
        width: 2.0,
        dash: Some(StrokeDash::new(vec![10.0, 10.0], 0.0).unwrap()),
        ..Default::default()
    };
    let shadow_stroke = Stroke {
        width: SHADOW_HEIGHT as f32,
        dash: Some(StrokeDash::new(vec![10.0, 10.0], 0.0).unwrap()),
        ..Default::default()
    };

    let mut mask = Mask::new(w, h).unwrap();
    mask.fill_path(&border_path, FillRule::EvenOdd, false, Transform::identity());

    // Fill background
    let mut paint_inside = Paint::default();
    paint_inside.set_color(bg_color);
    pixmap.fill_path(&border_path, &paint_inside, FillRule::EvenOdd, Transform::identity(), None);

    // Draw stale fill gradient
    pixmap.fill_path(
        &fill_path_stale,
        &paint_fill_stale,
        FillRule::EvenOdd,
        Transform::identity(),
        Some(&mask),
    );

    // Draw stale line and shadow
    pixmap.stroke_path(
        &graph_stale_path,
        &paint_line_stale,
        &graph_stroke,
        Transform::identity(),
        Some(&mask),
    );
    pixmap.stroke_path(
        &shadow_stale_path,
        &paint_line_shadow_stale,
        &shadow_stroke,
        Transform::identity(),
        Some(&mask),
    );
}

fn draw_normal_graph(
    pixmap: &mut tiny_skia::PixmapMut,
    data: &[PricePoint],
    w: u32,
    h: u32,
    max_height: u32,
    is_dark_mode: bool,
    offset_x: f32,
    offset_y: f32,
) {
    const SHADOW_HEIGHT: usize = 6;

    let bg_color = if is_dark_mode {
        Color::from_rgba8(0x11, 0x11, 0x11, 0xff)
    } else {
        Color::from_rgba8(0xf6, 0xf6, 0xf6, 0xff)
    };
    let fg_color = Color::from_rgba8(0xbf, 0x75, 0x5f, 0xff);
    let stale_fg_color = match is_dark_mode {
        true => Color::from_rgba8(0x5a, 0x59, 0x5a, 0xff), // #5A595A
        false => Color::from_rgba8(0x95, 0x93, 0x94, 0xff), // #959394
    };
    let shadow = Color::from_rgba8(0x11, 0x11, 0x11, 0x0d);

    let mut pb_line = PathBuilder::new();
    let mut pb_line_shadow = PathBuilder::new();
    let mut pb_line_stale = PathBuilder::new();
    let mut pb_line_shadow_stale = PathBuilder::new();
    let mut pb_fill_fresh = PathBuilder::new();
    let mut pb_fill_stale = PathBuilder::new();

    let max_y = h.saturating_sub(BOTTOM_VERTICAL_MARGIN) as f32;
    let mut points = Vec::with_capacity(data.len());
    for point in graph_points(data, w, h, max_height) {
        let shadow_offset = SHADOW_HEIGHT.saturating_sub(1) as f32;
        let adjusted_value = max_y - point.y;
        let y_shadow = max_y - (adjusted_value - shadow_offset).max(0.0);
        points.push((point.x, point.y, y_shadow));
    }

    let mut last_segment_stale = None;
    for i in 1..points.len() {
        let (prev_x, prev_y, prev_y_shadow) = points[i - 1];
        let (x, y, y_shadow) = points[i];
        let is_pad_segment = data[i - 1].is_pad || data[i].is_pad;
        let fill = if is_pad_segment { &mut pb_fill_stale } else { &mut pb_fill_fresh };
        fill.move_to(prev_x, h as f32);
        fill.line_to(prev_x, prev_y);
        fill.line_to(x, y);
        fill.line_to(x, h as f32);
        fill.close();

        let start_new_run = last_segment_stale != Some(is_pad_segment);
        if is_pad_segment {
            if start_new_run {
                pb_line_stale.move_to(prev_x, prev_y);
                pb_line_shadow_stale.move_to(prev_x, prev_y_shadow);
            }
            pb_line_stale.line_to(x, y);
            pb_line_shadow_stale.line_to(x, y_shadow);
        } else {
            if start_new_run {
                pb_line.move_to(prev_x, prev_y);
                pb_line_shadow.move_to(prev_x, prev_y_shadow);
            }
            pb_line.line_to(x, y);
            pb_line_shadow.line_to(x, y_shadow);
        }
        last_segment_stale = Some(is_pad_segment);
    }

    let mut border_path = PathBuilder::default();
    border_path.push_rect(Rect::from_xywh(0., 0., w as f32, h as f32).unwrap());
    let border_path = border_path.finish().unwrap();

    let graph_path = pb_line.finish();
    let shadow_path = pb_line_shadow.finish();
    let graph_stale_path = pb_line_stale.finish();
    let shadow_stale_path = pb_line_shadow_stale.finish();
    let fill_path_fresh = pb_fill_fresh.finish();
    let fill_path_stale = pb_fill_stale.finish();
    let mut paint_line = Paint::default();
    paint_line.set_color(fg_color);
    let mut paint_line_shadow = Paint::default();
    paint_line_shadow.set_color(shadow);
    let mut paint_line_stale = Paint::default();
    paint_line_stale.set_color(stale_fg_color);
    let mut paint_line_shadow_stale = Paint::default();
    paint_line_shadow_stale.set_color(shadow);

    let (start, end) =
        (Point::from_xy(w as f32 / 2.0, h as f32), Point::from_xy(w as f32 / 2.0, (h - max_height) as f32));

    let stale_stops = if is_dark_mode {
        [(0.0, Color::from_rgba8(0x86, 0x83, 0x85, 0x33)), (1.0, Color::from_rgba8(0x45, 0x44, 0x44, 0xff))]
    } else {
        // Light mode: use lighter, more transparent colors for stale gradient
        [(0.0, Color::from_rgba8(0x95, 0x93, 0x94, 0x00)), (1.0, Color::from_rgba8(0x95, 0x93, 0x94, 0x66))]
    };
    let stale_stops: Vec<_> =
        stale_stops.into_iter().map(|(pos, color)| GradientStop::new(pos, color)).collect();
    let paint_fill_stale = Paint {
        shader: LinearGradient::new(start, end, stale_stops, SpreadMode::Pad, Transform::identity()).unwrap(),
        ..Default::default()
    };

    let fresh_stops =
        [(0.0, Color::from_rgba8(0xD6, 0x8B, 0x6E, 0x3f)), (1.0, Color::from_rgba8(0xD6, 0x8B, 0x6E, 0xff))];
    let fresh_stops: Vec<_> =
        fresh_stops.into_iter().map(|(pos, color)| GradientStop::new(pos, color)).collect();
    let paint_fill_fresh = Paint {
        shader: LinearGradient::new(start, end, fresh_stops, SpreadMode::Pad, Transform::identity()).unwrap(),
        ..Default::default()
    };

    let graph_stroke = Stroke { width: 2.0, ..Default::default() };
    let shadow_stroke = Stroke { width: SHADOW_HEIGHT as f32, ..Default::default() };

    let mut mask = Mask::new(w, h).unwrap();
    mask.fill_path(&border_path, FillRule::EvenOdd, false, Transform::identity());

    // Fill background
    let mut paint_inside = Paint::default();
    paint_inside.set_color(bg_color);
    pixmap.fill_path(&border_path, &paint_inside, FillRule::EvenOdd, Transform::identity(), None);

    // Create a mask for all graph fill areas
    let mut graph_fill_mask = Mask::new(w, h).unwrap();
    for path in [&fill_path_fresh, &fill_path_stale].into_iter().flatten() {
        graph_fill_mask.fill_path(path, FillRule::EvenOdd, false, Transform::identity());
    }

    // Draw fill gradients
    if let Some(path) = &fill_path_stale {
        pixmap.fill_path(path, &paint_fill_stale, FillRule::EvenOdd, Transform::identity(), Some(&mask));
    }
    if let Some(path) = &fill_path_fresh {
        pixmap.fill_path(path, &paint_fill_fresh, FillRule::EvenOdd, Transform::identity(), Some(&mask));
    }

    // Draw stripes on top of gradients
    let stripe_bg = get_stripe_background(w, h, offset_x, offset_y);
    pixmap.draw_pixmap(
        0,
        0,
        stripe_bg.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        Transform::identity(),
        Some(&graph_fill_mask),
    );

    // Draw lines and shadows
    for (path, paint, stroke) in [
        (&graph_path, &paint_line, &graph_stroke),
        (&shadow_path, &paint_line_shadow, &shadow_stroke),
        (&graph_stale_path, &paint_line_stale, &graph_stroke),
        (&shadow_stale_path, &paint_line_shadow_stale, &shadow_stroke),
    ] {
        if let Some(path) = path {
            pixmap.stroke_path(path, paint, stroke, Transform::identity(), Some(&mask));
        }
    }
}
