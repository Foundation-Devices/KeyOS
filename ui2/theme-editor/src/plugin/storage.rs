#![allow(dead_code)]
// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Dynamic property storage for theme data.
//!
//! This module provides a flexible storage system for theme properties that can
//! be modified at runtime. It bridges the gap between JSON plugin definitions
//! and the Slint UI.

use std::collections::{HashMap, HashSet};

use slint::Color;

use super::{PluginDefinition, PropDefinition, TokenOrValue, TokenStore};

/// Virtual size key that holds the shared "Default" base value for every size
/// prop. Each concrete size (sm/md/lg/...) either inherits this base or has an
/// explicit per-size override tracked in `ComponentThemeData::size_overrides`.
pub const DEFAULT_SIZE_KEY: &str = "default";

/// A resolved property value that can be stored and edited.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Token(String),
    Color(Color),
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
}

impl PropertyValue {
    /// Get as color
    pub fn as_color(&self) -> Option<Color> {
        match self {
            PropertyValue::Color(c) => Some(*c),
            _ => None,
        }
    }

    /// Get as float
    pub fn as_float(&self) -> Option<f32> {
        match self {
            PropertyValue::Float(f) => Some(*f),
            PropertyValue::Int(i) => Some(*i as f32),
            _ => None,
        }
    }

    /// Get as int
    pub fn as_int(&self) -> Option<i32> {
        match self {
            PropertyValue::Int(i) => Some(*i),
            PropertyValue::Float(f) => Some(*f as i32),
            _ => None,
        }
    }

    pub fn token_key(&self) -> Option<&str> {
        match self {
            PropertyValue::Token(key) => Some(key.as_str()),
            _ => None,
        }
    }
}

/// Storage for all theme data for a component.
/// Organized as: variant -> state -> prop_name -> value (for variant props)
/// And: size -> prop_name -> value (for size props)
#[derive(Debug, Clone)]
pub struct ComponentThemeData {
    /// Variant properties: variant -> state -> prop_name -> value
    pub variant_props: HashMap<String, HashMap<String, HashMap<String, PropertyValue>>>,

    /// Size properties: size -> prop_name -> value. Includes the virtual
    /// `DEFAULT_SIZE_KEY` ("default") entry which holds the shared base for
    /// every size; each concrete size either equals that base (inheriting) or
    /// holds its own override (tracked in `size_overrides`).
    pub size_props: HashMap<String, HashMap<String, PropertyValue>>,

    /// Per-(concrete-size, prop) set of prop names that override the "default"
    /// base. A prop name absent from a size's set is inheriting from default.
    pub size_overrides: HashMap<String, HashSet<String>>,

    /// Optional parent component key (from the resolved plugin's `extends` /
    /// `parent_key`). When set, this component's "Default" base inherits from
    /// the parent's Default unless the prop is listed in `parent_overrides`.
    pub parent_key: Option<String>,

    /// Set of prop names whose "Default" value the user has locally overridden
    /// in this child component — these no longer follow the parent's Default
    /// edits. A prop name absent here is inheriting live from the parent.
    pub parent_overrides: HashSet<String>,

    /// Reference to states for iteration
    pub states: Vec<String>,

    /// Variant property definitions
    pub variant_prop_defs: Vec<PropDefinition>,

    /// Size property definitions
    pub size_prop_defs: Vec<PropDefinition>,
}

impl ComponentThemeData {
    /// Create theme data from a plugin definition, resolving defaults from tokens.
    pub fn from_plugin(plugin: &PluginDefinition, tokens: &TokenStore) -> Self {
        let mut data = Self {
            variant_props: HashMap::new(),
            size_props: HashMap::new(),
            size_overrides: HashMap::new(),
            parent_key: plugin.parent_key.clone(),
            parent_overrides: HashSet::new(),
            states: plugin.states.clone(),
            variant_prop_defs: plugin.variant_props.clone(),
            size_prop_defs: plugin.size_props.clone(),
        };

        // Initialize variant props with defaults
        for variant in &plugin.variants {
            let mut state_map = HashMap::new();

            for state in &plugin.states {
                let mut prop_map = HashMap::new();

                for prop_def in &plugin.variant_props {
                    let value = resolve_variant_default(prop_def, variant, state, tokens);
                    prop_map.insert(prop_def.name.clone(), value);
                }

                state_map.insert(state.clone(), prop_map);
            }

            data.variant_props.insert(variant.clone(), state_map);
        }

        // Seed the virtual "default" base from each prop's `default` key (or the
        // type fallback when none is declared).
        let mut default_props = HashMap::new();
        for prop_def in &plugin.size_props {
            let value = resolve_size_base(prop_def, tokens);
            default_props.insert(prop_def.name.clone(), value);
        }
        data.size_props.insert(DEFAULT_SIZE_KEY.to_string(), default_props);

        // For each concrete size: a prop with an explicit per-size key in the
        // JSON defaults is an override; otherwise the size inherits the base.
        for size in &plugin.sizes {
            let mut prop_map = HashMap::new();
            let mut overrides = HashSet::new();
            for prop_def in &plugin.size_props {
                let value = resolve_size_default(prop_def, size, tokens);
                if prop_def.defaults.values.contains_key(size) {
                    overrides.insert(prop_def.name.clone());
                }
                prop_map.insert(prop_def.name.clone(), value);
            }
            data.size_props.insert(size.clone(), prop_map);
            data.size_overrides.insert(size.clone(), overrides);
        }

        data
    }

    /// True when this concrete size has an override for the given prop (false
    /// means the size is inheriting the "default" base).
    pub fn size_prop_is_overridden(&self, size: &str, prop_name: &str) -> bool {
        self.size_overrides.get(size).map(|s| s.contains(prop_name)).unwrap_or(false)
    }

    /// Set the "default" base for a prop and cascade the new value into every
    /// concrete size that is currently inheriting (no override).
    pub fn set_size_default(&mut self, prop_name: &str, value: PropertyValue) {
        if let Some(base) = self.size_props.get_mut(DEFAULT_SIZE_KEY) {
            base.insert(prop_name.to_string(), value.clone());
        }
        let inheriting_sizes: Vec<String> = self
            .size_overrides
            .iter()
            .filter(|(size, overrides)| size.as_str() != DEFAULT_SIZE_KEY && !overrides.contains(prop_name))
            .map(|(size, _)| size.clone())
            .collect();
        for size in inheriting_sizes {
            if let Some(props) = self.size_props.get_mut(&size) {
                props.insert(prop_name.to_string(), value.clone());
            }
        }
    }

    /// Set a per-size override for a prop (marks the prop as overridden for
    /// that size; future "default" edits won't cascade into it).
    pub fn set_size_override(&mut self, size: &str, prop_name: &str, value: PropertyValue) {
        if let Some(props) = self.size_props.get_mut(size) {
            props.insert(prop_name.to_string(), value);
        }
        self.size_overrides.entry(size.to_string()).or_default().insert(prop_name.to_string());
    }

    /// Clear a per-size override and re-inherit from the "default" base.
    pub fn clear_size_override(&mut self, size: &str, prop_name: &str) {
        if let Some(overrides) = self.size_overrides.get_mut(size) {
            overrides.remove(prop_name);
        }
        let base_value = self.size_props.get(DEFAULT_SIZE_KEY).and_then(|base| base.get(prop_name)).cloned();
        if let Some(value) = base_value {
            if let Some(props) = self.size_props.get_mut(size) {
                props.insert(prop_name.to_string(), value);
            }
        }
    }

    /// True when this child component has locally overridden the parent's
    /// Default value for the given prop (so live parent edits won't reach it).
    pub fn parent_prop_is_overridden(&self, prop_name: &str) -> bool {
        self.parent_overrides.contains(prop_name)
    }

    /// Mark a prop's Default as locally overridden in this child — call when
    /// the user explicitly edits a child's Default prop.
    pub fn mark_parent_override(&mut self, prop_name: &str) {
        self.parent_overrides.insert(prop_name.to_string());
    }

    /// Drop the local parent-level override and re-inherit `value` from the
    /// parent's current Default. Also cascades within the child via
    /// `set_size_default` so per-size rows that inherit follow.
    pub fn clear_parent_override(&mut self, prop_name: &str, parent_value: PropertyValue) {
        self.parent_overrides.remove(prop_name);
        self.set_size_default(prop_name, parent_value);
    }

    /// Get all variant property values for a variant+state as a flat list.
    /// Order matches variant_prop_defs.
    pub fn get_variant_state_values(&self, variant: &str, state: &str) -> Vec<PropertyValue> {
        let mut values = Vec::new();
        if let Some(state_map) = self.variant_props.get(variant) {
            if let Some(prop_map) = state_map.get(state) {
                for prop_def in &self.variant_prop_defs {
                    if let Some(val) = prop_map.get(&prop_def.name) {
                        values.push(val.clone());
                    } else {
                        // Fallback to a default
                        values.push(PropertyValue::Float(0.0));
                    }
                }
            }
        }
        values
    }

    /// Get all size property values for a size as a flat list.
    /// Order matches size_prop_defs.
    pub fn get_size_values(&self, size: &str) -> Vec<PropertyValue> {
        let mut values = Vec::new();
        if let Some(prop_map) = self.size_props.get(size) {
            for prop_def in &self.size_prop_defs {
                if let Some(val) = prop_map.get(&prop_def.name) {
                    values.push(val.clone());
                } else {
                    values.push(PropertyValue::Float(0.0));
                }
            }
        }
        values
    }
}

/// Cross-component live cascade for a Default-base edit: every component whose
/// `parent_key` matches `parent_key` and which has NOT locally overridden this
/// prop gets its own Default updated (which then cascades within that child to
/// every per-size row that is still inheriting). Recursive — handles chained
/// inheritance like A extends B extends C.
pub fn cascade_default_to_children(
    themes: &mut HashMap<String, ComponentThemeData>,
    parent_key: &str,
    prop_name: &str,
    value: &PropertyValue,
) {
    let child_keys: Vec<String> = themes
        .iter()
        .filter(|(_, d)| d.parent_key.as_deref() == Some(parent_key))
        .filter(|(_, d)| !d.parent_overrides.contains(prop_name))
        .map(|(k, _)| k.clone())
        .collect();
    for child_key in child_keys {
        if let Some(child) = themes.get_mut(&child_key) {
            child.set_size_default(prop_name, value.clone());
        }
        cascade_default_to_children(themes, &child_key, prop_name, value);
    }
}

/// Resolve a default value for a variant property.
fn resolve_variant_default(
    prop_def: &PropDefinition,
    variant: &str,
    state: &str,
    tokens: &TokenStore,
) -> PropertyValue {
    // Look up the default for this variant/state
    if let Some(variant_defaults) = prop_def.defaults.values.get(variant) {
        if let Some(token_or_value) = variant_defaults.get_state(state) {
            return resolve_token_or_value(token_or_value, &prop_def.prop_type, tokens);
        }
    }

    // Fallback to type-appropriate default
    default_for_type(&prop_def.prop_type)
}

/// Resolve the shared "default" base value for a size property — used to seed
/// the virtual `DEFAULT_SIZE_KEY` row. Resolution order:
///   1. The plugin's explicit `"default"` key, if declared.
///   2. The `"md"` / `"medium"` value — most plugins ship per-size defaults but no `"default"` key, so using
///      md makes the Default row in the editor show a sensible mid-size value (otherwise it would be
///      0/empty).
///   3. The first per-size value declared.
///   4. The type fallback.
fn resolve_size_base(prop_def: &PropDefinition, tokens: &TokenStore) -> PropertyValue {
    if let Some(base) = prop_def.defaults.values.get(DEFAULT_SIZE_KEY) {
        if let Some(token_or_value) = base.get_direct() {
            return resolve_token_or_value(token_or_value, &prop_def.prop_type, tokens);
        }
    }
    for key in ["md", "medium"] {
        if let Some(value) = prop_def.defaults.values.get(key) {
            if let Some(token_or_value) = value.get_direct() {
                return resolve_token_or_value(token_or_value, &prop_def.prop_type, tokens);
            }
        }
    }
    if let Some((_, first)) = prop_def.defaults.values.iter().next() {
        if let Some(token_or_value) = first.get_direct() {
            return resolve_token_or_value(token_or_value, &prop_def.prop_type, tokens);
        }
    }
    default_for_type(&prop_def.prop_type)
}

/// Resolve a default value for a size property.
///
/// Resolution order: the size's own value, then the shared `"default"` base
/// (applies to every size unless that size overrides it), then a
/// type-appropriate fallback. This lets a plugin set one base value for a prop
/// (e.g. a single border-radius for all sizes) and only specify per-size entries
/// where a size genuinely differs.
fn resolve_size_default(prop_def: &PropDefinition, size: &str, tokens: &TokenStore) -> PropertyValue {
    // The size's own value wins.
    if let Some(size_default) = prop_def.defaults.values.get(size) {
        if let Some(token_or_value) = size_default.get_direct() {
            return resolve_token_or_value(token_or_value, &prop_def.prop_type, tokens);
        }
    }

    // Otherwise inherit the shared base shared by all sizes.
    if let Some(base) = prop_def.defaults.values.get("default") {
        if let Some(token_or_value) = base.get_direct() {
            return resolve_token_or_value(token_or_value, &prop_def.prop_type, tokens);
        }
    }

    // Fallback to type-appropriate default
    default_for_type(&prop_def.prop_type)
}

/// Resolve a TokenOrValue to a PropertyValue.
fn resolve_token_or_value(
    token_or_value: &TokenOrValue,
    prop_type: &str,
    tokens: &TokenStore,
) -> PropertyValue {
    match token_or_value {
        TokenOrValue::String(s) => {
            if token_or_value.is_token_reference() {
                let _ = tokens;
                let _ = prop_type;
                PropertyValue::Token(s.clone())
            } else if prop_type == "color" {
                // Direct hex color
                parse_color_string(s)
            } else {
                PropertyValue::String(s.clone())
            }
        }
        TokenOrValue::Float(f) => PropertyValue::Float(*f as f32),
        TokenOrValue::Int(i) => {
            if prop_type == "int" {
                PropertyValue::Int(*i as i32)
            } else {
                PropertyValue::Float(*i as f32)
            }
        }
        TokenOrValue::Bool(b) => PropertyValue::Bool(*b),
    }
}

/// Parse a color string (hex format) to PropertyValue::Color
fn parse_color_string(s: &str) -> PropertyValue {
    let hex = s.trim().trim_start_matches('#');

    // Handle 8-char ARGB format
    if hex.len() == 8 {
        if let (Ok(a), Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
            u8::from_str_radix(&hex[6..8], 16),
        ) {
            return PropertyValue::Color(Color::from_argb_u8(a, r, g, b));
        }
    }

    // Handle 3-char shorthand
    let hex = if hex.len() == 3 {
        hex.chars().flat_map(|c| std::iter::repeat(c).take(2)).collect::<String>()
    } else {
        hex.to_string()
    };

    // Parse 6-char RGB
    if hex.len() >= 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        ) {
            return PropertyValue::Color(Color::from_rgb_u8(r, g, b));
        }
    }

    // Fallback to black
    PropertyValue::Color(Color::from_rgb_u8(0, 0, 0))
}

/// Get a default value for a property type.
fn default_for_type(prop_type: &str) -> PropertyValue {
    match prop_type {
        "color" => PropertyValue::Color(Color::from_rgb_u8(128, 128, 128)),
        "float" => PropertyValue::Float(0.0),
        "int" => PropertyValue::Int(0),
        "bool" => PropertyValue::Bool(false),
        "string" => PropertyValue::String(String::new()),
        _ => PropertyValue::Float(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginDefinition;

    #[test]
    fn size_default_base_applies_to_unspecified_sizes() {
        // `default` is the shared base for every size; `lg` overrides it.
        let json = r##"{
            "component": "Test",
            "variants": ["default"],
            "states": ["normal"],
            "sizes": ["sm", "md", "lg"],
            "sizeProps": [
                { "name": "border-radius", "type": "float",
                  "defaults": { "default": 8.0, "lg": 12.0 } }
            ]
        }"##;
        let plugin: PluginDefinition = serde_json::from_str(json).unwrap();
        let data = ComponentThemeData::from_plugin(&plugin, &TokenStore::default());

        // sm and md inherit the base (8); lg uses its own override (12).
        assert_eq!(data.size_props["sm"]["border-radius"].as_float(), Some(8.0));
        assert_eq!(data.size_props["md"]["border-radius"].as_float(), Some(8.0));
        assert_eq!(data.size_props["lg"]["border-radius"].as_float(), Some(12.0));
    }

    #[test]
    fn default_key_seeds_virtual_default_size_and_marks_per_size_overrides() {
        // `default` is the shared base for every size; `lg` overrides it.
        let json = r##"{
            "component": "Test",
            "variants": ["default"],
            "states": ["normal"],
            "sizes": ["sm", "md", "lg"],
            "sizeProps": [
                { "name": "border-radius", "type": "float",
                  "defaults": { "default": 8.0, "lg": 12.0 } }
            ]
        }"##;
        let plugin: PluginDefinition = serde_json::from_str(json).unwrap();
        let data = ComponentThemeData::from_plugin(&plugin, &TokenStore::default());

        // Virtual "default" row holds the base (8).
        assert_eq!(data.size_props[DEFAULT_SIZE_KEY]["border-radius"].as_float(), Some(8.0));
        // sm and md inherit (no override flag); lg has an explicit per-size override.
        assert!(!data.size_prop_is_overridden("sm", "border-radius"));
        assert!(!data.size_prop_is_overridden("md", "border-radius"));
        assert!(data.size_prop_is_overridden("lg", "border-radius"));
    }

    #[test]
    fn editing_default_cascades_to_inheriting_sizes_only() {
        let json = r##"{
            "component": "Test",
            "variants": ["default"],
            "states": ["normal"],
            "sizes": ["sm", "md", "lg"],
            "sizeProps": [
                { "name": "border-radius", "type": "float",
                  "defaults": { "default": 8.0, "lg": 12.0 } }
            ]
        }"##;
        let plugin: PluginDefinition = serde_json::from_str(json).unwrap();
        let mut data = ComponentThemeData::from_plugin(&plugin, &TokenStore::default());

        data.set_size_default("border-radius", PropertyValue::Float(20.0));
        // The base + every inheriting size move to the new value...
        assert_eq!(data.size_props[DEFAULT_SIZE_KEY]["border-radius"].as_float(), Some(20.0));
        assert_eq!(data.size_props["sm"]["border-radius"].as_float(), Some(20.0));
        assert_eq!(data.size_props["md"]["border-radius"].as_float(), Some(20.0));
        // ...but the lg override is left alone.
        assert_eq!(data.size_props["lg"]["border-radius"].as_float(), Some(12.0));
    }

    #[test]
    fn set_and_clear_size_override_reinherits_from_default() {
        let json = r##"{
            "component": "Test",
            "variants": ["default"],
            "states": ["normal"],
            "sizes": ["sm", "md"],
            "sizeProps": [
                { "name": "border-radius", "type": "float",
                  "defaults": { "default": 8.0 } }
            ]
        }"##;
        let plugin: PluginDefinition = serde_json::from_str(json).unwrap();
        let mut data = ComponentThemeData::from_plugin(&plugin, &TokenStore::default());

        // Start: both inheriting from default = 8.
        assert!(!data.size_prop_is_overridden("sm", "border-radius"));
        // Override sm to 16.
        data.set_size_override("sm", "border-radius", PropertyValue::Float(16.0));
        assert!(data.size_prop_is_overridden("sm", "border-radius"));
        assert_eq!(data.size_props["sm"]["border-radius"].as_float(), Some(16.0));
        // A default edit should NOT touch sm (it's now overridden) but should still hit md.
        data.set_size_default("border-radius", PropertyValue::Float(4.0));
        assert_eq!(data.size_props["sm"]["border-radius"].as_float(), Some(16.0));
        assert_eq!(data.size_props["md"]["border-radius"].as_float(), Some(4.0));
        // Clearing the sm override re-inherits the current default (4).
        data.clear_size_override("sm", "border-radius");
        assert!(!data.size_prop_is_overridden("sm", "border-radius"));
        assert_eq!(data.size_props["sm"]["border-radius"].as_float(), Some(4.0));
    }

    #[test]
    fn cross_component_cascade_updates_inheriting_child_default() {
        // Parent (Input-like) declares per-size keys with no "default" key.
        // Child (Search-like) extends parent and starts with no local overrides.
        let parent_json = r##"{
            "component": "Input",
            "variants": ["default"],
            "states": ["normal"],
            "sizes": ["sm", "md", "lg"],
            "sizeProps": [
                { "name": "border-radius", "type": "float",
                  "defaults": { "sm": 4.0, "md": 8.0, "lg": 12.0 } }
            ]
        }"##;
        let mut child_plugin: PluginDefinition = serde_json::from_str(parent_json).unwrap();
        child_plugin.component = "Search".to_string();
        child_plugin.parent_key = Some("Input".to_string());
        let parent: PluginDefinition = serde_json::from_str(parent_json).unwrap();

        let tokens = TokenStore::default();
        let mut themes = HashMap::new();
        themes.insert("input".to_string(), ComponentThemeData::from_plugin(&parent, &tokens));
        themes.insert("search".to_string(), ComponentThemeData::from_plugin(&child_plugin, &tokens));

        // Both start with their Default base inferred from md = 8.0.
        assert_eq!(themes["input"].size_props[DEFAULT_SIZE_KEY]["border-radius"].as_float(), Some(8.0));
        assert_eq!(themes["search"].size_props[DEFAULT_SIZE_KEY]["border-radius"].as_float(), Some(8.0));

        // Edit Input's Default base: cascade should reach Search.
        themes.get_mut("input").unwrap().set_size_default("border-radius", PropertyValue::Float(20.0));
        cascade_default_to_children(&mut themes, "Input", "border-radius", &PropertyValue::Float(20.0));

        assert_eq!(
            themes["search"].size_props[DEFAULT_SIZE_KEY]["border-radius"].as_float(),
            Some(20.0),
            "Search's Default should follow Input's edit"
        );
    }

    #[test]
    fn locally_overridden_child_default_ignores_parent_cascade() {
        let parent_json = r##"{
            "component": "Input",
            "variants": ["default"],
            "states": ["normal"],
            "sizes": ["sm", "md", "lg"],
            "sizeProps": [
                { "name": "border-radius", "type": "float",
                  "defaults": { "sm": 4.0, "md": 8.0, "lg": 12.0 } }
            ]
        }"##;
        let mut child_plugin: PluginDefinition = serde_json::from_str(parent_json).unwrap();
        child_plugin.component = "Search".to_string();
        child_plugin.parent_key = Some("Input".to_string());
        let parent: PluginDefinition = serde_json::from_str(parent_json).unwrap();
        let tokens = TokenStore::default();
        let mut themes = HashMap::new();
        themes.insert("input".to_string(), ComponentThemeData::from_plugin(&parent, &tokens));
        themes.insert("search".to_string(), ComponentThemeData::from_plugin(&child_plugin, &tokens));

        // User overrides Search's Default (e.g., edits in the editor).
        themes.get_mut("search").unwrap().set_size_default("border-radius", PropertyValue::Float(40.0));
        themes.get_mut("search").unwrap().mark_parent_override("border-radius");

        // Now editing Input's Default should NOT reach Search.
        themes.get_mut("input").unwrap().set_size_default("border-radius", PropertyValue::Float(20.0));
        cascade_default_to_children(&mut themes, "Input", "border-radius", &PropertyValue::Float(20.0));

        assert_eq!(
            themes["search"].size_props[DEFAULT_SIZE_KEY]["border-radius"].as_float(),
            Some(40.0),
            "Search overridden its Default — parent cascade should skip it"
        );

        // Clearing the parent override re-inherits the parent's CURRENT value.
        let parent_value = themes["input"].size_props[DEFAULT_SIZE_KEY]["border-radius"].clone();
        themes.get_mut("search").unwrap().clear_parent_override("border-radius", parent_value);
        assert!(!themes["search"].parent_prop_is_overridden("border-radius"));
        assert_eq!(
            themes["search"].size_props[DEFAULT_SIZE_KEY]["border-radius"].as_float(),
            Some(20.0),
            "After clearing the override, Search re-inherits Input's current 20.0"
        );
    }

    #[test]
    fn sequential_clear_size_override_does_not_re_add_other_props() {
        // Regression: reproducing a report where resetting one prop on a
        // child-component's per-size row caused previously-cleared props to
        // re-appear as overridden. Pure data-model test: just hammer the API
        // and assert size_overrides only loses entries, never gains them.
        let parent_json = r##"{
            "component": "Input",
            "variants": ["default"],
            "states": ["normal"],
            "sizes": ["sm", "md", "lg"],
            "sizeProps": [
                { "name": "border-width", "type": "float",
                  "defaults": { "sm": 1.0, "md": 1.0, "lg": 1.0 } },
                { "name": "border-radius", "type": "float",
                  "defaults": { "sm": 4.0, "md": 8.0, "lg": 12.0 } },
                { "name": "padding-horizontal", "type": "float",
                  "defaults": { "sm": 8.0, "md": 12.0, "lg": 16.0 } },
                { "name": "field-height", "type": "float",
                  "defaults": { "sm": 32.0, "md": 40.0, "lg": 48.0 } },
                { "name": "font-size", "type": "float",
                  "defaults": { "sm": 12.0, "md": 14.0, "lg": 16.0 } },
                { "name": "font-family", "type": "string",
                  "defaults": { "sm": "font.primary", "md": "font.primary", "lg": "font.primary" } }
            ]
        }"##;
        let mut child_plugin: PluginDefinition = serde_json::from_str(parent_json).unwrap();
        child_plugin.component = "Search".to_string();
        child_plugin.parent_key = Some("Input".to_string());

        let mut data = ComponentThemeData::from_plugin(&child_plugin, &TokenStore::default());

        // All six props should start as overridden in lg (per JSON's lg key).
        let lg_init: HashSet<String> = data.size_overrides["lg"].iter().cloned().collect();
        assert_eq!(lg_init.len(), 6);

        // Clear four of them, one at a time.
        for name in ["border-radius", "padding-horizontal", "field-height", "font-size"] {
            data.clear_size_override("lg", name);
            assert!(!data.size_overrides["lg"].contains(name), "{} should be removed", name);
        }
        assert_eq!(data.size_overrides["lg"].len(), 2);
        assert!(data.size_overrides["lg"].contains("border-width"));
        assert!(data.size_overrides["lg"].contains("font-family"));

        // Now clear border-width — only font-family should remain.
        data.clear_size_override("lg", "border-width");
        let after: HashSet<String> = data.size_overrides["lg"].iter().cloned().collect();
        assert_eq!(after.len(), 1, "expected {{font-family}}, got {:?}", after);
        assert!(after.contains("font-family"));
    }

    #[test]
    fn per_size_only_defaults_still_work() {
        // Backward compatibility: no `default` base, each size specifies its own.
        let json = r##"{
            "component": "Test",
            "variants": ["default"],
            "states": ["normal"],
            "sizes": ["sm", "md", "lg"],
            "sizeProps": [
                { "name": "font-size", "type": "float",
                  "defaults": { "sm": 14.0, "md": 16.0, "lg": 18.0 } }
            ]
        }"##;
        let plugin: PluginDefinition = serde_json::from_str(json).unwrap();
        let data = ComponentThemeData::from_plugin(&plugin, &TokenStore::default());

        assert_eq!(data.size_props["sm"]["font-size"].as_float(), Some(14.0));
        assert_eq!(data.size_props["md"]["font-size"].as_float(), Some(16.0));
        assert_eq!(data.size_props["lg"]["font-size"].as_float(), Some(18.0));
    }
}
