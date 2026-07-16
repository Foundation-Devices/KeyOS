// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use tiny_skia::{ColorU8, IntRect, Path, PathBuilder, PixmapMut, PixmapRef, PremultipliedColorU8, Rect};

const ICON_SIZE: usize = 24;

pub type Icon = [u8; ICON_SIZE * ICON_SIZE];

pub fn round_rect_path(d: &Rect, radius: f32) -> Path {
    let mut pb = PathBuilder::new();

    // Approximating a quarter circle with cubic bezier
    // See https://pomax.github.io/bezierinfo/#circles_cubic
    let b = (1.0 - 0.551785) * radius;

    pb.move_to(d.x() + radius, d.y()); // top
    pb.line_to(d.x() + d.width() - radius, d.y());
    pb.cubic_to(
        // top-right
        d.x() + d.width() - b,
        d.y(),
        d.x() + d.width(),
        d.y() + b,
        d.x() + d.width(),
        d.y() + radius,
    );
    pb.line_to(d.x() + d.width(), d.y() + d.height() - radius); // right
    pb.cubic_to(
        //  bottom-right
        d.x() + d.width(),
        d.y() + d.height() - b,
        d.x() + d.width() - b,
        d.y() + d.height(),
        d.x() + d.width() - radius,
        d.y() + d.height(),
    );
    pb.line_to(d.x() + radius, d.y() + d.height()); // bottom
    pb.cubic_to(
        //  bottom-left
        d.x() + b,
        d.y() + d.height(),
        d.x(),
        d.y() + d.height() - b,
        d.x(),
        d.y() + d.height() - radius,
    );
    pb.line_to(d.x(), d.y() + radius); // lefft
    pb.cubic_to(
        //  top-left
        d.x(),
        d.y() + b,
        d.x() + b,
        d.y(),
        d.x() + radius,
        d.y(),
    );
    pb.close();
    pb.finish().unwrap()
}

pub fn draw_colorized_buffer(
    alpha_map: &[u8],
    alpha_map_w: usize,
    pixmap: &mut PixmapMut,
    src_x: usize,
    src_y: usize,
    dst_x: usize,
    dst_y: usize,
    width: usize,
    height: usize,
    color: ColorU8,
) {
    let dst_width = pixmap.width() as usize;
    let dst_buf = pixmap.pixels_mut();
    for y in 0..height {
        for x in 0..width {
            let dst_index = dst_x + x + (dst_y + y) * dst_width;
            let src_index = src_x + x + (src_y + y) * alpha_map_w;
            let dst = dst_buf[dst_index];
            let src_a = alpha_map[src_index] as u16;
            let blend = |dst, src| (((dst as u16) * (255 - src_a) + (src as u16) * src_a) / 255) as u8;
            let r = blend(dst.red(), color.red());
            let g = blend(dst.green(), color.green());
            let b = blend(dst.blue(), color.blue());
            let a = (dst.alpha() as u16 + src_a - (dst.alpha() as u16 * src_a as u16) / 255) as u8;
            dst_buf[dst_index] = PremultipliedColorU8::from_rgba(r, g, b, a).unwrap()
        }
    }
}

pub(crate) fn draw_icon(
    icon: &Icon,
    pixmap: &mut PixmapMut,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    color: ColorU8,
) {
    draw_colorized_buffer(
        icon,
        ICON_SIZE,
        pixmap,
        0,
        0,
        x + width / 2 - ICON_SIZE / 2,
        y + height / 2 - ICON_SIZE / 2,
        ICON_SIZE,
        ICON_SIZE,
        color,
    );
}

pub fn drop_shadow(
    pixmap: &mut PixmapMut,
    radius: u32,
    opacity: u16,
    offset_x: i32,
    offset_y: i32,
    inner: bool,
) {
    let height = pixmap.height();
    let width = pixmap.width();
    let shadow_width = width + radius * 2;
    let shadow_height = height + radius * 2;
    let mut alpha = vec![0u8; (shadow_width * shadow_height) as usize];

    for y in 0..height {
        for x in 0..width {
            alpha[((y + radius) * shadow_width + radius + x) as usize] =
                pixmap.pixels_mut()[(y * width + x) as usize].alpha();
        }
    }

    libblur::stack_blur(
        &mut alpha,
        shadow_width,
        shadow_width,
        shadow_height,
        radius,
        libblur::FastBlurChannels::Plane,
        libblur::ThreadingPolicy::Single,
    );

    for y in 0..height {
        for x in 0..width {
            let shadow_x = (x + radius).saturating_sub_signed(offset_x).min(shadow_width - 1);
            let shadow_y = (y + radius).saturating_sub_signed(offset_y).min(shadow_height - 1);
            let shadow = alpha[(shadow_y * shadow_width + shadow_x) as usize] as u16;
            let pixel = &mut pixmap.pixels_mut()[(y * width + x) as usize];
            if inner {
                let shadow = 255 - (255 - shadow).saturating_mul(opacity) / 255;
                *pixel = PremultipliedColorU8::from_rgba(
                    ((pixel.red() as u16 * shadow) / 255) as u8,
                    ((pixel.green() as u16 * shadow) / 255) as u8,
                    ((pixel.blue() as u16 * shadow) / 255) as u8,
                    pixel.alpha(),
                )
                .unwrap();
            } else {
                let shadow = shadow.saturating_mul(opacity) / 255;
                *pixel = PremultipliedColorU8::from_rgba(
                    pixel.red(),
                    pixel.green(),
                    pixel.blue(),
                    (shadow + pixel.alpha() as u16 - (shadow * pixel.alpha() as u16) / 255) as u8,
                )
                .unwrap();
            }
        }
    }
}

/// Copy a rectangular region of pixels from `source` into `target`. Both
/// pixmaps must have identical dimensions. Pure blit -- no alpha blending.
/// `region` is clamped to the source/target bounds; an empty intersection
/// is a no-op.
pub fn blit(target: &mut PixmapMut, source: PixmapRef, region: IntRect) {
    let bounds = IntRect::from_xywh(0, 0, source.width(), source.height()).unwrap();
    let Some(region) = region.intersect(&bounds) else { return };
    if region.width() == target.width() && region.height() == target.height() {
        target.data_mut()[..source.data().len()].copy_from_slice(source.data());
        return;
    }
    let width = target.width() as i32;
    let target_pixels = target.pixels_mut();
    let source_pixels = source.pixels();
    for row in region.top()..region.bottom() {
        let from = (row * width + region.left()) as usize;
        let to = (row * width + region.right()) as usize;
        target_pixels[from..to].copy_from_slice(&source_pixels[from..to]);
    }
}
