// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared plugin-schema types + the `<key>_theme.slint` emitter.
//!
//! This module is the single source of the component-theme generator, used by
//! two callers:
//!   * `slintthemegen` (theme-editor) — emits the shared default `ui2/components/ui/<key>_theme.slint` from
//!     `defaults/plugins/<key>.json`, with no app overrides.
//!   * `foundation-themes` (the `foundation build` theme-compile step) — emits a *per-app*
//!     `<key>_theme.slint` from the same schema plus the app's `resources/theme.json` `components.<key>`
//!     overrides.
//!
//! Generation rule (Approach X): a token-backed default becomes a reference to
//! the shared `Theme` global (live + dark/light-aware), e.g. `Theme.color-primary`;
//! a literal (hex color / number) is inlined. An app override replaces the base
//! default for that exact variant/state/prop (or size/prop) — so an overridden
//! color is inlined as a literal while everything else keeps cascading from the
//! tokens. The component does no theme evaluation at runtime beyond selecting the
//! active variant/size via `style()` / `size()`.

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
    "controlSize",
    "iconSize",
    "radius",
    "controlRadius",
    "controlPaddingInline",
    "spacing",
];

// ===========================================================================
// Plugin schema (parsed from defaults/plugins/<key>.json)
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
    fn variant(&self, variant: &str, state: &str, prop: &str) -> Option<&TokenOrValue> {
        self.variant_props.get(variant)?.get(state)?.get(prop)
    }

    /// Size override with the same `default`-cascade as the schema: a per-size
    /// override wins, else a `default` override applies to every size.
    fn size(&self, size: &str, prop: &str) -> Option<&TokenOrValue> {
        self.size_props
            .get(size)
            .and_then(|m| m.get(prop))
            .or_else(|| self.size_props.get("default").and_then(|m| m.get(prop)))
    }
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

/// Emit `<Component>_theme.slint` for `component_key`.
///
/// * `plugin` — the parsed schema (`defaults/plugins/<key>.json`); supplies the contract
///   (variants/states/sizes/props) and the token-backed base defaults.
/// * `overrides` — optional per-app overrides; when a variant/state/prop (or size/prop) is present it
///   replaces the base default (inlined as a literal, or a `Theme.*` ref if the override is itself a token).
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
    let variants = variant_enums(component_key).filter(|_| !plugin.variant_props.is_empty());
    if !has_size && variants.is_none() {
        return None;
    }
    let comp = &plugin.component;
    let size_struct = format!("{comp}SizeProps");
    let variant_struct = format!("{comp}VariantProps");
    let sizes: Vec<&str> = plugin.sizes.iter().map(String::as_str).collect();
    let states: Vec<&str> = plugin.states.iter().map(String::as_str).collect();

    let mut s = String::new();
    s.push_str(HEADER);
    s.push('\n');
    s.push_str(&format!(
        "// GENERATED from {component_key}.json by foundation theme tooling — do not edit by hand.\n"
    ));
    s.push_str("// Token-backed props reference the shared Theme global; JSON literals are inlined.\n\n");

    let mut imports = vec!["Theme", "ControlSize"];
    if let Some((venum, senum)) = variants {
        imports.push(venum);
        imports.push(senum);
    }
    s.push_str(&format!("import {{ {} }} from \"{theme_import}\";\n\n", imports.join(", ")));

    if variants.is_some() {
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

    // variant × state matrix (only when variant enums are available)
    if variants.is_some() {
        for variant in &plugin.variants {
            for state in &states {
                s.push_str(&format!("    in-out property <{variant_struct}> {variant}-{state}: {{\n"));
                for p in &plugin.variant_props {
                    // App override wins over the schema's token-backed base.
                    let val = overrides
                        .and_then(|o| o.variant(variant, state, &p.name))
                        .or_else(|| p.defaults.values.get(variant).and_then(|d| d.get_state(state)))
                        .map(|tov| emit_value(slint_type(p), tov))
                        .unwrap_or_else(|| "0px /* missing */".to_string());
                    s.push_str(&format!("        {}: {},\n", kebab(&p.name), val));
                }
                s.push_str("    };\n");
            }
        }
    }

    if has_size {
        for size in &sizes {
            s.push_str(&format!("    in-out property <{size_struct}> size-{size}: {{\n"));
            for p in &plugin.size_props {
                // Cascade: app override (size, then default) → schema override
                // (size, then default base). Lets JSON store the base once + only
                // the per-size deltas.
                let val = overrides
                    .and_then(|o| o.size(size, &p.name))
                    .or_else(|| {
                        p.defaults
                            .values
                            .get(*size)
                            .or_else(|| p.defaults.values.get("default"))
                            .and_then(DefaultValue::get_direct)
                    })
                    .map(|tov| emit_value(slint_type(p), tov))
                    .unwrap_or_else(|| "0px /* missing default */".to_string());
                s.push_str(&format!("        {}: {},\n", kebab(&p.name), val));
            }
            s.push_str("    };\n");
        }
    }

    // style(variant, state) accessor: variant ladder → state ladder; the last
    // variant + "normal" state are the fall-through defaults (so e.g. an enum
    // state like `loading` with no JSON entry resolves to normal).
    if let Some((venum, senum)) = variants {
        s.push_str(&format!(
            "\n    public pure function style(v: {venum}, st: {senum}) -> {variant_struct} {{\n"
        ));
        for (vi, variant) in plugin.variants.iter().enumerate() {
            let last = vi + 1 == plugin.variants.len();
            let ind = if last { "        " } else { "            " };
            if !last {
                s.push_str(&format!("        if v == {venum}.{variant} {{\n"));
            }
            for state in &states {
                if *state == "normal" {
                    continue;
                }
                s.push_str(&format!("{ind}if st == {senum}.{state} {{\n"));
                s.push_str(&format!("{ind}    return self.{variant}-{state};\n"));
                s.push_str(&format!("{ind}}}\n"));
            }
            s.push_str(&format!("{ind}return self.{variant}-normal;\n"));
            if !last {
                s.push_str("        }\n");
            }
        }
        s.push_str("    }\n");
    }

    let canonical =
        has_size && sizes.iter().all(|z| matches!(*z, "sm" | "md" | "lg")) && sizes.contains(&"md");
    if canonical {
        s.push_str(&format!("\n    public pure function size(s: ControlSize) -> {size_struct} {{\n"));
        if sizes.contains(&"sm") {
            s.push_str("        if s == ControlSize.sm {\n            return self.size-sm;\n        }\n");
        }
        if sizes.contains(&"lg") {
            s.push_str("        if s == ControlSize.lg {\n            return self.size-lg;\n        }\n");
        }
        s.push_str("        return self.size-md;\n    }\n");
    }
    s.push_str("}\n");
    Some(s)
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
        // non-overridden: still a token ref
        assert!(out.contains("background: Theme.color-primary-dark,"), "missing token ref:\n{out}");
        // parameterized theme import
        assert!(out.contains("from \"@ui/theme.slint\""), "missing @ui import:\n{out}");
    }
}
