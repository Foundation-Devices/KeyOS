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

pub fn load_raw_image_file<P>(
    fs: &fs::FileSystem<P>,
    cache: &RefCell<HashMap<String, &'static StaticTextures>>,
    location: fs::Location,
    path: SharedString,
) -> Image
where
    P: server::CheckedPermissions,
    P: server::MessageAllowed<fs::messages::OpenFileMessage>,
    P: server::MessageAllowed<fs::messages::CloseFile>,
    P: server::MessageAllowed<fs::messages::ReadFile>,
    P: fs::MapFilePermissions,
{
    let path = path.to_string();
    let key = format!("{location:?}:{path}");
    match cache.borrow_mut().entry(key) {
        std::collections::hash_map::Entry::Occupied(entry) => {
            log::debug!("load_raw_image_file cache hit on {location:?}:{path}");
            Image::from(ImageInner::StaticTextures(*entry.get()))
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            let Some(archived_image) = load_archive_from_location::<RawImage, _>(fs, location, &path) else {
                log::warn!("Could not load image file {location:?}:{path}");
                return Image::from(ImageInner::None);
            };
            let texture = archived_image.into();
            entry.insert(texture);
            Image::from(ImageInner::StaticTextures(texture))
        }
    }
}

/// Bounded cache of decoded bundled app icons.
///
/// Icons are fetched on demand over IPC (one per app id), so the number of
/// distinct icons scales with the number of installed apps. Each decoded image
/// owns its pixels (refcounted), and the LRU bound caps how many stay resident:
/// evicting an entry drops the cache's reference, freeing the pixels once no
/// on-screen delegate still holds the image.
pub type BundledAppIconCache = RefCell<lru::LruCache<String, Image>>;

/// Maximum number of decoded bundled app icons kept resident at once.
pub const BUNDLED_APP_ICON_CACHE_CAP: usize = 32;

pub fn new_bundled_app_icon_cache() -> BundledAppIconCache {
    RefCell::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(BUNDLED_APP_ICON_CACHE_CAP).expect("cache cap is non-zero"),
    ))
}

/// Load a bundled app icon, fetching its bytes via `fetch_bytes()` only on a
/// cache miss. The decoded image owns its pixels so the LRU can free it on
/// eviction; a miss after eviction simply re-fetches and re-decodes.
pub fn load_raw_image_bytes<F>(cache: &BundledAppIconCache, cache_key: SharedString, fetch_bytes: F) -> Image
where
    F: FnOnce() -> Vec<u8>,
{
    let cached = cache.borrow_mut().get(cache_key.as_str()).cloned();
    if let Some(image) = cached {
        log::debug!("load_raw_image_bytes cache hit on {cache_key}");
        return image;
    }

    let bytes = fetch_bytes();
    if bytes.is_empty() {
        return Image::from(ImageInner::None);
    }

    let mut aligned_bytes: rkyv::util::AlignedVec = rkyv::util::AlignedVec::with_capacity(bytes.len());
    aligned_bytes.extend_from_slice(&bytes);
    let Some(archived_image) = rkyv::access::<
        slint_keyos_platform_common::ArchivedRawImage,
        rkyv::rancor::Error,
    >(aligned_bytes.as_slice())
    .ok() else {
        log::warn!("Could not load raw image bytes for {cache_key}");
        return Image::from(ImageInner::None);
    };

    let Some(image) = decode_raw_image_to_owned(archived_image) else {
        log::warn!("Could not decode raw image bytes for {cache_key}");
        return Image::from(ImageInner::None);
    };

    cache.borrow_mut().put(cache_key.to_string(), image.clone());
    image
}

/// Decode an archived raw image into an owned, refcounted `Image`.
///
/// Unlike the `StaticTextures` path (which borrows `&'static` data and therefore
/// must leak), this copies the texture's pixels into a `SharedPixelBuffer` that
/// frees with the last `Image` clone — required for an evicting cache.
fn decode_raw_image_to_owned(archived: &slint_keyos_platform_common::ArchivedRawImage) -> Option<Image> {
    use slint_keyos_platform_common::ArchivedPixelFormat;

    let total_width = archived.size.width.to_native() as usize;
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
        ArchivedPixelFormat::AlphaMap => {
            log::warn!("AlphaMap app icons are not supported by the owned-image path");
            return None;
        }
    };

    // The pixel data is laid out for `total_width`; the texture rect selects the
    // sub-region for this image. Validate the rect fits before copying.
    let stride = total_width.checked_mul(bpp)?;
    let row_bytes = rect_w.checked_mul(bpp)?;
    let last_row_start = (rect_y.checked_add(rect_h)?.checked_sub(1)?)
        .checked_mul(stride)?
        .checked_add(rect_x.checked_mul(bpp)?)?;
    if last_row_start.checked_add(row_bytes)? > bytes.len() {
        log::warn!("raw image bytes too small for {rect_w}x{rect_h} texture rect");
        return None;
    }

    let copy_rows = |dst: &mut [u8]| {
        for ry in 0..rect_h {
            let src_off = (rect_y + ry) * stride + rect_x * bpp;
            let dst_off = ry * row_bytes;
            dst[dst_off..dst_off + row_bytes].copy_from_slice(&bytes[src_off..src_off + row_bytes]);
        }
    };

    let width = rect_w as u32;
    let height = rect_h as u32;
    match &archived.pixel_format {
        ArchivedPixelFormat::Rgb => {
            let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
            copy_rows(buffer.make_mut_bytes());
            Some(Image::from_rgb8(buffer))
        }
        ArchivedPixelFormat::Rgba => {
            let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
            copy_rows(buffer.make_mut_bytes());
            Some(Image::from_rgba8(buffer))
        }
        ArchivedPixelFormat::RgbaPremultiplied => {
            let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
            copy_rows(buffer.make_mut_bytes());
            Some(Image::from_rgba8_premultiplied(buffer))
        }
        ArchivedPixelFormat::AlphaMap => None,
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
