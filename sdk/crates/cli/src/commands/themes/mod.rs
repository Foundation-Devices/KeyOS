// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! `foundation themes` - manage the theme cache under `~/.foundation/themes`.
//!
//! Themes are authored as JSON (source of truth) and compiled to Rust on
//! demand. This command seeds the SDK's base themes into the cache, runs the
//! `foundation-theme-compiler` codegen tool, lists what's available, and
//! scaffolds new themes from a base.
//!
//! Layout:
//! ```text
//! ~/.foundation/themes/
//!   json/   <- editor source of truth (base themes + user themes)
//!   rust/   <- generated; included by apps via foundation_themes::include_theme!
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use foundation_core::{AppConfig, SdkRoot};

const APP_THEME_ID: &str = "app_theme";

/// `~/.foundation/themes`
fn themes_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".foundation").join("themes"))
}

fn json_dir() -> Result<PathBuf> { Ok(themes_dir()?.join("json")) }
fn rust_dir() -> Result<PathBuf> { Ok(themes_dir()?.join("rust")) }

/// Directory of the SDK's bundled base theme JSON.
pub(crate) fn sdk_themes_dir(sdk: &SdkRoot) -> PathBuf {
    sdk.keyos_root().join("sdk").join("crates").join("foundation-themes").join("themes")
}

/// Copy any base theme JSON the user doesn't already have into `~/.foundation/themes/json`.
/// User edits win - we never overwrite an existing file.
fn seed_base_themes(sdk: &SdkRoot, json_dir: &Path) -> Result<()> {
    fs::create_dir_all(json_dir).with_context(|| format!("failed to create {}", json_dir.display()))?;

    let src = sdk_themes_dir(sdk);
    if !src.is_dir() {
        return Ok(());
    }

    let mut seeded = false;
    for entry in fs::read_dir(&src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(name) = path.file_name() else { continue };
        let dest = json_dir.join(name);
        if !dest.exists() {
            fs::copy(&path, &dest)
                .with_context(|| format!("failed to copy {} -> {}", path.display(), dest.display()))?;
            seeded = true;
        }
    }

    if seeded {
        println!("Seeding base themes into {}...", json_dir.display());
    }
    Ok(())
}

/// Optional per-app component-theme `.slint` generation, passed alongside the
/// Rust theme compile. When present, the compiler also emits
/// `<key>_theme.slint` into `slint_dir` from the plugin schemas in `plugin_dir`
/// plus the app theme's `components.<key>` overrides.
pub(crate) struct SlintThemeGen<'a> {
    pub plugin_dir: &'a Path,
    pub slint_dir: &'a Path,
    pub app_theme_json: &'a Path,
    pub components: &'a [&'a str],
}

/// Run the bundled `foundation-theme-compiler`, falling back to `cargo run`
/// from the SDK source tree when the tool isn't on PATH or in the SDK bin dir.
/// Mirrors how the build command resolves cosign2.
pub(crate) fn run_compiler(
    sdk: &SdkRoot,
    json_dir: &Path,
    rust_dir: &Path,
    slint: Option<SlintThemeGen<'_>>,
) -> Result<()> {
    let json = json_dir.display().to_string();
    let rust = rust_dir.display().to_string();

    let mut args: Vec<String> = vec!["--json-dir".into(), json, "--rust-dir".into(), rust];
    if let Some(gen) = slint {
        args.push("--plugin-dir".into());
        args.push(gen.plugin_dir.display().to_string());
        args.push("--slint-dir".into());
        args.push(gen.slint_dir.display().to_string());
        args.push("--app-theme-json".into());
        args.push(gen.app_theme_json.display().to_string());
        if !gen.components.is_empty() {
            args.push("--components".into());
            args.push(gen.components.join(","));
        }
    }

    let status = if let Some(tool) = sdk.tool_path(&["foundation-theme-compiler"]) {
        Command::new(tool)
            .args(&args)
            .status()
            .context("Could not find or run the theme compiler (foundation-theme-compiler).")?
    } else {
        let manifest =
            sdk.keyos_root().join("sdk").join("crates").join("foundation-themes").join("Cargo.toml");
        if !manifest.exists() {
            anyhow::bail!("Could not find or run the theme compiler (foundation-theme-compiler).");
        }
        Command::new("cargo")
            .arg("run")
            .arg("--quiet")
            .arg("--manifest-path")
            .arg(&manifest)
            .arg("--bin")
            .arg("foundation-theme-compiler")
            .arg("--")
            .args(&args)
            .status()
            .context("Could not find or run the theme compiler (foundation-theme-compiler).")?
    };

    if !status.success() {
        anyhow::bail!("Theme compiler failed");
    }
    Ok(())
}

/// The directory `foundation build`/`sim` point the `@theme` Slint namespace at
/// (per-app generated component themes). Sibling of the generated Rust dir.
pub fn project_theme_slint_dir(project_root: &Path) -> PathBuf {
    project_root.join("target").join("foundation").join("themes").join("slint")
}

/// Ensure the generated theme Rust exists and is up to date, returning the
/// rust directory so `build`/`sim` can point `FOUNDATION_THEMES_RUST_DIR` at it.
///
/// Regenerates when the `rust/` index is missing or any source JSON is newer
/// than it, so app builds always pick up edited themes without a manual
/// `foundation themes build`.
fn ensure_built_in(sdk: &SdkRoot, themes_dir: &Path) -> Result<PathBuf> {
    let json_dir = themes_dir.join("json");
    let rust_dir = themes_dir.join("rust");
    seed_base_themes(sdk, &json_dir)?;

    if needs_regen(&json_dir, &rust_dir) {
        fs::create_dir_all(&rust_dir)?;
        run_compiler(sdk, &json_dir, &rust_dir, None)?;
    }
    Ok(rust_dir)
}

/// Generate the app-local `app_theme` module when app-config.toml names a
/// theme. The generated directory also contains the regular user/built-in
/// theme modules, so older source that still includes a base theme id keeps
/// compiling if the config adds `theme` later.
pub fn ensure_project_theme(sdk: &SdkRoot, config: &AppConfig, project_root: &Path) -> Result<PathBuf> {
    ensure_project_theme_in(sdk, config, project_root, &themes_dir()?)
}

pub fn ensure_project_theme_in(
    sdk: &SdkRoot,
    config: &AppConfig,
    project_root: &Path,
    themes_dir: &Path,
) -> Result<PathBuf> {
    let Some(theme) = config.theme.as_deref().map(str::trim).filter(|theme| !theme.is_empty()) else {
        return ensure_built_in(sdk, themes_dir);
    };

    let global_json_dir = themes_dir.join("json");
    seed_base_themes(sdk, &global_json_dir)?;

    let project_theme_root = project_root.join("target").join("foundation").join("themes");
    let project_json_dir = project_theme_root.join("json");
    let project_rust_dir = project_theme_root.join("rust");
    fs::create_dir_all(&project_json_dir)?;
    fs::create_dir_all(&project_rust_dir)?;

    // Compile exactly two themes for the app: the default theme (the only
    // built-in, which the app inherits) and the app's own app_theme. We do NOT
    // mirror the rest of the shared theme cache — that drags stale or unrelated
    // themes into the build — and we prune anything left from an earlier build,
    // so only these two are ever compiled.
    let app_theme_json = project_json_dir.join(format!("{APP_THEME_ID}.json"));
    copy_if_changed(
        &global_json_dir.join("default_theme.json"),
        &project_json_dir.join("default_theme.json"),
    )?;
    write_app_theme_json(theme, sdk, project_root, &app_theme_json)?;
    prune_foreign_theme_jsons(&project_json_dir)?;

    // Also emit per-app component theme `.slint` files (button-first): the app's
    // `components.button` overrides become literals; everything else cascades
    // from tokens. Regenerate when the Rust is stale (theme edited) or the slint
    // hasn't been generated yet.
    let slint_dir = project_theme_slint_dir(project_root);
    let plugin_dir = sdk.plugin_schema_path();
    if needs_regen(&project_json_dir, &project_rust_dir) || !slint_dir.join("button_theme.slint").exists() {
        run_compiler(
            sdk,
            &project_json_dir,
            &project_rust_dir,
            Some(SlintThemeGen {
                plugin_dir: &plugin_dir,
                slint_dir: &slint_dir,
                app_theme_json: &app_theme_json,
                components: &["button"],
            }),
        )?;
    }

    Ok(project_rust_dir)
}

/// Remove any theme JSON in `dir` that isn't `default_theme` or the app theme,
/// so an app build only ever compiles those two. Stale or unrelated themes left
/// in the project's theme dir (e.g. from an older build that mirrored the whole
/// shared cache) are dropped.
fn prune_foreign_theme_jsons(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
        if stem != "default_theme" && stem != APP_THEME_ID {
            fs::remove_file(&path).ok();
        }
    }
    Ok(())
}

fn write_app_theme_json(theme: &str, sdk: &SdkRoot, project_root: &Path, destination: &Path) -> Result<()> {
    let json = if is_theme_path(theme) {
        let theme_path = resolve_project_path(project_root, theme);
        let contents = fs::read_to_string(&theme_path)
            .with_context(|| format!("failed to read configured app theme {}", theme_path.display()))?;
        let mut value: serde_json::Value = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse configured app theme {}", theme_path.display()))?;
        set_theme_json_id(&mut value, APP_THEME_ID)?;
        serde_json::to_string_pretty(&value)?
    } else {
        if !base_exists(&json_dir()?, sdk, theme) {
            anyhow::bail!(
                "Configured theme '{}' was not found. Run 'foundation themes list' to see available themes.",
                theme
            );
        }
        format!(
            "{{\n  \"id\": \"{APP_THEME_ID}\",\n  \"name\": \"App Theme\",\n  \"parent\": \"{}\"\n}}\n",
            normalize_id(theme)
        )
    };

    write_if_changed(destination, json.as_bytes())
}

fn set_theme_json_id(value: &mut serde_json::Value, id: &str) -> Result<()> {
    let Some(object) = value.as_object_mut() else {
        anyhow::bail!("theme JSON must be an object");
    };
    object.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    if !object.contains_key("name") {
        object.insert("name".to_string(), serde_json::Value::String("App Theme".to_string()));
    }
    Ok(())
}

pub(crate) fn is_theme_path(theme: &str) -> bool {
    theme.ends_with(".json") || theme.contains('/') || theme.contains('\\') || theme.starts_with('.')
}

pub(crate) fn resolve_project_path(project_root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

pub(crate) fn write_editable_app_theme(
    theme: &str,
    sdk: &SdkRoot,
    project_root: &Path,
    destination: &Path,
    display_name: &str,
) -> Result<()> {
    let mut value = if is_theme_path(theme) {
        let path = resolve_project_path(project_root, theme);
        if path.exists() {
            serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&path)?)
                .with_context(|| format!("failed to parse theme {}", path.display()))?
        } else {
            empty_app_theme_json(display_name, "default_theme")
        }
    } else if let Some(path) = find_theme_json(sdk, theme)? {
        serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&path)?)
            .with_context(|| format!("failed to parse theme {}", path.display()))?
    } else {
        empty_app_theme_json(display_name, &normalize_id(theme))
    };

    set_theme_json_id(&mut value, APP_THEME_ID)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("name".to_string(), serde_json::Value::String(display_name.to_string()));
    }

    let json = serde_json::to_string_pretty(&value)?;
    write_if_changed(destination, format!("{json}\n").as_bytes())
}

fn empty_app_theme_json(display_name: &str, parent: &str) -> serde_json::Value {
    serde_json::json!({
        "id": APP_THEME_ID,
        "name": display_name,
        "parent": parent,
    })
}

fn find_theme_json(sdk: &SdkRoot, theme: &str) -> Result<Option<PathBuf>> {
    let normalized = normalize_id(theme);
    for dir in [json_dir()?, sdk_themes_dir(sdk)] {
        for stem in [theme, normalized.as_str()] {
            let path = dir.join(format!("{stem}.json"));
            if path.exists() {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

fn copy_if_changed(source: &Path, destination: &Path) -> Result<()> {
    let contents = fs::read(source).with_context(|| format!("failed to read {}", source.display()))?;
    write_if_changed(destination, &contents)
}

fn write_if_changed(destination: &Path, contents: &[u8]) -> Result<()> {
    if fs::read(destination).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(destination, contents).with_context(|| format!("failed to write {}", destination.display()))
}

/// True if `rust/mod.rs` is missing or older than any `json/*.json`.
fn needs_regen(json_dir: &Path, rust_dir: &Path) -> bool {
    let index = rust_dir.join("mod.rs");
    let Ok(index_mtime) = fs::metadata(&index).and_then(|m| m.modified()) else {
        return true; // never generated
    };
    let Ok(entries) = fs::read_dir(json_dir) else {
        return true;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(json_mtime) = fs::metadata(&path).and_then(|m| m.modified()) {
            if json_mtime > index_mtime {
                return true;
            }
        }
    }
    false
}

/// `foundation themes build` - seed base themes then regenerate all Rust.
pub fn execute_build() -> Result<()> {
    let sdk = SdkRoot::discover().context("Could not locate the Foundation SDK root. Run from an SDK checkout or unpacked bundle, or set FOUNDATION_SDK_ROOT.")?;
    let json_dir = json_dir()?;
    let rust_dir = rust_dir()?;

    seed_base_themes(&sdk, &json_dir)?;
    fs::create_dir_all(&rust_dir)?;

    println!("Generating theme Rust from {}...", json_dir.display());
    run_compiler(&sdk, &json_dir, &rust_dir, None)?;

    let count = count_themes(&json_dir).unwrap_or(0);
    println!("Generated {} theme(s) into {}", count, rust_dir.display());
    Ok(())
}

/// `foundation themes list` - show the source-of-truth JSON themes.
pub fn execute_list() -> Result<()> {
    let sdk = SdkRoot::discover().context("Could not locate the Foundation SDK root. Run from an SDK checkout or unpacked bundle, or set FOUNDATION_SDK_ROOT.")?;
    let json_dir = json_dir()?;
    seed_base_themes(&sdk, &json_dir)?;

    println!("Available themes (source: {}):", json_dir.display());
    let mut names = theme_ids(&json_dir)?;
    names.sort();
    for name in names {
        println!("  {name}");
    }
    Ok(())
}

/// `foundation themes new <name> --from <base>` - copy a base JSON to a new
/// theme the user can edit, then regenerate Rust.
pub fn execute_new(name: &str, base: &str) -> Result<()> {
    let sdk = SdkRoot::discover().context("Could not locate the Foundation SDK root. Run from an SDK checkout or unpacked bundle, or set FOUNDATION_SDK_ROOT.")?;
    let json_dir = json_dir()?;
    seed_base_themes(&sdk, &json_dir)?;

    let dest = json_dir.join(format!("{name}.json"));
    if dest.exists() {
        anyhow::bail!("Theme '{}' already exists at {}", name, dest.display());
    }

    // A new theme is just a JSON that inherits from the base, so the user only
    // has to express overrides.
    let contents = format!(
        "{{\n  \"id\": \"{id}\",\n  \"name\": \"{display}\",\n  \"parent\": \"{base}\",\n  \"tokens\": {{}}\n}}\n",
        id = normalize_id(name),
        display = name,
        base = base,
    );

    if !base_exists(&json_dir, &sdk, base) {
        anyhow::bail!(
            "Base theme '{}' not found. Run 'foundation themes list' to see available themes.",
            base
        );
    }

    fs::write(&dest, contents).with_context(|| format!("failed to write {}", dest.display()))?;
    println!("Created theme '{}' from '{}' at {}", name, base, dest.display());
    println!(
        "Edit the JSON in {} and re-run 'foundation themes build' to regenerate Rust.",
        json_dir.display()
    );

    // Regenerate so the new theme is immediately includable.
    run_compiler(&sdk, &json_dir, &rust_dir()?, None)
}

fn base_exists(json_dir: &Path, sdk: &SdkRoot, base: &str) -> bool {
    let normalized = normalize_id(base);
    [json_dir.to_path_buf(), sdk_themes_dir(sdk)].iter().any(|dir| {
        dir.join(format!("{base}.json")).exists() || dir.join(format!("{normalized}.json")).exists()
    })
}

fn theme_ids(json_dir: &Path) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    if !json_dir.is_dir() {
        return Ok(ids);
    }
    for entry in fs::read_dir(json_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                ids.push(stem.to_string());
            }
        }
    }
    Ok(ids)
}

fn count_themes(json_dir: &Path) -> Result<usize> { Ok(theme_ids(json_dir)?.len()) }

fn normalize_id(value: &str) -> String {
    let mut out = String::new();
    let mut last_underscore = true;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            if out.is_empty() && ch.is_ascii_digit() {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "theme".to_string()
    } else {
        out
    }
}
