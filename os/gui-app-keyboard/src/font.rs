// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{collections::HashMap, sync::Mutex};

use ab_glyph::{Font, FontRef, GlyphId, ScaleFont};
use tiny_skia::{ColorU8, PixmapMut, Point, Rect};

use crate::drawing::draw_colorized_buffer;

fs::use_api!();

static TEXT_RENDERER: Mutex<Option<TextRenderer>> = Mutex::new(None);

#[derive(Default)]
struct RenderedGlyph {
    data: Vec<u8>,
    width: usize,
    offset_x: f32,
    offset_y: f32,
}

struct PositonedGlyph {
    x: f32,
    id: GlyphId,
}

struct TextRenderer {
    font: FontRef<'static>,
    glyph_cache: HashMap<(u16, GlyphId), RenderedGlyph>,
}

impl Default for TextRenderer {
    fn default() -> Self {
        #[cfg(keyos)]
        let font_mem_range = FileSystem::default()
            .map_file(fs::Location::CommonAssets, "fonts/Montserrat-Light.ttf")
            .expect("Could not map font file");
        #[cfg(keyos)]
        let font_data =
            unsafe { core::mem::transmute::<&[u8], &'static [u8]>(font_mem_range.as_slice::<u8>()) };

        #[cfg(not(keyos))]
        let font_data =
            ["../../resources/fonts/Montserrat-Light.ttf", "../../ui/ui/fonts/Montserrat-Light.ttf"]
                .into_iter()
                .find_map(|path| std::fs::read(path).ok())
                .unwrap_or_else(|| {
                    panic!(
                "Could not find Montserrat-Light.ttf. Tried ../../resources/fonts and ../../ui/ui/fonts"
            )
                })
                .leak();

        let font = FontRef::try_from_slice(font_data).expect("Could not parse font");

        Self { font, glyph_cache: Default::default() }
    }
}

impl TextRenderer {
    fn shape_text(&self, text: &str, scale: f32) -> (f32, Vec<PositonedGlyph>) {
        let scaled_font = self.font.as_scaled(scale);
        let mut cursor = 0.0;
        let mut last = None;
        let mut result = Vec::new();
        for c in text.chars() {
            let id = scaled_font.glyph_id(c);
            if let Some(last) = last {
                cursor += scaled_font.kern(last, id)
            }
            result.push(PositonedGlyph { x: cursor, id });
            cursor += scaled_font.h_advance(id);
            last = Some(id)
        }
        (cursor, result)
    }

    fn render_glyph(font: &FontRef<'static>, id: GlyphId, scale: f32) -> RenderedGlyph {
        let Some(outline) =
            font.outline_glyph(ab_glyph::Glyph { id, scale: scale.into(), position: Default::default() })
        else {
            return RenderedGlyph::default();
        };
        let bounds = outline.px_bounds();
        let width = bounds.width() as usize;
        let height = bounds.height() as usize;
        let mut data = vec![0u8; width * height];
        outline.draw(|x, y, opacity| data[x as usize + y as usize * width] = (opacity * 255.0) as u8);

        RenderedGlyph { data, width, offset_x: bounds.min.x, offset_y: bounds.min.y }
    }

    fn rendered_glyph(&mut self, id: GlyphId, scale: f32) -> &RenderedGlyph {
        let scale_key = scale as u16;
        self.glyph_cache.entry((scale_key, id)).or_insert_with(|| Self::render_glyph(&self.font, id, scale))
    }

    pub fn render_str(&mut self, text: &str, scale: f32, dst: &mut PixmapMut, rect: &Rect, color: ColorU8) {
        let (width, glyphs) = self.shape_text(text, scale);
        if glyphs.is_empty() {
            return;
        };

        let scaled_font = self.font.as_scaled(scale);
        let dx = (rect.width() - width) / 2.0;
        let dy = (rect.height() + scaled_font.ascent() * 0.8) / 2.0;
        let pos = Point::from_xy(rect.x() + dx, rect.y() + dy); // center of the box

        for glyph in glyphs {
            let rendered_glyph = self.rendered_glyph(glyph.id, scale);
            if rendered_glyph.data.is_empty() {
                continue;
            }
            draw_colorized_buffer(
                &rendered_glyph.data,
                rendered_glyph.width,
                dst,
                0,
                0,
                (pos.x + glyph.x + rendered_glyph.offset_x).round() as usize,
                (pos.y + rendered_glyph.offset_y).round() as usize,
                rendered_glyph.width,
                rendered_glyph.data.len() / rendered_glyph.width,
                color,
            );
        }
    }
}

pub fn draw_text(text: &str, scale: f32, pixmap: &mut PixmapMut, rect: &Rect, color: ColorU8) {
    let mut text_renderer_lock = TEXT_RENDERER.lock().unwrap();
    if text_renderer_lock.is_none() {
        *text_renderer_lock = Some(TextRenderer::default());
    }
    text_renderer_lock.as_mut().unwrap().render_str(text, scale, pixmap, rect, color);
}
