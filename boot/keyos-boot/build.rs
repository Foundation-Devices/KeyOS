// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    rxing::BarcodeFormat,
    std::{
        env,
        fs::{self, File},
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    },
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let assets = generate_assets()?;
    generate_asset_metadata(&assets)?;

    // See https://reproducible-builds.org/docs/source-date-epoch/
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let epoch = env::var("SOURCE_DATE_EPOCH")
        .unwrap_or_else(|_| SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs().to_string());
    println!("cargo:rustc-env=SOURCE_DATE_EPOCH={epoch}");

    Ok(())
}

struct AssetFile {
    name: String,
    fs_name: String,
    size: usize,
    qr_url: String,
    hash: [u8; 32],
}

fn generate_assets() -> Result<Vec<AssetFile>, Box<dyn std::error::Error>> {
    let crate_dir = env::var("CARGO_MANIFEST_DIR")?;

    let mut assets = vec![];
    let assets_path = PathBuf::from(&crate_dir).join("assets");
    println!("cargo:rerun-if-changed={}", assets_path.display());
    let assets_dir = fs::read_dir(assets_path)?;
    for dir in assets_dir {
        let dir = dir?;
        if !dir.path().extension().map(|s| s == "png").unwrap_or(false) {
            continue;
        }

        println!("cargo:rerun-if-changed={}", dir.path().as_os_str().to_str().expect("path"));

        assets.push(png_to_asset(dir.path())?);
    }

    Ok(assets)
}

fn png_to_asset(path: PathBuf) -> Result<AssetFile, Box<dyn std::error::Error>> {
    let file_name = path.file_stem().expect("unexpected file name format").to_str().expect("file name");
    let out_path = path.parent().unwrap().join(format!("{file_name}.raw"));

    let frame = fs::read(&path)?;
    let (_header, mut image_data_rgba8888) = png_decoder::decode(&frame).unwrap();

    // Swap R and B components
    for pixel in image_data_rgba8888.chunks_exact_mut(4) {
        pixel.swap(0, 2); // Swap R (index 0) and B (index 2)
    }

    let raw_is_current = match fs::read(&out_path) {
        Ok(current) => current == image_data_rgba8888,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if !raw_is_current {
        fs::write(&out_path, &image_data_rgba8888)?;
    }

    use sha2::Digest;
    let hash: [u8; 32] = sha2::Sha256::digest(&image_data_rgba8888).as_slice().try_into().unwrap();
    let asset = AssetFile {
        name: file_name.to_string(),
        fs_name: format!("blassets/{file_name}.raw"),
        size: image_data_rgba8888.len(),
        qr_url: recognize_qr_code(&path),
        hash,
    };

    Ok(asset)
}

fn generate_asset_metadata(assets: &[AssetFile]) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;

    let out_dir = env::var("OUT_DIR")?;
    let assets_metadata_file_path = PathBuf::from(out_dir).join("assets_metadata.rs");

    // Generate rust file that contains an array of assets, use core::fmt::Write and write!
    // macro
    let mut asset_metadata_file = File::create(assets_metadata_file_path)?;
    writeln!(
        &mut asset_metadata_file,
        "pub(crate) struct Asset {{ pub fs_name: &'static [u8], pub size: usize, pub qr_url: \
         &'static str, pub hash: [u8; 32] }}",
    )?;
    for asset in assets {
        let display_url = asset.qr_url.strip_prefix("https://").unwrap_or(&asset.qr_url);
        writeln!(
            &mut asset_metadata_file,
            "pub(crate) const ASSET_{}: Asset = Asset {{ fs_name: b\"{}\\0\", size: {}, qr_url: \
             \"{}\", hash: {:?} }};",
            asset.name.to_uppercase(),
            asset.fs_name,
            asset.size,
            display_url, // Store the stripped version
            asset.hash
        )?;
    }

    Ok(())
}

fn recognize_qr_code(image: &Path) -> String {
    let path = image.to_str().expect("path");
    let Ok(res) = rxing::helpers::detect_in_file(path, Some(BarcodeFormat::QR_CODE)) else {
        return "".to_string();
    };

    res.getText().to_string()
}
