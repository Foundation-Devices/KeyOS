// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use resvg::{tiny_skia, usvg};

const ICON_SIZE: usize = 24;
const ICONS: &[&str] = &["backspace", "caps", "shifted", "unshifted"];

const BG_WIDTH: usize = 480;
const BG_HEIGHT: usize = 306;

const UI_COLORS_PATH: &str = "../../ui/ui/palettes/ui-colors.slint";

fn ui_color(name: &str) -> [u8; 3] {
    let source = fs::read_to_string(UI_COLORS_PATH).expect("read shared UI colors");
    let prefix = format!("out property <brush> {name}: #");
    let value = source
        .lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .and_then(|value| value.strip_suffix(';'))
        .unwrap_or_else(|| panic!("missing UI color {name}"));

    assert_eq!(value.len(), 6, "UI color {name} must be an RGB hex value");
    [
        u8::from_str_radix(&value[0..2], 16).expect("red component"),
        u8::from_str_radix(&value[2..4], 16).expect("green component"),
        u8::from_str_radix(&value[4..6], 16).expect("blue component"),
    ]
}

fn add_ui_colors(out_dir: &Path) {
    let mut colors = File::create(out_dir.join("ui_colors.rs")).unwrap();
    for (constant, token) in [
        ("UI_PRIMARY_WHITE", "primary-white"),
        ("UI_BLUE_600", "blue-600"),
        ("UI_BLUE_500", "blue-500"),
        ("UI_TEAL_400", "teal-400"),
        ("UI_NEUTRAL_950", "neutral-950"),
        ("UI_NEUTRAL_900", "neutral-900"),
        ("UI_NEUTRAL_800", "neutral-800"),
        ("UI_NEUTRAL_600", "neutral-600"),
        ("UI_NEUTRAL_500", "neutral-500"),
        ("UI_NEUTRAL_200", "neutral-200"),
        ("UI_NEUTRAL_100", "neutral-100"),
    ] {
        let [red, green, blue] = ui_color(token);
        writeln!(
            colors,
            "pub(crate) const {constant}: ColorU8 = color!(0x{red:02X}, 0x{green:02X}, 0x{blue:02X});"
        )
        .unwrap();
    }

    println!("cargo:rerun-if-changed={UI_COLORS_PATH}");
}

fn load_svg(p: &str, width: usize, height: usize) -> image::RgbaImage {
    println!("cargo:rerun-if-changed={p}");
    let tree = usvg::Tree::from_data(std::fs::read(p).unwrap().as_slice(), &Default::default()).unwrap();
    let original_size = tree.size();

    let mut buffer = vec![0u8; width * height * 4];
    let mut skia_buffer =
        tiny_skia::PixmapMut::from_bytes(buffer.as_mut_slice(), width as u32, height as u32).unwrap();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(
            width as f32 / original_size.width() as f32,
            height as f32 / original_size.height() as f32,
        ),
        &mut skia_buffer,
    );
    image::RgbaImage::from_raw(width as u32, height as u32, buffer).unwrap()
}

fn add_background(out_dir: &Path, assets: &mut File) {
    for theme in ["light", "dark"] {
        let bg_image = load_svg(&format!("assets/background-{theme}.svg"), BG_WIDTH, BG_HEIGHT);

        let mut bg_file = File::create(out_dir.join(format!("bg-{theme}.raw"))).unwrap();
        bg_file.write_all(&bg_image.into_raw()).unwrap();
    }

    writeln!(assets, "pub const BG_DARK_IMAGE: &[u8] = include_bytes!(\"bg-dark.raw\");").unwrap();
    writeln!(assets, "pub const BG_LIGHT_IMAGE: &[u8] = include_bytes!(\"bg-light.raw\");").unwrap();
    writeln!(assets, "pub const BG_IMAGE_WIDTH: usize = {BG_WIDTH};").unwrap();
    writeln!(assets, "pub const BG_IMAGE_HEIGHT: usize = {BG_HEIGHT};").unwrap();
}

fn add_icon(icon_name: &str, assets: &mut File) {
    let svg_path = format!("../../ui/ui/icons/{icon_name}.svg");
    let icon = load_svg(&svg_path, ICON_SIZE, ICON_SIZE);
    let icon_a: Vec<u8> = icon.pixels().map(|p| p[3]).collect();

    let const_name = icon_name.to_uppercase().replace('-', "_");

    writeln!(assets, "pub const {const_name}: [u8; {ICON_SIZE}*{ICON_SIZE}] = {icon_a:?};").unwrap();
}

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    add_ui_colors(&out_dir);

    let mut assets = File::create(out_dir.join("assets.rs")).unwrap();
    add_background(&out_dir, &mut assets);
    for icon in ICONS {
        add_icon(icon, &mut assets);
    }

    println!("cargo:rerun-if-changed=build.rs");
}
