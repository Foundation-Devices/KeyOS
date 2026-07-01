// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use tiny_skia::{BlendMode, FilterQuality, PixmapMut, PixmapPaint, PixmapRef, Transform};

/// Scale a premultiplied RGBA buffer to `width` by `height`. tiny-skia treats pixmap bytes
/// as premultiplied, so `source` must be premultiplied (e.g. from `Image::to_rgba8_premultiplied`)
/// or scaled antialiased edges blend against the wrong colors; the result is premultiplied too.
pub(crate) fn scale_buffer(
    source: &SharedPixelBuffer<Rgba8Pixel>,
    width: f32,
    height: f32,
    smooth: bool,
) -> SharedPixelBuffer<Rgba8Pixel> {
    let (ow, oh) = (source.width(), source.height());
    let src_pixmap = PixmapRef::from_bytes(source.as_bytes(), ow, oh).unwrap();

    let mut scaled = SharedPixelBuffer::<Rgba8Pixel>::new(width as u32, height as u32);
    let mut pixmap = PixmapMut::from_bytes(scaled.make_mut_bytes(), width as u32, height as u32).unwrap();

    let mut paint = PixmapPaint::default();
    paint.opacity = 1.0;
    paint.blend_mode = BlendMode::Source;
    paint.quality = if smooth { FilterQuality::Bilinear } else { FilterQuality::Nearest };

    let scale = Transform::from_scale(width / ow as f32, height / oh as f32);
    pixmap.draw_pixmap(0, 0, src_pixmap, &paint, scale, None);

    scaled
}

pub fn scale_image(source_image: Image, width: f32, height: f32, smooth: bool) -> Image {
    if width as u32 == 0 || height as u32 == 0 {
        return Image::default();
    }
    let Some(source) = source_image.to_rgba8_premultiplied() else {
        return Image::default();
    };
    Image::from_rgba8_premultiplied(scale_buffer(&source, width, height, smooth))
}
