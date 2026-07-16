// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::{LazyLock, Mutex};

use lru::LruCache;
use slint::{Color as SlintColor, Image, Rgba8Pixel, SharedPixelBuffer};
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, Path, PathBuilder, Pattern, Pixmap, Point,
    SpreadMode, Stroke, Transform,
};

use super::{color_to_color, SlintGradientStop};

const CARD_CACHE_SIZE: usize = 16;
static CARD_CACHE: LazyLock<Mutex<LruCache<CardCacheKey, SharedPixelBuffer<Rgba8Pixel>>>> =
    LazyLock::new(|| Mutex::new(LruCache::new(CARD_CACHE_SIZE.try_into().unwrap())));

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct CardCacheKey {
    width: u32,
    height: u32,
    radius: u32,
    background_color: u32,
    border_color: u32,
    lines: bool,
}

pub fn round_rect_path(width: f32, height: f32, border_radius: f32, stroke_width: f32) -> Path {
    let z = stroke_width * 0.5; // little shift for the border
    let width = width - z; // Shrink a bit to let all border inside the box (stroke rendered )
    let height = height - z;
    let b = 0.448 * border_radius;
    let mut pb = PathBuilder::new();
    if border_radius > 0.0 {
        pb.move_to(border_radius, z); // top
        pb.line_to(width - border_radius, z);
        pb.cubic_to(
            // top-right
            width - b,
            z,
            width,
            b,
            width,
            border_radius,
        );
        pb.line_to(width, height - border_radius); // right
        pb.cubic_to(
            //  bottom-right
            width,
            height - b,
            width - b,
            height,
            width - border_radius,
            height,
        );
        pb.line_to(border_radius + z, height); // bottom
        pb.cubic_to(
            //  bottom-left
            b,
            height,
            z,
            height - b,
            z,
            height - border_radius,
        );
        pb.line_to(z, border_radius); // lefft
        pb.cubic_to(
            //  top-left
            z,
            z + b,
            z + b,
            z,
            border_radius,
            z,
        );
    } else {
        pb.move_to(z, z);
        pb.line_to(width, z); // top line
        pb.line_to(width, height); // right line
        pb.line_to(z, height); // bottom line
    }
    pb.close();

    pb.finish().unwrap()
}

// 2px semitransparent lines with 2px spacing rotated approximately 18deg
// Rendered into a tiny tileable pixmap that can be used as a brush.
pub const LINE_PATTERN: LazyLock<Pixmap> = LazyLock::new(|| {
    // Approximately 18 degrees
    const WIDTH: u32 = 50;
    const HEIGHT: u32 = 17;
    const LINES: usize = 4;
    const OPACITY: f32 = 0.2;
    let mut result = Pixmap::new(WIDTH, HEIGHT).unwrap();

    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba(0.0, 0.0, 0.0, OPACITY).expect("Color black with opacity 0.2"));
    let mut pb = PathBuilder::new();
    for i in 0..LINES * 2 + 1 {
        let offset_h = i as f32 * HEIGHT as f32 / LINES as f32;
        pb.move_to(0.0, offset_h);
        pb.line_to(WIDTH as f32, offset_h - HEIGHT as f32);
    }
    let path = pb.finish().unwrap();
    result.stroke_path(
        &path,
        &paint,
        &Stroke { width: 2.0, ..Default::default() },
        Transform::identity(),
        None,
    );
    result
});

fn multiply(bg: Color, fg: Color) -> Color {
    Color::from_rgba(
        bg.red() * (1.0 - fg.alpha()) + fg.red() * fg.alpha(),
        bg.green() * (1.0 - fg.alpha()) + fg.green() * fg.alpha(),
        bg.blue() * (1.0 - fg.alpha()) + fg.blue() * fg.alpha(),
        bg.alpha() * (1.0 - fg.alpha()) + fg.alpha(),
    )
    .unwrap()
}

pub fn line_card(
    width: f32,
    height: f32,
    background_color: SlintColor,
    background_gradient: Vec<SlintGradientStop>,
    border_radius: f32,
    border_width: f32,
    border_gradient: Vec<SlintGradientStop>,
    outer_border_width: f32,
    outer_border_gradient: Vec<SlintGradientStop>,
    template: String,
) -> Image {
    let w = width as u32;
    let h = height as u32;

    let cache_key = CardCacheKey {
        width: w,
        height: h,
        radius: border_radius as u32,
        background_color: background_color.as_argb_encoded(),
        border_color: border_gradient.get(0).map(|g| g.color.as_argb_encoded()).unwrap_or(0),
        lines: template == "lines",
    };
    if let Some(pixel_buffer) = CARD_CACHE.lock().unwrap().get(&cache_key) {
        return Image::from_rgba8_premultiplied(pixel_buffer.clone());
    }

    let mut pixel_buffer = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let mut pixmap = tiny_skia::PixmapMut::from_bytes(pixel_buffer.make_mut_bytes(), w, h).unwrap();
    pixmap.fill(tiny_skia::Color::TRANSPARENT);

    let path = round_rect_path(width, height, border_radius, border_width);

    let mut paint = Paint::default();

    let bg_color = color_to_color(background_color);

    let start = Point::from_xy(width * 0.5, 0.0);
    let end = Point::from_xy(width * 0.5, height);

    paint.set_color(bg_color);

    if background_gradient.len() == 0 {
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    } else {
        let stops = background_gradient
            .iter()
            .map(|gs| GradientStop::new(gs.stop * 0.01, multiply(bg_color, color_to_color(gs.color))))
            .collect();
        paint.shader =
            LinearGradient::new(start, end, stops, SpreadMode::Pad, Transform::identity()).unwrap();
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }

    if template == "lines" {
        let mut line_paint = Paint::default();
        let line_pattern = &*LINE_PATTERN;
        line_paint.shader = Pattern::new(
            line_pattern.as_ref(),
            SpreadMode::Repeat,
            tiny_skia::FilterQuality::Nearest,
            1.0,
            Transform::identity(),
        );
        pixmap.fill_path(&path, &line_paint, FillRule::Winding, Transform::identity(), None);
    }

    // inner border
    if border_width > 0.0 {
        let b = 2.0 * outer_border_width;
        let iw = width - b;
        let ih = height - b;
        let r = border_radius - (border_width + outer_border_width) / 2.0;
        let path = round_rect_path(iw, ih, r, border_width);
        let mut stroke = Stroke::default();
        stroke.width = border_width;

        match border_gradient.len() {
            0 => {}
            1 => {
                let c = border_gradient[0].color;
                paint.set_color(color_to_color(c));
            }
            _ => {
                let stops = border_gradient
                    .iter()
                    .map(|gs| GradientStop::new(gs.stop * 0.01, color_to_color(gs.color)))
                    .collect();
                paint.shader = LinearGradient::new(
                    start,
                    end,
                    stops,
                    SpreadMode::Pad,
                    Transform::from_translate(border_width, border_width),
                )
                .unwrap();
            }
        }

        pixmap.stroke_path(
            &path,
            &paint,
            &stroke,
            Transform::from_translate(outer_border_width, outer_border_width),
            None,
        );
    }

    // outer border
    if outer_border_width > 0.0 {
        let iw = width;
        let ih = height;
        let r = border_radius;
        let path = round_rect_path(iw, ih, r, outer_border_width);
        let mut stroke = Stroke::default();
        stroke.width = outer_border_width;
        match border_gradient.len() {
            0 => {}
            1 => {
                let c = outer_border_gradient[0].color;
                paint.set_color(color_to_color(c));
            }
            _ => {
                let stops = outer_border_gradient
                    .iter()
                    .map(|gs| GradientStop::new(gs.stop * 0.01, color_to_color(gs.color)))
                    .collect();
                paint.shader =
                    LinearGradient::new(start, end, stops, SpreadMode::Pad, Transform::identity()).unwrap();
            }
        }
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    CARD_CACHE.lock().unwrap().push(cache_key, pixel_buffer.clone());
    Image::from_rgba8_premultiplied(pixel_buffer)
}
