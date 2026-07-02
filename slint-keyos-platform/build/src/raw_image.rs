// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
use std::path::{Path, PathBuf};

use i_slint_compiler::{
    embedded_resources::Size,
    passes::embed_images::{generate_texture, load_image, SourceFormat},
};
use resvg::{tiny_skia, usvg};
use slint_keyos_platform_common::{utils::parse_nine_slice_filename, IconSet, RawImage};

pub fn convert_image_to_raw(path: &Path) -> image::ImageResult<(String, Vec<u8>)> {
    let (image, source_format, original_size) = load_image(
        i_slint_compiler::fileaccess::VirtualFile { canon_path: path.to_owned(), builtin_contents: None },
        1.0,
    )?;

    let texture = generate_texture(image, source_format, original_size);
    let mut image = RawImage::from(texture);
    let file_stem = path.file_stem().unwrap().to_str().unwrap();
    let mut image_name = file_stem.to_string();
    if let Some((ns_image_name, nine_slice)) = parse_nine_slice_filename(path) {
        image.nine_slice = Some(nine_slice);
        image_name = ns_image_name;
    }
    Ok((image_name, serialize(image)))
}

pub fn convert_icons<Icons, IconSizes>(icons: Icons) -> Vec<u8>
where
    Icons: IntoIterator<Item = (PathBuf, IconSizes)>,
    IconSizes: IntoIterator<Item = usize>,
{
    let mut icon_set = IconSet::default();
    for (path, sizes) in icons {
        let icon_name = path.file_stem().unwrap().to_string_lossy().to_string();
        let icons = sizes
            .into_iter()
            .map(|size| {
                let image = render_svg(&path, size);
                let texture = generate_texture(
                    image,
                    SourceFormat::Rgba,
                    Size { width: size as u32, height: size as u32 },
                );
                RawImage::from(texture)
            })
            .collect();
        icon_set.0.insert(icon_name, icons);
    }
    serialize(icon_set)
}

fn render_svg(svg_path: &Path, size: usize) -> image::ImageBuffer<image::Rgba<u8>, Vec<u8>> {
    let tree = usvg::Tree::from_data(
        std::fs::read(svg_path)
            .unwrap_or_else(|e| panic!("Could not read SVG file {svg_path:?}: {e:?}"))
            .as_slice(),
        &Default::default(),
    )
    .unwrap_or_else(|e| panic!("Could not parse SVG file {svg_path:?}: {e:?}"));
    let original_size = tree.size();

    let mut buffer = vec![0u8; size * size * 4];
    let mut skia_buffer =
        tiny_skia::PixmapMut::from_bytes(buffer.as_mut_slice(), size as u32, size as u32).unwrap();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(
            size as f32 / original_size.width() as f32,
            size as f32 / original_size.height() as f32,
        ),
        &mut skia_buffer,
    );
    image::RgbaImage::from_raw(size as u32, size as u32, buffer).unwrap()
}

fn serialize<T>(obj: T) -> Vec<u8>
where
    for<'a> T: rkyv::Serialize<
        rkyv::api::high::HighSerializer<
            rkyv::util::AlignedVec,
            rkyv::ser::allocator::ArenaHandle<'a>,
            rkyv::rancor::Error,
        >,
    >,
{
    rkyv::to_bytes::<rkyv::rancor::Error>(&obj).unwrap().to_vec()
}
