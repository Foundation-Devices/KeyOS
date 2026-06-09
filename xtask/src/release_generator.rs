// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;

use release_manifest::ReleaseManifest;

pub fn generate_release(manifest_path: &Path, output_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let manifest = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("failed to read manifest file \"{}\": {e}", manifest_path.display()))?;
    let manifest: ReleaseManifest = serde_json::from_str(&manifest)
        .map_err(|e| format!("failed to parse manifest file \"{}\": {e}", manifest_path.display()))?;
    verify_manifest(&manifest)?;
    let mut files = std::collections::HashSet::new();
    files_used_by_manifest(&manifest, &mut files);
    let mut tar = tar::Builder::new(
        std::fs::File::create(output_path)
            .map_err(|e| format!("failed to create output file \"{}\": {e}", output_path.display()))?,
    );
    for file in files.iter() {
        tar.append_path(file).map_err(|e| format!("failed to append file \"{file}\" to archive: {e}"))?;
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

fn verify_action(action: &release_manifest::Action) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        release_manifest::Action::Patch { patch_file, patch_source: _, base_version, new_version }
        | release_manifest::Action::PatchAdd {
            patch_file,
            patch_source: _,
            dest: _,
            base_version,
            new_version,
        } => {
            let patch_file = std::fs::read(patch_file)
                .map_err(|e| format!("failed to read patch file \"{patch_file}\": {e}"))?;
            let expected_base_version = parse_version(base_version)?;
            let expected_new_version = parse_version(new_version)?;
            let actual_base_version = patch_file.get(0..4).ok_or("patch file too short")?;
            let actual_new_version = patch_file.get(44..48).ok_or("patch file too short")?;
            if actual_base_version != expected_base_version {
                return Err("patch file base version does not match expected version".into());
            }
            if actual_new_version != expected_new_version {
                return Err("patch file new version does not match expected version".into());
            }
        }
        release_manifest::Action::Add { .. }
        | release_manifest::Action::Replace { .. }
        | release_manifest::Action::UpdateBt
        | release_manifest::Action::Delete { .. }
        | release_manifest::Action::Rename { .. }
        | release_manifest::Action::Move { .. }
        | release_manifest::Action::Copy { .. }
        | release_manifest::Action::Set { .. }
        | release_manifest::Action::OpenApp { .. } => {}
    }
    Ok(())
}

fn files_used_by_manifest(manifest: &ReleaseManifest, files: &mut std::collections::HashSet<String>) {
    for action in manifest.transactions.iter().flat_map(|tx| tx.actions()) {
        files_used_by_action(action, files);
    }
}

fn files_used_by_action(action: &release_manifest::Action, files: &mut std::collections::HashSet<String>) {
    match action {
        release_manifest::Action::Patch { patch_file: file, .. }
        | release_manifest::Action::PatchAdd { patch_file: file, .. }
        | release_manifest::Action::Add { source: file, .. }
        | release_manifest::Action::Replace { source: file, .. } => {
            files.insert(file.clone());
        }
        release_manifest::Action::UpdateBt
        | release_manifest::Action::Delete { .. }
        | release_manifest::Action::Rename { .. }
        | release_manifest::Action::Move { .. }
        | release_manifest::Action::Copy { .. }
        | release_manifest::Action::Set { .. }
        | release_manifest::Action::OpenApp { .. } => {}
    }
}

fn parse_version(s: &str) -> Result<[u8; 4], &'static str> {
    if !s.starts_with('v') {
        return Err("version must start with 'v'");
    }
    let s = &s[1..];
    let (major, rest) = s.split_once('.').ok_or("missing major version")?;
    let (minor, patch_and_beta) = rest.split_once('.').ok_or("missing minor version")?;
    let (patch, beta) = patch_and_beta.split_once('b').unwrap_or((patch_and_beta, ""));
    let major = major.parse().map_err(|_| "major version invalid or out of range")?;
    let minor = minor.parse().map_err(|_| "minor version invalid or out of range")?;
    let patch = patch.parse().map_err(|_| "patch version invalid or out of range")?;
    let beta = if beta.is_empty() {
        0xFF
    } else {
        let beta = beta.parse().map_err(|_| "beta version invalid or out of range")?;
        if beta == 0xFF {
            return Err("beta version may not be 0xFF");
        }
        beta
    };
    Ok([major, minor, patch, beta])
}
