// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use tiny_skia::{
    ColorU8, FillRule, GradientStop, LinearGradient, Paint, PixmapMut, Point, Rect, Shader, Stroke, Transform,
};

use super::{drawing::draw_icon, font::draw_text, keys::KeyDef};
use crate::{
    colors::{to_color, KeyStyle},
    drawing::{drop_shadow, round_rect_path},
    keyboard::{KEY_BORDER_RADIUS, KEY_DEFAULT_WIDTH, KEY_HEIGHT},
};

const KEY_PRESS_SHADOW_OPACITY: u16 = 128;
const KEY_PRESS_SHADOW_RADIUS: u32 = 8;

#[derive(Debug, PartialEq)]
pub struct KeySlot {
    pub key: &'static KeyDef, // If no key is given, then this slot is just a spacer
    pub width: f32,
    pub height: f32,
}

impl KeySlot {
    pub const fn new(key: &'static KeyDef) -> KeySlot { KeySlot::width(key, KEY_DEFAULT_WIDTH) }

    pub const fn width(key: &'static KeyDef, width: f32) -> KeySlot {
        Self { key, width, height: KEY_HEIGHT }
    }

    fn draw_label(&self, pixmap: &mut PixmapMut, rect: &Rect, color: ColorU8, label_override: Option<&str>) {
        let shift_y = -2_f32;
        let rect = Rect::from_xywh(rect.x(), rect.y() + shift_y, rect.width(), rect.height()).unwrap();

        // render label or icon
        match (label_override, self.key.icon) {
            (None, Some(icon)) => draw_icon(
                icon,
                pixmap,
                rect.x() as usize,
                2 + rect.y() as usize,
                rect.width() as usize,
                self.height as usize,
                color,
            ),
            (Some(label), _) => draw_text(label, self.key.font_scale, pixmap, &rect, color),
            (None, None) => draw_text(self.key.label, self.key.font_scale, pixmap, &rect, color),
        }
    }

    pub fn draw(
        &self,
        x: f32,
        y: f32,
        pixmap: &mut PixmapMut,
        pressed: bool,
        label_override: Option<&str>,
        style_override: Option<KeyStyle>,
    ) {
        let rect = Rect::from_xywh(x, y, self.width, self.height).unwrap(); // get button coordinates
        let style = style_override.unwrap_or(self.key.style);
        let colors = style.colors();
        let path = round_rect_path(&rect, KEY_BORDER_RADIUS);

        pixmap.fill_path(
            &path,
            &Paint {
                shader: tiny_skia::Shader::SolidColor(to_color(colors.background)),

                ..Default::default()
            },
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        if pressed {
            drop_shadow(
                pixmap,
                KEY_PRESS_SHADOW_RADIUS,
                KEY_PRESS_SHADOW_OPACITY,
                0,
                (KEY_PRESS_SHADOW_RADIUS / 2) as i32,
                true,
            );
        }

        let shader = if pressed {
            Shader::SolidColor(to_color(colors.gradient_bottom))
        } else {
            LinearGradient::new(
                Point::from_xy(x, y),
                Point::from_xy(x, y + self.height),
                vec![
                    GradientStop::new(0.0, to_color(colors.gradient_top)),
                    GradientStop::new(1.0, to_color(colors.gradient_bottom)),
                ],
                tiny_skia::SpreadMode::Pad,
                Transform::identity(),
            )
            .unwrap()
        };
        pixmap.stroke_path(
            &path,
            &Paint { shader, ..Default::default() },
            &Stroke { width: 1.0, ..Default::default() },
            Transform::identity(),
            None,
        );

        let label_rect =
            if pressed { rect.transform(Transform::from_translate(0.0, 4.0)).unwrap() } else { rect };
        self.draw_label(pixmap, &label_rect, colors.text, label_override);
    }
}
