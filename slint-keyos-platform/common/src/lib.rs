// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;

pub mod analyze_path;
pub mod utils;

#[derive(rkyv::Serialize, rkyv::Archive)]
pub struct ImageSize {
    pub width: u32,
    pub height: u32,
}

#[derive(rkyv::Serialize, rkyv::Archive)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(rkyv::Serialize, rkyv::Archive)]
#[repr(u8)]
pub enum PixelFormat {
    Rgb,
    Rgba,
    RgbaPremultiplied,
    AlphaMap,
}

#[derive(rkyv::Serialize, rkyv::Archive)]
pub struct Color {
    a: u8,
    r: u8,
    g: u8,
    b: u8,
}

#[derive(rkyv::Serialize, rkyv::Archive)]
pub struct RawImage {
    pub size: ImageSize,
    pub original_size: ImageSize,
    pub texture_rect: Rect,
    pub color_argb: Color,
    pub pixel_format: PixelFormat,
    pub nine_slice: Option<[u16; 4]>,
    pub bytes: Vec<u8>,
}

#[derive(Default, rkyv::Serialize, rkyv::Archive)]
pub struct IconSet(pub BTreeMap<String, Vec<RawImage>>);

#[cfg(feature = "i-slint-compiler")]
impl From<i_slint_compiler::embedded_resources::Texture> for RawImage {
    fn from(value: i_slint_compiler::embedded_resources::Texture) -> Self {
        Self {
            size: value.total_size.into(),
            original_size: value.original_size.into(),
            texture_rect: value.rect.into(),
            color_argb: value.format.into(),
            pixel_format: value.format.into(),
            nine_slice: None,
            bytes: value.data,
        }
    }
}

#[cfg(feature = "i-slint-compiler")]
impl From<i_slint_compiler::embedded_resources::Size> for ImageSize {
    fn from(value: i_slint_compiler::embedded_resources::Size) -> Self {
        Self { width: value.width, height: value.height }
    }
}

#[cfg(feature = "i-slint-compiler")]
impl From<i_slint_compiler::embedded_resources::Rect> for Rect {
    fn from(value: i_slint_compiler::embedded_resources::Rect) -> Self {
        Self { x: value.x(), y: value.y(), width: value.width(), height: value.height() }
    }
}

#[cfg(feature = "i-slint-compiler")]
impl From<i_slint_compiler::embedded_resources::PixelFormat> for PixelFormat {
    fn from(value: i_slint_compiler::embedded_resources::PixelFormat) -> Self {
        match value {
            i_slint_compiler::embedded_resources::PixelFormat::Rgb => Self::Rgb,
            i_slint_compiler::embedded_resources::PixelFormat::Rgba => Self::Rgba,
            i_slint_compiler::embedded_resources::PixelFormat::RgbaPremultiplied => Self::RgbaPremultiplied,
            i_slint_compiler::embedded_resources::PixelFormat::AlphaMap(_) => Self::AlphaMap,
        }
    }
}

#[cfg(feature = "i-slint-compiler")]
impl From<i_slint_compiler::embedded_resources::PixelFormat> for Color {
    fn from(value: i_slint_compiler::embedded_resources::PixelFormat) -> Self {
        if let i_slint_compiler::embedded_resources::PixelFormat::AlphaMap(c) = value {
            Self { a: 255, r: c[0], g: c[1], b: c[2] }
        } else {
            Self { a: 0, r: 0, g: 0, b: 0 }
        }
    }
}

#[cfg(feature = "slint")]
impl From<&'static ArchivedRawImage> for &'static slint::private_unstable_api::re_exports::StaticTextures {
    fn from(value: &'static ArchivedRawImage) -> Self {
        let texture = slint::private_unstable_api::re_exports::StaticTexture {
            rect: (&value.texture_rect).into(),
            format: (&value.pixel_format).into(),
            color: (&value.color_argb).into(),
            index: 0,
        };
        Box::leak(Box::new(slint::private_unstable_api::re_exports::StaticTextures {
            size: (&value.size).into(),
            original_size: (&value.original_size).into(),
            data: slint::private_unstable_api::re_exports::Slice::from_slice(value.bytes.as_slice()),
            textures: slint::private_unstable_api::re_exports::Slice::from_slice(vec![texture].leak()),
        }))
    }
}

#[cfg(feature = "slint")]
impl From<&ArchivedRect> for slint::private_unstable_api::re_exports::IntRect {
    fn from(value: &ArchivedRect) -> Self {
        slint::private_unstable_api::re_exports::euclid::rect(
            value.x.into(),
            value.y.into(),
            value.width.to_native() as i32,
            value.height.to_native() as i32,
        )
    }
}

#[cfg(feature = "slint")]
impl From<&ArchivedImageSize> for slint::private_unstable_api::re_exports::IntSize {
    fn from(value: &ArchivedImageSize) -> Self {
        slint::private_unstable_api::re_exports::IntSize::new(value.width.into(), value.height.into())
    }
}

#[cfg(feature = "slint")]
impl From<&ArchivedPixelFormat> for slint::private_unstable_api::re_exports::TexturePixelFormat {
    fn from(value: &ArchivedPixelFormat) -> Self {
        match value {
            ArchivedPixelFormat::Rgb => Self::Rgb,
            ArchivedPixelFormat::Rgba => Self::Rgba,
            ArchivedPixelFormat::RgbaPremultiplied => Self::RgbaPremultiplied,
            ArchivedPixelFormat::AlphaMap => Self::AlphaMap,
        }
    }
}

#[cfg(feature = "slint")]
impl From<&ArchivedColor> for slint::private_unstable_api::re_exports::Color {
    fn from(value: &ArchivedColor) -> Self { Self::from_argb_u8(value.a, value.r, value.g, value.b) }
}

#[cfg(feature = "slint")]
impl From<&ArchivedColor> for slint::Rgb8Pixel {
    fn from(value: &ArchivedColor) -> Self { Self { r: value.r, g: value.g, b: value.b } }
}
