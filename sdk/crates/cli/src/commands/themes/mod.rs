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

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use foundation_core::{AppConfig, SdkRoot};

const APP_THEME_ID: &str = "app_theme";
const BASE_THEME_ID: &str = "base_theme";
const LEGACY_DEFAULT_THEME_ID: &str = "default_theme";

#[derive(Args)]
pub struct ThemesArgs {
    #[command(subcommand)]
    pub command: ThemesCommands,
}

#[derive(Subcommand)]
pub enum ThemesCommands {
    /// Generate theme Rust from JSON
    #[command(
        long_about = "Seed base themes and compile every theme JSON in ~/.foundation/themes/json into Rust under ~/.foundation/themes/rust"
    )]
    Build,

    /// List available themes
    #[command(long_about = "List the theme JSON files in ~/.foundation/themes/json")]
    List,

    /// Create a new theme from a base
    #[command(
        long_about = "Copy a base theme to a new editable JSON in ~/.foundation/themes/json and regenerate Rust"
    )]
    New(ThemesNewArgs),
}

#[derive(Args)]
pub struct ThemesNewArgs {
    /// Name of the new theme
    pub name: String,

    /// Base theme to inherit from (default: base_theme)
    #[arg(long, value_name = "BASE", default_value = "base_theme")]
    pub from: String,
}

pub fn execute(args: &ThemesArgs) -> Result<()> {
    match &args.command {
        ThemesCommands::Build => execute_build(),
        ThemesCommands::List => execute_list(),
        ThemesCommands::New(args) => execute_new(&args.name, &args.from),
    }
}

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
/// `<key>_theme.slint` into `slint_dir` from the component schemas in `plugin_dir`
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
    let plugin_dir =
        slint.as_ref().map(|gen| gen.plugin_dir.to_path_buf()).unwrap_or_else(|| sdk.plugin_schema_path());
    args.push("--plugin-dir".into());
    args.push(plugin_dir.display().to_string());
    if let Some(gen) = slint {
        args.push("--slint-dir".into());
        args.push(gen.slint_dir.display().to_string());
        args.push("--app-theme-json".into());
        args.push(gen.app_theme_json.display().to_string());
        if !gen.components.is_empty() {
            args.push("--components".into());
            args.push(gen.components.join(","));
        }
    }

    let output = if let Some(tool) = sdk.tool_path(&["foundation-theme-compiler"]) {
        Command::new(tool)
            .args(&args)
            .output()
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
            .output()
            .context("Could not find or run the theme compiler (foundation-theme-compiler).")?
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            anyhow::bail!("Theme compiler failed");
        }
        anyhow::bail!("Theme compiler failed:\n{detail}");
    }
    Ok(())
}

/// The directory `foundation build`/`sim` point the `@theme` Slint namespace at
/// (per-app generated component themes). Sibling of the generated Rust dir.
pub fn project_theme_slint_dir(project_root: &Path) -> PathBuf {
    project_root.join("target").join("foundation").join("themes").join("slint")
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
    // Every SDK component imports its schema-backed theme through `@theme`.
    // Apps without an explicit theme still need a generated directory, using
    // the bundled base theme as their app theme.
    let theme =
        config.theme.as_deref().map(str::trim).filter(|theme| !theme.is_empty()).unwrap_or(BASE_THEME_ID);

    let global_json_dir = themes_dir.join("json");
    seed_base_themes(sdk, &global_json_dir)?;

    let project_theme_root = project_root.join("target").join("foundation").join("themes");
    let project_json_dir = project_theme_root.join("json");
    let project_rust_dir = project_theme_root.join("rust");
    fs::create_dir_all(&project_json_dir)?;
    fs::create_dir_all(&project_rust_dir)?;

    // Compile exactly two themes for the app: the base theme (the only
    // built-in, which the app inherits) and the app's own app_theme. We do NOT
    // mirror the rest of the shared theme cache — that drags stale or unrelated
    // themes into the build — and we prune anything left from an earlier build,
    // so only these two are ever compiled.
    let app_theme_json = project_json_dir.join(format!("{APP_THEME_ID}.json"));
    copy_if_changed(
        &global_json_dir.join(format!("{BASE_THEME_ID}.json")),
        &project_json_dir.join(format!("{BASE_THEME_ID}.json")),
    )?;
    write_app_theme_json(theme, sdk, project_root, &app_theme_json)?;
    // Stage the app theme's full parent chain, so an app can inherit any theme
    // created with `foundation themes new` — not only base_theme. base_theme and
    // app_theme are always kept; each custom ancestor is copied in and kept.
    let mut keep: BTreeSet<String> =
        [APP_THEME_ID.to_string(), BASE_THEME_ID.to_string()].into_iter().collect();
    stage_theme_parent_chain(&app_theme_json, &global_json_dir, sdk, &project_json_dir, &mut keep)?;
    prune_foreign_theme_jsons(&project_json_dir, &keep)?;

    // Also emit every schema-backed per-app component theme `.slint` file. The
    // app's component overrides become literals; everything else cascades from
    // tokens. Regenerate when theme JSON or any component schema is newer than
    // its generated output, or when an expected output is missing.
    let slint_dir = project_theme_slint_dir(project_root);
    let plugin_dir = sdk.plugin_schema_path();
    if needs_regen(&project_json_dir, &project_rust_dir)
        || component_themes_need_regen(&project_json_dir, &plugin_dir, &slint_dir)
    {
        run_compiler(
            sdk,
            &project_json_dir,
            &project_rust_dir,
            Some(SlintThemeGen {
                plugin_dir: &plugin_dir,
                slint_dir: &slint_dir,
                app_theme_json: &app_theme_json,
                // Empty means every `*.schema.json`, discovered in stable order
                // by the compiler.
                components: &[],
            }),
        )?;
    }

    Ok(project_rust_dir)
}

/// Remove any theme JSON in `dir` that isn't `base_theme` or the app theme,
/// so an app build only ever compiles those two. Stale or unrelated themes left
/// in the project's theme dir (e.g. from an older build that mirrored the whole
/// shared cache) are dropped.
fn prune_foreign_theme_jsons(dir: &Path, keep: &BTreeSet<String>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
        if !keep.contains(stem) {
            fs::remove_file(&path).ok();
        }
    }
    Ok(())
}

/// Walk the app theme's `parent` chain and copy each ancestor theme JSON into
/// the staging dir, so the compiler can resolve inheritance beyond base_theme.
/// `keep` accumulates every staged theme id (the caller seeds it with
/// base_theme + app_theme). Ancestors are looked up in the global theme cache
/// first, then the SDK's bundled themes dir.
fn stage_theme_parent_chain(
    app_theme_json: &Path,
    global_json_dir: &Path,
    sdk: &SdkRoot,
    project_json_dir: &Path,
    keep: &mut BTreeSet<String>,
) -> Result<()> {
    let mut current = read_theme_parent(app_theme_json)?;
    while let Some(parent) = current {
        let parent = normalize_theme_alias(&parent);
        // A path-based parent (e.g. "./base.json") is resolved by the compiler's
        // path-aware resolve_parent_reference; ID staging would mangle it into
        // "<path>.json.json" and fail the lookup. Defer the rest of the chain to
        // the compiler, which walks it from the referenced file's own location.
        if is_theme_path(&parent) {
            break;
        }
        // base_theme is always staged already; stop there, and guard cycles.
        if parent == BASE_THEME_ID || !keep.insert(parent.clone()) {
            break;
        }
        let source = find_theme_json(sdk, &parent)?.ok_or_else(|| {
            anyhow::anyhow!(
                "app theme inherits from \"{parent}\", but no \"{parent}.json\" was found in {} \
                 or the SDK themes dir. Run 'foundation themes list' to see available themes.",
                global_json_dir.display()
            )
        })?;
        let dest = project_json_dir.join(format!("{parent}.json"));
        copy_if_changed(&source, &dest)?;
        current = read_theme_parent(&dest)?;
    }
    Ok(())
}

/// Read a theme JSON's `parent` field (trimmed; `None` when absent or empty).
fn read_theme_parent(path: &Path) -> Result<Option<String>> {
    let contents = fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(value
        .get("parent")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|parent| !parent.is_empty())
        .map(ToOwned::to_owned))
}

fn write_app_theme_json(theme: &str, sdk: &SdkRoot, project_root: &Path, destination: &Path) -> Result<()> {
    let json = if is_theme_path(theme) {
        let theme_path = resolve_project_path(project_root, theme);
        let contents = fs::read_to_string(&theme_path)
            .with_context(|| format!("failed to read configured app theme {}", theme_path.display()))?;
        let mut value: serde_json::Value = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse configured app theme {}", theme_path.display()))?;
        set_theme_json_id(&mut value, APP_THEME_ID)?;
        ensure_app_theme_parent(&mut value, BASE_THEME_ID)?;
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
            normalize_theme_alias(&normalize_id(theme))
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
    forced_parent: Option<&str>,
) -> Result<()> {
    let mut value = if is_theme_path(theme) {
        let path = resolve_project_path(project_root, theme);
        if path.exists() {
            serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&path)?)
                .with_context(|| format!("failed to parse theme {}", path.display()))?
        } else {
            empty_app_theme_json(display_name, BASE_THEME_ID)
        }
    } else if find_theme_json(sdk, theme)?.is_some() {
        // Named themes are parents, not templates to flatten into the app.
        // Keeping the app JSON as a small child preserves inheritance and
        // makes the editor's Base Theme selector reflect the chosen theme.
        empty_app_theme_json(display_name, &normalize_theme_alias(&normalize_id(theme)))
    } else {
        empty_app_theme_json(display_name, &normalize_id(theme))
    };

    set_theme_json_id(&mut value, APP_THEME_ID)?;
    if let Some(parent) = forced_parent {
        value["parent"] = serde_json::Value::String(normalize_theme_alias(parent));
    } else {
        ensure_app_theme_parent(&mut value, BASE_THEME_ID)?;
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("name".to_string(), serde_json::Value::String(display_name.to_string()));
    }

    let json = serde_json::to_string_pretty(&value)?;
    write_if_changed(destination, format!("{json}\n").as_bytes())
}

/// Migrate an existing app-owned theme to the current inheritance contract
/// without changing its id, name, or overrides.
pub(crate) fn ensure_editable_app_theme_parent(path: &Path) -> Result<()> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read theme {}", path.display()))?;
    let mut value = serde_json::from_str::<serde_json::Value>(&contents)
        .with_context(|| format!("failed to parse theme {}", path.display()))?;
    ensure_app_theme_parent(&mut value, BASE_THEME_ID)?;
    let json = serde_json::to_string_pretty(&value)?;
    write_if_changed(path, format!("{json}\n").as_bytes())
}

fn empty_app_theme_json(display_name: &str, parent: &str) -> serde_json::Value {
    serde_json::json!({
        "id": APP_THEME_ID,
        "name": display_name,
        "parent": normalize_theme_alias(parent),
    })
}

fn find_theme_json(sdk: &SdkRoot, theme: &str) -> Result<Option<PathBuf>> {
    let normalized = normalize_theme_alias(&normalize_id(theme));
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

/// True when any schema-backed component theme output is missing or older
/// than either its schema or one of the project theme JSON inputs.
fn component_themes_need_regen(json_dir: &Path, plugin_dir: &Path, slint_dir: &Path) -> bool {
    let Ok(schema_entries) = fs::read_dir(plugin_dir) else {
        return true;
    };
    let theme_sources = fs::read_dir(json_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .collect::<Vec<_>>();

    for entry in schema_entries.flatten() {
        let schema_path = entry.path();
        let Some(file_name) = schema_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(key) = file_name.strip_suffix(".schema.json") else {
            continue;
        };
        let output_path = slint_dir.join(format!("{key}_theme.slint"));
        let Ok(output_mtime) = fs::metadata(&output_path).and_then(|metadata| metadata.modified()) else {
            return true;
        };
        if fs::metadata(&schema_path)
            .and_then(|metadata| metadata.modified())
            .map(|mtime| mtime > output_mtime)
            .unwrap_or(true)
        {
            return true;
        }
        if theme_sources.iter().any(|source| {
            fs::metadata(source)
                .and_then(|metadata| metadata.modified())
                .map(|mtime| mtime > output_mtime)
                .unwrap_or(true)
        }) {
            return true;
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
        base = normalize_theme_alias(&normalize_id(base)),
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
    let normalized = normalize_theme_alias(&normalize_id(base));
    [json_dir.to_path_buf(), sdk_themes_dir(sdk)].iter().any(|dir| {
        dir.join(format!("{base}.json")).exists() || dir.join(format!("{normalized}.json")).exists()
    })
}

fn normalize_legacy_theme_parent(value: &mut serde_json::Value) {
    if let Some(parent) = value
        .as_object_mut()
        .and_then(|object| object.get_mut("parent"))
        .and_then(|parent| parent.as_str())
        .map(normalize_theme_alias)
    {
        value["parent"] = serde_json::Value::String(parent);
    }
}

fn ensure_app_theme_parent(value: &mut serde_json::Value, fallback_parent: &str) -> Result<()> {
    let Some(object) = value.as_object_mut() else {
        anyhow::bail!("theme JSON must be an object");
    };
    let needs_parent = object
        .get("parent")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .map(str::is_empty)
        .unwrap_or(true);
    if needs_parent {
        object
            .insert("parent".to_string(), serde_json::Value::String(normalize_theme_alias(fallback_parent)));
    }
    normalize_legacy_theme_parent(value);
    Ok(())
}

fn normalize_theme_alias(id: &str) -> String {
    if id == LEGACY_DEFAULT_THEME_ID {
        BASE_THEME_ID.to_string()
    } else {
        id.to_string()
    }
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
                ids.push(normalize_theme_alias(&normalize_id(stem)));
            }
        }
    }
    ids.sort();
    ids.dedup();
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

#[cfg(test)]
mod tests {
    use super::{
        ensure_app_theme_parent, normalize_legacy_theme_parent, normalize_theme_alias, BASE_THEME_ID,
    };

    #[test]
    fn legacy_default_theme_parent_is_written_as_base_theme() {
        let mut value = serde_json::json!({
            "id": "app_theme",
            "name": "App Theme",
            "parent": "default_theme",
        });

        normalize_legacy_theme_parent(&mut value);

        assert_eq!(value["parent"].as_str(), Some(BASE_THEME_ID));
    }

    #[test]
    fn legacy_default_theme_id_is_a_base_theme_alias() {
        assert_eq!(normalize_theme_alias("default_theme"), BASE_THEME_ID);
        assert_eq!(normalize_theme_alias("base_theme"), BASE_THEME_ID);
    }

    #[test]
    fn parentless_app_theme_inherits_from_base_theme() {
        for parent in [serde_json::Value::Null, serde_json::Value::String(String::new())] {
            let mut value = serde_json::json!({
                "id": "app_theme",
                "name": "App Theme",
                "parent": parent,
            });

            ensure_app_theme_parent(&mut value, BASE_THEME_ID).unwrap();

            assert_eq!(value["parent"].as_str(), Some(BASE_THEME_ID));
        }

        let mut value = serde_json::json!({
            "id": "app_theme",
            "name": "App Theme",
        });
        ensure_app_theme_parent(&mut value, BASE_THEME_ID).unwrap();
        assert_eq!(value["parent"].as_str(), Some(BASE_THEME_ID));
    }

    #[test]
    fn explicit_app_theme_parent_is_preserved() {
        let mut value = serde_json::json!({
            "id": "app_theme",
            "name": "App Theme",
            "parent": "designer_theme",
        });

        ensure_app_theme_parent(&mut value, BASE_THEME_ID).unwrap();

        assert_eq!(value["parent"].as_str(), Some("designer_theme"));
    }
}
