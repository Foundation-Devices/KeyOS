// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Mask, Paint, PathBuilder, Point, Rect, SpreadMode, Stroke,
    StrokeDash, Transform,
};

const BOTTOM_VERTICAL_MARGIN: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PricePoint {
    pub price: u32,
    pub timestamp: u64,
    #[serde(default)]
    pub is_pad: bool,
}

const GRAPH_BORDER_RADIUS: f32 = 14.0;
const GRAPH_BORDER_STROKE_WIDTH: f32 = 2.0;

#[derive(Debug, Clone, Copy)]
pub struct GraphPoint {
    pub x: f32,
    pub y: f32,
    pub price: f32,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct GraphStyle {
    pub is_price_positive: bool,
    pub is_dark_mode: bool,
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

pub fn draw_graph(data: &[PricePoint], w: u32, h: u32, max_height: u32, style: GraphStyle) -> Image {
    let mut pixel_buffer = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let mut pixmap = tiny_skia::PixmapMut::from_bytes(pixel_buffer.make_mut_bytes(), w, h).unwrap();
    pixmap.fill(Color::TRANSPARENT);

    if data.len() < 2 {
        return Image::from_rgba8_premultiplied(pixel_buffer);
    }

    let is_flatline = data.len() == 2 && data[0].price == data[1].price && data[0].is_pad;

    if is_flatline {
        draw_flatline_graph(&mut pixmap, data, w, h, max_height, style.is_dark_mode);
    } else {
        draw_normal_graph(&mut pixmap, data, w, h, max_height, style.is_price_positive, style.is_dark_mode);
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
    let stale_fg_color = if is_dark_mode {
        Color::from_rgba8(0x5a, 0x59, 0x5a, 0xff)
    } else {
        Color::from_rgba8(0x95, 0x93, 0x94, 0xff)
    };
    let max_y = h.saturating_sub(BOTTOM_VERTICAL_MARGIN);
    let adjusted_value = max_y as f32 - h as f32 * 0.25;
    let y = max_y as f32 - adjusted_value;

    let mut pb_line_stale = PathBuilder::new();
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

    let mut border_path = PathBuilder::default();
    border_path.push_rect(Rect::from_xywh(0., 0., w as f32, h as f32).unwrap());
    let border_path = border_path.finish().unwrap();

    let graph_stale_path = pb_line_stale.finish().unwrap();
    let fill_path_stale = pb_fill_stale.finish().unwrap();

    let mut paint_line_stale = Paint::default();
    paint_line_stale.set_color(stale_fg_color);
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
    let mut mask = Mask::new(w, h).unwrap();
    mask.fill_path(&border_path, FillRule::EvenOdd, false, Transform::identity());

    // Draw stale fill gradient
    pixmap.fill_path(
        &fill_path_stale,
        &paint_fill_stale,
        FillRule::EvenOdd,
        Transform::identity(),
        Some(&mask),
    );

    // Draw stale line.
    pixmap.stroke_path(
        &graph_stale_path,
        &paint_line_stale,
        &graph_stroke,
        Transform::identity(),
        Some(&mask),
    );
    draw_graph_border(pixmap, w, h);
}

fn draw_normal_graph(
    pixmap: &mut tiny_skia::PixmapMut,
    data: &[PricePoint],
    w: u32,
    h: u32,
    max_height: u32,
    is_price_positive: bool,
    is_dark_mode: bool,
) {
    // Match the price-change pill's Palette.content-positive/negative color.
    let (fresh_r, fresh_g, fresh_b) = if is_price_positive {
        (0x2e, 0x94, 0x83) // UIColors.green-500
    } else {
        (0xff, 0x33, 0x33) // UIColors.red-500
    };
    let fg_color = Color::from_rgba8(fresh_r, fresh_g, fresh_b, 0xff);
    let stale_fg_color = match is_dark_mode {
        true => Color::from_rgba8(0x5a, 0x59, 0x5a, 0xff), // #5A595A
        false => Color::from_rgba8(0x95, 0x93, 0x94, 0xff), // #959394
    };
    let mut pb_line = PathBuilder::new();
    let mut pb_line_stale = PathBuilder::new();
    let mut pb_fill_fresh = PathBuilder::new();
    let mut pb_fill_stale = PathBuilder::new();

    let mut points = Vec::with_capacity(data.len());
    for point in graph_points(data, w, h, max_height) {
        points.push((point.x, point.y));
    }

    let mut last_segment_stale = None;
    for i in 1..points.len() {
        let (prev_x, prev_y) = points[i - 1];
        let (x, y) = points[i];
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
            }
            pb_line_stale.line_to(x, y);
        } else {
            if start_new_run {
                pb_line.move_to(prev_x, prev_y);
            }
            pb_line.line_to(x, y);
        }
        last_segment_stale = Some(is_pad_segment);
    }

    let mut border_path = PathBuilder::default();
    border_path.push_rect(Rect::from_xywh(0., 0., w as f32, h as f32).unwrap());
    let border_path = border_path.finish().unwrap();

    let graph_path = pb_line.finish();
    let graph_stale_path = pb_line_stale.finish();
    let fill_path_fresh = pb_fill_fresh.finish();
    let fill_path_stale = pb_fill_stale.finish();
    let mut paint_line = Paint::default();
    paint_line.set_color(fg_color);
    let mut paint_line_stale = Paint::default();
    paint_line_stale.set_color(stale_fg_color);

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

    // Keep the existing 8%-to-80% opacity gradient while applying the sign color.
    let fresh_stops = [
        (0.0, Color::from_rgba8(fresh_r, fresh_g, fresh_b, 0x14)),
        (1.0, Color::from_rgba8(fresh_r, fresh_g, fresh_b, 0xcc)),
    ];
    let fresh_stops: Vec<_> =
        fresh_stops.into_iter().map(|(pos, color)| GradientStop::new(pos, color)).collect();
    let paint_fill_fresh = Paint {
        shader: LinearGradient::new(start, end, fresh_stops, SpreadMode::Pad, Transform::identity()).unwrap(),
        ..Default::default()
    };

    let graph_stroke = Stroke { width: 2.0, ..Default::default() };
    let mut mask = Mask::new(w, h).unwrap();
    mask.fill_path(&border_path, FillRule::EvenOdd, false, Transform::identity());

    // Draw fill gradients
    if let Some(path) = &fill_path_stale {
        pixmap.fill_path(path, &paint_fill_stale, FillRule::EvenOdd, Transform::identity(), Some(&mask));
    }
    if let Some(path) = &fill_path_fresh {
        pixmap.fill_path(path, &paint_fill_fresh, FillRule::EvenOdd, Transform::identity(), Some(&mask));
    }

    // Draw lines.
    for (path, paint, stroke) in
        [(&graph_path, &paint_line, &graph_stroke), (&graph_stale_path, &paint_line_stale, &graph_stroke)]
    {
        if let Some(path) = path {
            pixmap.stroke_path(path, paint, stroke, Transform::identity(), Some(&mask));
        }
    }

    draw_graph_border(pixmap, w, h);
}

fn draw_graph_border(pixmap: &mut tiny_skia::PixmapMut, w: u32, h: u32) {
    if w < 2 || h < 2 {
        return;
    }

    let stroke_width = GRAPH_BORDER_STROKE_WIDTH;
    let inset = stroke_width / 2.0 + 0.5;
    let left = inset;
    let right = w as f32 - inset;
    let top = 0.0;
    let bottom = h as f32 - inset;
    let radius = GRAPH_BORDER_RADIUS.min((right - left) / 2.0).min(bottom - top);
    let control = radius * 0.552_284_8;

    let mut pb = PathBuilder::new();
    pb.move_to(left, top);
    pb.line_to(left, bottom - radius);
    pb.cubic_to(left, bottom - radius + control, left + radius - control, bottom, left + radius, bottom);
    pb.line_to(right - radius, bottom);
    pb.cubic_to(right - radius + control, bottom, right, bottom - radius + control, right, bottom - radius);
    pb.line_to(right, top);

    let Some(path) = pb.finish() else {
        return;
    };

    let stops = vec![
        GradientStop::new(0.0, Color::from_rgba8(0x73, 0x50, 0x45, 0x00)),
        GradientStop::new(0.25, Color::from_rgba8(0x73, 0x50, 0x45, 0x00)),
        GradientStop::new(1.0, Color::from_rgba8(0xbf, 0xaf, 0xa9, 0xff)),
    ];
    let paint = Paint {
        shader: LinearGradient::new(
            Point::from_xy(w as f32 / 2.0, 0.0),
            Point::from_xy(w as f32 / 2.0, h as f32),
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        )
        .unwrap(),
        ..Default::default()
    };
    let stroke = Stroke { width: stroke_width, ..Default::default() };

    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}
