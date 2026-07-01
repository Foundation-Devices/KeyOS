// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::usize;

use slint::{
    private_unstable_api::re_exports::{ImageInner, SharedImageBuffer},
    Image, Rgba8Pixel, SharedPixelBuffer,
};
use tiny_skia::{
    BlendMode, FillRule, FilterQuality, Mask, PathBuilder, PixmapMut, PixmapPaint, PixmapRef, Transform,
};

use super::scale::scale_buffer;

fn create_mask(border_radius: f32, width: f32, height: f32) -> Mask {
    let mut mask = Mask::new(width as u32, height as u32).unwrap();

    let clip_path = {
        let z = 0.0; //let a = 0.552 * border_radius;
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
    };

    mask.fill_path(&clip_path, FillRule::Winding, true, Transform::default());

    mask
}

trait BufData {
    fn data(&self) -> &[u8];
}

impl BufData for SharedImageBuffer {
    #[inline]
    fn data(&self) -> &[u8] {
        match self {
            Self::RGB8(buffer) => buffer.as_bytes(),
            Self::RGBA8(buffer) => buffer.as_bytes(),
            Self::RGBA8Premultiplied(buffer) => buffer.as_bytes(),
        }
    }
}

pub fn round_corners(
    source_image: Image,
    border_radius: f32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Image {
    let w = width as u32;
    let h = height as u32;

    if w == 0 || h == 0 {
        return Image::default();
    }

    let (ow, oh) = source_image.size().to_tuple();

    let buffer = unsafe {
        let inner = std::mem::transmute::<Image, ImageInner>(source_image); //  pretend that source_image is our ImageInner
        let buf = inner.render_to_buffer(None); //  get pixels from this image
        let _ = std::mem::transmute::<ImageInner, Image>(inner); //  roll back, so it can be deallocated properly
        buf
    };

    if buffer.is_none() {
        // no input image provided
        println!("buffer.is_none()");
        Image::default() // return empty image
    } else {
        // println!("rendering round corners ...");
        let buffer = buffer.unwrap();
        let src = buffer.data();
        let x = x as usize;
        let y = y as usize;
        let w = w as usize;
        let h = h as usize;
        let row_size = w * 4;
        let ow = ow as usize;
        let oh = oh as usize;
        let osize = ow * oh * 4usize - row_size; //  last index for source row
        let rsize = w * h * 4usize - row_size; //  last index for destination row
        let mut dst = vec![0u8; w * h * 4];
        let dst = dst.as_mut_slice();
        let mut oshift = (x + y * ow) * 4usize;
        let mut rshift = 0usize;
        // println!("dst.len(): {}", dst.len());
        // println!("x:y = {x}:{y}");
        // println!("w:h = {w}:{h}");
        // println!("ow:oh = {ow}:{oh}");
        // println!("osize:{osize}; rsize:{rsize}; rowsize: {row_size};");
        while oshift < osize && rshift < rsize {
            // println!("oshift:{oshift}; rshift: {rshift}");
            // println!("llen:{}; rlen: {}", &dst[rshift..(rshift + row_size)].len(), &src[oshift..(oshift +
            // row_size)].len());
            dst[rshift..(rshift + row_size)].copy_from_slice(&src[oshift..(oshift + row_size)]);
            oshift += ow * 4usize;
            rshift += row_size;
        }
        // original buffer size is (ox, oy), we want to cut area from buffer starting from (x,y) size (w,h)
        // pixel size is 4 bytes ( r, g, b, a )
        // 1) skip y rows ( y * w * 4 bytes )
        // 2) for rows from y to y + h we copy w pixels (w * 4 bytes) from row position x * 4 to our result
        //    pixmap
        // 3) apply mask for cutting off round corners
        // let bytes = bytes
        //     .chunks_exact(4 * w as usize)  // split into rows
        //     .skip(y as usize)  // skip first y rows
        //     .map(|row| row[x..])
        //     ;

        let w = w as u32;
        let h = h as u32;
        let src_pixmap = PixmapRef::from_bytes(dst, w, h).unwrap();
        let mut pixel_buffer = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
        let mut pixmap = PixmapMut::from_bytes(pixel_buffer.make_mut_bytes(), w, h).unwrap();

        //println!("orig: {:#?}", orig_size);

        // draw source image to the destination
        let mut pmp = PixmapPaint::default();
        pmp.opacity = 1.0;
        pmp.blend_mode = BlendMode::Source;
        pmp.quality = FilterQuality::Nearest;

        let mask = create_mask(border_radius, width, height);
        pixmap.draw_pixmap(0, 0, src_pixmap, &pmp, Transform::identity(), Some(&mask));

        Image::from_rgba8(pixel_buffer)
    }
}

pub fn round_corners_scaling(source_image: Image, border_radius: f32, width: f32, height: f32) -> Image {
    let w = width as u32;
    let h = height as u32;

    if w == 0 || h == 0 {
        return Image::default();
    }

    let Some(source) = source_image.to_rgba8_premultiplied() else {
        return Image::default();
    };

    // Mask after scaling: the corner radius is in output pixels, so masking the
    // source first would scale the radius along with the image and distort it.
    let mut scaled = scale_buffer(&source, width, height, false);
    let mut pixmap = PixmapMut::from_bytes(scaled.make_mut_bytes(), w, h).unwrap();
    let mask = create_mask(border_radius, w as f32, h as f32);
    pixmap.apply_mask(&mask);

    Image::from_rgba8_premultiplied(scaled)
}
