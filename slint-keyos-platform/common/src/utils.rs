// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::{Path, PathBuf};

const KEYOS_UI_LIBRARY_NAME: &str = "ui";

pub fn workspace_dir() -> PathBuf {
    let output = std::process::Command::new(env!("CARGO"))
        .arg("locate-project")
        .arg("--workspace")
        .arg("--message-format=plain")
        .output()
        .unwrap()
        .stdout;
    let cargo_path = Path::new(std::str::from_utf8(&output).unwrap().trim());
    cargo_path.parent().unwrap().to_path_buf()
}

/// Slint library namespace for per-app generated component themes
/// (for example, `@theme/button_theme.slint`), set by `foundation build`/`sim`.
const KEYOS_THEME_LIBRARY_NAME: &str = "theme";

pub fn library_paths() -> std::collections::HashMap<String, PathBuf> {
    let ui_path = std::env::var_os("FOUNDATION_UI_LIBRARY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_dir().join("ui/ui"));
    // Per-app generated component themes, emitted by `foundation build` into the
    // app's target dir. Falls back to the UI dir (which ships the shared
    // no-overrides defaults) so a plain in-tree build still resolves `@theme/*`.
    let theme_path =
        std::env::var_os("FOUNDATION_THEMES_SLINT_DIR").map(PathBuf::from).unwrap_or_else(|| ui_path.clone());
    std::collections::HashMap::from([
        (KEYOS_UI_LIBRARY_NAME.to_owned(), ui_path),
        (KEYOS_THEME_LIBRARY_NAME.to_owned(), theme_path),
    ])
}

pub fn parse_nine_slice_filename(path: &Path) -> Option<(String, [u16; 4])> {
    let (image_name, nine_slice_str) = path.file_stem()?.to_str()?.split_once("__")?;
    let values: Vec<u16> = nine_slice_str.split('-').map(|s| s.parse().ok()).collect::<Option<Vec<_>>>()?;
    let ns_values: [u16; 4] = values.try_into().ok()?;
    Some((image_name.to_string(), ns_values))
}
