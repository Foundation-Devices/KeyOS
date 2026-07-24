// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared plugin-schema types + the `<key>_theme.slint` emitter.
//!
//! This module is the single source of the component-theme generator, used by
//! two callers:
//!   * `slintthemegen` (theme-editor) — emits the shared default `ui2/components/ui/<key>_theme.slint` from
//!     `defaults/components/<key>.schema.json`, with no app overrides.
//!   * `foundation-themes` (the `foundation build` theme-compile step) — emits a *per-app*
//!     `<key>_theme.slint` from the same schema plus the app's resolved theme `components.<key>` values.
//!
//! Generation rule (Approach X): a token-backed theme value becomes a reference
//! to the shared `Theme` global (live + dark/light-aware), e.g.
//! `Theme.color-primary`; a literal (hex color / number) is inlined. Component
//! schemas may still carry defaults as a migration seed, but resolved theme JSON
//! values win and follow the editor's fixed inheritance chain: state falls back
//! to Normal, concrete sizes fall back to Common, and later variants fall back
//! to the first variant.

#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// REUSE-IgnoreStart -- this constant is the header emitted into generated
// *_theme.slint files; it is data, not this file's own license.
const HEADER: &str = "// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>\n\
// SPDX-License-Identifier: MIT OR Apache-2.0\n";
// REUSE-IgnoreEnd

/// Token categories exposed on the Slint `Theme` global (see `theme.slint`).
/// A prop referencing one of these becomes `Theme.<kebab(cat)>-<key>`.
const EXPOSED_TOKEN_CATEGORIES: &[&str] = &[
    "font",
    "fontSize",
    "fontWeight",
    "borderWidth",
    "controlSize",
    "choiceControlSize",
    "switchSize",
    "iconSize",
    "radius",
    "controlRadius",
    "controlPaddingInline",
    "spacing",
];

// ===========================================================================
// Plugin schema (parsed from defaults/components/<key>.schema.json)
// ===========================================================================

/// A complete plugin definition loaded from JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDefinition {
    /// Component name (e.g., "Button")
    pub component: String,

    /// Optional parent component to inherit variants/states/sizes and prop
    /// definitions from. Child lists/props override the parent's by name; any
    /// prop the child omits is inherited verbatim from the parent. This lets
    /// e.g. Search/Dropdown reuse Input's full theme prop set with just
    /// `{"component": "Search", "extends": "Input"}`. The loader clears this
    /// after the inheritance merge (the resolved plugin is self-contained), but
    /// stashes the same value in `parent_key` for the runtime live-cascade
    /// (so editing Input's defaults can still update Search/Dropdown).
    #[serde(default)]
    pub extends: Option<String>,

    /// Runtime-only: the component this plugin extended, preserved after the
    /// inheritance merge clears `extends`. Used by `ComponentThemeData` to
    /// look up the parent at edit time and cascade changes.
    #[serde(default, skip)]
    pub parent_key: Option<String>,

    /// Available variants (e.g., ["primary", "secondary", "tertiary", "ghost", "danger"])
    #[serde(default)]
    pub variants: Vec<String>,

    /// Available states (e.g., ["normal", "focused", "pressed", "disabled"])
    #[serde(default)]
    pub states: Vec<String>,

    /// Available sizes (e.g., ["small", "medium", "large"])
    #[serde(default)]
    pub sizes: Vec<String>,

    /// Properties that vary by variant and state
    #[serde(default)]
    pub variant_props: Vec<PropDefinition>,

    /// Properties that vary by size only
    #[serde(default)]
    pub size_props: Vec<PropDefinition>,
}

/// Definition of a single property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropDefinition {
    /// Property name (e.g., "background", "fontSize")
    pub name: String,

    /// Display name for UI (e.g., "Background", "Font Size")
    #[serde(default)]
    pub display_name: Option<String>,

    /// Property type: "color", "float", "int", "bool", "string"
    #[serde(rename = "type")]
    pub prop_type: String,

    /// Minimum value (for numeric types)
    #[serde(default)]
    pub min: Option<f32>,

    /// Maximum value (for numeric types)
    #[serde(default)]
    pub max: Option<f32>,

    /// Step value (for numeric types)
    #[serde(default)]
    pub step: Option<f32>,

    /// Default values.
    /// For variant props: { "variant": { "state": "value_or_token" } }
    /// For size props: { "size": "value_or_token" }
    #[serde(default)]
    pub defaults: PropDefaults,
}

impl PropDefinition {
    /// Get the display name, falling back to the property name
    pub fn display_name(&self) -> &str { self.display_name.as_deref().unwrap_or(&self.name) }

    /// Get min value with default
    pub fn min_value(&self) -> f32 { self.min.unwrap_or(0.0) }

    /// Get max value with default
    pub fn max_value(&self) -> f32 { self.max.unwrap_or(100.0) }

    /// Get step value with default
    pub fn step_value(&self) -> f32 { self.step.unwrap_or(1.0) }
}

/// Default values for a property.
/// Can be nested (variant -> state -> value) or flat (size -> value).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PropDefaults {
    /// Outer key is variant or size, inner is state (for variant props) or value (for size props)
    pub values: HashMap<String, DefaultValue>,
}

/// A default value can be:
/// - A direct value (for size props)
/// - A nested map of state -> value (for variant props)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DefaultValue {
    /// Nested: { "normal": "color.primary", "pressed": "color.primary.dark" }
    Nested(HashMap<String, TokenOrValue>),
    /// Direct value (token reference or literal)
    Direct(TokenOrValue),
}

impl DefaultValue {
    /// Get value for a specific state (for variant props)
    pub fn get_state(&self, state: &str) -> Option<&TokenOrValue> {
        match self {
            DefaultValue::Nested(map) => map.get(state),
            DefaultValue::Direct(val) => Some(val),
        }
    }

    /// Get direct value (for size props)
    pub fn get_direct(&self) -> Option<&TokenOrValue> {
        match self {
            DefaultValue::Direct(val) => Some(val),
            DefaultValue::Nested(_) => None,
        }
    }
}

/// A value that can be either a token reference or a literal value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TokenOrValue {
    /// String value - could be a token reference like "color.primary" or a hex color like "#ff0000"
    String(String),
    /// Float value
    Float(f64),
    /// Integer value
    Int(i64),
    /// Boolean value
    Bool(bool),
}

impl TokenOrValue {
    /// Check if this is a token reference (contains a dot and doesn't start with #)
    pub fn is_token_reference(&self) -> bool {
        match self {
            TokenOrValue::String(s) => s.contains('.') && !s.starts_with('#'),
            _ => false,
        }
    }
}

// ===========================================================================
// App overrides (parsed from resources/theme.json `components.<key>`)
// ===========================================================================

/// Per-app overrides for one component, deserialized from a theme.json
/// `components.<key>` object. Each present entry replaces the schema default
/// for that exact variant/state/prop (variant props) or size/prop (size props).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentOverrides {
    /// variant -> state -> prop name -> value
    #[serde(default)]
    pub variant_props: HashMap<String, HashMap<String, HashMap<String, TokenOrValue>>>,
    /// size -> prop name -> value
    #[serde(default)]
    pub size_props: HashMap<String, HashMap<String, TokenOrValue>>,
}

impl ComponentOverrides {
    fn variant_exact(&self, variant: &str, state: &str, prop: &str) -> Option<&TokenOrValue> {
        self.variant_props.get(variant)?.get(state)?.get(prop)
    }

    /// Size override with the same Common cascade as the schema: a per-size
    /// override wins, else a `common` override applies to every size. Legacy
    /// `default` is accepted for old theme JSON.
    fn size(&self, size: &str, prop: &str) -> Option<&TokenOrValue> {
        self.size_props
            .get(size)
            .and_then(|m| m.get(prop))
            .or_else(|| self.size_props.get("common").and_then(|m| m.get(prop)))
            .or_else(|| self.size_props.get("default").and_then(|m| m.get(prop)))
    }
}

fn variant_override<'a>(
    plugin: &'a PluginDefinition,
    overrides: Option<&'a ComponentOverrides>,
    variant: &str,
    state: &str,
    prop: &str,
) -> Option<&'a TokenOrValue> {
    let overrides = overrides?;
    if let Some(value) = overrides.variant_exact(variant, state, prop) {
        return Some(value);
    }
    if state != "normal" {
        if let Some(value) = overrides.variant_exact(variant, "normal", prop) {
            return Some(value);
        }
    }
    let common_variant = plugin.variants.first()?;
    if variant != common_variant {
        if let Some(value) = overrides.variant_exact(common_variant, "normal", prop) {
            return Some(value);
        }
    }
    None
}

fn schema_variant_default<'a>(
    plugin: &'a PluginDefinition,
    prop: &'a PropDefinition,
    variant: &str,
    state: &str,
) -> Option<&'a TokenOrValue> {
    if let Some(value) = prop.defaults.values.get(variant).and_then(|d| d.get_state(state)) {
        return Some(value);
    }
    if state != "normal" {
        if let Some(value) = prop.defaults.values.get(variant).and_then(|d| d.get_state("normal")) {
            return Some(value);
        }
    }
    let common_variant = plugin.variants.first()?;
    if variant != common_variant {
        if let Some(value) = prop.defaults.values.get(common_variant).and_then(|d| d.get_state("normal")) {
            return Some(value);
        }
    }
    None
}

// ===========================================================================
// Emitter
// ===========================================================================

/// Components whose variant/state enums live in `theme.slint`, so a typed
/// variant accessor here doesn't create an import cycle. Currently only button
/// (ButtonVariant/ButtonState are centralized in theme.slint). Others keep their
/// variant colors inline until their enums are likewise centralized.
pub fn variant_enums(component_key: &str) -> Option<(&'static str, &'static str)> {
    match component_key {
        "button" => Some(("ButtonVariant", "ButtonState")),
        _ => None,
    }
}

enum VariantStyleMode<'a> {
    ImportedEnums { variant_enum: &'static str, state_enum: &'static str },
    GeneratedEnums { variant_enum: String, state_enum: String, variants: &'a [String], states: &'a [String] },
    SingleDefault { state_enum: String, states: &'a [String] },
}

/// Emit `<Component>_theme.slint` for `component_key`.
///
/// * `plugin` — the parsed schema (`defaults/components/<key>.schema.json`); supplies the component contract
///   (variants/states/sizes/props) plus legacy defaults used only when the theme JSON is incomplete.
/// * `overrides` — optional resolved theme values; when a variant/state/prop (or size/prop) is present it
///   replaces the schema seed (inlined as a literal, or a `Theme.*` ref if the override is itself a token).
/// * `theme_import` — the import path for `theme.slint`: use `"theme.slint"` for the shared in-tree file
///   (sits beside theme.slint) and `"@ui/theme.slint"` for a per-app file generated into a different
///   directory.
///
/// Returns `None` if the component has neither size nor variant props to emit.
pub fn component_theme_slint(
    component_key: &str,
    plugin: &PluginDefinition,
    overrides: Option<&ComponentOverrides>,
    theme_import: &str,
) -> Option<String> {
    let has_size = !plugin.size_props.is_empty();
    let imported_variants = variant_enums(component_key).filter(|_| !plugin.variant_props.is_empty());
    let has_generated_variant = imported_variants.is_none() && !plugin.variant_props.is_empty();
    let single_default_variant = has_generated_variant && plugin.variants.as_slice() == ["default"];
    let has_variant = imported_variants.is_some() || has_generated_variant;
    if !has_size && !has_variant {
        return None;
    }
    let comp = &plugin.component;
    let size_struct = format!("{comp}SizeProps");
    let variant_struct = format!("{comp}VariantProps");
    let sizes: Vec<&str> = if plugin.sizes.is_empty() {
        vec!["default"]
    } else {
        plugin.sizes.iter().map(String::as_str).collect()
    };
    let states: Vec<&str> = if plugin.states.is_empty() {
        vec!["normal"]
    } else {
        plugin.states.iter().map(String::as_str).collect()
    };
    let canonical_sizes =
        has_size && sizes.iter().all(|size| matches!(*size, "sm" | "md" | "lg")) && sizes.contains(&"md");
    let single_default_size = has_size && sizes.as_slice() == ["default"];
    let generated_size_enum =
        (has_size && !canonical_sizes && !single_default_size).then(|| format!("{comp}ThemeSize"));
    let variant_style = imported_variants
        .map(|(variant_enum, state_enum)| VariantStyleMode::ImportedEnums { variant_enum, state_enum })
        .or_else(|| {
            if single_default_variant {
                return Some(VariantStyleMode::SingleDefault {
                    state_enum: format!("{comp}State"),
                    states: &plugin.states,
                });
            }
            has_generated_variant.then(|| VariantStyleMode::GeneratedEnums {
                variant_enum: format!("{comp}ThemeVariant"),
                state_enum: format!("{comp}State"),
                variants: &plugin.variants,
                states: &plugin.states,
            })
        });

    // Resolve and deduplicate every variant/state value set before emission.
    // Component themes are build-time artifacts: generated globals expose only
    // immutable lookup functions, avoiding one Slint/Rust property (plus its
    // getter, setter and binding machinery) for every matrix cell.
    let mut unique_variant_styles: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut variant_style_targets: Vec<(String, String, String)> = Vec::new();
    if has_variant {
        let variants: Vec<&str> = if single_default_variant {
            vec!["default"]
        } else {
            plugin.variants.iter().map(String::as_str).collect()
        };
        for variant in variants {
            for state in &states {
                let fields = resolved_variant_fields(plugin, overrides, variant, state);
                let property_name = unique_variant_styles
                    .iter()
                    .find(|(_, existing)| existing == &fields)
                    .map(|(name, _)| name.clone())
                    .unwrap_or_else(|| {
                        let name = if single_default_variant {
                            format!("style-{state}")
                        } else {
                            format!("{variant}-{state}")
                        };
                        unique_variant_styles.push((name.clone(), fields));
                        name
                    });
                variant_style_targets.push((variant.to_string(), (*state).to_string(), property_name));
            }
        }
    }

    let mut unique_size_styles: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut size_style_targets: Vec<(String, String)> = Vec::new();
    if has_size {
        for size in &sizes {
            let fields = resolved_size_fields(plugin, overrides, size);
            let representative = unique_size_styles
                .iter()
                .find(|(_, existing)| existing == &fields)
                .map(|(name, _)| name.clone())
                .unwrap_or_else(|| {
                    let name = (*size).to_string();
                    unique_size_styles.push((name.clone(), fields));
                    name
                });
            size_style_targets.push(((*size).to_string(), representative));
        }
    }

    let mut s = String::new();
    s.push_str(HEADER);
    s.push('\n');
    s.push_str(&format!(
        "// GENERATED from {component_key}.schema.json by foundation theme tooling — do not edit by hand.\n"
    ));
    s.push_str("// Token-backed props reference the shared Theme global; JSON literals are inlined.\n\n");

    let mut imports = vec!["Theme", "ControlSize"];
    if let Some(VariantStyleMode::ImportedEnums { variant_enum, state_enum }) = &variant_style {
        imports.push(variant_enum);
        imports.push(state_enum);
    }
    s.push_str(&format!("import {{ {} }} from \"{theme_import}\";\n\n", imports.join(", ")));

    if let Some(VariantStyleMode::SingleDefault { state_enum, states }) = &variant_style {
        s.push_str(&format!("export enum {state_enum} {{\n"));
        for state in *states {
            s.push_str(&format!("    {state},\n"));
        }
        s.push_str("}\n\n");
    } else if let Some(VariantStyleMode::GeneratedEnums { variant_enum, state_enum, variants, states }) =
        &variant_style
    {
        s.push_str(&format!("export enum {variant_enum} {{\n"));
        for variant in *variants {
            s.push_str(&format!("    {variant},\n"));
        }
        s.push_str("}\n\n");
        s.push_str(&format!("export enum {state_enum} {{\n"));
        for state in *states {
            s.push_str(&format!("    {state},\n"));
        }
        s.push_str("}\n\n");
    }
    if let Some(size_enum) = &generated_size_enum {
        s.push_str(&format!("export enum {size_enum} {{\n"));
        for size in &sizes {
            s.push_str(&format!("    {size},\n"));
        }
        s.push_str("}\n\n");
    }

    if has_variant {
        s.push_str(&format!("export struct {variant_struct} {{\n"));
        for p in &plugin.variant_props {
            s.push_str(&format!("    {}: {},\n", kebab(&p.name), slint_type(p)));
        }
        s.push_str("}\n\n");
    }
    if has_size {
        s.push_str(&format!("export struct {size_struct} {{\n"));
        for p in &plugin.size_props {
            s.push_str(&format!("    {}: {},\n", kebab(&p.name), slint_type(p)));
        }
        s.push_str("}\n\n");
    }

    s.push_str(&format!("export global {comp}Theme {{\n"));

    // Emit immutable style lookup functions. The most widely shared value set
    // is the fall-through; every other unique set is emitted exactly once.
    if let Some((venum, senum)) = match &variant_style {
        Some(VariantStyleMode::ImportedEnums { variant_enum, state_enum }) => {
            Some((variant_enum.to_string(), state_enum.to_string()))
        }
        Some(VariantStyleMode::GeneratedEnums { variant_enum, state_enum, .. }) => {
            Some((variant_enum.clone(), state_enum.clone()))
        }
        _ => None,
    } {
        s.push_str(&format!(
            "    public pure function style(v: {venum}, st: {senum}) -> {variant_struct} {{\n"
        ));
        emit_variant_lookup_body(
            &mut s,
            &unique_variant_styles,
            &variant_style_targets,
            Some((&venum, &senum)),
        );
        s.push_str("    }\n");
    } else if let Some(VariantStyleMode::SingleDefault { state_enum, .. }) = &variant_style {
        s.push_str(&format!("    public pure function style(st: {state_enum}) -> {variant_struct} {{\n"));
        emit_variant_lookup_body(
            &mut s,
            &unique_variant_styles,
            &variant_style_targets,
            Some(("", state_enum)),
        );
        s.push_str("    }\n");
    }

    if has_size {
        if single_default_size {
            s.push_str(&format!("    public pure function size() -> {size_struct} {{\n"));
            emit_struct_return(&mut s, &unique_size_styles[0].1, "        ");
            s.push_str("    }\n");
        } else {
            let size_enum = generated_size_enum.as_deref().unwrap_or("ControlSize");
            s.push_str(&format!("    public pure function size(s: {size_enum}) -> {size_struct} {{\n"));
            emit_size_lookup_body(&mut s, &unique_size_styles, &size_style_targets, size_enum);
            s.push_str("    }\n");
        }
    }
    s.push_str("}\n");
    Some(s)
}

fn emit_variant_lookup_body(
    output: &mut String,
    unique_styles: &[(String, Vec<(String, String)>)],
    targets: &[(String, String, String)],
    enum_names: Option<(&str, &str)>,
) {
    let (variant_enum, state_enum) = enum_names.expect("variant lookup enum names must exist");
    if variant_enum.is_empty() {
        let profile =
            targets.iter().map(|(_, state, target)| (state.clone(), target.clone())).collect::<Vec<_>>();
        emit_state_lookup_body(output, unique_styles, &profile, state_enum, "        ");
        return;
    }

    // Variants with identical state maps share one branch. Each branch still
    // falls back to that variant profile's Normal style so imported enums may
    // add states (for example ButtonState.loading) without acquiring an
    // unrelated variant's fallback.
    let mut variants = Vec::new();
    for (variant, _, _) in targets {
        if !variants.contains(variant) {
            variants.push(variant.clone());
        }
    }
    let mut profiles: Vec<(Vec<String>, Vec<(String, String)>)> = Vec::new();
    for variant in &variants {
        let profile = targets
            .iter()
            .filter(|(candidate, _, _)| candidate == variant)
            .map(|(_, state, target)| (state.clone(), target.clone()))
            .collect::<Vec<_>>();
        if let Some((profile_variants, _)) = profiles.iter_mut().find(|(_, existing)| existing == &profile) {
            profile_variants.push(variant.clone());
        } else {
            profiles.push((vec![variant.clone()], profile));
        }
    }
    let last_variant = variants.last().expect("variant lookup must contain a variant");
    let fallback_index = profiles
        .iter()
        .position(|(profile_variants, _)| profile_variants.contains(last_variant))
        .expect("last variant profile must exist");

    for (index, (profile_variants, profile)) in profiles.iter().enumerate() {
        if index == fallback_index {
            continue;
        }
        let clauses = profile_variants
            .iter()
            .map(|variant| format!("v == {variant_enum}.{variant}"))
            .collect::<Vec<_>>();
        output.push_str("        if ");
        output.push_str(&clauses.join("\n            || "));
        output.push_str(" {\n");
        emit_state_lookup_body(output, unique_styles, profile, state_enum, "            ");
        output.push_str("        }\n");
    }
    emit_state_lookup_body(output, unique_styles, &profiles[fallback_index].1, state_enum, "        ");
}

fn emit_state_lookup_body(
    output: &mut String,
    unique_styles: &[(String, Vec<(String, String)>)],
    profile: &[(String, String)],
    state_enum: &str,
    indent: &str,
) {
    let normal_target = profile
        .iter()
        .find(|(state, _)| state == "normal")
        .or_else(|| profile.first())
        .map(|(_, target)| target)
        .expect("variant profile must contain a state");
    let mut emitted_targets = Vec::new();
    for (_, target) in profile {
        if target == normal_target || emitted_targets.contains(target) {
            continue;
        }
        emitted_targets.push(target.clone());
        let clauses = profile
            .iter()
            .filter(|(_, candidate)| candidate == target)
            .map(|(state, _)| format!("st == {state_enum}.{state}"))
            .collect::<Vec<_>>();
        output.push_str(indent);
        output.push_str("if ");
        output.push_str(&clauses.join(&format!("\n{indent}    || ")));
        output.push_str(" {\n");
        let fields = unique_styles
            .iter()
            .find_map(|(name, fields)| (name == target).then_some(fields))
            .expect("state style must exist");
        emit_struct_return(output, fields, &format!("{indent}    "));
        output.push_str(indent);
        output.push_str("}\n");
    }

    let fields = unique_styles
        .iter()
        .find_map(|(name, fields)| (name == normal_target).then_some(fields))
        .expect("normal state style must exist");
    emit_struct_return(output, fields, indent);
}

fn emit_size_lookup_body(
    output: &mut String,
    unique_styles: &[(String, Vec<(String, String)>)],
    targets: &[(String, String)],
    size_enum: &str,
) {
    let fallback = unique_styles
        .iter()
        .max_by_key(|(name, _)| targets.iter().filter(|(_, target)| target == name).count())
        .map(|(name, _)| name)
        .expect("size lookup must contain at least one style");

    for (name, fields) in unique_styles {
        if name == fallback {
            continue;
        }
        let clauses = targets
            .iter()
            .filter(|(_, target)| target == name)
            .map(|(size, _)| format!("s == {size_enum}.{size}"))
            .collect::<Vec<_>>();
        output.push_str("        if ");
        output.push_str(&clauses.join("\n            || "));
        output.push_str(" {\n");
        emit_struct_return(output, fields, "            ");
        output.push_str("        }\n");
    }

    let fields = unique_styles
        .iter()
        .find_map(|(name, fields)| (name == fallback).then_some(fields))
        .expect("fallback size style must exist");
    emit_struct_return(output, fields, "        ");
}

fn emit_struct_return(output: &mut String, fields: &[(String, String)], indent: &str) {
    output.push_str(indent);
    output.push_str("return {\n");
    for (name, value) in fields {
        output.push_str(indent);
        output.push_str("    ");
        output.push_str(&format!("{name}: {value},\n"));
    }
    output.push_str(indent);
    output.push_str("};\n");
}

fn resolved_variant_fields(
    plugin: &PluginDefinition,
    overrides: Option<&ComponentOverrides>,
    variant: &str,
    state: &str,
) -> Vec<(String, String)> {
    plugin
        .variant_props
        .iter()
        .map(|prop| {
            let value = variant_override(plugin, overrides, variant, state, &prop.name)
                .or_else(|| schema_variant_default(plugin, prop, variant, state))
                .map(|value| emit_value(slint_type(prop), value))
                .unwrap_or_else(|| "0px /* missing */".to_string());
            (kebab(&prop.name), value)
        })
        .collect()
}

fn resolved_size_fields(
    plugin: &PluginDefinition,
    overrides: Option<&ComponentOverrides>,
    size: &str,
) -> Vec<(String, String)> {
    plugin
        .size_props
        .iter()
        .map(|prop| {
            // Cascade: app override (size, then common/default) → schema
            // override (size, then common/default base). This lets JSON store
            // the base once plus only per-size deltas.
            let value = overrides
                .and_then(|overrides| overrides.size(size, &prop.name))
                .or_else(|| {
                    prop.defaults
                        .values
                        .get(size)
                        .or_else(|| prop.defaults.values.get("common"))
                        .or_else(|| prop.defaults.values.get("default"))
                        .and_then(DefaultValue::get_direct)
                })
                .map(|value| emit_value(slint_type(prop), value))
                .unwrap_or_else(|| "0px /* missing default */".to_string());
            (kebab(&prop.name), value)
        })
        .collect()
}

/// Slint type for a prop. In sizeProps a numeric value is a dimension unless its
/// name says otherwise (opacity → float).
pub fn slint_type(prop: &PropDefinition) -> &'static str {
    match prop.prop_type.as_str() {
        "string" => "string",
        "color" => "color",
        "int" => "int",
        "bool" => "bool",
        "float" if prop.name.contains("opacity") => "float",
        "float" => "length",
        _ => "float",
    }
}

fn emit_value(slint_type: &str, tov: &TokenOrValue) -> String {
    match tov {
        TokenOrValue::String(s) => {
            if s.contains('.') && !s.starts_with('#') {
                slint_token_ref(s).unwrap_or_else(|| format!("0px /* TODO unmapped token: {s} */"))
            } else if s.starts_with('#') {
                s.clone()
            } else {
                format!("\"{s}\"")
            }
        }
        TokenOrValue::Float(f) => match slint_type {
            "length" => format!("{}px", fmt_num(*f)),
            "int" => format!("{}", *f as i64),
            _ => fmt_num(*f),
        },
        TokenOrValue::Int(i) => match slint_type {
            "length" => format!("{i}px"),
            _ => format!("{i}"),
        },
        TokenOrValue::Bool(b) => format!("{b}"),
    }
}

/// Map a token like `fontSize.md` → `Theme.font-size-md`. Returns `None` for
/// categories not exposed on the Theme global.
fn slint_token_ref(token: &str) -> Option<String> {
    let (cat, key) = token.split_once('.')?;
    // color.X (incl. color.primary.dark) -> Theme.color-X (dots become dashes).
    if cat == "color" {
        return Some(format!("Theme.color-{}", key.replace('.', "-")));
    }
    if !EXPOSED_TOKEN_CATEGORIES.contains(&cat) {
        return None;
    }
    Some(format!("Theme.{}-{}", kebab(cat), key))
}

fn kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn fmt_num(f: f64) -> String {
    if f.fract() == 0.0 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_plugin() {
        let json = r##"{
            "component": "Button",
            "variants": ["primary", "secondary"],
            "states": ["normal", "pressed"],
            "sizes": ["small", "medium"],
            "variantProps": [
                {
                    "name": "background",
                    "type": "color",
                    "defaults": {
                        "primary": { "normal": "color.primary", "pressed": "#ff0000" },
                        "secondary": { "normal": "#cccccc", "pressed": "#aaaaaa" }
                    }
                }
            ],
            "sizeProps": [
                {
                    "name": "fontSize",
                    "type": "float",
                    "min": 8,
                    "max": 32,
                    "step": 1,
                    "defaults": {
                        "small": "fontSize.sm",
                        "medium": 16
                    }
                }
            ]
        }"##;

        let plugin: PluginDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(plugin.component, "Button");
        assert_eq!(plugin.variants.len(), 2);
        assert_eq!(plugin.states.len(), 2);
        assert_eq!(plugin.sizes.len(), 2);
        assert_eq!(plugin.variant_props.len(), 1);
        assert_eq!(plugin.size_props.len(), 1);
    }

    #[test]
    fn override_inlines_literal_over_token_base() {
        let plugin: PluginDefinition = serde_json::from_str(
            r##"{
                "component": "Button",
                "variants": ["primary"],
                "states": ["normal", "pressed"],
                "sizes": ["sm", "md", "lg"],
                "variantProps": [
                    { "name": "background", "type": "color",
                      "defaults": { "primary": { "normal": "color.primary", "pressed": "color.primary.dark" } } }
                ],
                "sizeProps": []
            }"##,
        )
        .unwrap();

        let overrides: ComponentOverrides = serde_json::from_str(
            r##"{ "variantProps": { "primary": { "normal": { "background": "#ef4444" } } } }"##,
        )
        .unwrap();

        let out = component_theme_slint("button", &plugin, Some(&overrides), "@ui/theme.slint").unwrap();
        // overridden: inlined literal (struct fields are comma-separated)
        assert!(out.contains("background: #ef4444,"), "missing override literal:\n{out}");
        // pressed inherits the overridden Normal value unless it has its own override.
        assert_eq!(
            out.matches("background: #ef4444,").count(),
            1,
            "identical states should be deduplicated:\n{out}"
        );
        assert!(
            out.contains("public pure function style(v: ButtonVariant, st: ButtonState)"),
            "missing immutable style lookup:\n{out}"
        );
        assert!(
            !out.contains("in-out property"),
            "immutable themes must not expose mutable properties:\n{out}"
        );
        assert!(!out.contains("return self."), "lookup must return values directly:\n{out}");
        // parameterized theme import
        assert!(out.contains("from \"@ui/theme.slint\""), "missing @ui import:\n{out}");
    }

    #[test]
    fn imported_extra_states_fall_back_to_the_selected_variants_normal_style() {
        let plugin: PluginDefinition = serde_json::from_str(
            r##"{
                "component": "Button",
                "variants": ["primary", "secondary"],
                "states": ["normal", "pressed"],
                "sizes": [],
                "variantProps": [
                    { "name": "background", "type": "color",
                      "defaults": {
                        "primary": { "normal": "#111111", "pressed": "#222222" },
                        "secondary": { "normal": "#333333", "pressed": "#444444" }
                      } }
                ],
                "sizeProps": []
            }"##,
        )
        .unwrap();

        let out = component_theme_slint("button", &plugin, None, "@ui/theme.slint").unwrap();

        // ButtonState may contain values (such as `loading`) that are not in a
        // component schema. Each variant branch must therefore end in its own
        // normal style instead of sharing one global fallback.
        let primary_branch = out
            .split_once("if v == ButtonVariant.primary {")
            .and_then(|(_, rest)| rest.split_once("\n        }\n"))
            .map(|(branch, _)| branch)
            .expect("primary variant branch");
        assert!(primary_branch.contains("background: #222222,"), "missing primary pressed style:\n{out}");
        assert!(primary_branch.contains("background: #111111,"), "missing primary normal fallback:\n{out}");
        assert!(!primary_branch.contains("#333333"), "primary branch used secondary fallback:\n{out}");
        assert!(
            !out.contains("ButtonState.loading"),
            "undefined imported states should use fall-through:\n{out}"
        );
    }

    #[test]
    fn schema_defaults_are_legacy_fallbacks_when_theme_value_is_missing() {
        let plugin: PluginDefinition = serde_json::from_str(
            r##"{
                "component": "Button",
                "variants": ["primary"],
                "states": ["normal", "pressed"],
                "sizes": [],
                "variantProps": [
                    { "name": "background", "type": "color",
                      "defaults": { "primary": { "normal": "color.primary", "pressed": "color.primary.dark" } } }
                ],
                "sizeProps": []
            }"##,
        )
        .unwrap();

        let out = component_theme_slint("button", &plugin, None, "@ui/theme.slint").unwrap();

        assert!(out.contains("background: Theme.color-primary,"), "missing normal schema fallback:\n{out}");
        assert!(
            out.contains("background: Theme.color-primary-dark,"),
            "missing pressed schema fallback:\n{out}"
        );
    }

    #[test]
    fn common_size_override_is_the_serialized_base_key() {
        let plugin: PluginDefinition = serde_json::from_str(
            r##"{
                "component": "Input",
                "variants": [],
                "states": [],
                "sizes": ["sm", "md"],
                "variantProps": [],
                "sizeProps": [
                    { "name": "field-height", "type": "float",
                      "defaults": { "default": 40.0 } }
                ]
            }"##,
        )
        .unwrap();

        let overrides: ComponentOverrides = serde_json::from_str(
            r##"{
                "sizeProps": {
                    "common": { "field-height": 44.0 },
                    "sm": { "field-height": 36.0 }
                }
            }"##,
        )
        .unwrap();

        let out = component_theme_slint("input", &plugin, Some(&overrides), "@ui/theme.slint").unwrap();

        assert!(out.contains("public pure function size(s: ControlSize)"));
        assert!(!out.contains("property <InputSizeProps>"));
        assert!(out.contains("field-height: 36px,"));
        assert!(out.contains("field-height: 44px,"));
    }
}
