// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use slint::{Color as SlintColor, Image, Rgba8Pixel, SharedPixelBuffer};
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, PixmapMut, Point, RadialGradient,
    SpreadMode, Stroke, StrokeDash, Transform,
};

use super::{card::round_rect_path, color_to_color, color_to_paint};

#[derive(PartialEq)]
#[repr(u8)]
pub enum GradientDirection {
    None = 0,
    Vertical = 1,
    Horizontal = 2,
    DiagonalFolling = 3,
    DiagonalRizing = 4,
    Radial = 5,
}

pub struct SlintGradientStop {
    pub color: SlintColor,
    pub stop: f32,
}

impl SlintGradientStop {
    pub fn new(color: SlintColor, stop: f32) -> Self { Self { color, stop } }
}

pub fn frame(
    width: f32,
    height: f32,
    border_radius: f32,
    dash: Vec<f32>,
    gradient_direction: GradientDirection,
    gradient_stops: Vec<SlintGradientStop>,
    stroke_width: f32,
    stroke_color: SlintColor,
) -> Image {
    let w = width as u32;
    let h = height as u32;

    let mut pixel_buffer = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let mut pixmap = PixmapMut::from_bytes(pixel_buffer.make_mut_bytes(), w, h).unwrap();
    pixmap.fill(Color::TRANSPARENT);

    let path = round_rect_path(width, height, border_radius, stroke_width);
    if gradient_direction != GradientDirection::None {
        let width = width - stroke_width; // Shrink a bit to let border be drawn correctly
        let height = height - stroke_width;

        let mut paint = Paint::default();
        paint.anti_alias = false;

        let stops = gradient_stops
            .iter()
            .map(|gs| GradientStop::new(gs.stop * 0.01, color_to_color(gs.color)))
            .collect();

        match gradient_direction {
            GradientDirection::Vertical
            | GradientDirection::Horizontal
            | GradientDirection::DiagonalFolling
            | GradientDirection::DiagonalRizing => {
                let start = match gradient_direction {
                    GradientDirection::Vertical => Point::from_xy(width * 0.5, 0.0),
                    GradientDirection::DiagonalFolling => Point::from_xy(0.0, 0.0),
                    GradientDirection::DiagonalRizing => Point::from_xy(0.0, height),
                    GradientDirection::Horizontal => Point::from_xy(0.0, height * 0.5),
                    _ => Point::from_xy(width * 0.5, 0.0),
                };
                let end = match gradient_direction {
                    GradientDirection::Vertical => Point::from_xy(width * 0.5, height),
                    GradientDirection::DiagonalFolling => Point::from_xy(width, height),
                    GradientDirection::DiagonalRizing => Point::from_xy(width, 0.0),
                    GradientDirection::Horizontal => Point::from_xy(width, height * 0.5),
                    _ => Point::from_xy(width * 0.5, height),
                };

                paint.shader =
                    LinearGradient::new(start, end, stops, SpreadMode::Pad, Transform::identity()).unwrap();
            }
            GradientDirection::Radial => {
                paint.shader = RadialGradient::new(
                    Point::from_xy(width / 2.0, height / 2.0),
                    0.0,
                    Point::from_xy(width / 2.0, height / 2.0),
                    f32::max(width, height),
                    stops,
                    SpreadMode::Pad,
                    Transform::identity(),
                )
                .unwrap();
            }
            _ => {}
        }
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }

    if stroke_width > 0.0 {
        let mut stroke = Stroke::default();
        stroke.width = stroke_width;
        if dash.len() > 1 {
            let dash_offset = if dash.len() > 2 { dash[2] } else { 0.0 };
            stroke.dash = StrokeDash::new(vec![dash[0], dash[1]], dash_offset);
        }
        pixmap.stroke_path(&path, &color_to_paint(stroke_color), &stroke, Transform::identity(), None);
    }

    Image::from_rgba8_premultiplied(pixel_buffer)
}

pub fn circle(
    radius: f32,
    stroke_width: f32,
    stroke_color: SlintColor,
    fill: SlintColor,
    dash: slint::ModelRc<f32>,
) -> Image {
    use slint::Model; // enable iter() for ModelRc

    let r = radius as u32;
    let size = r * 2;

    let mut pixel_buffer = SharedPixelBuffer::<Rgba8Pixel>::new(size, size);
    let mut pixmap = PixmapMut::from_bytes(pixel_buffer.make_mut_bytes(), size, size).unwrap();
    pixmap.fill(Color::TRANSPARENT);

    let path = {
        let mut pb = PathBuilder::new();
        if stroke_width > 0.0 {
            let z = stroke_width * 0.5; // little shift for the border
            pb.push_circle(radius, radius, radius - z);
        } else {
            pb.push_circle(radius, radius, radius);
        }
        pb.close();
        pb.finish().unwrap()
    };

    if stroke_width > 0.0 {
        let mut stroke = tiny_skia::Stroke { width: stroke_width, ..Default::default() };
        let dash: Vec<f32> = dash.iter().collect();
        if dash.len() > 1 {
            let dash_offset = if dash.len() > 2 { dash[2] } else { 0.0 };
            stroke.dash = StrokeDash::new(vec![dash[0], dash[1]], dash_offset);
        }
        pixmap.stroke_path(&path, &color_to_paint(stroke_color), &stroke, Transform::identity(), None);
    }

    if fill.alpha() != 0 {
        let mut paint = Paint::default();
        paint.anti_alias = false;
        paint.set_color_rgba8(fill.red(), fill.green(), fill.blue(), fill.alpha());
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }

    Image::from_rgba8_premultiplied(pixel_buffer)
}
