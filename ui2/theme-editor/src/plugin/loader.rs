// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugin loader - discovers and loads plugins from the filesystem.
//!
//! Plugins are loaded from `~/.foundation/theme-editor/plugins/` as JSON files.
//! If the directory doesn't exist, it's created with default plugins.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::{DefaultValue, PluginDefinition, PropDefaults, PropDefinition, TokenOrValue};

#[derive(Clone, Copy)]
pub struct BuiltinComponentSpec {
    pub key: &'static str,
    pub component: &'static str,
    pub source_file: &'static str,
}

const BUILTIN_COMPONENTS: &[BuiltinComponentSpec] = &[
    BuiltinComponentSpec { key: "button", component: "Button", source_file: "button.slint" },
    BuiltinComponentSpec { key: "icon_button", component: "IconButton", source_file: "icon_button.slint" },
    BuiltinComponentSpec { key: "icon", component: "Icon", source_file: "icon.slint" },
    BuiltinComponentSpec { key: "chip", component: "Chip", source_file: "chip.slint" },
    BuiltinComponentSpec { key: "input", component: "Input", source_file: "input.slint" },
    BuiltinComponentSpec { key: "search", component: "Search", source_file: "search.slint" },
    BuiltinComponentSpec { key: "dropdown", component: "Dropdown", source_file: "dropdown.slint" },
    BuiltinComponentSpec { key: "checkbox", component: "Checkbox", source_file: "checkbox.slint" },
    BuiltinComponentSpec { key: "radio", component: "Radio", source_file: "radio.slint" },
    BuiltinComponentSpec { key: "radio_group", component: "RadioGroup", source_file: "radio_group.slint" },
    BuiltinComponentSpec { key: "switch", component: "Switch", source_file: "switch.slint" },
    BuiltinComponentSpec { key: "tabs", component: "Tabs", source_file: "tabs.slint" },
    BuiltinComponentSpec {
        key: "slide_to_confirm",
        component: "SlideToConfirm",
        source_file: "slide_to_confirm.slint",
    },
    BuiltinComponentSpec { key: "link", component: "Link", source_file: "link.slint" },
    BuiltinComponentSpec { key: "slider", component: "Slider", source_file: "slider.slint" },
    BuiltinComponentSpec { key: "textarea", component: "TextArea", source_file: "textarea.slint" },
    BuiltinComponentSpec { key: "card", component: "Card", source_file: "card.slint" },
    BuiltinComponentSpec { key: "menu", component: "Menu", source_file: "menu.slint" },
    BuiltinComponentSpec { key: "menu_item", component: "MenuItem", source_file: "menu_item.slint" },
    BuiltinComponentSpec { key: "accordion", component: "Accordion", source_file: "accordion.slint" },
    BuiltinComponentSpec { key: "image", component: "Image", source_file: "image_view.slint" },
    BuiltinComponentSpec { key: "divider", component: "Divider", source_file: "divider.slint" },
    BuiltinComponentSpec { key: "dialog", component: "Dialog", source_file: "dialog.slint" },
    BuiltinComponentSpec { key: "sheet", component: "Sheet", source_file: "sheet.slint" },
    BuiltinComponentSpec { key: "toast", component: "Toast", source_file: "toast.slint" },
    BuiltinComponentSpec { key: "progress_bar", component: "ProgressBar", source_file: "progress_bar.slint" },
    BuiltinComponentSpec { key: "spinner", component: "Spinner", source_file: "spinner.slint" },
];

#[derive(Debug, Clone)]
struct ParsedProperty {
    type_name: String,
    name: String,
}

#[derive(Debug, Clone, Default)]
struct ParsedComponentFile {
    enums: std::collections::HashMap<String, Vec<String>>,
    properties: Vec<ParsedProperty>,
    imports: Vec<(Vec<String>, String)>,
    content: String,
}

pub fn builtin_component_specs() -> &'static [BuiltinComponentSpec] { BUILTIN_COMPONENTS }

// Pulled in by build.rs from defaults/plugins/*.json. Used by the parity test
// below so a new plugin JSON dropped into defaults/plugins/ without a matching
// BUILTIN_COMPONENTS entry fails the build instead of being silently ignored.
include!(concat!(env!("OUT_DIR"), "/components_generated.rs"));

#[cfg(test)]
mod component_parity {
    use super::{BUILTIN_COMPONENTS, GENERATED_BUILTIN_COMPONENTS};

    /// Catches drift between the hand-written BUILTIN_COMPONENTS list and the
    /// set of plugin JSON files actually present on disk.
    #[test]
    fn every_plugin_json_has_a_builtin_component_entry() {
        let hand_keys: std::collections::BTreeSet<&str> =
            BUILTIN_COMPONENTS.iter().map(|spec| spec.key).collect();
        let json_keys: std::collections::BTreeSet<&str> =
            GENERATED_BUILTIN_COMPONENTS.iter().map(|(k, _, _)| *k).collect();

        let only_in_json: Vec<_> = json_keys.difference(&hand_keys).collect();
        let only_in_hand: Vec<_> = hand_keys.difference(&json_keys).collect();

        assert!(
            only_in_json.is_empty() && only_in_hand.is_empty(),
            "BUILTIN_COMPONENTS vs defaults/plugins/*.json drift:\n  \
             keys with a plugin JSON but no BUILTIN_COMPONENTS entry: {only_in_json:?}\n  \
             keys in BUILTIN_COMPONENTS but no plugin JSON: {only_in_hand:?}"
        );
    }
}

/// Get the base path for theme editor data
pub fn get_theme_editor_path() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".foundation").join("theme-editor")
}

/// Get the plugins directory path
pub fn get_plugins_path() -> PathBuf { get_theme_editor_path().join("plugins") }

/// Get the tokens file path
pub fn get_tokens_path() -> PathBuf { get_theme_editor_path().join("tokens.json") }

fn repo_default_plugins_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("defaults/plugins")
}

fn repo_default_plugin_path(name: &str) -> PathBuf {
    repo_default_plugins_path().join(format!("{name}.json"))
}

fn default_plugin_json(name: &str) -> Option<String> {
    fs::read_to_string(repo_default_plugin_path(name)).ok().or_else(|| {
        if name == "button" {
            Some(default_button_plugin_json())
        } else {
            None
        }
    })
}

fn repo_default_plugin_definition(name: &str) -> Option<PluginDefinition> {
    default_plugin_json(name)
        .and_then(|content| serde_json::from_str::<PluginDefinition>(&content).ok())
        .map(normalize_legacy_variant_names)
}

fn parse_plugin_definition(name: &str, content: &str) -> Result<PluginDefinition, String> {
    serde_json::from_str(content)
        .map(|plugin| normalize_plugin_definition(name, plugin))
        .map_err(|e| format!("Failed to parse plugin {}: {}", name, e))
}

fn normalize_plugin_definition(name: &str, plugin: PluginDefinition) -> PluginDefinition {
    let plugin = normalize_legacy_variant_names(plugin);
    let plugin = normalize_legacy_component_renames(name, plugin);
    let plugin = normalize_legacy_textarea_plugin(name, plugin);
    let plugin = normalize_legacy_progress_bar_plugin(name, plugin);
    let plugin = if let Some(default_plugin) = repo_default_plugin_definition(name) {
        let plugin = normalize_plugin_variant_schema(plugin, &default_plugin);
        normalize_plugin_size_schema(plugin, &default_plugin)
    } else {
        plugin
    };
    let plugin = normalize_plugin_states_against_default(name, plugin);
    let plugin = normalize_legacy_shipped_size_defaults(name, plugin);
    dedupe_plugin_props(plugin)
}

fn dedupe_plugin_props(mut plugin: PluginDefinition) -> PluginDefinition {
    let mut seen_variant_props = HashSet::new();
    plugin.variant_props.retain(|prop| seen_variant_props.insert(prop.name.clone()));

    let mut seen_size_props = HashSet::new();
    plugin.size_props.retain(|prop| seen_size_props.insert(prop.name.clone()));

    plugin
}

/// Ensure the theme editor directories exist and have default files
pub fn ensure_default_files() -> std::io::Result<()> {
    let plugins_path = get_plugins_path();
    let tokens_path = get_tokens_path();

    // Create directories
    fs::create_dir_all(&plugins_path)?;

    // Create default tokens.json if it doesn't exist
    if !tokens_path.exists() {
        let default_tokens = default_tokens_json();
        fs::write(&tokens_path, default_tokens)?;
        eprintln!("Created default tokens.json at {}", tokens_path.display());
    }

    let legacy_segmented_path = plugins_path.join("segmented_control.json");
    let tabs_path = plugins_path.join("tabs.json");
    if !tabs_path.exists() && legacy_segmented_path.exists() {
        fs::copy(&legacy_segmented_path, &tabs_path)?;
        eprintln!("Migrated legacy segmented_control.json to {}", tabs_path.display());
    }

    for spec in builtin_component_specs() {
        let plugin_path = plugins_path.join(format!("{}.json", spec.key));

        if !plugin_path.exists() {
            if let Some(default_plugin) = default_plugin_json(spec.key) {
                fs::write(&plugin_path, default_plugin)?;
                eprintln!("Created default {}.json at {}", spec.key, plugin_path.display());
            }
        }

        if let Ok(content) = fs::read_to_string(&plugin_path) {
            if let Ok(plugin) = serde_json::from_str::<PluginDefinition>(&content) {
                // Recovery: a prior version of the editor had a normalize bug
                // that clobbered the variants / variant_props / states of an
                // extending plugin's user file using the slim default as the
                // schema reference (sizes / size_props were spared because the
                // size schema normalizer bailed on empty defaults). The result
                // is a user file with no variants AND no extends — structurally
                // invalid for any builtin (every real plugin has at least one
                // variant, even if just "default"). Restore the default so the
                // loader's inheritance pipeline can do its job.
                let is_mangled = plugin.extends.is_none() && plugin.variants.is_empty();
                if is_mangled {
                    if let Some(default_plugin) = default_plugin_json(spec.key) {
                        fs::write(&plugin_path, &default_plugin)?;
                        eprintln!(
                            "Restored mangled {}.json from defaults at {}",
                            spec.key,
                            plugin_path.display()
                        );
                        continue;
                    }
                }
                let normalized = normalize_plugin_definition(spec.key, plugin.clone());
                if normalized != plugin {
                    if let Ok(json) = serde_json::to_string_pretty(&normalized) {
                        fs::write(&plugin_path, json)?;
                        eprintln!("Updated {}.json at {}", spec.key, plugin_path.display());
                    }
                }
            }
        }
    }

    Ok(())
}

/// Hard cap on plugin-JSON file size. Plugin defs are tiny (a few KB), so
/// anything larger than this is either a misnamed file or a hostile input
/// (zip bomb, recursive ref, etc). Reject before serde_json walks it.
const MAX_PLUGIN_FILE_BYTES: u64 = 1 * 1024 * 1024;

/// Load a specific plugin by name (e.g., "button" loads "button.json")
pub fn load_plugin(name: &str) -> Result<PluginDefinition, String> {
    let plugins_path = get_plugins_path();
    let plugin_path = plugins_path.join(format!("{}.json", name));

    if plugin_path.exists() {
        let metadata =
            fs::metadata(&plugin_path).map_err(|e| format!("Failed to stat plugin {}: {}", name, e))?;
        if metadata.len() > MAX_PLUGIN_FILE_BYTES {
            return Err(format!(
                "Plugin {} at {} is {} bytes (limit {}); refusing to load",
                name,
                plugin_path.display(),
                metadata.len(),
                MAX_PLUGIN_FILE_BYTES
            ));
        }
        let content =
            fs::read_to_string(&plugin_path).map_err(|e| format!("Failed to read plugin {}: {}", name, e))?;
        return parse_plugin_definition(name, &content);
    }

    if let Some(default_plugin) = default_plugin_json(name) {
        return parse_plugin_definition(name, &default_plugin);
    }

    Err(format!(
        "Plugin not found: {} or {}",
        plugin_path.display(),
        repo_default_plugin_path(name).display()
    ))
}

pub fn load_all_plugins() -> Result<Vec<(BuiltinComponentSpec, PluginDefinition)>, String> {
    // Two-pass load:
    //   1) Read each plugin's raw JSON (no normalization yet) so a pure `extends:` child with empty lists
    //      doesn't get defaulted to `["default"]` by the normalizers and then "override" its parent.
    //   2) Resolve `extends:` so each plugin is self-contained.
    //   3) Normalize the resolved plugins (dedup props, legacy migrations, ...).
    let mut raw = Vec::new();
    for spec in builtin_component_specs() {
        let plugin = load_plugin(spec.key)
            .or_else(|_| auto_generate_plugin(spec))
            .map_err(|err| format!("Failed to load plugin for {}: {}", spec.component, err))?;
        raw.push((*spec, plugin));
    }
    let resolved = apply_inheritance(raw)?;
    Ok(resolved
        .into_iter()
        .map(|(spec, plugin)| (spec, normalize_plugin_definition(spec.key, plugin)))
        .collect())
}

/// Like [`load_all_plugins`], but reads ONLY the repo-pinned defaults under
/// `CARGO_MANIFEST_DIR/defaults/plugins`, ignoring the user's mutable
/// `~/.foundation/theme-editor/plugins`. The build-time theme generator
/// (`slintthemegen`) uses this so its output is reproducible from the repo
/// rather than from editor runtime state.
pub fn load_all_plugins_from_repo() -> Result<Vec<(BuiltinComponentSpec, PluginDefinition)>, String> {
    let mut raw = Vec::new();
    for spec in builtin_component_specs() {
        let plugin = match default_plugin_json(spec.key) {
            Some(content) => serde_json::from_str::<PluginDefinition>(&content)
                .map_err(|e| format!("Failed to parse repo plugin {}: {}", spec.key, e))?,
            None => auto_generate_plugin(spec)
                .map_err(|err| format!("Failed to load repo plugin for {}: {}", spec.component, err))?,
        };
        raw.push((*spec, plugin));
    }
    let resolved = apply_inheritance(raw)?;
    Ok(resolved
        .into_iter()
        .map(|(spec, plugin)| (spec, normalize_plugin_definition(spec.key, plugin)))
        .collect())
}

/// Resolve `extends:` inheritance for every plugin. Each child inherits its
/// parent's variants/states/sizes and prop definitions; the child overrides
/// any of those by re-declaring them (lists by being non-empty, props by name).
fn apply_inheritance(
    plugins: Vec<(BuiltinComponentSpec, PluginDefinition)>,
) -> Result<Vec<(BuiltinComponentSpec, PluginDefinition)>, String> {
    let by_name: std::collections::HashMap<String, PluginDefinition> =
        plugins.iter().map(|(_, p)| (p.component.clone(), p.clone())).collect();
    plugins
        .into_iter()
        .map(|(spec, plugin)| {
            let mut visited = HashSet::new();
            let resolved = resolve_extends_chain(plugin, &by_name, &mut visited)?;
            Ok((spec, resolved))
        })
        .collect()
}

fn resolve_extends_chain(
    plugin: PluginDefinition,
    by_name: &std::collections::HashMap<String, PluginDefinition>,
    visited: &mut HashSet<String>,
) -> Result<PluginDefinition, String> {
    if !visited.insert(plugin.component.clone()) {
        return Err(format!("plugin extends cycle detected at '{}'", plugin.component));
    }
    let Some(parent_name) = plugin.extends.clone() else {
        return Ok(plugin);
    };
    let parent = by_name
        .get(&parent_name)
        .cloned()
        .ok_or_else(|| format!("plugin '{}' extends unknown plugin '{}'", plugin.component, parent_name))?;
    let resolved_parent = resolve_extends_chain(parent, by_name, visited)?;
    Ok(merge_with_parent(plugin, &resolved_parent))
}

/// Merge a child plugin on top of its resolved parent: the child wins for any
/// non-empty list and for any prop it re-declares by name; otherwise the
/// parent's value is inherited verbatim.
fn merge_with_parent(child: PluginDefinition, parent: &PluginDefinition) -> PluginDefinition {
    fn take_list(child: Vec<String>, parent: &[String]) -> Vec<String> {
        if child.is_empty() {
            parent.to_vec()
        } else {
            child
        }
    }
    fn merge_props(child_props: Vec<PropDefinition>, parent_props: &[PropDefinition]) -> Vec<PropDefinition> {
        let mut merged: Vec<PropDefinition> = parent_props.to_vec();
        for child_prop in child_props {
            if let Some(idx) = merged.iter().position(|p| p.name == child_prop.name) {
                merged[idx] = child_prop;
            } else {
                merged.push(child_prop);
            }
        }
        merged
    }
    // Remember who we extended so the runtime can cascade parent edits to this
    // child. Prefer the original `extends` (set in the JSON) but fall back to a
    // previously-resolved parent_key (for chained inheritance).
    let inherited_parent = child.extends.clone().or_else(|| child.parent_key.clone());
    PluginDefinition {
        component: child.component,
        // Fully resolved: clear extends so downstream code doesn't try to chase
        // it again. parent_key keeps the link for runtime cascade.
        extends: None,
        parent_key: inherited_parent,
        variants: take_list(child.variants, &parent.variants),
        states: take_list(child.states, &parent.states),
        sizes: take_list(child.sizes, &parent.sizes),
        variant_props: merge_props(child.variant_props, &parent.variant_props),
        size_props: merge_props(child.size_props, &parent.size_props),
    }
}

fn auto_generate_plugin(spec: &BuiltinComponentSpec) -> Result<PluginDefinition, String> {
    let source_path = component_source_path(spec.source_file);
    let parsed = parse_component_file(&source_path)?;
    let variant_prop = axis_enum_values(&source_path, &parsed, &["variant", "tone", "color"])?;
    let size_prop = axis_enum_values(&source_path, &parsed, &["size"])?;
    let themeable_props = parsed.properties;

    let variants = if has_variant_props(&themeable_props) {
        variant_prop.unwrap_or_else(|| vec!["default".to_string()])
    } else {
        vec!["default".to_string()]
    };
    let states = infer_states(&parsed.content);
    let sizes = if has_size_props(&themeable_props) {
        size_prop.unwrap_or_else(|| vec!["default".to_string()])
    } else {
        vec!["default".to_string()]
    };

    let mut variant_props = Vec::new();
    let mut size_props = Vec::new();

    for prop in themeable_props {
        let prop_type = map_prop_type(&prop.type_name);
        let defaults = if is_size_prop(&prop.name) {
            build_size_defaults(&prop.name, &prop_type, &sizes)
        } else {
            build_variant_defaults(spec.key, &prop.name, &prop_type, &variants, &states)
        };

        let definition = PropDefinition {
            name: prop.name.clone(),
            display_name: Some(humanize_prop_name(&prop.name)),
            prop_type,
            min: infer_min(&prop.name),
            max: infer_max(&prop.name),
            step: infer_step(&prop.name),
            defaults,
        };

        if is_size_prop(&prop.name) {
            size_props.push(definition);
        } else {
            variant_props.push(definition);
        }
    }

    Ok(PluginDefinition {
        extends: None,
        parent_key: None,
        component: spec.component.to_string(),
        variants,
        states,
        sizes,
        variant_props,
        size_props,
    })
}

/// Normalize legacy names from older plugin files.
fn normalize_legacy_variant_names(mut plugin: PluginDefinition) -> PluginDefinition {
    plugin.states.retain(|state| state != "loading");

    for variant in &mut plugin.variants {
        if variant == "outline" {
            *variant = "tertiary".to_string();
        }
    }

    for prop in &mut plugin.variant_props {
        if let Some(outline_defaults) = prop.defaults.values.remove("outline") {
            prop.defaults.values.entry("tertiary".to_string()).or_insert(outline_defaults);
        }

        for variant_defaults in prop.defaults.values.values_mut() {
            if let super::DefaultValue::Nested(states) = variant_defaults {
                states.remove("loading");
            }
        }
    }

    plugin
}

fn normalize_legacy_component_renames(name: &str, mut plugin: PluginDefinition) -> PluginDefinition {
    if name == "tabs" && plugin.component == "SegmentedControl" {
        if let Some(default_plugin) = repo_default_plugin_definition("tabs") {
            plugin.component = default_plugin.component;
            plugin.variants = default_plugin.variants;
            plugin.states = default_plugin.states;
            plugin.variant_props = default_plugin.variant_props;
        } else {
            plugin.component = "Tabs".to_string();
        }
    }

    plugin
}

fn normalize_legacy_textarea_plugin(name: &str, mut plugin: PluginDefinition) -> PluginDefinition {
    if name != "textarea" {
        return plugin;
    }

    let Some(default_plugin) = repo_default_plugin_definition("textarea") else {
        return plugin;
    };

    for variant in &mut plugin.variants {
        if variant == "underlined" {
            *variant = "faded".to_string();
        }
    }

    plugin.variants = default_plugin
        .variants
        .iter()
        .filter(|variant| plugin.variants.iter().any(|existing| existing == *variant))
        .cloned()
        .collect();

    for prop in &mut plugin.variant_props {
        prop.defaults.values.remove("underlined");
    }

    for default_prop in &default_plugin.variant_props {
        if let Some(prop) = plugin.variant_props.iter_mut().find(|prop| prop.name == default_prop.name) {
            prop.defaults
                .values
                .retain(|variant, _| default_plugin.variants.iter().any(|allowed| allowed == variant));

            for variant in &default_plugin.variants {
                if let Some(default_value) = default_prop.defaults.values.get(variant).cloned() {
                    prop.defaults.values.entry(variant.clone()).or_insert(default_value);
                }
            }
        }
    }

    plugin
}

fn normalize_legacy_progress_bar_plugin(name: &str, mut plugin: PluginDefinition) -> PluginDefinition {
    if name != "progress_bar" {
        return plugin;
    }

    for variant in &mut plugin.variants {
        if variant == "neutral" {
            *variant = "default".to_string();
        }
    }

    for prop in &mut plugin.variant_props {
        if let Some(default_defaults) = prop.defaults.values.remove("neutral") {
            prop.defaults.values.entry("default".to_string()).or_insert(default_defaults);
        }
    }

    if let Some(default_plugin) = repo_default_plugin_definition("progress_bar") {
        for variant in &default_plugin.variants {
            if !plugin.variants.iter().any(|existing| existing == variant) {
                plugin.variants.push(variant.clone());
            }
        }

        plugin.variants = default_plugin
            .variants
            .iter()
            .filter(|variant| plugin.variants.iter().any(|existing| existing == *variant))
            .cloned()
            .collect();

        for default_prop in &default_plugin.variant_props {
            if let Some(prop) = plugin.variant_props.iter_mut().find(|prop| prop.name == default_prop.name) {
                for variant in &default_plugin.variants {
                    if let Some(default_value) = default_prop.defaults.values.get(variant).cloned() {
                        prop.defaults.values.entry(variant.clone()).or_insert(default_value);
                    }
                }
            } else {
                plugin.variant_props.push(default_prop.clone());
            }
        }
    }

    plugin
}

fn normalize_plugin_states_against_default(name: &str, mut plugin: PluginDefinition) -> PluginDefinition {
    let Some(default_plugin) = repo_default_plugin_definition(name) else {
        return plugin;
    };
    // A slim `extends:` default has no states declared; using it as the canonical
    // list would wipe the resolved plugin's inherited states.
    if default_plugin.extends.is_some() || default_plugin.states.is_empty() {
        return plugin;
    }

    plugin.states = default_plugin.states.clone();

    for prop in &mut plugin.variant_props {
        for variant_defaults in prop.defaults.values.values_mut() {
            if let super::DefaultValue::Nested(states) = variant_defaults {
                for default_state in &default_plugin.states {
                    if !states.contains_key(default_state) {
                        let fallback_value = states
                            .get("normal")
                            .cloned()
                            .or_else(|| states.get("pressed").cloned())
                            .or_else(|| states.values().next().cloned());

                        if let Some(value) = fallback_value {
                            states.insert(default_state.clone(), value);
                        }
                    }
                }

                states.retain(|state, _| default_plugin.states.iter().any(|allowed| allowed == state));
            }
        }
    }

    plugin
}

fn normalize_plugin_size_schema(
    mut plugin: PluginDefinition,
    default_plugin: &PluginDefinition,
) -> PluginDefinition {
    // Same guard as normalize_plugin_variant_schema: a slim `extends:` default is
    // not a usable schema reference.
    if default_plugin.extends.is_some()
        || default_plugin.size_props.is_empty()
        || default_plugin.sizes.is_empty()
    {
        return plugin;
    }

    let old_sizes = plugin.sizes.clone();
    let target_sizes = &default_plugin.sizes;
    let local_size_props = plugin.size_props.clone();

    plugin.sizes = target_sizes.clone();
    plugin.size_props =
        merge_size_props(&local_size_props, &old_sizes, &default_plugin.size_props, target_sizes);

    plugin
}

fn normalize_plugin_variant_schema(
    mut plugin: PluginDefinition,
    default_plugin: &PluginDefinition,
) -> PluginDefinition {
    // A slim `extends:` default has empty variants / variant_props; using it as a
    // schema reference would clobber the resolved plugin's inherited data. The
    // resolved plugin is already correct — leave it alone.
    if default_plugin.extends.is_some()
        || default_plugin.variants.is_empty()
        || default_plugin.variant_props.is_empty()
    {
        return plugin;
    }
    let old_variants = plugin.variants.clone();
    let target_variants = &default_plugin.variants;
    let local_variant_props = plugin.variant_props.clone();

    plugin.variants = target_variants.clone();
    plugin.variant_props = merge_variant_props(
        &local_variant_props,
        &old_variants,
        &default_plugin.variant_props,
        target_variants,
    );

    plugin
}

fn merge_variant_props(
    local_props: &[PropDefinition],
    old_variants: &[String],
    default_props: &[PropDefinition],
    target_variants: &[String],
) -> Vec<PropDefinition> {
    let mut merged = Vec::new();

    for default_prop in default_props {
        if let Some(local_prop) = local_props.iter().find(|prop| prop.name == default_prop.name) {
            let mut prop = default_prop.clone();
            prop.defaults = migrate_variant_defaults(
                &local_prop.defaults,
                old_variants,
                &default_prop.defaults,
                target_variants,
            );
            merged.push(prop);
        } else {
            merged.push(default_prop.clone());
        }
    }

    merged
}

fn migrate_variant_defaults(
    local_defaults: &PropDefaults,
    old_variants: &[String],
    fallback_defaults: &PropDefaults,
    target_variants: &[String],
) -> PropDefaults {
    let mut migrated = PropDefaults::default();

    for (index, target_variant) in target_variants.iter().enumerate() {
        let value = local_defaults
            .values
            .get(target_variant)
            .cloned()
            .or_else(|| {
                if old_variants.len() == target_variants.len() {
                    old_variants
                        .get(index)
                        .and_then(|old_variant| local_defaults.values.get(old_variant).cloned())
                } else {
                    None
                }
            })
            .or_else(|| local_defaults.values.values().next().cloned())
            .or_else(|| fallback_defaults.values.get(target_variant).cloned());

        if let Some(value) = value {
            migrated.values.insert(target_variant.clone(), value);
        }
    }

    if migrated.values.is_empty() {
        fallback_defaults.clone()
    } else {
        migrated
    }
}

fn merge_size_props(
    local_props: &[PropDefinition],
    old_sizes: &[String],
    default_props: &[PropDefinition],
    target_sizes: &[String],
) -> Vec<PropDefinition> {
    let mut merged = Vec::new();

    for default_prop in default_props {
        if let Some(local_prop) = local_props.iter().find(|prop| prop.name == default_prop.name) {
            let mut prop = default_prop.clone();
            prop.defaults =
                migrate_size_defaults(&local_prop.defaults, old_sizes, &default_prop.defaults, target_sizes);
            merged.push(prop);
        } else {
            merged.push(default_prop.clone());
        }
    }

    merged
}

fn migrate_size_defaults(
    local_defaults: &PropDefaults,
    old_sizes: &[String],
    fallback_defaults: &PropDefaults,
    target_sizes: &[String],
) -> PropDefaults {
    // New model: the local prop already declares an explicit "default" base with
    // sparse per-size overrides. Preserve the base verbatim and keep only the
    // per-size keys that are actually present — a missing size inherits the base,
    // so the legacy positional/first-value backfill below would wrongly promote
    // an inheriting size into an override (with the wrong value).
    if local_defaults.values.contains_key("default") {
        let mut migrated = PropDefaults::default();
        if let Some(base) = local_defaults.values.get("default") {
            migrated.values.insert("default".to_string(), base.clone());
        }
        for target_size in target_sizes {
            if let Some(value) = local_defaults.values.get(target_size) {
                migrated.values.insert(target_size.clone(), value.clone());
            }
        }
        return migrated;
    }

    let mut migrated = PropDefaults::default();

    for (index, target_size) in target_sizes.iter().enumerate() {
        let value = local_defaults
            .values
            .get(target_size)
            .cloned()
            .or_else(|| {
                if target_size == "default" {
                    local_defaults
                        .values
                        .get("md")
                        .cloned()
                        .or_else(|| local_defaults.values.get("medium").cloned())
                } else {
                    None
                }
            })
            .or_else(|| {
                size_aliases(target_size).iter().find_map(|alias| local_defaults.values.get(*alias).cloned())
            })
            .or_else(|| {
                if old_sizes.len() == target_sizes.len() {
                    old_sizes.get(index).and_then(|old_size| local_defaults.values.get(old_size).cloned())
                } else {
                    None
                }
            })
            .or_else(|| {
                if old_sizes.len() == 1 {
                    old_sizes.first().and_then(|old_size| local_defaults.values.get(old_size).cloned())
                } else {
                    None
                }
            })
            .or_else(|| local_defaults.values.values().next().cloned())
            .or_else(|| fallback_defaults.values.get(target_size).cloned());

        if let Some(value) = value {
            migrated.values.insert(target_size.clone(), value);
        }
    }

    if migrated.values.is_empty() {
        fallback_defaults.clone()
    } else {
        migrated
    }
}

fn size_aliases(size: &str) -> &'static [&'static str] {
    match size {
        "sm" => &["small"],
        "md" => &["medium"],
        "lg" => &["large"],
        "small" => &["sm"],
        "medium" => &["md"],
        "large" => &["lg"],
        _ => &[],
    }
}

fn normalize_legacy_shipped_size_defaults(name: &str, mut plugin: PluginDefinition) -> PluginDefinition {
    match name {
        "icon_button" => {
            rewrite_float_size_prop_if_exact_match(
                &mut plugin,
                "control-size",
                &[("sm", 32.0), ("md", 40.0), ("lg", 48.0)],
                &[("sm", 36.0), ("md", 44.0), ("lg", 52.0)],
            );
        }
        "link" => {
            rewrite_float_size_prop_if_exact_match(
                &mut plugin,
                "row-height",
                &[("sm", 28.0), ("md", 32.0), ("lg", 36.0)],
                &[("sm", 36.0), ("md", 44.0), ("lg", 52.0)],
            );
        }
        "switch" => {
            rewrite_float_size_prop_if_exact_match(
                &mut plugin,
                "track-width",
                &[("sm", 32.0), ("md", 40.0), ("lg", 48.0)],
                &[("sm", 36.0), ("md", 44.0), ("lg", 52.0)],
            );
            rewrite_float_size_prop_if_exact_match(
                &mut plugin,
                "track-height",
                &[("sm", 18.0), ("md", 22.0), ("lg", 26.0)],
                &[("sm", 20.0), ("md", 24.0), ("lg", 28.0)],
            );
            rewrite_float_size_prop_if_exact_match(
                &mut plugin,
                "knob-size",
                &[("sm", 14.0), ("md", 18.0), ("lg", 22.0)],
                &[("sm", 16.0), ("md", 20.0), ("lg", 24.0)],
            );
        }
        "radio" => {
            if let Some(default_plugin) = repo_default_plugin_definition("radio") {
                if let Some(default_prop) =
                    default_plugin.size_props.iter().find(|prop| prop.name == "control-size").cloned()
                {
                    if let Some(prop) = plugin.size_props.iter_mut().find(|prop| prop.name == "control-size")
                    {
                        prop.defaults = default_prop.defaults;
                    }
                }
            }
        }
        "checkbox" => {
            if let Some(default_plugin) = repo_default_plugin_definition("checkbox") {
                if let Some(default_prop) =
                    default_plugin.size_props.iter().find(|prop| prop.name == "control-size").cloned()
                {
                    if let Some(prop) = plugin.size_props.iter_mut().find(|prop| prop.name == "control-size")
                    {
                        prop.defaults = default_prop.defaults;
                    }
                }
            }
        }
        "icon" => {
            plugin.states = vec!["normal".to_string()];
            for prop in &mut plugin.variant_props {
                for variant_defaults in prop.defaults.values.values_mut() {
                    if let super::DefaultValue::Nested(states) = variant_defaults {
                        states.retain(|state, _| state == "normal");
                    }
                }
            }
        }
        "slider" => {
            plugin.variant_props.retain(|prop| prop.name != "focus-ring-color");
            rewrite_float_size_prop_if_exact_match(
                &mut plugin,
                "thumb-size",
                &[("sm", 16.0), ("md", 20.0), ("lg", 24.0)],
                &[("sm", 8.0), ("md", 12.0), ("lg", 16.0)],
            );
        }
        "slide_to_confirm" => {
            rewrite_float_size_prop_if_exact_match(
                &mut plugin,
                "control-height",
                &[("sm", 44.0), ("md", 52.0), ("lg", 60.0)],
                &[("sm", 36.0), ("md", 44.0), ("lg", 52.0)],
            );
        }
        "accordion" => {
            rewrite_float_size_prop_if_exact_match(
                &mut plugin,
                "header-height",
                &[("sm", 44.0), ("md", 52.0), ("lg", 60.0)],
                &[("sm", 36.0), ("md", 44.0), ("lg", 52.0)],
            );
        }
        "chip" => {}
        "image" => {
            plugin.variants = vec!["default".to_string()];
            plugin.states = vec!["normal".to_string()];
            plugin.sizes = vec!["default".to_string()];
            plugin.variant_props.clear();
            plugin.size_props.retain(|prop| prop.name == "frame-width" || prop.name == "frame-height");
            rewrite_float_size_prop_if_exact_match(
                &mut plugin,
                "frame-height",
                &[("default", 220.0)],
                &[("default", 192.0)],
            );
        }
        _ => {}
    }

    plugin
}

fn rewrite_float_size_prop_if_exact_match(
    plugin: &mut PluginDefinition,
    prop_name: &str,
    old_values: &[(&str, f64)],
    new_values: &[(&str, f64)],
) {
    let old_defaults = float_size_defaults(old_values);
    let new_defaults = float_size_defaults(new_values);

    if let Some(prop) = plugin.size_props.iter_mut().find(|prop| prop.name == prop_name) {
        if prop.defaults == old_defaults {
            prop.defaults = new_defaults;
        }
    }
}

fn float_size_defaults(values: &[(&str, f64)]) -> PropDefaults {
    let mut defaults = PropDefaults::default();
    for (size, value) in values {
        defaults.values.insert((*size).to_string(), DefaultValue::Direct(TokenOrValue::Float(*value)));
    }
    defaults
}

#[cfg(test)]
fn variant_state_defaults(values: &[(&str, &[(&str, &str)])]) -> PropDefaults {
    let mut defaults = PropDefaults::default();
    for (variant, states) in values {
        let mut nested = std::collections::HashMap::new();
        for (state, value) in *states {
            nested.insert((*state).to_string(), TokenOrValue::String((*value).to_string()));
        }
        defaults.values.insert((*variant).to_string(), DefaultValue::Nested(nested));
    }
    defaults
}

fn component_source_path(source_file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../components/ui").join(source_file)
}

fn parse_component_file(path: &Path) -> Result<ParsedComponentFile, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read component file {}: {}", path.display(), e))?;
    let lines: Vec<&str> = content.lines().collect();
    let mut parsed = ParsedComponentFile { content: content.clone(), ..ParsedComponentFile::default() };
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index].trim();
        if line.starts_with("import {") && line.contains("} from ") {
            if let Some((symbols, source)) = parse_import_line(line) {
                parsed.imports.push((symbols, source));
            }
        }

        if let Some(enum_name) =
            line.strip_prefix("export enum ").and_then(|value| value.split_whitespace().next())
        {
            let mut values = Vec::new();
            index += 1;
            while index < lines.len() {
                let enum_line = lines[index].trim().trim_end_matches(',');
                if enum_line == "}" {
                    break;
                }
                if !enum_line.is_empty() && !enum_line.starts_with("//") {
                    values.push(enum_line.to_string());
                }
                index += 1;
            }
            parsed.enums.insert(enum_name.to_string(), values);
        } else if let Some(property) = parse_themeable_property(line) {
            parsed.properties.push(property);
        }

        index += 1;
    }

    Ok(parsed)
}

fn parse_import_line(line: &str) -> Option<(Vec<String>, String)> {
    let import_body = line.strip_prefix("import {")?;
    let (symbols_part, source_part) = import_body.split_once("} from ")?;
    let source = source_part.trim().trim_end_matches(';').trim_matches('"').to_string();
    let symbols = symbols_part
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    Some((symbols, source))
}

fn parse_themeable_property(line: &str) -> Option<ParsedProperty> {
    let line = line.trim();
    if !line.starts_with("in property <") && !line.starts_with("in-out property <") {
        return None;
    }

    let property_start = line.find("property <")? + "property <".len();
    let property_end = line[property_start..].find('>')? + property_start;
    let type_name = line[property_start..property_end].trim().to_string();
    if !is_themeable_type(&type_name) {
        return None;
    }

    let name_start = property_end + 1;
    let after_type = line[name_start..].trim();
    let name = after_type.split(':').next()?.split_whitespace().next()?.trim().to_string();
    if !is_size_prop(&name) && !is_variant_prop(&name) {
        return None;
    }

    Some(ParsedProperty { type_name, name })
}

fn is_themeable_type(type_name: &str) -> bool {
    matches!(type_name, "color" | "length" | "float" | "int" | "string")
}

fn axis_enum_values(
    path: &Path,
    parsed: &ParsedComponentFile,
    axis_names: &[&str],
) -> Result<Option<Vec<String>>, String> {
    for axis in axis_names {
        if let Some(type_name) = property_type_for_name(&parsed.content, axis) {
            if let Some(values) = resolve_enum_values(path, parsed, &type_name)? {
                return Ok(Some(values));
            }
        }
    }

    Ok(None)
}

fn property_type_for_name(content: &str, property_name: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("in property <") && !trimmed.starts_with("in-out property <") {
            continue;
        }
        let property_start = trimmed.find("property <")? + "property <".len();
        let property_end = trimmed[property_start..].find('>')? + property_start;
        let type_name = trimmed[property_start..property_end].trim().to_string();
        let after_type = trimmed[property_end + 1..].trim();
        let name =
            after_type.split(':').next().unwrap_or_default().split_whitespace().next().unwrap_or_default();
        if name == property_name {
            return Some(type_name);
        }
    }
    None
}

fn resolve_enum_values(
    current_path: &Path,
    parsed: &ParsedComponentFile,
    enum_name: &str,
) -> Result<Option<Vec<String>>, String> {
    if let Some(values) = parsed.enums.get(enum_name) {
        return Ok(Some(values.clone()));
    }

    for (symbols, import_path) in &parsed.imports {
        if !symbols.iter().any(|symbol| symbol == enum_name) {
            continue;
        }

        let resolved_import_path = current_path.parent().unwrap_or_else(|| Path::new(".")).join(import_path);
        let imported = parse_component_file(&resolved_import_path)?;
        if let Some(values) = imported.enums.get(enum_name) {
            return Ok(Some(values.clone()));
        }
    }

    Ok(None)
}

fn has_variant_props(properties: &[ParsedProperty]) -> bool {
    properties.iter().any(|prop| !is_size_prop(&prop.name))
}

fn has_size_props(properties: &[ParsedProperty]) -> bool {
    properties.iter().any(|prop| is_size_prop(&prop.name))
}

fn infer_states(content: &str) -> Vec<String> {
    let mut states = vec!["normal".to_string()];

    if content.contains("force-focused") || content.contains("has-focus") {
        states.push("focused".to_string());
    }
    if content.contains("force-pressed") || content.contains("pressed ?") || content.contains("pressed-color")
    {
        states.push("pressed".to_string());
    }
    if content.contains("disabled") {
        states.push("disabled".to_string());
    }

    states
}

fn map_prop_type(type_name: &str) -> String {
    match type_name {
        "color" => "color".to_string(),
        "string" => "string".to_string(),
        "int" => "int".to_string(),
        "float" => "float".to_string(),
        "length" => "float".to_string(),
        _ => "string".to_string(),
    }
}

fn is_size_prop(prop_name: &str) -> bool {
    matches!(
        prop_name,
        "font-size"
            | "font-family"
            | "padding-horizontal"
            | "padding-vertical"
            | "icon-size"
            | "control-size"
            | "field-height"
            | "border-radius"
            | "menu-height"
            | "bar-height"
            | "spinner-size"
            | "stroke-width"
            | "border-width"
            | "padding"
    ) || prop_name.contains("size")
        || prop_name.contains("height")
        || prop_name.contains("radius")
        || prop_name.contains("padding")
}

fn is_variant_prop(prop_name: &str) -> bool {
    matches!(
        prop_name,
        "background"
            | "foreground"
            | "placeholder-color"
            | "border-color"
            | "border-color-focus"
            | "border-color-error"
            | "description-color"
            | "error-color"
            | "icon-color"
            | "track-color"
            | "track-border-color"
            | "fill-color"
            | "fill-color-override"
            | "thumb-color"
            | "thumb-border-color"
            | "thumb-label-color"
            | "label-color"
            | "value-color"
            | "text-color"
            | "underline-color"
            | "visited-color"
            | "divider-color"
            | "menu-background"
            | "menu-border-color"
            | "menu-border-width"
            | "pressed-background"
            | "separator-color"
            | "accent-color-override"
            | "button-opacity"
            | "opacity"
            | "font-weight"
            | "touch-expansion"
    )
}

fn build_variant_defaults(
    component_key: &str,
    prop_name: &str,
    prop_type: &str,
    variants: &[String],
    states: &[String],
) -> PropDefaults {
    let mut defaults = std::collections::HashMap::new();

    for variant in variants {
        let mut state_defaults = std::collections::HashMap::new();
        for state in states {
            state_defaults.insert(
                state.clone(),
                default_variant_value(component_key, prop_name, prop_type, variant, state),
            );
        }
        defaults.insert(variant.clone(), DefaultValue::Nested(state_defaults));
    }

    PropDefaults { values: defaults }
}

fn build_size_defaults(prop_name: &str, prop_type: &str, sizes: &[String]) -> PropDefaults {
    let mut defaults = std::collections::HashMap::new();

    for size in sizes {
        defaults.insert(size.clone(), DefaultValue::Direct(default_size_value(prop_name, prop_type, size)));
    }

    PropDefaults { values: defaults }
}

fn default_variant_value(
    component_key: &str,
    prop_name: &str,
    prop_type: &str,
    variant: &str,
    state: &str,
) -> TokenOrValue {
    if prop_type == "color" {
        let accent = accent_token_for_variant(component_key, variant);
        return TokenOrValue::String(match prop_name {
            "background" => background_token_for_state(component_key, accent, state),
            "foreground" => foreground_token_for_variant(component_key, accent, variant),
            "placeholder-color" => "color.foreground.light".to_string(),
            "border-color" => border_token_for_state(component_key, accent, state),
            "border-color-focus" => accent.focus_token().to_string(),
            "border-color-error" => "color.danger".to_string(),
            "icon-color" => foreground_token_for_variant(component_key, accent, variant),
            "track-color" => "color.secondary".to_string(),
            "fill-color-override" => accent.normal_token().to_string(),
            "accent-color-override" => accent.normal_token().to_string(),
            _ => "color.foreground".to_string(),
        });
    }

    match prop_name {
        "button-opacity" | "opacity" => TokenOrValue::Float(if state == "disabled" { 0.5 } else { 1.0 }),
        "font-weight" => TokenOrValue::Int(500),
        "touch-expansion" => TokenOrValue::Float(4.0),
        _ if prop_type == "string" => TokenOrValue::String("font.primary".to_string()),
        _ => TokenOrValue::Float(default_numeric_value(prop_name, "default")),
    }
}

fn default_size_value(prop_name: &str, prop_type: &str, size: &str) -> TokenOrValue {
    if prop_type == "string" {
        return TokenOrValue::String("font.primary".to_string());
    }

    TokenOrValue::String(match prop_name {
        "font-size" => match normalize_size_key(size).as_str() {
            "small" => "fontSize.sm",
            "large" => "fontSize.lg",
            _ => "fontSize.md",
        }
        .to_string(),
        "padding-horizontal" => match normalize_size_key(size).as_str() {
            "small" => "spacing.sm",
            "large" => "spacing.xl",
            _ => "spacing.md",
        }
        .to_string(),
        "padding-vertical" => match normalize_size_key(size).as_str() {
            "small" => "spacing.xs",
            "large" => "spacing.md",
            _ => "spacing.sm",
        }
        .to_string(),
        "border-radius" => "radius.default".to_string(),
        _ => return TokenOrValue::Float(default_numeric_value(prop_name, size)),
    })
}

fn normalize_size_key(size: &str) -> String {
    match size {
        "sm" => "small".to_string(),
        "md" => "medium".to_string(),
        "lg" => "large".to_string(),
        _ => size.to_string(),
    }
}

fn default_numeric_value(prop_name: &str, size: &str) -> f64 {
    let size_key = normalize_size_key(size);
    match prop_name {
        "border-width" => 1.0,
        "menu-height" => 172.0,
        "field-height" => 42.0,
        "control-size" => match size_key.as_str() {
            "small" => 32.0,
            "large" => 48.0,
            _ => 40.0,
        },
        "icon-size" => match size_key.as_str() {
            "small" => 16.0,
            "large" => 20.0,
            _ => 18.0,
        },
        "bar-height" => 10.0,
        "spinner-size" => match size_key.as_str() {
            "small" => 18.0,
            "large" => 36.0,
            _ => 26.0,
        },
        "stroke-width" => match size_key.as_str() {
            "small" => 2.0,
            "large" => 4.0,
            _ => 3.0,
        },
        "padding" => 4.0,
        _ => 14.0,
    }
}

fn humanize_prop_name(prop_name: &str) -> String {
    prop_name.split('-').map(capitalize_word).collect::<Vec<_>>().join(" ")
}

fn capitalize_word(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn infer_min(prop_name: &str) -> Option<f32> {
    Some(match prop_name {
        "button-opacity" | "opacity" => 0.0,
        "font-weight" => 100.0,
        _ => 0.0,
    })
}

fn infer_max(prop_name: &str) -> Option<f32> {
    Some(match prop_name {
        "button-opacity" | "opacity" => 1.0,
        "font-weight" => 900.0,
        "menu-height" => 480.0,
        "spinner-size" | "control-size" | "field-height" => 160.0,
        _ => 256.0,
    })
}

fn infer_step(prop_name: &str) -> Option<f32> {
    Some(match prop_name {
        "button-opacity" | "opacity" => 0.05,
        "font-weight" => 100.0,
        "border-width" | "stroke-width" => 0.5,
        _ => 1.0,
    })
}

#[derive(Clone, Copy)]
enum AccentFamily {
    Primary,
    Danger,
    Success,
    Warning,
    Secondary,
    Neutral,
}

impl AccentFamily {
    fn normal_token(self) -> &'static str {
        match self {
            AccentFamily::Primary => "color.primary",
            AccentFamily::Danger => "color.danger",
            AccentFamily::Success => "color.success",
            AccentFamily::Warning => "color.warning",
            AccentFamily::Secondary => "color.secondary",
            AccentFamily::Neutral => "color.foreground",
        }
    }

    fn pressed_token(self) -> &'static str {
        match self {
            AccentFamily::Primary => "color.primary.dark",
            AccentFamily::Danger => "color.danger.dark",
            AccentFamily::Success => "color.success",
            AccentFamily::Warning => "color.warning",
            AccentFamily::Secondary => "color.secondary.dark",
            AccentFamily::Neutral => "color.foreground.light",
        }
    }

    fn focus_token(self) -> &'static str {
        match self {
            AccentFamily::Primary => "color.primary",
            AccentFamily::Danger => "color.danger",
            AccentFamily::Success => "color.success",
            AccentFamily::Warning => "color.warning",
            AccentFamily::Secondary => "color.secondary.dark",
            AccentFamily::Neutral => "color.foreground",
        }
    }
}

fn accent_token_for_variant(component_key: &str, variant: &str) -> AccentFamily {
    match variant {
        "danger" | "error" => AccentFamily::Danger,
        "success" => AccentFamily::Success,
        "warning" => AccentFamily::Warning,
        "secondary" => AccentFamily::Secondary,
        "neutral" => AccentFamily::Neutral,
        "default" if component_key == "icon_button" => AccentFamily::Secondary,
        _ => AccentFamily::Primary,
    }
}

fn background_token_for_state(component_key: &str, accent: AccentFamily, state: &str) -> String {
    if matches!(
        component_key,
        "icon_button"
            | "checkbox"
            | "radio"
            | "switch"
            | "dropdown"
            | "input"
            | "search"
            | "menu"
            | "menu_item"
            | "dialog"
            | "sheet"
            | "toast"
            | "card"
            | "accordion"
            | "image"
    ) {
        return match state {
            "pressed" => "color.secondary.dark".to_string(),
            _ => "color.surface".to_string(),
        };
    }

    match state {
        "pressed" => accent.pressed_token().to_string(),
        _ => accent.normal_token().to_string(),
    }
}

fn foreground_token_for_variant(component_key: &str, accent: AccentFamily, variant: &str) -> String {
    if matches!(component_key, "chip" | "button") {
        return match variant {
            "primary" | "danger" => "color.white".to_string(),
            _ => "color.foreground".to_string(),
        };
    }

    match accent {
        AccentFamily::Secondary | AccentFamily::Neutral => "color.foreground".to_string(),
        _ => accent.normal_token().to_string(),
    }
}

fn border_token_for_state(component_key: &str, accent: AccentFamily, state: &str) -> String {
    match state {
        "focused" => accent.focus_token().to_string(),
        _ if matches!(
            component_key,
            "input"
                | "search"
                | "dropdown"
                | "menu"
                | "accordion"
                | "dialog"
                | "sheet"
                | "toast"
                | "card"
                | "image"
        ) =>
        {
            "color.border".to_string()
        }
        _ => "color.transparent".to_string(),
    }
}

/// Default tokens.json content
fn default_tokens_json() -> String {
    r##"{
  "color": {
    "primary": "#009db9",
    "primary.light": "#33b1c7",
    "primary.dark": "#006f83",
    "secondary": "#d5d4d5",
    "secondary.dark": "#c2c1c2",
    "foreground": "#231f20",
    "foreground.light": "#231f201a",
    "danger": "#ff3333",
    "danger.light": "#ff5c5c",
    "danger.dark": "#b52424",
    "success": "#16a34a",
    "warning": "#d97706",
    "surface": "#ffffff",
    "background": "#ffffff",
    "muted": "#959394",
    "border": "#d5d4d5",
    "transparent": "#00000000",
    "white": "#ffffff"
  },
  "spacing": {
    "xs": 4,
    "sm": 8,
    "md": 12,
    "lg": 16,
    "xl": 24
  },
  "font": {
    "primary": "Montserrat",
    "secondary": "Montserrat",
    "tertiary": "Montserrat"
  },
  "fontSize": {
    "sm": 20,
    "md": 22,
    "lg": 24
  },
  "radius": {
    "sm": 8,
    "md": 16,
    "default": "radius.lg",
    "lg": 24,
    "full": 9999
  },
  "controlSize": {
    "sm": 48,
    "md": 56,
    "lg": 64
  },
  "iconSize": {
    "sm": 20,
    "md": 24,
    "lg": 28
  },
  "controlRadius": {
    "sm": 20,
    "md": 24,
    "lg": 28
  },
  "controlPaddingInline": {
    "sm": 14,
    "md": 16,
    "lg": 20
  },
  "fontWeight": {
    "normal": 400,
    "medium": 500,
    "semibold": 600,
    "bold": 700
  },
  "opacity": {
    "disabled": 0.5
  }
}
"##
    .to_string()
}

/// Default button.json plugin content
fn default_button_plugin_json() -> String { include_str!("../../defaults/plugins/button.json").to_string() }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::TokenStore;

    fn make_parent() -> PluginDefinition {
        serde_json::from_str(
            r##"{
            "component": "Input",
            "variants": ["default"],
            "states": ["normal", "focused", "disabled"],
            "sizes": ["sm", "md", "lg"],
            "variantProps": [
                { "name": "background", "type": "color",
                  "defaults": { "default": { "normal": "color.surface" } } },
                { "name": "borderColor", "type": "color",
                  "defaults": { "default": { "normal": "color.border" } } }
            ],
            "sizeProps": [
                { "name": "borderRadius", "type": "float",
                  "defaults": { "default": 8.0 } }
            ]
        }"##,
        )
        .unwrap()
    }

    #[test]
    fn pure_extends_inherits_all_props_and_lists() {
        let child: PluginDefinition =
            serde_json::from_str(r##"{ "component": "Search", "extends": "Input" }"##).unwrap();
        assert_eq!(child.extends.as_deref(), Some("Input"));
        let mut by_name = std::collections::HashMap::new();
        by_name.insert("Input".to_string(), make_parent());
        let mut visited = HashSet::new();
        let resolved = resolve_extends_chain(child, &by_name, &mut visited).unwrap();
        assert!(resolved.extends.is_none()); // fully resolved
        assert_eq!(resolved.component, "Search");
        assert_eq!(resolved.variants, vec!["default"]);
        assert_eq!(resolved.states, vec!["normal", "focused", "disabled"]);
        assert_eq!(resolved.sizes, vec!["sm", "md", "lg"]);
        assert_eq!(resolved.variant_props.len(), 2);
        assert_eq!(resolved.variant_props[0].name, "background");
        assert_eq!(resolved.size_props.len(), 1);
    }

    #[test]
    fn child_overrides_a_prop_by_name_and_adds_new_ones() {
        // Child overrides borderRadius default and adds a Dropdown-only chevronSize.
        let child: PluginDefinition = serde_json::from_str(
            r##"{
            "component": "Dropdown",
            "extends": "Input",
            "sizeProps": [
                { "name": "borderRadius", "type": "float",
                  "defaults": { "default": 12.0 } },
                { "name": "chevronSize", "type": "float",
                  "defaults": { "default": 16.0 } }
            ]
        }"##,
        )
        .unwrap();
        let mut by_name = std::collections::HashMap::new();
        by_name.insert("Input".to_string(), make_parent());
        let mut visited = HashSet::new();
        let resolved = resolve_extends_chain(child, &by_name, &mut visited).unwrap();
        // borderRadius overridden (12, not the parent's 8), chevronSize appended.
        assert_eq!(resolved.size_props.len(), 2);
        let radius = resolved.size_props.iter().find(|p| p.name == "borderRadius").unwrap();
        assert_eq!(radius.defaults.values["default"].get_direct(), Some(&TokenOrValue::Float(12.0)));
        assert!(resolved.size_props.iter().any(|p| p.name == "chevronSize"));
        // Parent's variantProps still inherited.
        assert_eq!(resolved.variant_props.len(), 2);
    }

    #[test]
    fn full_pipeline_preserves_inherited_data_through_normalize() {
        // Regression: normalize_plugin_variant_schema / _states_against_default
        // were clobbering the resolved Search plugin because they used the slim
        // search.json default as the schema reference. Pipeline = raw + extends
        // resolve + normalize, mirroring load_all_plugins().
        let input_raw: PluginDefinition =
            serde_json::from_str(&default_plugin_json("input").expect("input.json missing")).unwrap();
        let search_raw: PluginDefinition =
            serde_json::from_str(&default_plugin_json("search").expect("search.json missing")).unwrap();
        let raw = vec![
            (
                BuiltinComponentSpec { key: "input", component: "Input", source_file: "input.slint" },
                input_raw,
            ),
            (
                BuiltinComponentSpec { key: "search", component: "Search", source_file: "search.slint" },
                search_raw,
            ),
        ];
        let resolved = apply_inheritance(raw).unwrap();
        let normalized: Vec<_> =
            resolved.into_iter().map(|(spec, p)| (spec, normalize_plugin_definition(spec.key, p))).collect();
        let (_, search) = normalized.iter().find(|(s, _)| s.key == "search").unwrap();
        let (_, input) = normalized.iter().find(|(s, _)| s.key == "input").unwrap();
        assert!(!search.variants.is_empty(), "Search lost its variants");
        assert!(!search.states.is_empty(), "Search lost its states");
        assert!(!search.sizes.is_empty(), "Search lost its sizes");
        assert!(!search.variant_props.is_empty(), "Search lost its variantProps after normalize",);
        assert!(!search.size_props.is_empty(), "Search lost its sizeProps after normalize",);
        assert_eq!(search.variants, input.variants);
        assert_eq!(search.states, input.states);
        assert_eq!(search.sizes, input.sizes);
        assert_eq!(search.variant_props.len(), input.variant_props.len());
        assert_eq!(search.size_props.len(), input.size_props.len());
    }

    #[test]
    fn loaded_search_themedata_marks_per_size_props_as_overrides() {
        // After raw → inherit → normalize, Search inherits Input's per-size
        // defaults. ComponentThemeData::from_plugin should then see those
        // explicit per-size keys and mark each (size, prop) as overridden.
        let input_raw: PluginDefinition =
            serde_json::from_str(&default_plugin_json("input").expect("input.json missing")).unwrap();
        let search_raw: PluginDefinition =
            serde_json::from_str(&default_plugin_json("search").expect("search.json missing")).unwrap();
        let raw = vec![
            (
                BuiltinComponentSpec { key: "input", component: "Input", source_file: "input.slint" },
                input_raw,
            ),
            (
                BuiltinComponentSpec { key: "search", component: "Search", source_file: "search.slint" },
                search_raw,
            ),
        ];
        let resolved = apply_inheritance(raw).unwrap();
        let normalized: Vec<_> =
            resolved.into_iter().map(|(spec, p)| (spec, normalize_plugin_definition(spec.key, p))).collect();
        let (_, search_plugin) = normalized.iter().find(|(s, _)| s.key == "search").unwrap();

        // Sanity: Search inherited Input's size_props.
        assert!(!search_plugin.size_props.is_empty(), "Search should have inherited Input's size_props");
        // For every size prop, every concrete size key in plugin.sizes should
        // either match a key in defaults or not (let's just print).
        for prop_def in &search_plugin.size_props {
            for size in &search_plugin.sizes {
                let present = prop_def.defaults.values.contains_key(size);
                eprintln!(
                    "search/{}/{}: defaults.contains_key={} (defaults keys: {:?})",
                    size,
                    prop_def.name,
                    present,
                    prop_def.defaults.values.keys().collect::<Vec<_>>()
                );
            }
        }

        // Now run from_plugin on Search and inspect size_overrides.
        let data = crate::plugin::ComponentThemeData::from_plugin(search_plugin, &TokenStore::default());
        for size in &search_plugin.sizes {
            let overrides = data.size_overrides.get(size).expect("size_overrides[size]");
            eprintln!("search/{}: overrides = {:?}", size, overrides);
        }
        // The assertion we care about: lg should have border-radius listed.
        let lg_overrides = data.size_overrides.get("lg").unwrap();
        assert!(
            lg_overrides.contains("border-radius"),
            "Search/lg should mark border-radius as overridden because Input's JSON has a 'lg' key for it"
        );
    }

    #[test]
    fn real_search_plugin_extends_input_fully() {
        // The default search.json should be `{ component: "Search", extends: "Input" }`
        // and resolve via apply_inheritance to Input's full prop set.
        let raw = vec![
            (
                BuiltinComponentSpec { key: "input", component: "Input", source_file: "input.slint" },
                serde_json::from_str::<PluginDefinition>(
                    &default_plugin_json("input").expect("input.json missing"),
                )
                .unwrap(),
            ),
            (
                BuiltinComponentSpec { key: "search", component: "Search", source_file: "search.slint" },
                serde_json::from_str::<PluginDefinition>(
                    &default_plugin_json("search").expect("search.json missing"),
                )
                .unwrap(),
            ),
        ];
        let resolved = apply_inheritance(raw).unwrap();
        let (_, input_plugin) = resolved.iter().find(|(s, _)| s.key == "input").unwrap();
        let (_, search_plugin) = resolved.iter().find(|(s, _)| s.key == "search").unwrap();
        assert!(search_plugin.extends.is_none(), "extends should be resolved");
        assert_eq!(search_plugin.component, "Search");
        // Search should now have ALL of Input's prop definitions.
        assert_eq!(
            search_plugin.variant_props.len(),
            input_plugin.variant_props.len(),
            "Search should inherit every variantProp from Input"
        );
        assert_eq!(
            search_plugin.size_props.len(),
            input_plugin.size_props.len(),
            "Search should inherit every sizeProp from Input"
        );
        assert_eq!(search_plugin.variants, input_plugin.variants);
        assert_eq!(search_plugin.states, input_plugin.states);
        assert_eq!(search_plugin.sizes, input_plugin.sizes);
    }

    #[test]
    fn extends_unknown_parent_errors() {
        let child: PluginDefinition =
            serde_json::from_str(r##"{ "component": "Foo", "extends": "Nonexistent" }"##).unwrap();
        let by_name = std::collections::HashMap::new();
        let mut visited = HashSet::new();
        let err = resolve_extends_chain(child, &by_name, &mut visited).unwrap_err();
        assert!(err.contains("unknown plugin"), "got {err}");
    }

    #[test]
    fn extends_cycle_errors() {
        let mut by_name: std::collections::HashMap<String, PluginDefinition> =
            std::collections::HashMap::new();
        by_name.insert(
            "A".to_string(),
            serde_json::from_str(r##"{ "component": "A", "extends": "B" }"##).unwrap(),
        );
        by_name.insert(
            "B".to_string(),
            serde_json::from_str(r##"{ "component": "B", "extends": "A" }"##).unwrap(),
        );
        let mut visited = HashSet::new();
        let err = resolve_extends_chain(by_name["A"].clone(), &by_name, &mut visited).unwrap_err();
        assert!(err.contains("cycle"), "got {err}");
    }

    #[test]
    fn test_default_button_plugin_parses() {
        let json = default_plugin_json("button").expect("missing default button plugin");
        let plugin: PluginDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(plugin.component, "Button");
        assert_eq!(plugin.variants.len(), 5);
        assert_eq!(plugin.states.len(), 4);
        assert_eq!(plugin.sizes.len(), 3);
        assert_eq!(plugin.variant_props.len(), 7);
        assert_eq!(plugin.size_props.len(), 7);
    }

    #[test]
    fn test_all_builtin_default_plugins_parse() {
        for spec in builtin_component_specs() {
            let json = default_plugin_json(spec.key)
                .unwrap_or_else(|| panic!("missing default plugin for {}", spec.key));
            let plugin: PluginDefinition = serde_json::from_str(&json)
                .unwrap_or_else(|err| panic!("failed to parse default plugin {}: {}", spec.key, err));
            assert_eq!(plugin.component, spec.component);
        }
    }

    #[test]
    fn test_normalize_plugin_definition_migrates_default_size_axis() {
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "Input".to_string(),
            variants: vec!["default".to_string(), "filled".to_string(), "outlined".to_string()],
            states: vec!["normal".to_string(), "focused".to_string(), "disabled".to_string()],
            sizes: vec!["default".to_string()],
            variant_props: Vec::new(),
            size_props: vec![PropDefinition {
                name: "field-height".to_string(),
                display_name: Some("Field Height".to_string()),
                prop_type: "float".to_string(),
                min: Some(0.0),
                max: Some(128.0),
                step: Some(1.0),
                defaults: PropDefaults {
                    values: [("default".to_string(), DefaultValue::Direct(TokenOrValue::Float(44.0)))]
                        .into_iter()
                        .collect(),
                },
            }],
        };

        let normalized = normalize_plugin_definition("input", plugin);
        assert_eq!(normalized.sizes, vec!["sm", "md", "lg"]);

        let field_height = normalized
            .size_props
            .iter()
            .find(|prop| prop.name == "field-height")
            .expect("missing field-height size prop");

        assert_eq!(
            field_height.defaults.values.get("sm"),
            Some(&DefaultValue::Direct(TokenOrValue::Float(44.0)))
        );
        assert_eq!(
            field_height.defaults.values.get("md"),
            Some(&DefaultValue::Direct(TokenOrValue::Float(44.0)))
        );
        assert_eq!(
            field_height.defaults.values.get("lg"),
            Some(&DefaultValue::Direct(TokenOrValue::Float(44.0)))
        );
    }

    #[test]
    fn test_normalize_plugin_definition_collapses_card_sizes_to_default_using_md() {
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "Card".to_string(),
            variants: vec!["default".to_string()],
            states: vec!["normal".to_string()],
            sizes: vec!["sm".to_string(), "md".to_string(), "lg".to_string()],
            variant_props: Vec::new(),
            size_props: vec![PropDefinition {
                name: "card-padding".to_string(),
                display_name: Some("Card Padding".to_string()),
                prop_type: "float".to_string(),
                min: Some(0.0),
                max: Some(128.0),
                step: Some(1.0),
                defaults: PropDefaults {
                    values: [
                        ("sm".to_string(), DefaultValue::Direct(TokenOrValue::Float(12.0))),
                        ("md".to_string(), DefaultValue::Direct(TokenOrValue::Float(16.0))),
                        ("lg".to_string(), DefaultValue::Direct(TokenOrValue::Float(20.0))),
                    ]
                    .into_iter()
                    .collect(),
                },
            }],
        };

        let normalized = normalize_plugin_definition("card", plugin);
        assert_eq!(normalized.sizes, vec!["default"]);

        let padding = normalized
            .size_props
            .iter()
            .find(|prop| prop.name == "card-padding")
            .expect("missing card-padding size prop");

        assert_eq!(
            padding.defaults.values.get("default"),
            Some(&DefaultValue::Direct(TokenOrValue::Float(16.0)))
        );
    }

    #[test]
    fn test_normalize_plugin_definition_collapses_image_to_width_and_height_only() {
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "Image".to_string(),
            variants: vec!["default".to_string()],
            states: vec!["normal".to_string(), "focused".to_string()],
            sizes: vec!["sm".to_string(), "md".to_string(), "lg".to_string()],
            variant_props: Vec::new(),
            size_props: vec![
                PropDefinition {
                    name: "font-size".to_string(),
                    display_name: Some("Font Size".to_string()),
                    prop_type: "float".to_string(),
                    min: Some(0.0),
                    max: Some(64.0),
                    step: Some(1.0),
                    defaults: PropDefaults {
                        values: [
                            ("sm".to_string(), DefaultValue::Direct(TokenOrValue::Float(14.0))),
                            ("md".to_string(), DefaultValue::Direct(TokenOrValue::Float(16.0))),
                            ("lg".to_string(), DefaultValue::Direct(TokenOrValue::Float(18.0))),
                        ]
                        .into_iter()
                        .collect(),
                    },
                },
                PropDefinition {
                    name: "frame-height".to_string(),
                    display_name: Some("Frame Height".to_string()),
                    prop_type: "float".to_string(),
                    min: Some(0.0),
                    max: Some(512.0),
                    step: Some(1.0),
                    defaults: PropDefaults {
                        values: [
                            ("sm".to_string(), DefaultValue::Direct(TokenOrValue::Float(180.0))),
                            ("md".to_string(), DefaultValue::Direct(TokenOrValue::Float(220.0))),
                            ("lg".to_string(), DefaultValue::Direct(TokenOrValue::Float(280.0))),
                        ]
                        .into_iter()
                        .collect(),
                    },
                },
            ],
        };

        let normalized = normalize_plugin_definition("image", plugin);
        assert_eq!(normalized.states, vec!["normal"]);
        assert_eq!(normalized.sizes, vec!["default"]);
        assert_eq!(normalized.variant_props.len(), 0);
        assert_eq!(normalized.size_props.len(), 2);
        assert!(normalized.size_props.iter().any(|prop| prop.name == "frame-width"));
        assert!(normalized.size_props.iter().any(|prop| prop.name == "frame-height"));

        let frame_height = normalized
            .size_props
            .iter()
            .find(|prop| prop.name == "frame-height")
            .expect("missing frame-height size prop");

        assert_eq!(
            frame_height.defaults.values.get("default"),
            Some(&DefaultValue::Direct(TokenOrValue::Float(192.0)))
        );
    }

    #[test]
    fn test_normalize_plugin_definition_removes_loading_state() {
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "Button".to_string(),
            variants: vec!["primary".to_string()],
            states: vec!["normal".to_string(), "loading".to_string(), "disabled".to_string()],
            sizes: vec!["small".to_string()],
            variant_props: vec![PropDefinition {
                name: "background".to_string(),
                display_name: Some("Background".to_string()),
                prop_type: "color".to_string(),
                min: None,
                max: None,
                step: None,
                defaults: PropDefaults {
                    values: [(
                        "primary".to_string(),
                        DefaultValue::Nested(
                            [
                                ("normal".to_string(), TokenOrValue::String("color.primary".to_string())),
                                (
                                    "loading".to_string(),
                                    TokenOrValue::String("color.primary.light".to_string()),
                                ),
                            ]
                            .into_iter()
                            .collect(),
                        ),
                    )]
                    .into_iter()
                    .collect(),
                },
            }],
            size_props: Vec::new(),
        };

        let normalized = normalize_plugin_definition("button", plugin);
        assert!(!normalized.states.iter().any(|state| state == "loading"));

        let background = normalized
            .variant_props
            .iter()
            .find(|prop| prop.name == "background")
            .expect("missing background prop");

        let variant_defaults = background.defaults.values.get("primary").expect("missing primary defaults");

        match variant_defaults {
            DefaultValue::Nested(states) => assert!(!states.contains_key("loading")),
            DefaultValue::Direct(_) => panic!("expected nested defaults"),
        }
    }

    #[test]
    fn test_normalize_legacy_variant_names_does_not_invent_focused_state() {
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "Spinner".to_string(),
            variants: vec!["primary".to_string()],
            states: vec!["normal".to_string()],
            sizes: vec!["md".to_string()],
            variant_props: vec![PropDefinition {
                name: "accent-color-override".to_string(),
                display_name: Some("Accent".to_string()),
                prop_type: "color".to_string(),
                min: None,
                max: None,
                step: None,
                defaults: variant_state_defaults(&[("primary", &[("normal", "color.primary")])]),
            }],
            size_props: Vec::new(),
        };

        let normalized = normalize_legacy_variant_names(plugin);
        assert_eq!(normalized.states, vec!["normal".to_string()]);

        let accent = normalized
            .variant_props
            .iter()
            .find(|prop| prop.name == "accent-color-override")
            .expect("missing accent-color-override prop");
        let defaults = accent.defaults.values.get("primary").expect("missing primary defaults");

        match defaults {
            DefaultValue::Nested(states) => {
                assert_eq!(states.len(), 1);
                assert!(states.contains_key("normal"));
                assert!(!states.contains_key("focused"));
            }
            DefaultValue::Direct(_) => panic!("expected nested defaults"),
        }
    }

    #[test]
    fn test_normalize_plugin_definition_removes_slider_focus_ring_prop() {
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "Slider".to_string(),
            variants: vec!["default".to_string()],
            states: vec!["normal".to_string(), "focused".to_string(), "disabled".to_string()],
            sizes: vec!["sm".to_string(), "md".to_string(), "lg".to_string()],
            variant_props: vec![
                PropDefinition {
                    name: "track-color".to_string(),
                    display_name: Some("Track Color".to_string()),
                    prop_type: "color".to_string(),
                    min: None,
                    max: None,
                    step: None,
                    defaults: variant_state_defaults(&[(
                        "default",
                        &[
                            ("normal", "color.secondary"),
                            ("focused", "color.secondary"),
                            ("disabled", "color.border"),
                        ],
                    )]),
                },
                PropDefinition {
                    name: "focus-ring-color".to_string(),
                    display_name: Some("Focus Ring".to_string()),
                    prop_type: "color".to_string(),
                    min: None,
                    max: None,
                    step: None,
                    defaults: variant_state_defaults(&[(
                        "default",
                        &[
                            ("normal", "color.transparent"),
                            ("focused", "color.primary.light"),
                            ("disabled", "color.transparent"),
                        ],
                    )]),
                },
            ],
            size_props: Vec::new(),
        };

        let normalized = normalize_plugin_definition("slider", plugin);
        assert!(normalized.variant_props.iter().all(|prop| prop.name != "focus-ring-color"));
        assert!(normalized.variant_props.iter().any(|prop| prop.name == "track-color"));
    }

    #[test]
    fn test_normalize_plugin_definition_removes_icon_focused_state() {
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "Icon".to_string(),
            variants: vec!["default".to_string()],
            states: vec!["normal".to_string(), "focused".to_string()],
            sizes: vec!["sm".to_string(), "md".to_string(), "lg".to_string()],
            variant_props: vec![PropDefinition {
                name: "icon-color".to_string(),
                display_name: Some("Color".to_string()),
                prop_type: "color".to_string(),
                min: None,
                max: None,
                step: None,
                defaults: variant_state_defaults(&[(
                    "default",
                    &[("normal", "color.foreground"), ("focused", "color.primary")],
                )]),
            }],
            size_props: Vec::new(),
        };

        let normalized = normalize_plugin_definition("icon", plugin);
        assert_eq!(normalized.states, vec!["normal".to_string()]);

        let icon_color = normalized
            .variant_props
            .iter()
            .find(|prop| prop.name == "icon-color")
            .expect("missing icon-color prop");
        let defaults =
            icon_color.defaults.values.get("default").expect("missing default icon-color defaults");

        match defaults {
            DefaultValue::Nested(states) => {
                assert_eq!(states.len(), 1);
                assert!(states.contains_key("normal"));
                assert!(!states.contains_key("focused"));
            }
            DefaultValue::Direct(_) => panic!("expected nested defaults"),
        }
    }

    #[test]
    fn test_repo_default_non_interactive_plugins_are_normal_only() {
        for name in ["icon", "image", "divider", "dialog", "sheet", "toast", "progress_bar", "spinner"] {
            let plugin = repo_default_plugin_definition(name)
                .unwrap_or_else(|| panic!("missing repo default plugin definition for {}", name));
            assert_eq!(plugin.states, vec!["normal".to_string()], "{}", name);
        }
    }

    #[test]
    fn test_normalize_plugin_definition_updates_legacy_shipped_size_defaults() {
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "Link".to_string(),
            variants: vec!["default".to_string()],
            states: vec!["normal".to_string(), "disabled".to_string()],
            sizes: vec!["sm".to_string(), "md".to_string(), "lg".to_string()],
            variant_props: Vec::new(),
            size_props: vec![PropDefinition {
                name: "row-height".to_string(),
                display_name: Some("Row Height".to_string()),
                prop_type: "float".to_string(),
                min: Some(0.0),
                max: Some(128.0),
                step: Some(1.0),
                defaults: PropDefaults {
                    values: [
                        ("sm".to_string(), DefaultValue::Direct(TokenOrValue::Float(28.0))),
                        ("md".to_string(), DefaultValue::Direct(TokenOrValue::Float(32.0))),
                        ("lg".to_string(), DefaultValue::Direct(TokenOrValue::Float(36.0))),
                    ]
                    .into_iter()
                    .collect(),
                },
            }],
        };

        let normalized = normalize_plugin_definition("link", plugin);
        let row_height = normalized
            .size_props
            .iter()
            .find(|prop| prop.name == "row-height")
            .expect("missing row-height size prop");

        assert_eq!(
            row_height.defaults.values.get("sm"),
            Some(&DefaultValue::Direct(TokenOrValue::Float(36.0)))
        );
        assert_eq!(
            row_height.defaults.values.get("md"),
            Some(&DefaultValue::Direct(TokenOrValue::Float(44.0)))
        );
        assert_eq!(
            row_height.defaults.values.get("lg"),
            Some(&DefaultValue::Direct(TokenOrValue::Float(52.0)))
        );
    }

    #[test]
    fn test_normalize_plugin_definition_removes_legacy_chip_height_controls() {
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "Chip".to_string(),
            variants: vec!["filled".to_string(), "outlined".to_string(), "soft".to_string()],
            states: vec![
                "normal".to_string(),
                "focused".to_string(),
                "pressed".to_string(),
                "disabled".to_string(),
            ],
            sizes: vec!["sm".to_string(), "md".to_string(), "lg".to_string()],
            variant_props: Vec::new(),
            size_props: vec![
                PropDefinition {
                    name: "chip-height".to_string(),
                    display_name: Some("Chip Height".to_string()),
                    prop_type: "float".to_string(),
                    min: Some(0.0),
                    max: Some(160.0),
                    step: Some(1.0),
                    defaults: float_size_defaults(&[("sm", 24.0), ("md", 28.0), ("lg", 32.0)]),
                },
                PropDefinition {
                    name: "row-height".to_string(),
                    display_name: Some("Row Height".to_string()),
                    prop_type: "float".to_string(),
                    min: Some(0.0),
                    max: Some(160.0),
                    step: Some(1.0),
                    defaults: float_size_defaults(&[("sm", 36.0), ("md", 44.0), ("lg", 52.0)]),
                },
            ],
        };

        let normalized = normalize_plugin_definition("chip", plugin);

        assert!(normalized.size_props.iter().any(|prop| prop.name == "padding-vertical"));
        assert!(!normalized.size_props.iter().any(|prop| prop.name == "chip-height"));
        assert!(!normalized.size_props.iter().any(|prop| prop.name == "row-height"));
    }

    #[test]
    fn test_normalize_plugin_definition_migrates_progress_bar_variants_and_props() {
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "ProgressBar".to_string(),
            variants: vec![
                "primary".to_string(),
                "neutral".to_string(),
                "success".to_string(),
                "warning".to_string(),
                "danger".to_string(),
            ],
            states: vec!["normal".to_string()],
            sizes: vec!["sm".to_string(), "md".to_string(), "lg".to_string()],
            variant_props: vec![
                PropDefinition {
                    name: "track-color".to_string(),
                    display_name: Some("Track Color".to_string()),
                    prop_type: "color".to_string(),
                    min: None,
                    max: None,
                    step: None,
                    defaults: PropDefaults {
                        values: [
                            (
                                "primary".to_string(),
                                DefaultValue::Nested(
                                    [(
                                        "normal".to_string(),
                                        TokenOrValue::String("color.secondary".to_string()),
                                    )]
                                    .into_iter()
                                    .collect(),
                                ),
                            ),
                            (
                                "neutral".to_string(),
                                DefaultValue::Nested(
                                    [(
                                        "normal".to_string(),
                                        TokenOrValue::String("color.secondary".to_string()),
                                    )]
                                    .into_iter()
                                    .collect(),
                                ),
                            ),
                        ]
                        .into_iter()
                        .collect(),
                    },
                },
                PropDefinition {
                    name: "stripe-color".to_string(),
                    display_name: Some("Stripe Color".to_string()),
                    prop_type: "color".to_string(),
                    min: None,
                    max: None,
                    step: None,
                    defaults: PropDefaults {
                        values: [(
                            "primary".to_string(),
                            DefaultValue::Nested(
                                [("normal".to_string(), TokenOrValue::String("#ffffff4d".to_string()))]
                                    .into_iter()
                                    .collect(),
                            ),
                        )]
                        .into_iter()
                        .collect(),
                    },
                },
            ],
            size_props: Vec::new(),
        };

        let normalized = normalize_plugin_definition("progress_bar", plugin);
        assert!(normalized.variants.iter().any(|variant| variant == "default"));
        assert!(normalized.variants.iter().any(|variant| variant == "secondary"));
        assert!(!normalized.variants.iter().any(|variant| variant == "neutral"));
        assert!(normalized.variant_props.iter().any(|prop| prop.name == "label-color"));
        assert!(!normalized.variant_props.iter().any(|prop| prop.name == "stripe-color"));

        let track_color = normalized
            .variant_props
            .iter()
            .find(|prop| prop.name == "track-color")
            .expect("missing track-color prop");

        assert!(track_color.defaults.values.contains_key("default"));
        assert!(track_color.defaults.values.contains_key("secondary"));
    }

    #[test]
    fn test_normalize_plugin_definition_removes_textarea_underlined_variant() {
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "TextArea".to_string(),
            variants: vec![
                "flat".to_string(),
                "bordered".to_string(),
                "faded".to_string(),
                "underlined".to_string(),
            ],
            states: vec!["normal".to_string(), "focused".to_string(), "disabled".to_string()],
            sizes: vec!["sm".to_string(), "md".to_string(), "lg".to_string()],
            variant_props: vec![PropDefinition {
                name: "background".to_string(),
                display_name: Some("Background".to_string()),
                prop_type: "color".to_string(),
                min: None,
                max: None,
                step: None,
                defaults: variant_state_defaults(&[
                    (
                        "flat",
                        &[
                            ("normal", "color.secondary"),
                            ("focused", "color.secondary"),
                            ("disabled", "color.secondary"),
                        ],
                    ),
                    (
                        "bordered",
                        &[
                            ("normal", "color.surface"),
                            ("focused", "color.surface"),
                            ("disabled", "color.surface"),
                        ],
                    ),
                    (
                        "faded",
                        &[
                            ("normal", "color.secondary"),
                            ("focused", "color.secondary"),
                            ("disabled", "color.secondary"),
                        ],
                    ),
                    (
                        "underlined",
                        &[
                            ("normal", "color.transparent"),
                            ("focused", "color.transparent"),
                            ("disabled", "color.transparent"),
                        ],
                    ),
                ]),
            }],
            size_props: Vec::new(),
        };

        let normalized = normalize_plugin_definition("textarea", plugin);
        assert_eq!(
            normalized.variants,
            vec!["flat".to_string(), "bordered".to_string(), "faded".to_string()]
        );
        let background = normalized
            .variant_props
            .iter()
            .find(|prop| prop.name == "background")
            .expect("missing background prop");
        assert!(!background.defaults.values.contains_key("underlined"));
    }

    #[test]
    fn test_normalize_plugin_definition_dedupes_duplicate_props() {
        let duplicate_prop = PropDefinition {
            name: "control-height".to_string(),
            display_name: Some("Control Height".to_string()),
            prop_type: "float".to_string(),
            min: Some(0.0),
            max: Some(160.0),
            step: Some(1.0),
            defaults: float_size_defaults(&[("sm", 36.0), ("md", 44.0), ("lg", 52.0)]),
        };
        let plugin = PluginDefinition {
            extends: None,
            parent_key: None,
            component: "Tabs".to_string(),
            variants: vec![
                "solid".to_string(),
                "bordered".to_string(),
                "light".to_string(),
                "underlined".to_string(),
            ],
            states: vec![
                "normal".to_string(),
                "focused".to_string(),
                "pressed".to_string(),
                "disabled".to_string(),
            ],
            sizes: vec!["sm".to_string(), "md".to_string(), "lg".to_string()],
            variant_props: vec![],
            size_props: vec![duplicate_prop.clone(), duplicate_prop],
        };

        let normalized = normalize_plugin_definition("tabs", plugin);

        assert_eq!(normalized.size_props.iter().filter(|prop| prop.name == "control-height").count(), 1);
    }

    #[test]
    fn test_default_tokens_parses() {
        let json = default_tokens_json();
        let tokens = TokenStore::from_json(&json).unwrap();
        assert!(tokens.resolve("color.primary").is_some());
        assert!(tokens.resolve("color.muted").is_some());
        assert!(tokens.resolve("color.border").is_some());
        assert!(tokens.resolve("color.success").is_some());
        assert!(tokens.resolve("color.warning").is_some());
        assert!(tokens.resolve("spacing.md").is_some());
    }
}
