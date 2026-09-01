// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Read;
use std::path::Path;

use update_image::patch::Format;
use update_image::{Header, ReleaseManifest, Version};

/// Newest source version that gets a [`Format::LegacyBzip2`] patch, because it
/// cannot read a zstd body. Anything above it gets [`Format::Zstd`].
const LAST_LEGACY_UPDATE_VERSION: (u8, u8, u8) = (1, 3, 1);

/// Write the body of every patch `manifest` references.
///
/// The two source trees are read at the same relative path the manifest gives for
/// the patch, and the patch is written under `out` at that path.
pub fn build_patches(
    manifest_path: &Path,
    base: &Path,
    new: &Path,
    out: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest = read_manifest(manifest_path)?;

    for action in manifest.transactions.iter().flat_map(|tx| tx.actions()) {
        let (patch_file, base_version, new_version) = match action {
            update_image::Action::Patch { patch_file, base_version, new_version, .. }
            | update_image::Action::PatchAdd { patch_file, base_version, new_version, .. } => {
                (patch_file, base_version, new_version)
            }
            _ => continue,
        };

        let old_version = Version::parse(base_version)?;
        let new_version = Version::parse(new_version)?;
        let triple = (old_version.major, old_version.minor, old_version.patch);
        let format = if triple <= LAST_LEGACY_UPDATE_VERSION { Format::LegacyBzip2 } else { Format::Zstd };

        let old = read_file(&base.join(patch_file))?;
        let new = read_file(&new.join(patch_file))?;

        let patch_path = out.join(patch_file);
        let parent = patch_path.parent().expect("a patch path has a parent");
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create \"{}\": {e}", parent.display()))?;
        let mut patch = std::fs::File::create(&patch_path)
            .map_err(|e| format!("failed to create \"{}\": {e}", patch_path.display()))?;
        update_image::patch::build(&old, old_version, &new, new_version, format, &mut patch)
            .map_err(|e| format!("failed to build \"{}\": {e}", patch_path.display()))?;

        let size = patch.metadata().map(|m| m.len()).unwrap_or(0);
        println!("[INFO] {} ({format:?}, {size} bytes)", patch_path.display());
    }
    Ok(())
}

fn read_file(path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    std::fs::read(path).map_err(|e| format!("failed to read \"{}\": {e}", path.display()).into())
}

fn read_manifest(manifest_path: &Path) -> Result<ReleaseManifest, Box<dyn std::error::Error>> {
    let manifest = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("failed to read manifest file \"{}\": {e}", manifest_path.display()))?;
    serde_json::from_str(&manifest)
        .map_err(|e| format!("failed to parse manifest file \"{}\": {e}", manifest_path.display()).into())
}

pub fn generate_release(manifest_path: &Path, output_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let manifest = read_manifest(manifest_path)?;
    verify_manifest(&manifest)?;
    let mut files = std::collections::HashSet::new();
    files_used_by_manifest(&manifest, &mut files);
    let mut tar = tar::Builder::new(
        std::fs::File::create(output_path)
            .map_err(|e| format!("failed to create output file \"{}\": {e}", output_path.display()))?,
    );
    let mut patch_header = tar::Header::new_gnu();
    patch_header.set_entry_type(tar::EntryType::Directory);
    patch_header.set_size(0);
    patch_header.set_mode(0o755);
    tar.append_data(&mut patch_header, "patch/", &[][..])
        .map_err(|e| format!("failed to append patch directory: {e}"))?;
    let mut dirs = std::collections::BTreeSet::new();
    for file in files.iter() {
        dirs.extend(
            Path::new(file)
                .ancestors()
                .skip(1)
                .filter(|path| !path.as_os_str().is_empty())
                .map(Path::to_path_buf),
        );
    }
    for dir in dirs {
        tar.append_dir(Path::new("patch").join(&dir), &dir)
            .map_err(|e| format!("failed to append directory \"{}\": {e}", dir.display()))?;
    }
    for file in files.iter() {
        tar.append_path_with_name(file, Path::new("patch").join(file))
            .map_err(|e| format!("failed to append file \"{file}\" to archive: {e}"))?;
    }
    let mut manifest_file = std::fs::File::open(manifest_path)
        .map_err(|e| format!("failed to open manifest file \"{}\": {e}", manifest_path.display()))?;
    tar.append_file("manifest.json", &mut manifest_file).map_err(|e| {
        format!("failed to append manifest file \"{}\" to archive: {e}", manifest_path.display())
    })?;
    Ok(())
}

fn verify_manifest(manifest: &ReleaseManifest) -> Result<(), Box<dyn std::error::Error>> {
    for action in manifest.transactions.iter().flat_map(|tx| tx.actions()) {
        verify_action(action)?;
    }
    Ok(())
}

fn verify_action(action: &update_image::Action) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        update_image::Action::Patch { patch_file, patch_source: _, base_version, new_version }
        | update_image::Action::PatchAdd {
            patch_file,
            patch_source: _,
            dest: _,
            base_version,
            new_version,
        } => {
            let mut file = std::fs::File::open(patch_file)
                .map_err(|e| format!("failed to open patch file \"{patch_file}\": {e}"))?;
            let header = Header::read_from(&mut file)
                .map_err(|e| format!("failed to read the header of patch file \"{patch_file}\": {e}"))?;
            if header.old_version != Version::parse(base_version)? {
                return Err("patch file base version does not match expected version".into());
            }
            if header.new_version != Version::parse(new_version)? {
                return Err("patch file new version does not match expected version".into());
            }
            let mut magic = [0; Format::MAGIC_LEN];
            file.read_exact(&mut magic)
                .map_err(|e| format!("failed to read the body of patch file \"{patch_file}\": {e}"))?;
            if Format::detect(&magic).is_none() {
                return Err(format!("patch file \"{patch_file}\" has an unknown body format").into());
            }
        }
        update_image::Action::Add { .. }
        | update_image::Action::Replace { .. }
        | update_image::Action::UpdateBt
        | update_image::Action::Delete { .. }
        | update_image::Action::Rename { .. }
        | update_image::Action::Move { .. }
        | update_image::Action::Copy { .. }
        | update_image::Action::Set { .. }
        | update_image::Action::OpenApp { .. } => {}
    }
    Ok(())
}

fn files_used_by_manifest(manifest: &ReleaseManifest, files: &mut std::collections::HashSet<String>) {
    for action in manifest.transactions.iter().flat_map(|tx| tx.actions()) {
        files_used_by_action(action, files);
    }
}

fn files_used_by_action(action: &update_image::Action, files: &mut std::collections::HashSet<String>) {
    match action {
        update_image::Action::Patch { patch_file: file, .. }
        | update_image::Action::PatchAdd { patch_file: file, .. }
        | update_image::Action::Add { source: file, .. }
        | update_image::Action::Replace { source: file, .. } => {
            files.insert(file.clone());
        }
        update_image::Action::UpdateBt
        | update_image::Action::Delete { .. }
        | update_image::Action::Rename { .. }
        | update_image::Action::Move { .. }
        | update_image::Action::Copy { .. }
        | update_image::Action::Set { .. }
        | update_image::Action::OpenApp { .. } => {}
    }
}
