// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use components::theme::{
    self, component_mut, set_style_prop, set_token, size_style_mut, state_style_mut, token_ref, variant_mut,
    Color, ColorScheme, ComponentTheme, StyleProps, StyleValue, Theme, ThemeValue, TokenSet, TokenValue,
    VariantTheme,
};
use components::theme_gen::{component_theme_slint, ComponentOverrides, PluginDefinition, PropDefinition};
use serde::Deserialize;
use serde_json::{Map, Value};

const BASE_THEME_ID: &str = "base_theme";
const LEGACY_DEFAULT_THEME_ID: &str = "default_theme";

/// JSON values owned by the Base Theme editor. A compiled theme must provide
/// all of them after resolving its Base Theme and override chain; the editor
/// may save an incomplete draft, but code generation must not substitute the
/// hard-coded schema/runtime fallbacks for these values.
const REQUIRED_BASE_TOKEN_PATHS: &[&str] = &[
    "colors.light.primary",
    "colors.light.primary-pressed",
    "colors.light.secondary",
    "colors.light.danger",
    "colors.light.surface",
    "colors.light.background",
    "colors.light.text",
    "colors.light.text-muted",
    "colors.light.transparent",
    "colors.dark.primary",
    "colors.dark.primary-pressed",
    "colors.dark.secondary",
    "colors.dark.danger",
    "colors.dark.surface",
    "colors.dark.background",
    "colors.dark.text",
    "colors.dark.text-muted",
    "colors.dark.transparent",
    "spacing.xs",
    "spacing.sm",
    "spacing.md",
    "spacing.lg",
    "spacing.xl",
    "controlSize.sm",
    "controlSize.md",
    "controlSize.lg",
    "choiceControlSize.sm",
    "choiceControlSize.md",
    "choiceControlSize.lg",
    "switchSize.sm",
    "switchSize.md",
    "switchSize.lg",
    "iconSize.sm",
    "iconSize.md",
    "iconSize.lg",
    "borderWidth.none",
    "borderWidth.sm",
    "borderWidth.focus",
    "controlRadius.sm",
    "controlRadius.md",
    "controlRadius.lg",
    "controlPaddingInline.sm",
    "controlPaddingInline.md",
    "controlPaddingInline.lg",
    "typography.font-size-xs",
    "typography.font-size-caption",
    "typography.font-size-sm",
    "typography.font-size-md",
    "typography.font-size-lg",
    "typography.font-size-helper",
    "typography.font-primary",
    "typography.font-secondary",
    "typography.font-tertiary",
    "fontWeight.normal",
    "fontWeight.medium",
    "fontWeight.semibold",
    "fontWeight.bold",
    "radius.sm",
    "radius.md",
    "radius.lg",
    "radius.full",
    "radius.default",
];

#[derive(Debug, Clone)]
pub struct AvailableTheme {
    pub id: String,
    pub display_name: String,
    pub parent: Option<String>,
    pub path: PathBuf,
    pub sort_order: i64,
}

#[derive(Debug, Clone)]
struct ThemeFile {
    metadata: AvailableTheme,
    patch: Theme,
}

#[derive(Debug, Deserialize)]
struct ThemeMetadataFile {
    id: Option<String>,
    name: Option<String>,
    parent: Option<String>,
    sort_order: Option<i64>,
}

pub fn builtin_theme_dir() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("themes") }

pub fn list_builtin_themes() -> Result<Vec<AvailableTheme>> { list_themes_in_dir(&builtin_theme_dir()) }

pub fn list_themes_in_dir(dir: &Path) -> Result<Vec<AvailableTheme>> {
    let mut themes = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read theme directory {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read theme file {}", path.display()))?;
        let metadata: ThemeMetadataFile = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse theme file {}", path.display()))?;
        let file_stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| "theme".to_string());
        let raw_id = metadata.id.unwrap_or_else(|| normalize_identifier(&file_stem));
        let id = canonical_theme_id(&raw_id);
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let display_name = if id == BASE_THEME_ID {
            "Base Theme".to_string()
        } else {
            metadata.name.unwrap_or_else(|| prettify_identifier(&file_stem))
        };
        themes.push(AvailableTheme {
            id,
            display_name,
            parent: metadata.parent,
            path,
            sort_order: metadata.sort_order.unwrap_or(i64::MAX),
        });
    }

    themes.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.display_name.to_ascii_lowercase().cmp(&b.display_name.to_ascii_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });

    if themes.is_empty() {
        bail!("no theme JSON files found in {}", dir.display());
    }

    Ok(themes)
}

pub fn compile_app_theme_json(
    theme_json_path: impl AsRef<Path>,
    output_rust_path: impl AsRef<Path>,
    function_name: &str,
) -> Result<()> {
    let theme_json_path = theme_json_path.as_ref();
    let output_rust_path = output_rust_path.as_ref();

    println!("cargo:rerun-if-changed={}", theme_json_path.display());
    println!("cargo:rerun-if-changed={}", builtin_theme_dir().display());

    validate_theme_json_for_compile(theme_json_path, None)?;

    let mut visited = Vec::new();
    let resolved = resolve_theme(theme_json_path, &mut visited)?;
    for path in visited {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let rust = emit_theme_module(&resolved, function_name);
    write_rust_module(output_rust_path, &rust)?;
    Ok(())
}

/// Resolve a theme JSON (walking its parent chain) and return the generated
/// Rust source as a string. Unlike [`compile_app_theme_json`] this emits no
/// `cargo:` directives, so it's safe to call from a normal CLI (not just a
/// build script).
pub fn render_theme_rust(theme_json_path: impl AsRef<Path>, function_name: &str) -> Result<String> {
    validate_theme_json_for_compile(theme_json_path.as_ref(), None)?;
    let mut visited = Vec::new();
    let resolved = resolve_theme(theme_json_path.as_ref(), &mut visited)?;
    Ok(emit_theme_module(&resolved, function_name))
}

/// Generate one `<id>.rs` per theme JSON in `json_dir` into `rust_dir`, each
/// exposing a `pub fn theme() -> ExportTheme`. Also writes a `mod.rs` index so
/// consumers can `include!` a single file to pull in every theme module.
///
/// Returns the list of theme ids written. Parent references resolve against
/// `json_dir` first, then the SDK's built-in theme directory, so user themes
/// can inherit from a bundled base.
pub fn compile_theme_dir(json_dir: impl AsRef<Path>, rust_dir: impl AsRef<Path>) -> Result<Vec<String>> {
    let json_dir = json_dir.as_ref();
    let rust_dir = rust_dir.as_ref();
    fs::create_dir_all(rust_dir)
        .with_context(|| format!("failed to create generated theme dir {}", rust_dir.display()))?;

    let themes = list_themes_in_dir(json_dir)?;
    let mut ids = Vec::new();
    for theme in &themes {
        let rust = render_theme_rust(&theme.path, "theme")
            .with_context(|| format!("failed to generate Rust for theme {}", theme.id))?;
        let out_path = rust_dir.join(format!("{}.rs", theme.id));
        write_rust_module(&out_path, &rust)?;
        ids.push(theme.id.clone());
    }

    // mod.rs index — `mod <id> { include!("<id>.rs"); }` for every theme.
    let mut index = String::from("// AUTOGENERATED by `foundation themes build` — do not edit.\n\n");
    for id in &ids {
        index.push_str(&format!("pub mod {id} {{\n    include!(\"{id}.rs\");\n}}\n"));
    }
    write_rust_module(&rust_dir.join("mod.rs"), &index)?;

    Ok(ids)
}

/// Strict directory compile used by the host compiler when component schemas
/// are available. Validation happens before any generated file is written, so
/// a draft with unresolved Base Theme values cannot leave a partly-updated
/// output directory.
pub fn compile_theme_dir_with_schemas(
    json_dir: impl AsRef<Path>,
    rust_dir: impl AsRef<Path>,
    plugin_dir: &Path,
) -> Result<Vec<String>> {
    let json_dir = json_dir.as_ref();
    let themes = list_themes_in_dir(json_dir)?;
    for theme in &themes {
        validate_theme_json_for_compile(&theme.path, Some(plugin_dir))
            .with_context(|| format!("theme {} is incomplete", theme.id))?;
    }
    compile_theme_dir(json_dir, rust_dir)
}

/// Per-app generated `<key>_theme.slint` files import the shared UI via the
/// `@ui` Slint library namespace (the files live in a different directory than
/// `theme.slint`, so a relative import wouldn't resolve).
const APP_THEME_IMPORT: &str = "@ui/theme.slint";

/// Emit per-app component theme `.slint` files into `slint_dir`.
///
/// For each key in `component_keys`, reads the component schema `plugin_dir/<key>.schema.json`
/// (the editable contract) and the resolved theme chain's `components.<key>`
/// object from `app_theme_json`, then writes `slint_dir/<key>_theme.slint`.
/// Missing schema files are skipped. Returns the keys actually written.
pub fn compile_app_component_themes(
    app_theme_json: &Path,
    plugin_dir: &Path,
    slint_dir: &Path,
    component_keys: &[&str],
) -> Result<Vec<String>> {
    validate_component_values_for_compile(app_theme_json, plugin_dir)?;
    fs::create_dir_all(slint_dir)
        .with_context(|| format!("failed to create generated slint theme dir {}", slint_dir.display()))?;

    let resolved_components = resolve_component_json_chain(app_theme_json)?;
    let plugins = load_resolved_component_plugins(plugin_dir)?;

    let discovered_keys;
    let component_keys: Vec<&str> = if component_keys.is_empty() {
        discovered_keys = component_schema_keys(plugin_dir)?;
        discovered_keys.iter().map(String::as_str).collect()
    } else {
        component_keys.to_vec()
    };

    let mut written = Vec::new();
    for key in component_keys {
        let schema_path = plugin_dir.join(format!("{key}.schema.json"));
        if !schema_path.exists() {
            // No schema for this component — nothing to generate.
            continue;
        }
        let plugin = plugins
            .get(key)
            .with_context(|| format!("failed to resolve plugin schema {}", schema_path.display()))?;

        let resolved_values = resolved_component_values(key, &plugins, &resolved_components)?;
        let overrides: ComponentOverrides = serde_json::from_value(resolved_values).with_context(|| {
            format!("failed to parse resolved '{key}' values for {}", app_theme_json.display())
        })?;

        let Some(text) = component_theme_slint(key, plugin, Some(&overrides), APP_THEME_IMPORT) else {
            continue;
        };
        let out_path = slint_dir.join(format!("{key}_theme.slint"));
        // Write only when changed so we don't force a Slint recompile each build.
        if fs::read_to_string(&out_path).ok().as_deref() != Some(text.as_str()) {
            fs::write(&out_path, &text)
                .with_context(|| format!("failed to write generated theme {}", out_path.display()))?;
        }
        written.push(key.to_string());
    }
    Ok(written)
}

/// Return the component keys represented by `*.schema.json` files in stable
/// filename order. An empty component list passed to
/// [`compile_app_component_themes`] uses this as the single source of truth.
pub fn component_schema_keys(plugin_dir: &Path) -> Result<Vec<String>> {
    let mut entries = fs::read_dir(plugin_dir)
        .with_context(|| format!("failed to read component schema directory {}", plugin_dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    Ok(entries
        .into_iter()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            name.strip_suffix(".schema.json").filter(|key| !key.is_empty()).map(str::to_string)
        })
        .collect())
}

fn load_resolved_component_plugins(plugin_dir: &Path) -> Result<HashMap<String, PluginDefinition>> {
    let mut raw_by_key = Vec::new();
    for key in component_schema_keys(plugin_dir)? {
        let path = plugin_dir.join(format!("{key}.schema.json"));
        let plugin: PluginDefinition = serde_json::from_str(
            &fs::read_to_string(&path)
                .with_context(|| format!("failed to read plugin schema {}", path.display()))?,
        )
        .with_context(|| format!("failed to parse plugin schema {}", path.display()))?;
        raw_by_key.push((key, plugin));
    }

    let by_component = raw_by_key
        .iter()
        .map(|(_, plugin)| (plugin.component.clone(), plugin.clone()))
        .collect::<HashMap<_, _>>();
    raw_by_key
        .into_iter()
        .map(|(key, plugin)| {
            let mut visited = HashSet::new();
            resolve_plugin_extends(plugin, &by_component, &mut visited).map(|plugin| (key, plugin))
        })
        .collect()
}

fn resolve_plugin_extends(
    plugin: PluginDefinition,
    by_component: &HashMap<String, PluginDefinition>,
    visited: &mut HashSet<String>,
) -> Result<PluginDefinition> {
    if !visited.insert(plugin.component.clone()) {
        bail!("component schema inheritance cycle detected at {:?}", plugin.component);
    }
    let Some(parent_name) = plugin.extends.clone() else {
        return Ok(plugin);
    };
    let parent = by_component.get(&parent_name).cloned().with_context(|| {
        format!("component schema {:?} extends unknown component {parent_name:?}", plugin.component)
    })?;
    let parent = resolve_plugin_extends(parent, by_component, visited)?;
    Ok(merge_plugin_with_parent(plugin, &parent))
}

fn merge_plugin_with_parent(child: PluginDefinition, parent: &PluginDefinition) -> PluginDefinition {
    fn take_list(child: Vec<String>, parent: &[String]) -> Vec<String> {
        if child.is_empty() {
            parent.to_vec()
        } else {
            child
        }
    }

    fn merge_props(child: Vec<PropDefinition>, parent: &[PropDefinition]) -> Vec<PropDefinition> {
        let mut merged = parent.to_vec();
        for child_prop in child {
            if let Some(index) = merged.iter().position(|prop| prop.name == child_prop.name) {
                merged[index] = child_prop;
            } else {
                merged.push(child_prop);
            }
        }
        merged
    }

    let parent_key = child.extends.clone().or_else(|| child.parent_key.clone());
    PluginDefinition {
        component: child.component,
        extends: None,
        parent_key,
        variants: take_list(child.variants, &parent.variants),
        states: take_list(child.states, &parent.states),
        sizes: take_list(child.sizes, &parent.sizes),
        variant_props: merge_props(child.variant_props, &parent.variant_props),
        size_props: merge_props(child.size_props, &parent.size_props),
    }
}

fn resolve_theme_json_chain(theme_path: &Path) -> Result<Value> {
    let mut stack = HashSet::new();
    resolve_theme_json_chain_inner(theme_path, &mut stack)
}

fn resolve_theme_json_chain_inner(theme_path: &Path, stack: &mut HashSet<PathBuf>) -> Result<Value> {
    let canonical = canonical_theme_path(theme_path)?;
    if !stack.insert(canonical.clone()) {
        bail!("theme inheritance cycle detected at {}", canonical.display());
    }

    let contents = fs::read_to_string(&canonical)
        .with_context(|| format!("failed to read theme file {}", canonical.display()))?;
    let value: Value = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse theme file {}", canonical.display()))?;

    let mut resolved = if let Some(parent) = value.get("parent").and_then(Value::as_str) {
        let parent_path = resolve_parent_reference(parent, &canonical).with_context(|| {
            format!("failed to resolve parent {:?} for theme {}", parent, canonical.display())
        })?;
        resolve_theme_json_chain_inner(&parent_path, stack)?
    } else {
        Value::Object(Map::new())
    };

    let resolved_object = resolved.as_object_mut().expect("resolved theme JSON chain is always an object");
    for section in ["tokens", "components"] {
        let Some(patch) = value.get(section).and_then(Value::as_object) else {
            continue;
        };
        let target = resolved_object
            .entry(section.to_string())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .with_context(|| format!("theme {section} must be an object in {}", canonical.display()))?;
        merge_json_object(target, patch);
    }

    stack.remove(&canonical);
    Ok(resolved)
}

fn resolve_component_json_chain(theme_path: &Path) -> Result<Map<String, Value>> {
    Ok(resolve_theme_json_chain(theme_path)?
        .get("components")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

fn json_path_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |current, segment| current.get(segment))
}

fn required_token_value_is_concrete(resolved: &Value, path: &str) -> bool {
    let Some(value) = json_path_value(resolved, &format!("tokens.{path}")) else {
        return false;
    };

    if path.starts_with("colors.") {
        return value.as_str().and_then(Color::from_hex).is_some();
    }
    if matches!(path, "typography.font-primary" | "typography.font-secondary" | "typography.font-tertiary") {
        return value.as_str().is_some_and(|text| !text.trim().is_empty());
    }
    if path == "radius.default" {
        let Some(reference) = value.as_str() else {
            return false;
        };
        let reference = reference.strip_prefix("tokens.").unwrap_or(reference);
        return json_path_value(resolved, &format!("tokens.{reference}")).and_then(Value::as_f64).is_some();
    }

    value.as_f64().is_some()
}

fn component_key_for_name<'a>(
    plugins: &'a HashMap<String, PluginDefinition>,
    component_name: &str,
) -> Option<&'a str> {
    plugins.iter().find_map(|(key, plugin)| (plugin.component == component_name).then_some(key.as_str()))
}

fn resolved_component_values(
    component_key: &str,
    plugins: &HashMap<String, PluginDefinition>,
    components: &Map<String, Value>,
) -> Result<Value> {
    fn resolve_inner(
        component_key: &str,
        plugins: &HashMap<String, PluginDefinition>,
        components: &Map<String, Value>,
        stack: &mut HashSet<String>,
    ) -> Result<Value> {
        if !stack.insert(component_key.to_string()) {
            bail!("component theme inheritance cycle detected at {component_key}");
        }
        let plugin = plugins
            .get(component_key)
            .with_context(|| format!("unknown component schema {component_key}"))?;
        let mut resolved = if let Some(parent_name) = plugin.parent_key.as_deref() {
            let parent_key = component_key_for_name(plugins, parent_name).with_context(|| {
                format!("component {component_key} extends unknown component {parent_name}")
            })?;
            resolve_inner(parent_key, plugins, components, stack)?
        } else {
            Value::Object(Map::new())
        };

        if let Some(patch) = components.get(component_key) {
            let patch =
                patch.as_object().with_context(|| format!("components.{component_key} must be an object"))?;
            merge_json_object(
                resolved.as_object_mut().expect("resolved component values are always an object"),
                patch,
            );
        }
        stack.remove(component_key);
        Ok(resolved)
    }

    resolve_inner(component_key, plugins, components, &mut HashSet::new())
}

fn component_variant_value<'a>(
    plugin: &'a PluginDefinition,
    overrides: &'a ComponentOverrides,
    variant: &str,
    state: &str,
    prop: &str,
) -> Option<&'a components::theme_gen::TokenOrValue> {
    if let Some(value) = overrides
        .variant_props
        .get(variant)
        .and_then(|states| states.get(state))
        .and_then(|props| props.get(prop))
    {
        return Some(value);
    }
    if state != "normal" {
        if let Some(value) = overrides
            .variant_props
            .get(variant)
            .and_then(|states| states.get("normal"))
            .and_then(|props| props.get(prop))
        {
            return Some(value);
        }
    }
    let common_variant = plugin.variants.first()?;
    (variant != common_variant)
        .then(|| {
            overrides
                .variant_props
                .get(common_variant)
                .and_then(|states| states.get("normal"))
                .and_then(|props| props.get(prop))
        })
        .flatten()
}

fn component_size_value<'a>(
    overrides: &'a ComponentOverrides,
    size: &str,
    prop: &str,
) -> Option<&'a components::theme_gen::TokenOrValue> {
    overrides
        .size_props
        .get(size)
        .and_then(|props| props.get(prop))
        .or_else(|| overrides.size_props.get("common").and_then(|props| props.get(prop)))
        .or_else(|| overrides.size_props.get("default").and_then(|props| props.get(prop)))
}

fn missing_component_values(
    component_key: &str,
    plugin: &PluginDefinition,
    overrides: &ComponentOverrides,
) -> Vec<String> {
    let variants: Vec<&str> = if plugin.variants.is_empty() {
        vec!["default"]
    } else {
        plugin.variants.iter().map(String::as_str).collect()
    };
    let states: Vec<&str> = if plugin.states.is_empty() {
        vec!["normal"]
    } else {
        plugin.states.iter().map(String::as_str).collect()
    };
    let sizes: Vec<&str> = if plugin.sizes.is_empty() {
        vec!["default"]
    } else {
        plugin.sizes.iter().map(String::as_str).collect()
    };

    let mut missing = Vec::new();
    for variant in variants {
        for state in &states {
            for prop in &plugin.variant_props {
                if component_variant_value(plugin, overrides, variant, state, &prop.name).is_none() {
                    missing.push(format!(
                        "component {component_key} variant {variant}/{state} property {}",
                        prop.name
                    ));
                }
            }
        }
    }
    for size in sizes {
        for prop in &plugin.size_props {
            if component_size_value(overrides, size, &prop.name).is_none() {
                missing.push(format!("component {component_key} size {size} property {}", prop.name));
            }
        }
    }
    missing
}

fn validate_component_values_for_compile(theme_path: &Path, plugin_dir: &Path) -> Result<()> {
    let resolved = resolve_theme_json_chain(theme_path)?;
    let components = resolved.get("components").and_then(Value::as_object).cloned().unwrap_or_default();
    let plugins = load_resolved_component_plugins(plugin_dir)?;
    let mut component_keys = plugins.keys().cloned().collect::<Vec<_>>();
    component_keys.sort();

    let mut missing = Vec::new();
    for component_key in component_keys {
        let plugin = plugins.get(&component_key).expect("component key came from plugin map");
        let values = resolved_component_values(&component_key, &plugins, &components)?;
        let overrides: ComponentOverrides = serde_json::from_value(values)
            .with_context(|| format!("failed to parse resolved values for component {component_key}"))?;
        missing.extend(missing_component_values(&component_key, plugin, &overrides));
    }

    if missing.is_empty() {
        return Ok(());
    }
    missing.sort();
    bail!(
        "theme has unresolved component values after applying Base Theme and overrides:\n  {}",
        missing.join("\n  ")
    )
}

fn missing_token_references(theme: &Theme) -> Vec<String> {
    fn inspect_style(
        missing: &mut Vec<String>,
        theme: &Theme,
        component: &str,
        location: &str,
        style: &StyleProps,
    ) {
        for (prop, reference) in &style.token_refs {
            for (scheme, tokens) in [("light", &theme.tokens.light), ("dark", &theme.tokens.dark)] {
                if tokens.resolve_reference(reference, None).is_none() {
                    missing.push(format!(
                        "component {component} {location} property {prop} references unresolved {scheme} token {reference}"
                    ));
                }
            }
        }
    }

    let mut missing = Vec::new();
    for (component_name, component) in &theme.components {
        inspect_style(&mut missing, theme, component_name, "base", &component.base);
        for (variant_name, variant) in &component.variants {
            inspect_style(
                &mut missing,
                theme,
                component_name,
                &format!("variant {variant_name}/normal"),
                &variant.default,
            );
            for (size_name, style) in &variant.sizes {
                inspect_style(
                    &mut missing,
                    theme,
                    component_name,
                    &format!("variant {variant_name} size {size_name}"),
                    style,
                );
            }
            for (state_name, style) in &variant.states {
                inspect_style(
                    &mut missing,
                    theme,
                    component_name,
                    &format!("variant {variant_name}/{state_name}"),
                    style,
                );
            }
        }
    }
    missing
}

fn validate_theme_json_for_compile(theme_path: &Path, plugin_dir: Option<&Path>) -> Result<()> {
    let resolved = resolve_theme_json_chain(theme_path)?;
    let mut missing = REQUIRED_BASE_TOKEN_PATHS
        .iter()
        .filter(|path| !required_token_value_is_concrete(&resolved, path))
        .map(|path| format!("token {path}"))
        .collect::<Vec<_>>();

    if let Some(plugin_dir) = plugin_dir {
        if let Err(error) = validate_component_values_for_compile(theme_path, plugin_dir) {
            missing.push(error.to_string());
        }
    }

    if missing.is_empty() {
        let mut visited = Vec::new();
        let theme = resolve_theme(theme_path, &mut visited)?;
        missing.extend(missing_token_references(&theme));
    }

    if missing.is_empty() {
        return Ok(());
    }
    missing.sort();
    bail!("theme has unresolved values after applying Base Theme and overrides:\n  {}", missing.join("\n  "))
}

fn merge_json_object(target: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (key, patch_value) in patch {
        match (target.get_mut(key), patch_value) {
            (Some(Value::Object(target_map)), Value::Object(patch_map)) => {
                merge_json_object(target_map, patch_map);
            }
            _ => {
                target.insert(key.clone(), patch_value.clone());
            }
        }
    }
}

fn write_rust_module(output_rust_path: &Path, rust: &str) -> Result<()> {
    if let Some(parent) = output_rust_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create generated theme dir {}", parent.display()))?;
    }
    fs::write(output_rust_path, rust)
        .with_context(|| format!("failed to write generated theme module {}", output_rust_path.display()))?;
    Ok(())
}

fn resolve_theme(path: &Path, visited: &mut Vec<PathBuf>) -> Result<Theme> {
    let mut stack = HashSet::new();
    resolve_theme_inner(path, visited, &mut stack)
}

fn resolve_theme_inner(
    path: &Path,
    visited: &mut Vec<PathBuf>,
    stack: &mut HashSet<PathBuf>,
) -> Result<Theme> {
    let canonical = canonical_theme_path(path)?;
    if !stack.insert(canonical.clone()) {
        bail!("theme inheritance cycle detected at {}", canonical.display());
    }

    let theme_file = load_theme_file(&canonical)?;
    visited.push(canonical.clone());

    let mut resolved = if let Some(parent) = &theme_file.metadata.parent {
        let parent_path = resolve_parent_reference(parent, &canonical).with_context(|| {
            format!("failed to resolve parent {:?} for theme {}", parent, canonical.display())
        })?;
        resolve_theme_inner(&parent_path, visited, stack)?
    } else {
        // Parentless files must stand on their own. In particular, Base Theme
        // cannot silently inherit a second schema fallback theme.
        let mut theme = Theme::new();
        seed_intrinsic_tokens(&mut theme);
        theme
    };

    let mut merged = theme_file.patch;
    merged.inherit_from(&resolved);
    resolved = merged;

    stack.remove(&canonical);
    Ok(resolved)
}

/// Tokens with platform-wide semantic meanings rather than authored Base
/// Theme values. Keeping these few constants explicit avoids smuggling in a
/// second complete theme while ensuring component token references resolve.
fn seed_intrinsic_tokens(theme: &mut Theme) {
    for (scheme, border, success, warning, info) in [
        (
            ColorScheme::Light,
            Color::rgb(213, 212, 213),
            Color::rgb(22, 163, 74),
            Color::rgb(217, 119, 6),
            Color::rgb(37, 99, 235),
        ),
        (
            ColorScheme::Dark,
            Color::rgb(69, 68, 68),
            Color::rgb(34, 197, 94),
            Color::rgb(245, 158, 11),
            Color::rgb(59, 130, 246),
        ),
    ] {
        set_token(theme, scheme, "color.border", ThemeValue::Color(border));
        set_token(theme, scheme, "color.success", ThemeValue::Color(success));
        set_token(theme, scheme, "color.warning", ThemeValue::Color(warning));
        set_token(theme, scheme, "color.info", ThemeValue::Color(info));
        set_token(theme, scheme, "color.white", ThemeValue::Color(Color::rgb(255, 255, 255)));
    }
}

fn load_theme_file(path: &Path) -> Result<ThemeFile> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read theme file {}", path.display()))?;
    let value: Value = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse theme file {}", path.display()))?;
    let metadata: ThemeMetadataFile = serde_json::from_value(value.clone())
        .with_context(|| format!("failed to parse theme metadata from {}", path.display()))?;

    let file_stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "theme".to_string());
    let available = AvailableTheme {
        id: metadata.id.unwrap_or_else(|| normalize_identifier(&file_stem)),
        display_name: metadata.name.unwrap_or_else(|| prettify_identifier(&file_stem)),
        parent: metadata.parent,
        path: path.to_path_buf(),
        sort_order: metadata.sort_order.unwrap_or(i64::MAX),
    };

    let patch = parse_theme_patch(&value)
        .with_context(|| format!("failed to build theme patch from {}", path.display()))?;

    Ok(ThemeFile { metadata: available, patch })
}

fn parse_theme_patch(value: &Value) -> Result<Theme> {
    let mut theme = Theme::new();

    if let Some(tokens) = value.get("tokens").and_then(Value::as_object) {
        apply_tokens_patch(&mut theme, tokens)?;
    }
    if let Some(components) = value.get("components").and_then(Value::as_object) {
        apply_components_patch(&mut theme, components)?;
    }

    Ok(theme)
}

fn apply_tokens_patch(theme: &mut Theme, tokens: &Map<String, Value>) -> Result<()> {
    if let Some(colors) = tokens.get("colors").and_then(Value::as_object) {
        if let Some(light) = colors.get("light").and_then(Value::as_object) {
            apply_color_scheme_patch(theme, ColorScheme::Light, light)?;
        }
        if let Some(dark) = colors.get("dark").and_then(Value::as_object) {
            apply_color_scheme_patch(theme, ColorScheme::Dark, dark)?;
        }
    }

    if let Some(spacing) = tokens.get("spacing").and_then(Value::as_object) {
        for key in ["xs", "sm", "md", "lg", "xl", "2xl"] {
            if let Some(number) = spacing.get(key).and_then(Value::as_f64) {
                set_shared_token(theme, &format!("spacing.{key}"), ThemeValue::Float(number as f32));
            }
        }
    }

    for (json_key, token_category) in [
        ("controlSize", "controlSize"),
        ("choiceControlSize", "choiceControlSize"),
        ("switchSize", "switchSize"),
        ("iconSize", "iconSize"),
        ("controlRadius", "controlRadius"),
        ("controlPaddingInline", "controlPaddingInline"),
    ] {
        if let Some(category) = tokens.get(json_key).and_then(Value::as_object) {
            for key in ["sm", "md", "lg"] {
                if let Some(number) = category.get(key).and_then(Value::as_f64) {
                    set_shared_token(
                        theme,
                        &format!("{token_category}.{key}"),
                        ThemeValue::Float(number as f32),
                    );
                }
            }
        }
    }

    if let Some(border_width) = tokens.get("borderWidth").and_then(Value::as_object) {
        for key in ["none", "sm", "focus"] {
            if let Some(number) = border_width.get(key).and_then(Value::as_f64) {
                set_shared_token(theme, &format!("borderWidth.{key}"), ThemeValue::Float(number as f32));
            }
        }
    }

    if let Some(font_weight) = tokens.get("fontWeight").and_then(Value::as_object) {
        for key in ["normal", "medium", "semibold", "bold"] {
            if let Some(number) = font_weight.get(key).and_then(Value::as_f64) {
                set_shared_token(theme, &format!("fontWeight.{key}"), ThemeValue::Float(number as f32));
            }
        }
    }

    if let Some(typography) = tokens.get("typography").and_then(Value::as_object) {
        for (json_key, token_key) in [
            ("font-size-xs", "fontSize.xs"),
            ("font-size-caption", "fontSize.caption"),
            ("font-size-sm", "fontSize.sm"),
            ("font-size-md", "fontSize.md"),
            ("font-size-lg", "fontSize.lg"),
            ("font-size-helper", "fontSize.helper"),
            ("font-size-xl", "fontSize.xl"),
            ("font-size-2xl", "fontSize.2xl"),
        ] {
            if let Some(number) = typography.get(json_key).and_then(Value::as_f64) {
                set_shared_token(theme, token_key, ThemeValue::Float(number as f32));
            }
        }

        for (json_key, token_key) in [
            ("font-primary", "font.primary"),
            ("font-secondary", "font.secondary"),
            ("font-tertiary", "font.tertiary"),
        ] {
            if let Some(text) = typography.get(json_key).and_then(Value::as_str) {
                set_shared_token(theme, token_key, ThemeValue::String(text.to_string()));
            }
        }
    }

    if let Some(radius) = tokens.get("radius").and_then(Value::as_object) {
        for key in ["none", "sm", "md", "lg", "xl", "full"] {
            if let Some(number) = radius.get(key).and_then(Value::as_f64) {
                set_shared_token(theme, &format!("radius.{key}"), ThemeValue::Float(number as f32));
            }
        }

        if let Some(default_ref) = radius.get("default").and_then(Value::as_str) {
            let normalized = if default_ref.contains('.') {
                default_ref.to_string()
            } else {
                format!("radius.{default_ref}")
            };
            set_shared_token(theme, "radius.default", ThemeValue::TokenRef(normalized));
        }
    }

    Ok(())
}

fn apply_color_scheme_patch(
    theme: &mut Theme,
    scheme: ColorScheme,
    colors: &Map<String, Value>,
) -> Result<()> {
    let primary = parse_optional_color(colors.get("primary"), "primary")?;
    let primary_pressed = parse_optional_color(colors.get("primary-pressed"), "primary-pressed")?;
    let secondary = parse_optional_color(colors.get("secondary"), "secondary")?;
    let danger = parse_optional_color(colors.get("danger"), "danger")?;
    let surface = parse_optional_color(colors.get("surface"), "surface")?;
    let background = parse_optional_color(colors.get("background"), "background")?;
    let text = parse_optional_color(colors.get("text"), "text")?;
    let text_muted = parse_optional_color(colors.get("text-muted"), "text-muted")?;
    let transparent = parse_optional_color(colors.get("transparent"), "transparent")?;

    if let Some(primary) = primary {
        set_token(theme, scheme, "color.primary", ThemeValue::Color(primary));
        set_token(theme, scheme, "color.primary.light", ThemeValue::Color(primary.lighten(0.2)));
    }
    if let Some(primary_pressed) = primary_pressed {
        set_token(theme, scheme, "color.primary.dark", ThemeValue::Color(primary_pressed));
        set_token(theme, scheme, "color.primary_pressed", ThemeValue::Color(primary_pressed));
    }
    if let Some(secondary) = secondary {
        set_token(theme, scheme, "color.secondary", ThemeValue::Color(secondary));
        set_token(theme, scheme, "color.secondary.dark", ThemeValue::Color(secondary.darken(0.1)));
    }
    if let Some(danger) = danger {
        set_token(theme, scheme, "color.danger", ThemeValue::Color(danger));
        set_token(theme, scheme, "color.danger.light", ThemeValue::Color(danger.lighten(0.15)));
        set_token(theme, scheme, "color.danger.dark", ThemeValue::Color(danger.darken(0.1)));
    }
    if let Some(surface) = surface {
        set_token(theme, scheme, "color.surface", ThemeValue::Color(surface));
    }
    if let Some(background) = background {
        set_token(theme, scheme, "color.background", ThemeValue::Color(background));
    }
    if let Some(text) = text {
        set_token(theme, scheme, "color.foreground", ThemeValue::Color(text));
        set_token(theme, scheme, "color.foreground.light", ThemeValue::Color(text.with_alpha(0.1)));
        set_token(theme, scheme, "color.text", ThemeValue::Color(text));
    }
    if let Some(text_muted) = text_muted {
        set_token(theme, scheme, "color.muted", ThemeValue::Color(text_muted));
        set_token(theme, scheme, "color.text_muted", ThemeValue::Color(text_muted));
    }
    if let Some(transparent) = transparent {
        set_token(theme, scheme, "color.transparent", ThemeValue::Color(transparent));
    }

    Ok(())
}

fn apply_components_patch(theme: &mut Theme, components: &Map<String, Value>) -> Result<()> {
    for (component_name, component_value) in components {
        if component_name == "button" {
            apply_button_component_patch(theme, component_value)
                .with_context(|| format!("failed to apply button theme patch"))?;
        } else {
            apply_generic_component_patch(theme, component_name, component_value)
                .with_context(|| format!("failed to apply component theme patch for {component_name}"))?;
        }
    }

    Ok(())
}

fn apply_button_component_patch(theme: &mut Theme, value: &Value) -> Result<()> {
    let Some(button) = value.as_object() else {
        bail!("button component patch must be a JSON object");
    };

    if let Some(variants) = button.get("variantProps").and_then(Value::as_object) {
        for (variant_name, variant_value) in variants {
            let Some(variant_states) = variant_value.as_object() else {
                bail!("button variantProps.{variant_name} must be an object");
            };
            let variant = variant_mut(component_mut(theme, "button"), variant_name);
            for (state_name, state_value) in variant_states {
                let style = if matches!(state_name.as_str(), "default" | "normal") {
                    &mut variant.default
                } else {
                    state_style_mut(variant, state_name)
                };
                apply_button_state_patch(style, state_value)
                    .with_context(|| format!("failed to apply button state {variant_name}.{state_name}"))?;
            }
        }
    }

    if let Some(sizes) = button.get("sizeProps").and_then(Value::as_object) {
        if let Some(common_size) = common_size_props(sizes) {
            let component = component_mut(theme, "button");
            for variant_name in ["primary", "secondary", "tertiary", "ghost", "danger"] {
                for size_name in ["sm", "md", "lg"] {
                    let style = size_style_mut(variant_mut(component, variant_name), size_name);
                    apply_button_size_patch(style, common_size).with_context(|| {
                        format!("failed to apply button common size patch {variant_name}.{size_name}")
                    })?;
                }
            }
        }
        let mut size_names: Vec<_> = sizes.keys().filter(|key| !is_common_size_key(key)).cloned().collect();
        size_names.sort();
        for size_name in size_names {
            let Some(size_value) = sizes.get(&size_name) else {
                continue;
            };
            let component = component_mut(theme, "button");
            for variant_name in ["primary", "secondary", "tertiary", "ghost", "danger"] {
                let style = size_style_mut(variant_mut(component, variant_name), &size_name);
                apply_button_size_patch(style, size_value).with_context(|| {
                    format!("failed to apply button size patch {variant_name}.{size_name}")
                })?;
            }
        }
    }

    Ok(())
}

fn apply_button_state_patch(style: &mut StyleProps, value: &Value) -> Result<()> {
    let Some(map) = value.as_object() else {
        bail!("button state patch must be an object");
    };

    apply_optional_token_or_color(style, "background", map.get("backgroundToken"), map.get("background"))?;
    apply_optional_token_or_color(style, "foreground", map.get("foregroundToken"), map.get("foreground"))?;
    apply_optional_token_or_color(style, "borderColor", map.get("borderColorToken"), map.get("borderColor"))?;

    apply_optional_number_or_token_prop(
        style,
        "borderWidth",
        map.get("borderWidthToken"),
        map.get("borderWidth"),
    )?;
    apply_optional_number_or_token_prop(
        style,
        "fontWeight",
        map.get("fontWeightToken"),
        map.get("fontWeight"),
    )?;
    apply_optional_number_or_token_prop(style, "opacity", map.get("opacityToken"), map.get("opacity"))?;
    apply_optional_number_or_token_prop(
        style,
        "touchExpansion",
        map.get("touchExpansionToken"),
        map.get("touchExpansion"),
    )?;

    Ok(())
}

fn apply_button_size_patch(style: &mut StyleProps, value: &Value) -> Result<()> {
    let Some(map) = value.as_object() else {
        bail!("button size patch must be an object");
    };

    apply_optional_token_or_string(style, "fontFamily", map.get("fontFamilyToken"), map.get("fontFamily"))?;
    apply_optional_number_or_token_prop(style, "fontSize", map.get("fontSizeToken"), map.get("fontSize"))?;
    apply_optional_number_or_token_prop(style, "paddingH", map.get("paddingHToken"), map.get("paddingH"))?;
    apply_optional_number_or_token_prop(style, "paddingV", map.get("paddingVToken"), map.get("paddingV"))?;
    apply_optional_number_or_token_prop(style, "iconSize", map.get("iconSizeToken"), map.get("iconSize"))?;
    apply_optional_number_or_token_prop(style, "minHeight", map.get("minHeightToken"), map.get("minHeight"))?;
    apply_optional_number_or_token_prop(
        style,
        "borderRadius",
        map.get("borderRadiusToken"),
        map.get("borderRadius"),
    )?;

    Ok(())
}

fn apply_generic_component_patch(theme: &mut Theme, component_name: &str, value: &Value) -> Result<()> {
    let Some(component) = value.as_object() else {
        bail!("component patch must be an object");
    };
    let mut variant_names = BTreeSet::new();
    if let Some(variants) = component.get("variantProps").and_then(Value::as_object) {
        variant_names.extend(variants.keys().cloned());
    }
    if variant_names.is_empty() {
        variant_names.insert("default".to_string());
    }

    if let Some(variants) = component.get("variantProps").and_then(Value::as_object) {
        for (variant_name, variant_value) in variants {
            let Some(states) = variant_value.as_object() else {
                bail!("component {component_name} variantProps.{variant_name} must be an object");
            };
            let variant = variant_mut(component_mut(theme, component_name), variant_name);
            for (state_name, props) in states {
                let Some(props) = props.as_object() else {
                    bail!("component {component_name} state {variant_name}.{state_name} must be an object");
                };
                let style = if matches!(state_name.as_str(), "default" | "normal") {
                    &mut variant.default
                } else {
                    state_style_mut(variant, state_name)
                };
                apply_property_map(style, props)?;
            }
        }
    }

    if let Some(size_props) = component.get("sizeProps").and_then(Value::as_object) {
        if let Some(default_props) = common_size_props(size_props).and_then(Value::as_object) {
            for variant_name in &variant_names {
                let style = size_style_mut(
                    variant_mut(component_mut(theme, component_name), variant_name),
                    "default",
                );
                apply_property_map(style, default_props)?;
            }
        }
        let mut size_names: Vec<_> =
            size_props.keys().filter(|key| !is_common_size_key(key)).cloned().collect();
        size_names.sort();
        for size_name in size_names {
            let Some(props) = size_props.get(&size_name).and_then(Value::as_object) else {
                continue;
            };
            for variant_name in &variant_names {
                let style = size_style_mut(
                    variant_mut(component_mut(theme, component_name), variant_name),
                    &size_name,
                );
                apply_property_map(style, props)?;
            }
        }
    }

    Ok(())
}

fn is_common_size_key(key: &str) -> bool { key == "common" || key == "default" }

fn common_size_props<'a>(sizes: &'a Map<String, Value>) -> Option<&'a Value> {
    sizes.get("common").or_else(|| sizes.get("default"))
}

fn apply_property_map(style: &mut StyleProps, props: &Map<String, Value>) -> Result<()> {
    for (prop_name, raw_value) in props {
        apply_json_prop(style, prop_name, raw_value)
            .with_context(|| format!("failed to apply property {prop_name}"))?;
    }
    Ok(())
}

fn apply_json_prop(style: &mut StyleProps, prop_name: &str, raw_value: &Value) -> Result<()> {
    let value = json_value_to_theme_value(raw_value)
        .with_context(|| format!("unsupported theme value for property {prop_name}"))?;
    set_style_prop(style, prop_name, value);
    Ok(())
}

fn json_value_to_theme_value(value: &Value) -> Result<ThemeValue> {
    match value {
        Value::String(text) if looks_like_token_ref(text) => Ok(ThemeValue::TokenRef(text.to_string())),
        Value::String(text) if text.starts_with('#') => Ok(ThemeValue::Color(parse_color(text)?)),
        Value::String(text) => Ok(ThemeValue::String(text.to_string())),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Ok(ThemeValue::Int(integer as i32))
            } else if let Some(float) = number.as_f64() {
                Ok(ThemeValue::Float(float as f32))
            } else {
                bail!("unsupported numeric literal {number}");
            }
        }
        Value::Bool(flag) => Ok(ThemeValue::Bool(*flag)),
        _ => bail!("expected string, number, or bool"),
    }
}

fn apply_optional_token_or_color(
    style: &mut StyleProps,
    prop_name: &str,
    token_value: Option<&Value>,
    literal_value: Option<&Value>,
) -> Result<()> {
    if let Some(token) = token_value.and_then(Value::as_str) {
        set_style_prop(style, prop_name, token_ref(token));
        return Ok(());
    }
    if let Some(literal) = literal_value.and_then(Value::as_str) {
        if looks_like_token_ref(literal) {
            set_style_prop(style, prop_name, token_ref(literal));
        } else {
            set_style_prop(style, prop_name, parse_color(literal)?);
        }
    }
    Ok(())
}

fn apply_optional_token_or_string(
    style: &mut StyleProps,
    prop_name: &str,
    token_value: Option<&Value>,
    literal_value: Option<&Value>,
) -> Result<()> {
    if let Some(token) = token_value.and_then(Value::as_str) {
        set_style_prop(style, prop_name, token_ref(token));
        return Ok(());
    }
    if let Some(literal) = literal_value.and_then(Value::as_str) {
        if looks_like_token_ref(literal) {
            set_style_prop(style, prop_name, token_ref(literal));
        } else {
            set_style_prop(style, prop_name, literal.to_string());
        }
    }
    Ok(())
}

fn apply_optional_number_prop(
    style: &mut StyleProps,
    prop_name: &str,
    literal_value: Option<&Value>,
) -> Result<()> {
    if let Some(literal) = literal_value {
        let number = literal
            .as_f64()
            .or_else(|| literal.as_i64().map(|value| value as f64))
            .with_context(|| format!("expected numeric value for {prop_name}"))?;
        set_style_prop(style, prop_name, number as f32);
    }
    Ok(())
}

fn apply_optional_number_or_token_prop(
    style: &mut StyleProps,
    prop_name: &str,
    token_value: Option<&Value>,
    literal_value: Option<&Value>,
) -> Result<()> {
    if let Some(token) = token_value.and_then(Value::as_str) {
        set_style_prop(style, prop_name, token_ref(token));
        return Ok(());
    }
    if let Some(token) = literal_value.and_then(Value::as_str).filter(|text| looks_like_token_ref(text)) {
        set_style_prop(style, prop_name, token_ref(token));
        return Ok(());
    }
    apply_optional_number_prop(style, prop_name, literal_value)
}

fn parse_optional_color(value: Option<&Value>, field_name: &str) -> Result<Option<Color>> {
    match value {
        Some(Value::String(text)) => {
            Ok(Some(parse_color(text).with_context(|| format!("invalid {field_name}"))?))
        }
        Some(_) => bail!("expected a color string for {field_name}"),
        None => Ok(None),
    }
}

fn parse_color(text: &str) -> Result<Color> {
    Color::from_hex(text).with_context(|| format!("invalid hex color {text:?}"))
}

fn resolve_parent_reference(parent: &str, current_path: &Path) -> Result<PathBuf> {
    let mut candidates = Vec::new();
    let builtin_dir = builtin_theme_dir();
    let current_dir = current_path.parent().unwrap_or_else(|| Path::new("."));

    if parent.ends_with(".json") || parent.contains('/') || parent.contains('\\') {
        let candidate =
            if Path::new(parent).is_absolute() { PathBuf::from(parent) } else { current_dir.join(parent) };
        candidates.push(candidate);
    }

    let normalized = normalize_identifier(parent);
    let canonical = canonical_theme_id(parent);
    for base_dir in [current_dir, builtin_dir.as_path()] {
        // `default_theme` is only a compatibility spelling for the one
        // canonical Base Theme. Do not let a stale default_theme.json shadow
        // base_theme.json when both are present in an old user cache.
        if normalized != LEGACY_DEFAULT_THEME_ID {
            candidates.push(base_dir.join(format!("{parent}.json")));
        }
        candidates.push(base_dir.join(format!("{canonical}.json")));
    }

    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    bail!("could not find a parent theme JSON for {parent:?}")
}

fn canonical_theme_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        path.canonicalize().with_context(|| format!("failed to canonicalize {}", path.display()))
    } else {
        bail!("theme file does not exist: {}", path.display())
    }
}

fn normalize_identifier(value: &str) -> String {
    let mut ident = String::new();
    let mut last_was_underscore = true;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            if ident.is_empty() && ch.is_ascii_digit() {
                ident.push('_');
            }
            ident.push(ch.to_ascii_lowercase());
            last_was_underscore = false;
        } else if !last_was_underscore {
            ident.push('_');
            last_was_underscore = true;
        }
    }
    while ident.ends_with('_') {
        ident.pop();
    }
    if ident.is_empty() {
        "theme".to_string()
    } else {
        ident
    }
}

fn canonical_theme_id(value: &str) -> String {
    let normalized = normalize_identifier(value);
    if normalized == LEGACY_DEFAULT_THEME_ID {
        BASE_THEME_ID.to_string()
    } else {
        normalized
    }
}

fn prettify_identifier(value: &str) -> String {
    normalize_identifier(value)
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let Some(first) = chars.next() else { return String::new() };
            format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn set_shared_token(theme: &mut Theme, path: &str, value: ThemeValue) {
    set_token(theme, ColorScheme::Light, path, value.clone());
    set_token(theme, ColorScheme::Dark, path, value);
}

fn emit_theme_module(theme: &Theme, function_name: &str) -> String {
    let mut out = String::new();
    out.push_str("// Generated from theme JSON at build time.\n\n");
    out.push_str("use foundation_themes::{color, define_theme, token_ref};\n\n");
    out.push_str("define_theme! {\n");
    push_line(&mut out, 4, format!("pub fn {function_name}() -> foundation_themes::ExportTheme {{"));
    emit_tokens(&mut out, 8, &theme.tokens);
    emit_components(&mut out, 8, &theme.components);
    push_line(&mut out, 4, "}");
    out.push_str("}\n");
    out
}

fn emit_tokens(out: &mut String, indent: usize, tokens: &theme::ThemeTokens) {
    push_line(out, indent, "tokens {");
    emit_token_set(out, indent + 4, "light", &tokens.light);
    emit_token_set(out, indent + 4, "dark", &tokens.dark);
    push_line(out, indent, "}");
}

fn emit_token_set(out: &mut String, indent: usize, scheme_name: &str, token_set: &TokenSet) {
    push_line(out, indent, format!("{scheme_name} {{"));

    let mut entries = Vec::new();
    for (category, keys) in token_set.categories() {
        for (key, value) in keys {
            entries.push((format!("{category}.{key}"), token_literal(value)));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (path, literal) in entries {
        push_line(out, indent + 4, format!("\"{path}\" = {};", literal.to_macro_expr()));
    }

    push_line(out, indent, "}");
}

fn emit_components(out: &mut String, indent: usize, components: &HashMap<String, ComponentTheme>) {
    let mut names: Vec<_> = components.keys().cloned().collect();
    names.sort();

    for name in names {
        let Some(component) = components.get(&name) else { continue };
        emit_component_block(out, indent, &name, component);
    }
}

fn emit_component_block(out: &mut String, indent: usize, component_name: &str, component: &ComponentTheme) {
    push_line(out, indent, format!("component {} {{", rust_string_literal(component_name)));
    emit_style_block(out, indent + 4, "base", &component.base);

    let mut variant_names: Vec<_> = component.variants.keys().cloned().collect();
    variant_names.sort();
    for variant_name in variant_names {
        if let Some(variant) = component.variants.get(&variant_name) {
            emit_variant_block(out, indent + 4, &variant_name, variant);
        }
    }

    push_line(out, indent, "}");
}

fn emit_variant_block(out: &mut String, indent: usize, variant_name: &str, variant: &VariantTheme) {
    push_line(out, indent, format!("variant {} {{", rust_string_literal(variant_name)));
    emit_style_block(out, indent + 4, "default", &variant.default);

    let mut size_names: Vec<_> = variant.sizes.keys().cloned().collect();
    size_names.sort();
    for size_name in size_names {
        if let Some(style) = variant.sizes.get(&size_name) {
            emit_style_block(out, indent + 4, &format!("size {}", rust_string_literal(&size_name)), style);
        }
    }

    let mut state_names: Vec<_> = variant.states.keys().cloned().collect();
    state_names.sort();
    for state_name in state_names {
        if let Some(style) = variant.states.get(&state_name) {
            emit_style_block(out, indent + 4, &format!("state {}", rust_string_literal(&state_name)), style);
        }
    }

    push_line(out, indent, "}");
}

fn emit_style_block(out: &mut String, indent: usize, header: &str, style: &StyleProps) {
    push_line(out, indent, format!("{header} {{"));
    for line in style_prop_lines(style) {
        push_line(out, indent + 4, line);
    }
    push_line(out, indent, "}");
}

fn style_prop_lines(style: &StyleProps) -> Vec<String> {
    let mut lines = Vec::new();
    for key in [
        "background",
        "foreground",
        "border_color",
        "border_width",
        "border_radius",
        "padding_horizontal",
        "padding_vertical",
        "min_height",
        "min_width",
        "font_family",
        "font_size",
        "font_weight",
        "icon_size",
        "opacity",
        "touch_expansion",
    ] {
        if let Some(value) = style_prop_literal(style, key) {
            lines.push(format!("\"{key}\" = {};", value.to_macro_expr()));
        }
    }

    let mut extra_keys: BTreeSet<String> = style.extra.keys().cloned().collect();
    extra_keys.extend(style.token_refs.keys().filter(|key| !is_standard_prop(key)).cloned());
    for key in extra_keys {
        if let Some(value) = style_prop_literal(style, &key) {
            lines.push(format!("\"{key}\" = {};", value.to_macro_expr()));
        }
    }

    lines
}

fn style_prop_literal(style: &StyleProps, key: &str) -> Option<ExportLiteral> {
    if let Some(path) = style.token_refs.get(key) {
        return Some(ExportLiteral::TokenRef(path.clone()));
    }

    match key {
        "background" => style.background.map(ExportLiteral::Color),
        "foreground" => style.foreground.map(ExportLiteral::Color),
        "border_color" => style.border_color.map(ExportLiteral::Color),
        "border_width" => style.border_width.map(ExportLiteral::Float),
        "border_radius" => style.border_radius.map(ExportLiteral::Float),
        "padding_horizontal" => style.padding_horizontal.map(ExportLiteral::Float),
        "padding_vertical" => style.padding_vertical.map(ExportLiteral::Float),
        "min_height" => style.min_height.map(ExportLiteral::Float),
        "min_width" => style.min_width.map(ExportLiteral::Float),
        "font_family" => style.font_family.clone().map(ExportLiteral::String),
        "font_size" => style.font_size.map(ExportLiteral::Float),
        "font_weight" => style.font_weight.map(ExportLiteral::Int),
        "icon_size" => style.icon_size.map(ExportLiteral::Float),
        "opacity" => style.opacity.map(ExportLiteral::Float),
        "touch_expansion" => style.touch_expansion.map(ExportLiteral::Float),
        key => style.extra.get(key).map(style_value_literal),
    }
}

fn token_literal(value: &TokenValue) -> ExportLiteral {
    match value {
        TokenValue::Color(color) => ExportLiteral::Color(*color),
        TokenValue::Float(number) => ExportLiteral::Float(*number),
        TokenValue::Int(number) => ExportLiteral::Int(*number),
        TokenValue::Bool(flag) => ExportLiteral::Bool(*flag),
        TokenValue::String(text) => ExportLiteral::String(text.clone()),
        TokenValue::Ref(path) => ExportLiteral::TokenRef(path.clone()),
    }
}

fn style_value_literal(value: &StyleValue) -> ExportLiteral {
    match value {
        StyleValue::Color(color) => ExportLiteral::Color(*color),
        StyleValue::Float(number) => ExportLiteral::Float(*number),
        StyleValue::Int(number) => ExportLiteral::Int(*number),
        StyleValue::Bool(flag) => ExportLiteral::Bool(*flag),
        StyleValue::String(text) => ExportLiteral::String(text.clone()),
    }
}

fn is_standard_prop(prop_name: &str) -> bool {
    matches!(
        prop_name,
        "background"
            | "foreground"
            | "border_color"
            | "border_width"
            | "border_radius"
            | "padding_horizontal"
            | "padding_vertical"
            | "min_height"
            | "min_width"
            | "font_family"
            | "font_size"
            | "font_weight"
            | "icon_size"
            | "opacity"
            | "touch_expansion"
    )
}

#[derive(Clone, PartialEq)]
enum ExportLiteral {
    Color(Color),
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
    TokenRef(String),
}

impl ExportLiteral {
    fn to_macro_expr(&self) -> String {
        match self {
            ExportLiteral::Color(color) => format!("color({})", rust_string_literal(&color_literal(*color))),
            ExportLiteral::Float(number) => format_rust_float(*number),
            ExportLiteral::Int(number) => number.to_string(),
            ExportLiteral::Bool(flag) => flag.to_string(),
            ExportLiteral::String(text) => rust_string_literal(text),
            ExportLiteral::TokenRef(path) => format!("token_ref({})", rust_string_literal(path)),
        }
    }
}

fn color_literal(color: Color) -> String {
    if color.a == 255 {
        format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", color.r, color.g, color.b, color.a)
    }
}

fn format_rust_float(value: f32) -> String {
    if value.fract().abs() < f32::EPSILON {
        return format!("{value:.1}");
    }
    let mut text = format!("{value:.4}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.push('0');
    }
    text
}

fn rust_string_literal(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn push_line(out: &mut String, indent: usize, line: impl AsRef<str>) {
    out.push_str(&" ".repeat(indent));
    out.push_str(line.as_ref());
    out.push('\n');
}

fn looks_like_token_ref(text: &str) -> bool { text.contains('.') && !text.starts_with('#') }

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::Value;

    use super::{compile_app_component_themes, compile_app_theme_json, list_themes_in_dir};

    #[test]
    fn list_themes_respects_sort_order() {
        let themes = list_themes_in_dir(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("themes"))
            .expect("built-in themes should load");

        let names = themes.into_iter().map(|theme| theme.display_name).collect::<Vec<_>>();
        assert_eq!(names, vec!["Base Theme".to_string()]);
    }

    #[test]
    fn compile_app_theme_json_resolves_builtin_parent() {
        let temp_root_dir = tempfile::tempdir().unwrap();
        let temp_root = temp_root_dir.path();

        let theme_json = temp_root.join("theme.json");
        let output_rs = temp_root.join("app_theme.rs");
        std::fs::write(
            &theme_json,
            r#"{
  "id": "app_theme",
  "name": "App Theme",
  "parent": "base_theme",
  "tokens": {
    "typography": {
      "font-primary": "Avenir"
    }
  }
}"#,
        )
        .unwrap();

        compile_app_theme_json(&theme_json, &output_rs, "app_theme").unwrap();

        let generated = std::fs::read_to_string(&output_rs).unwrap();
        assert!(generated.contains("pub fn app_theme() -> foundation_themes::ExportTheme"));
        assert!(generated.contains("\"font.primary\" = \"Avenir\";"));
        assert!(generated.contains("\"color.primary\" = color("));
    }

    #[test]
    fn parentless_incomplete_base_theme_is_rejected() {
        let temp_root = std::env::temp_dir().join(format!(
            "foundation-base-theme-empty-test-{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&temp_root).unwrap();

        let theme_json = temp_root.join("base_theme.json");
        let output_rs = temp_root.join("base_theme.rs");
        std::fs::write(
            &theme_json,
            r##"{
  "id": "base_theme",
  "name": "Base Theme",
  "tokens": {
    "colors": {
      "light": {
        "primary": "#123456",
        "transparent": "#00000000"
      }
    }
  }
}"##,
        )
        .unwrap();

        let error = compile_app_theme_json(&theme_json, &output_rs, "base_theme")
            .expect_err("an incomplete Base Theme draft must not compile");
        assert!(error.to_string().contains("token colors.dark.transparent"));
        assert!(error.to_string().contains("token typography.font-primary"));
        assert!(!output_rs.exists(), "validation should happen before code generation");

        std::fs::remove_dir_all(&temp_root).unwrap();
    }

    #[test]
    fn null_required_base_value_is_not_treated_as_initialized() {
        let temp_root_dir = tempfile::tempdir().unwrap();
        let theme_json = temp_root_dir.path().join("base_theme.json");
        let output_rs = temp_root_dir.path().join("base_theme.rs");
        let mut base: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/themes/base_theme.json"
        )))
        .unwrap();
        *base.pointer_mut("/tokens/spacing/md").unwrap() = Value::Null;
        std::fs::write(&theme_json, serde_json::to_vec_pretty(&base).unwrap()).unwrap();

        let error = compile_app_theme_json(&theme_json, &output_rs, "base_theme")
            .expect_err("null is not an actual Base Theme value");
        assert!(error.to_string().contains("token spacing.md"));
        assert!(!output_rs.exists());
    }

    #[test]
    fn unresolved_component_token_reference_is_rejected() {
        let temp_root_dir = tempfile::tempdir().unwrap();
        let theme_json = temp_root_dir.path().join("app_theme.json");
        let output_rs = temp_root_dir.path().join("app_theme.rs");
        std::fs::write(
            &theme_json,
            r#"{
  "id": "app_theme",
  "name": "App Theme",
  "parent": "base_theme",
  "components": {
    "button": {
      "variantProps": {
        "primary": {
          "normal": {
            "background": "color.does-not-exist"
          }
        }
      }
    }
  }
}"#,
        )
        .unwrap();

        let error = compile_app_theme_json(&theme_json, &output_rs, "app_theme")
            .expect_err("unresolved component tokens must not compile");
        assert!(error.to_string().contains("color.does-not-exist"));
        assert!(!output_rs.exists());
    }

    #[test]
    fn shared_font_tokens_are_parsed_from_theme_json() {
        let temp_root = std::env::temp_dir().join(format!(
            "foundation-shared-font-token-test-{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&temp_root).unwrap();

        let theme_json = temp_root.join("app_theme.json");
        let output_rs = temp_root.join("app_theme.rs");
        std::fs::write(
            &theme_json,
            r#"{
  "id": "app_theme",
  "name": "App Theme",
  "parent": "base_theme",
  "tokens": {
    "fontWeight": {
      "normal": 410,
      "medium": 520,
      "semibold": 630,
      "bold": 740
    },
    "typography": {
      "font-size-helper": 17
    }
  }
}"#,
        )
        .unwrap();

        compile_app_theme_json(&theme_json, &output_rs, "app_theme").unwrap();

        let generated = std::fs::read_to_string(&output_rs).unwrap();
        assert!(generated.contains("\"fontSize.helper\" = 17.0;"));
        assert!(generated.contains("\"fontWeight.normal\" = 410.0;"));
        assert!(generated.contains("\"fontWeight.medium\" = 520.0;"));
        assert!(generated.contains("\"fontWeight.semibold\" = 630.0;"));
        assert!(generated.contains("\"fontWeight.bold\" = 740.0;"));

        std::fs::remove_dir_all(&temp_root).unwrap();
    }

    #[test]
    fn legacy_default_parent_alias_resolves_to_builtin_base_theme() {
        let temp_root = std::env::temp_dir().join(format!(
            "foundation-legacy-parent-test-{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&temp_root).unwrap();

        let theme_json = temp_root.join("theme.json");
        let output_rs = temp_root.join("theme.rs");
        // A stale legacy cache entry must not become a second Base Theme or
        // shadow the canonical built-in file.
        std::fs::write(temp_root.join("default_theme.json"), "{ not valid json").unwrap();
        std::fs::write(
            &theme_json,
            r#"{
  "id": "legacy_child",
  "name": "Legacy Child",
  "parent": "default_theme"
}"#,
        )
        .unwrap();

        compile_app_theme_json(&theme_json, &output_rs, "legacy_child").unwrap();
        let generated = std::fs::read_to_string(&output_rs).unwrap();
        assert!(generated.contains("\"color.primary\" = color("));
        assert!(generated.contains("\"color.transparent\" = color(\"#00000000\")"));

        std::fs::remove_dir_all(&temp_root).unwrap();
    }

    #[test]
    fn legacy_and_canonical_base_files_list_as_one_base_theme() {
        let temp_root_dir = tempfile::tempdir().unwrap();
        let temp_root = temp_root_dir.path();
        std::fs::write(temp_root.join("base_theme.json"), r#"{"id":"base_theme","name":"Base Theme"}"#)
            .unwrap();
        std::fs::write(
            temp_root.join("default_theme.json"),
            r#"{"id":"default_theme","name":"Default Theme"}"#,
        )
        .unwrap();

        let themes = list_themes_in_dir(temp_root).unwrap();
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].id, "base_theme");
        assert_eq!(themes[0].display_name, "Base Theme");
        assert_eq!(themes[0].path, temp_root.join("base_theme.json"));
    }

    #[test]
    fn empty_component_list_discovers_schemas_and_resolves_extends() {
        let temp_root = std::env::temp_dir().join(format!(
            "foundation-component-discovery-test-{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        let plugin_dir = temp_root.join("plugins");
        let slint_dir = temp_root.join("slint");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        std::fs::write(
            plugin_dir.join("input.schema.json"),
            r##"{
  "component": "Input",
  "variants": ["default"],
  "states": ["normal", "focused"],
  "sizes": ["sm", "md", "lg"],
  "variantProps": [{
    "name": "border-color",
    "type": "color",
    "defaults": {"default": {"normal": "color.border", "focused": "color.primary"}}
  }],
  "sizeProps": [{
    "name": "field-height",
    "type": "float",
    "defaults": {"sm": 40, "md": 48, "lg": 56}
  }]
}"##,
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("dropdown.schema.json"),
            r#"{
  "component": "Dropdown",
  "extends": "Input",
  "sizeProps": [{
    "name": "option-height",
    "type": "float",
    "defaults": {"sm": 48, "md": 56, "lg": 64}
  }]
}"#,
        )
        .unwrap();
        let theme_json = temp_root.join("app_theme.json");
        std::fs::write(
            &theme_json,
            r##"{
  "id": "app_theme",
  "name": "App Theme",
  "components": {
    "input": {
      "variantProps": {
        "default": {
          "normal": { "border-color": "#111111" }
        }
      },
      "sizeProps": {
        "common": { "field-height": 48 }
      }
    },
    "dropdown": {
      "sizeProps": {
        "common": { "option-height": 56 }
      }
    }
  }
}"##,
        )
        .unwrap();

        let written = compile_app_component_themes(&theme_json, &plugin_dir, &slint_dir, &[]).unwrap();
        assert_eq!(written, vec!["dropdown", "input"]);
        let dropdown = std::fs::read_to_string(slint_dir.join("dropdown_theme.slint")).unwrap();
        assert!(dropdown.contains("export enum DropdownState"));
        assert!(dropdown.contains("border-color: #111111"));
        assert!(dropdown.contains("option-height: 56px"));

        std::fs::remove_dir_all(&temp_root).unwrap();
    }

    #[test]
    fn component_theme_generation_resolves_base_component_values() {
        let temp_root = std::env::temp_dir().join(format!(
            "foundation-component-theme-test-{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        let json_dir = temp_root.join("themes");
        let plugin_dir = temp_root.join("plugins");
        let slint_dir = temp_root.join("slint");
        std::fs::create_dir_all(&json_dir).unwrap();
        std::fs::create_dir_all(&plugin_dir).unwrap();

        std::fs::write(
            plugin_dir.join("button.schema.json"),
            r##"{
  "component": "Button",
  "variants": ["primary"],
  "states": ["normal", "pressed"],
  "sizes": [],
  "variantProps": [
    {
      "name": "background",
      "type": "color",
      "defaults": {
        "primary": {
          "normal": "color.primary",
          "pressed": "color.primary.dark"
        }
      }
    }
  ],
  "sizeProps": []
}"##,
        )
        .unwrap();

        std::fs::write(
            json_dir.join("base_theme.json"),
            r##"{
  "id": "base_theme",
  "name": "Base Theme",
  "components": {
    "button": {
      "variantProps": {
        "primary": {
          "normal": {
            "background": "#111111"
          }
        }
      }
    }
  }
}"##,
        )
        .unwrap();
        let app_theme_json = json_dir.join("app_theme.json");
        std::fs::write(
            &app_theme_json,
            r##"{
  "id": "app_theme",
  "name": "App Theme",
  "parent": "base_theme",
  "components": {
    "button": {
      "variantProps": {
        "primary": {
          "pressed": {
            "background": "#222222"
          }
        }
      }
    }
  }
}"##,
        )
        .unwrap();

        compile_app_component_themes(&app_theme_json, &plugin_dir, &slint_dir, &["button"]).unwrap();

        let generated = std::fs::read_to_string(slint_dir.join("button_theme.slint")).unwrap();
        assert!(
            generated.contains("background: #111111,"),
            "normal should come from Base theme:\n{generated}"
        );
        assert!(
            generated.contains("background: #222222,"),
            "pressed should come from app override:\n{generated}"
        );
        assert!(
            !generated.contains("Theme.color-primary-dark"),
            "resolved app/base component values should win over schema defaults:\n{generated}"
        );

        std::fs::remove_dir_all(&temp_root).unwrap();
    }
}
