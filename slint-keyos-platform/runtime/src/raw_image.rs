// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{cell::RefCell, collections::HashMap};

use slint::private_unstable_api::re_exports::StaticTextures;
use slint::{private_unstable_api::re_exports::ImageInner, Image, SharedString};
use slint_keyos_platform_common::{ArchivedIconSet, IconSet, RawImage};

pub fn load_raw_image<P>(
    fs: &fs::FileSystem<P>,
    cache: &RefCell<HashMap<String, (&'static StaticTextures, Option<[u16; 4]>)>>,
    image_name: SharedString,
    nine_slice: bool,
    is_dark: bool,
) -> Image
where
    P: server::CheckedPermissions,
    P: server::MessageAllowed<fs::messages::OpenFileMessage>,
    P: server::MessageAllowed<fs::messages::CloseFile>,
    P: server::MessageAllowed<fs::messages::ReadFile>,
    P: fs::MapFilePermissions,
    P: server::MessageAllowed<fs::messages::GetMetadata>,
{
    // Try dark variant first if in dark mode
    let mut actual_image_name = image_name.to_string();
    if is_dark {
        let dark_variant = format!("{}-dark", actual_image_name);
        let dark_path = format!("{dark_variant}.raw");

        // Check if dark variant exists
        if raw_image_location(fs, &dark_path).is_some() {
            log::debug!("Using dark variant: {dark_path}");
            actual_image_name = dark_variant;
        }
    }

    let path = format!("{actual_image_name}.raw");
    let Some(location) = raw_image_location(fs, &path) else {
        log::warn!("Could not load image {actual_image_name:?}");
        return Image::from(ImageInner::None);
    };
    let cache_key = format!("{location:?}:{path}");
    let (texture, nine_slice_edges) = match cache.borrow_mut().entry(cache_key.into()) {
        std::collections::hash_map::Entry::Occupied(entry) => {
            log::debug!("load_raw_image cache hit on {location:?}:{path}");
            entry.get().clone()
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            let Some(archived_image) = load_archive_from_location::<RawImage, _>(fs, location, &path) else {
                log::warn!("Could not load image {actual_image_name:?}");
                return Image::from(ImageInner::None);
            };
            let texture = archived_image.into();
            let nine_slice_edges = archived_image.nine_slice.as_ref().map(|edges| edges.map(From::from));
            entry.insert((texture, nine_slice_edges));
            (texture, nine_slice_edges)
        }
    };
    let mut image = Image::from(ImageInner::StaticTextures(texture));
    if nine_slice {
        if let Some(nine_slice_edges) = nine_slice_edges {
            image.set_nine_slice_edges(
                nine_slice_edges[0],
                nine_slice_edges[1],
                nine_slice_edges[2],
                nine_slice_edges[3],
            );
        } else {
            log::warn!("No nine slice info found for {actual_image_name}");
        }
    }
    image
}

fn raw_image_location<P>(fs: &fs::FileSystem<P>, path: &str) -> Option<fs::Location>
where
    P: server::CheckedPermissions,
    P: server::MessageAllowed<fs::messages::GetMetadata>,
{
    [fs::Location::AppResources, fs::Location::CommonAssets]
        .into_iter()
        .find(|location| fs.metadata(path, *location).is_ok())
}

#[derive(Default)]
pub struct IconCache {
    icon_set: Option<&'static ArchivedIconSet>,
    cache: HashMap<(usize, String), &'static StaticTextures>,
}

pub fn load_icon<P>(
    fs: &fs::FileSystem<P>,
    cache: &RefCell<IconCache>,
    name: SharedString,
    requested_size: f32,
) -> Image
where
    P: server::CheckedPermissions,
    P: fs::MapFilePermissions,
{
    if cache.borrow().icon_set.is_none() {
        let Some(icon_set) = map_archive::<IconSet, _>(fs, "icon_set.bin") else {
            return Image::from(ImageInner::None);
        };
        cache.borrow_mut().icon_set = Some(icon_set);
    };
    let icon_set = cache.borrow().icon_set.unwrap();
    let Some(icons) = icon_set.0.get(name.as_str()) else {
        log::warn!("Could not load icon {name:?}");
        return Image::from(ImageInner::None);
    };
    let requested_size = requested_size.round() as u32;
    let icon = icons
        .iter()
        .find(|icon| icon.size.width.to_native() >= requested_size)
        .unwrap_or(icons.last().unwrap());
    let chosen_size = icon.size.width.to_native() as usize;
    if chosen_size == 0 {
        log::debug!("Icon {name} has no valid size, returning an empty image");
        return Image::from(ImageInner::None);
    }

    if let Some(texture) = cache.borrow().cache.get(&(chosen_size, name.to_string())) {
        log::debug!("load_icon cache hit on {name}@{chosen_size}");
        return Image::from(ImageInner::StaticTextures(texture));
    }

    let texture = icon.into();
    cache.borrow_mut().cache.insert((chosen_size, name.to_string()), texture);
    Image::from(ImageInner::StaticTextures(texture))
}

/// Decode rkyv-archived raw image bytes (an app's `icon.bin`) into an owned,
/// refcounted `Image`, or an empty image when the bytes are empty or invalid.
pub fn raw_image_from_bytes(bytes: &[u8]) -> Image {
    if bytes.is_empty() {
        return Image::from(ImageInner::None);
    }

    let mut aligned_bytes: rkyv::util::AlignedVec = rkyv::util::AlignedVec::with_capacity(bytes.len());
    aligned_bytes.extend_from_slice(bytes);
    let Some(archived_image) = rkyv::access::<
        slint_keyos_platform_common::ArchivedRawImage,
        rkyv::rancor::Error,
    >(aligned_bytes.as_slice())
    .ok() else {
        log::warn!("Could not parse raw image bytes");
        return Image::from(ImageInner::None);
    };

    decode_raw_image_to_owned(archived_image).unwrap_or_else(|| {
        log::warn!("Could not decode raw image bytes");
        Image::from(ImageInner::None)
    })
}

const MAX_IMAGE_CANVAS_DIM: usize = 2048;

/// Decode an archived raw image into an owned, refcounted `Image`.
///
/// Unlike the `StaticTextures` path (which borrows `&'static` data and therefore
/// must leak), this copies the texture's pixels into a `SharedPixelBuffer` that
/// frees with the last `Image` clone.
fn decode_raw_image_to_owned(archived: &slint_keyos_platform_common::ArchivedRawImage) -> Option<Image> {
    use slint_keyos_platform_common::ArchivedPixelFormat;

    let total_w = archived.size.width.to_native() as usize;
    let total_h = archived.size.height.to_native() as usize;
    let rect_x = archived.texture_rect.x.to_native() as usize;
    let rect_y = archived.texture_rect.y.to_native() as usize;
    let rect_w = archived.texture_rect.width.to_native() as usize;
    let rect_h = archived.texture_rect.height.to_native() as usize;
    let bytes = archived.bytes.as_slice();

    if rect_w == 0 || rect_h == 0 {
        return None;
    }

    let bpp = match &archived.pixel_format {
        ArchivedPixelFormat::Rgb => 3usize,
        ArchivedPixelFormat::Rgba | ArchivedPixelFormat::RgbaPremultiplied => 4,
        // One alpha byte per pixel; the single tint color lives in color_argb.
        ArchivedPixelFormat::AlphaMap => 1,
    };

    if total_w > MAX_IMAGE_CANVAS_DIM || total_h > MAX_IMAGE_CANVAS_DIM {
        log::warn!("raw image canvas {total_w}x{total_h} exceeds the maximum");
        return None;
    }

    // generate_texture stores only the cropped texture_rect, tightly packed; blit
    // it back onto the full total_size canvas so aspect and padding survive scaling.
    let needed = rect_w.checked_mul(rect_h)?.checked_mul(bpp)?;
    if needed > bytes.len() || rect_x.checked_add(rect_w)? > total_w || rect_y.checked_add(rect_h)? > total_h
    {
        log::warn!("raw image bytes too small for {rect_w}x{rect_h} texture rect");
        return None;
    }

    let width = total_w as u32;
    let height = total_h as u32;
    let blit = |dst: &mut [u8], px_bytes: usize| {
        for ry in 0..rect_h {
            let dst_off = ((rect_y + ry) * total_w + rect_x) * px_bytes;
            let src_off = ry * rect_w * px_bytes;
            dst[dst_off..dst_off + rect_w * px_bytes]
                .copy_from_slice(&bytes[src_off..src_off + rect_w * px_bytes]);
        }
    };

    match &archived.pixel_format {
        // Slint encodes an RGBA image that's an opaque, multi-color rect surrounded
        // by transparent pixels as a cropped RGB image. RGB can't carry that
        // transparent margin, so widen it back to RGBA and leave the area outside
        // the rect transparent.
        ArchivedPixelFormat::Rgb if rect_w != total_w || rect_h != total_h => {
            let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
            let dst = buffer.make_mut_slice();
            for ry in 0..rect_h {
                let row = &bytes[ry * rect_w * 3..(ry + 1) * rect_w * 3];
                for rx in 0..rect_w {
                    let px = &row[rx * 3..rx * 3 + 3];
                    dst[(rect_y + ry) * total_w + rect_x + rx] =
                        slint::Rgba8Pixel { r: px[0], g: px[1], b: px[2], a: 255 };
                }
            }
            Some(Image::from_rgba8(buffer))
        }
        ArchivedPixelFormat::Rgb => {
            let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
            blit(buffer.make_mut_bytes(), 3);
            Some(Image::from_rgb8(buffer))
        }
        ArchivedPixelFormat::Rgba => {
            let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
            blit(buffer.make_mut_bytes(), 4);
            Some(Image::from_rgba8(buffer))
        }
        ArchivedPixelFormat::RgbaPremultiplied => {
            let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
            blit(buffer.make_mut_bytes(), 4);
            Some(Image::from_rgba8_premultiplied(buffer))
        }
        ArchivedPixelFormat::AlphaMap => {
            let tint = slint::Rgb8Pixel::from(&archived.color_argb);
            let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
            let dst = buffer.make_mut_slice();
            for ry in 0..rect_h {
                let src = &bytes[ry * rect_w..(ry + 1) * rect_w];
                for rx in 0..rect_w {
                    dst[(rect_y + ry) * total_w + rect_x + rx] =
                        slint::Rgba8Pixel { r: tint.r, g: tint.g, b: tint.b, a: src[rx] };
                }
            }
            Some(Image::from_rgba8(buffer))
        }
    }
}

fn load_archive_from_location<T, P>(
    fs: &fs::FileSystem<P>,
    location: fs::Location,
    path: &str,
) -> Option<&'static T::Archived>
where
    T: rkyv::Archive,
    T::Archived: for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
    P: server::CheckedPermissions,
    P: server::MessageAllowed<fs::messages::OpenFileMessage>,
    P: server::MessageAllowed<fs::messages::CloseFile>,
    P: server::MessageAllowed<fs::messages::ReadFile>,
    P: fs::MapFilePermissions,
{
    if location == fs::Location::AppResources {
        read_archive_from_location::<T, P>(fs, location, path)
    } else {
        map_archive_from_location::<T, P>(fs, location, path)
    }
}

fn read_archive_from_location<T, P>(
    fs: &fs::FileSystem<P>,
    location: fs::Location,
    path: &str,
) -> Option<&'static T::Archived>
where
    T: rkyv::Archive,
    T::Archived: for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
    P: server::CheckedPermissions,
    P: server::MessageAllowed<fs::messages::OpenFileMessage>,
    P: server::MessageAllowed<fs::messages::CloseFile>,
    P: server::MessageAllowed<fs::messages::ReadFile>,
{
    log::debug!("Reading file {path}");
    let mut file = match fs.open_file(path, location, fs::OpenFlags::READ_ONLY) {
        Ok(file) => file,
        Err(e) => {
            log::warn!("Error opening file at {location:?}:\"{path}\": {e:?}");
            return None;
        }
    };
    let mut bytes: rkyv::util::AlignedVec = rkyv::util::AlignedVec::new();
    if let Err(e) = bytes.extend_from_reader(&mut file) {
        log::warn!("Error reading file at {location:?}:\"{path}\": {e:?}");
        return None;
    }
    let bytes = Box::leak(Box::new(bytes));
    let archived = rkyv::access::<T::Archived, rkyv::rancor::Error>(bytes.as_slice()).ok()?;
    Some(archived)
}

fn map_archive<T, P>(fs: &fs::FileSystem<P>, path: &str) -> Option<&'static T::Archived>
where
    T: rkyv::Archive,
    T::Archived: for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
    P: server::CheckedPermissions,
    P: fs::MapFilePermissions,
{
    map_archive_from_location::<T, P>(fs, fs::Location::CommonAssets, path)
}

fn map_archive_from_location<T, P>(
    fs: &fs::FileSystem<P>,
    location: fs::Location,
    path: &str,
) -> Option<&'static T::Archived>
where
    T: rkyv::Archive,
    T::Archived: for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
    P: server::CheckedPermissions,
    P: fs::MapFilePermissions,
{
    log::debug!("Mapping file {path}");
    let mapping = match fs.map_file(location, path) {
        Ok(mapping) => mapping,
        Err(e) => {
            log::warn!("Error loading file at {location:?}:\"{path}\": {e:?}");
            return None;
        }
    };
    // Transmuting to static because we know we are not dropping this memory.
    let mapping = unsafe { core::mem::transmute::<&[u8], &'static [u8]>(mapping.as_slice()) };
    let archived = rkyv::access::<T::Archived, rkyv::rancor::Error>(mapping).ok()?;
    Some(archived)
}
