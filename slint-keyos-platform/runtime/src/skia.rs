// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub(crate) mod arc;
pub(crate) mod card;
pub(crate) mod circular_progress;
pub(crate) mod corners;
pub(crate) mod graph;
pub(crate) mod loader;
pub(crate) mod panel;
pub(crate) mod picker;
pub(crate) mod scale;

pub use arc::doughnut;
pub use card::line_card;
pub use circular_progress::circular_progress;
pub use corners::round_corners;
pub use corners::round_corners_scaling;
pub use graph::{draw_graph, graph_points, GraphPoint, PricePoint};
pub use loader::loader;
pub use panel::circle;
pub use panel::frame;
pub use panel::GradientDirection;
pub use panel::SlintGradientStop;
pub use picker::color_palette;
pub use picker::hue_slider;
pub use picker::pick_color;
pub use scale::scale_image;
use slint::Color as SlintColor;
use tiny_skia::{Color, Paint};

pub(crate) fn color_to_paint(color: SlintColor) -> Paint<'static> { color_to_paint_dist(color, 100.0) }

pub(crate) fn color_to_color(color: SlintColor) -> Color {
    let c = color.to_argb_u8();
    Color::from_rgba8(c.red, c.green, c.blue, c.alpha)
}

pub(crate) fn color_to_paint_dist(color: SlintColor, dist: f32) -> Paint<'static> {
    let c = color.to_argb_u8();
    let a = if dist < 10.0 { (c.alpha as f32 * dist / 10.0) as u8 } else { c.alpha };

    let mut paint = Paint::default();
    paint.set_color_rgba8(c.red, c.green, c.blue, a);
    paint.anti_alias = true;
    paint
}
