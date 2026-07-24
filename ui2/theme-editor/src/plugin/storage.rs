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

/// In-memory key for the virtual size row that holds the shared Common value
/// for every size prop. This remains `"default"` internally for compatibility
/// with the existing editor model; JSON import/export normalizes at the edge.
pub const DEFAULT_SIZE_KEY: &str = "default";
pub const NORMAL_STATE_KEY: &str = "normal";

/// Serialized key used for Common size values in newly-written theme JSON.
pub const COMMON_SIZE_KEY: &str = "common";

/// Legacy serialized key accepted for Common size values in old theme JSON and
/// plugin defaults.
pub const LEGACY_COMMON_SIZE_KEY: &str = "default";

pub fn is_common_size_key(key: &str) -> bool { key == COMMON_SIZE_KEY || key == LEGACY_COMMON_SIZE_KEY }

pub fn normalize_size_key(key: &str) -> &str {
    if is_common_size_key(key) {
        DEFAULT_SIZE_KEY
    } else {
        key
    }
}

pub fn serialize_size_key(key: &str) -> &str {
    if key == DEFAULT_SIZE_KEY {
        COMMON_SIZE_KEY
    } else {
        key
    }
}

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

    /// Per-(variant, state, prop) set of props that override the component's
    /// first/Common variant. Props absent from the set inherit the Common
    /// variant's value for the same state.
    pub variant_overrides: HashMap<String, HashMap<String, HashSet<String>>>,

    /// Size properties: size -> prop_name -> value. Includes the virtual
    /// `DEFAULT_SIZE_KEY` entry which holds the shared Common value for
    /// every size; each concrete size either equals that base (inheriting) or
    /// holds its own override (tracked in `size_overrides`).
    pub size_props: HashMap<String, HashMap<String, PropertyValue>>,

    /// Per-(concrete-size, prop) set of prop names that override the Common
    /// base. A prop name absent from a size's set is inheriting from Common.
    pub size_overrides: HashMap<String, HashSet<String>>,

    /// Optional parent component key (from the resolved plugin's `extends` /
    /// `parent_key`). When set, this component's Common base inherits from
    /// the parent's Common unless the prop is listed in `parent_overrides`.
    pub parent_key: Option<String>,

    /// Set of prop names whose Common value the user has locally overridden
    /// in this child component — these no longer follow the parent's Common
    /// edits. A prop name absent here is inheriting live from the parent.
    pub parent_overrides: HashSet<String>,

    /// First plugin variant. This is the Common/baseline variant for override
    /// comparisons even when the component does not define a variant literally
    /// named `"default"` (for example Button starts with `"primary"`).
    pub common_variant_key: Option<String>,

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
            variant_overrides: HashMap::new(),
            size_props: HashMap::new(),
            size_overrides: HashMap::new(),
            parent_key: plugin.parent_key.clone(),
            parent_overrides: HashSet::new(),
            common_variant_key: plugin.variants.first().cloned(),
            states: plugin.states.clone(),
            variant_prop_defs: plugin.variant_props.clone(),
            size_prop_defs: plugin.size_props.clone(),
        };
        // Initialize variant props with resolved defaults.
        for variant in &plugin.variants {
            let mut state_map = HashMap::new();

            for state in &plugin.states {
                let mut prop_map = HashMap::new();

                for prop_def in &plugin.variant_props {
                    let value = resolve_variant_default(
                        prop_def,
                        plugin.variants.first().map(String::as_str),
                        variant,
                        state,
                        tokens,
                    );
                    prop_map.insert(prop_def.name.clone(), value);
                }

                state_map.insert(state.clone(), prop_map);
            }

            data.variant_props.insert(variant.clone(), state_map);
        }
        data.seed_variant_override_flags_from_defaults(plugin);

        // Seed the virtual "default" base from each prop's `default` key (or the
        // type fallback when none is declared).
        let mut default_props = HashMap::new();
        for prop_def in &plugin.size_props {
            let value = resolve_size_base(prop_def, tokens);
            default_props.insert(prop_def.name.clone(), value);
        }
        data.size_props.insert(DEFAULT_SIZE_KEY.to_string(), default_props.clone());
        data.size_overrides.insert(DEFAULT_SIZE_KEY.to_string(), HashSet::new());

        // For each concrete size: a prop with an explicit per-size key in the
        // JSON defaults is an override; otherwise the size inherits the base.
        for size in &plugin.sizes {
            let mut prop_map = HashMap::new();
            let mut overrides = HashSet::new();
            for prop_def in &plugin.size_props {
                let value = resolve_size_default(prop_def, size, tokens);
                let base_value = default_props.get(&prop_def.name);
                if prop_def.defaults.values.contains_key(size) && base_value != Some(&value) {
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

    /// True when this variant/state has an override for the given prop. For
    /// Normal on later variants this means it overrides the Common variant; for
    /// non-Normal states this means it overrides that variant's Normal state.
    pub fn variant_prop_is_overridden(&self, variant: &str, state: &str, prop_name: &str) -> bool {
        self.variant_overrides
            .get(variant)
            .and_then(|states| states.get(state))
            .map(|props| props.contains(prop_name))
            .unwrap_or(false)
    }

    pub fn common_variant_key(&self) -> Option<&str> { self.common_variant_key.as_deref() }

    pub fn variant_is_common(&self, variant: &str) -> bool {
        self.common_variant_key.as_deref() == Some(variant)
    }

    pub fn variant_state_is_root(&self, variant: &str, state: &str) -> bool {
        self.variant_is_common(variant) && state == NORMAL_STATE_KEY
    }

    pub fn clear_local_override_flags(&mut self) {
        for states in self.variant_overrides.values_mut() {
            for props in states.values_mut() {
                props.clear();
            }
        }
        for props in self.size_overrides.values_mut() {
            props.clear();
        }
        self.parent_overrides.clear();
    }

    fn seed_variant_override_flags_from_defaults(&mut self, plugin: &PluginDefinition) {
        for variant in &plugin.variants {
            for state in &plugin.states {
                self.variant_overrides.entry(variant.clone()).or_default().entry(state.clone()).or_default();
                if self.variant_is_common(variant) && state == NORMAL_STATE_KEY {
                    continue;
                }

                for prop_def in &plugin.variant_props {
                    let Some(value) = self
                        .variant_props
                        .get(variant)
                        .and_then(|states| states.get(state))
                        .and_then(|props| props.get(&prop_def.name))
                    else {
                        continue;
                    };
                    let Some(fallback) =
                        self.variant_immediate_fallback_value(variant, state, &prop_def.name)
                    else {
                        continue;
                    };
                    if value != &fallback && prop_def.defaults.values.contains_key(variant) {
                        self.set_variant_override_flag(variant, state, &prop_def.name, true);
                    }
                }
            }
        }
    }

    fn variant_immediate_fallback_value(
        &self,
        variant: &str,
        state: &str,
        prop_name: &str,
    ) -> Option<PropertyValue> {
        if state != NORMAL_STATE_KEY {
            return self
                .variant_props
                .get(variant)
                .and_then(|states| states.get(NORMAL_STATE_KEY))
                .and_then(|props| props.get(prop_name))
                .cloned();
        }
        if !self.variant_is_common(variant) {
            return self
                .common_variant_key()
                .and_then(|common| self.variant_props.get(common))
                .and_then(|states| states.get(NORMAL_STATE_KEY))
                .and_then(|props| props.get(prop_name))
                .cloned();
        }
        None
    }

    fn set_variant_prop_value(&mut self, variant: &str, state: &str, prop_name: &str, value: PropertyValue) {
        if let Some(props) = self.variant_props.get_mut(variant).and_then(|states| states.get_mut(state)) {
            props.insert(prop_name.to_string(), value);
        }
    }

    fn set_variant_override_flag(
        &mut self,
        variant: &str,
        state: &str,
        prop_name: &str,
        is_overridden: bool,
    ) {
        let overrides = self
            .variant_overrides
            .entry(variant.to_string())
            .or_default()
            .entry(state.to_string())
            .or_default();
        if is_overridden {
            overrides.insert(prop_name.to_string());
        } else {
            overrides.remove(prop_name);
        }
    }

    /// Set a resolved variant value and its local override flag without
    /// re-running the component-local variant/state cascade. Component-parent
    /// inheritance is resolved by the editor because it needs access to the
    /// parent component's current values.
    pub fn set_variant_resolved_value(
        &mut self,
        variant: &str,
        state: &str,
        prop_name: &str,
        value: PropertyValue,
        is_overridden: bool,
    ) {
        self.set_variant_prop_value(variant, state, prop_name, value);
        self.set_variant_override_flag(variant, state, prop_name, is_overridden);
    }

    fn reapply_variant_inheritance_for_prop(&mut self, prop_name: &str) {
        let Some(common_variant_key) = self.common_variant_key.clone() else {
            return;
        };
        let common_normal_value = self
            .variant_props
            .get(&common_variant_key)
            .and_then(|states| states.get(NORMAL_STATE_KEY))
            .and_then(|props| props.get(prop_name))
            .cloned();

        if let Some(value) = common_normal_value {
            let variants: Vec<String> = self.variant_props.keys().cloned().collect();
            for variant in variants {
                if variant == common_variant_key {
                    continue;
                }
                if !self.variant_prop_is_overridden(&variant, NORMAL_STATE_KEY, prop_name) {
                    self.set_variant_prop_value(&variant, NORMAL_STATE_KEY, prop_name, value.clone());
                }
            }
        }

        let variants: Vec<String> = self.variant_props.keys().cloned().collect();
        for variant in variants {
            let normal_value = self
                .variant_props
                .get(&variant)
                .and_then(|states| states.get(NORMAL_STATE_KEY))
                .and_then(|props| props.get(prop_name))
                .cloned();
            let Some(normal_value) = normal_value else {
                continue;
            };
            let states: Vec<String> = self
                .variant_props
                .get(&variant)
                .map(|states| states.keys().cloned().collect())
                .unwrap_or_default();
            for state in states {
                if state == NORMAL_STATE_KEY {
                    continue;
                }
                if !self.variant_prop_is_overridden(&variant, &state, prop_name) {
                    self.set_variant_prop_value(&variant, &state, prop_name, normal_value.clone());
                }
            }
        }
    }

    /// Set a Common variant prop and cascade to every variant/state prop that
    /// currently inherits from it.
    pub fn set_variant_default(&mut self, state: &str, prop_name: &str, value: PropertyValue) {
        let Some(common_variant_key) = self.common_variant_key.clone() else {
            return;
        };
        self.set_variant_override(&common_variant_key, state, prop_name, value);
    }

    /// Clear a Common variant override and restore the schema default. The
    /// restored value cascades to variants that still inherit from Common.
    pub fn clear_variant_default(&mut self, state: &str, prop_name: &str, tokens: &TokenStore) {
        let Some(common_variant_key) = self.common_variant_key.clone() else {
            return;
        };
        let default_value = self
            .variant_prop_defs
            .iter()
            .find(|prop| prop.name == prop_name)
            .map(|prop| {
                resolve_variant_default(
                    prop,
                    self.common_variant_key.as_deref(),
                    &common_variant_key,
                    state,
                    tokens,
                )
            })
            .unwrap_or_else(|| default_for_type("float"));
        self.clear_variant_default_to_value(state, prop_name, default_value);
    }

    pub fn clear_variant_default_to_value(&mut self, state: &str, prop_name: &str, value: PropertyValue) {
        let Some(common_variant_key) = self.common_variant_key.clone() else {
            return;
        };
        self.clear_variant_override_to_value(&common_variant_key, state, prop_name, value);
    }

    /// Set a per-variant prop override.
    pub fn set_variant_override(
        &mut self,
        variant: &str,
        state: &str,
        prop_name: &str,
        value: PropertyValue,
    ) {
        self.set_variant_prop_value(variant, state, prop_name, value.clone());
        let is_overridden = if self.variant_is_common(variant) && state == NORMAL_STATE_KEY {
            true
        } else {
            self.variant_immediate_fallback_value(variant, state, prop_name)
                .map(|fallback| fallback != value)
                .unwrap_or(true)
        };
        self.set_variant_override_flag(variant, state, prop_name, is_overridden);
        self.reapply_variant_inheritance_for_prop(prop_name);
    }

    pub fn set_variant_import_override(
        &mut self,
        variant: &str,
        state: &str,
        prop_name: &str,
        value: PropertyValue,
    ) {
        self.set_variant_override(variant, state, prop_name, value);
    }

    /// Clear a per-variant override and re-inherit from the Common variant.
    pub fn clear_variant_override(&mut self, variant: &str, state: &str, prop_name: &str) {
        if self.variant_is_common(variant) && state == NORMAL_STATE_KEY {
            return;
        }
        let Some(value) = self.variant_immediate_fallback_value(variant, state, prop_name) else {
            return;
        };
        self.clear_variant_override_to_value(variant, state, prop_name, value);
    }

    pub fn clear_variant_override_to_value(
        &mut self,
        variant: &str,
        state: &str,
        prop_name: &str,
        value: PropertyValue,
    ) {
        self.set_variant_override_flag(variant, state, prop_name, false);
        self.set_variant_prop_value(variant, state, prop_name, value);
        self.reapply_variant_inheritance_for_prop(prop_name);
    }

    pub fn set_size_import_override(&mut self, size: &str, prop_name: &str, value: PropertyValue) {
        if size == DEFAULT_SIZE_KEY {
            self.set_size_default(prop_name, value);
            self.mark_size_default_override(prop_name);
            if self.parent_key.is_some() {
                self.mark_parent_override(prop_name);
            }
            return;
        }
        self.set_size_override(size, prop_name, value);
    }

    /// Mark the Common size value as explicitly supplied by the theme JSON (or
    /// by a user edit). This is distinct from the schema seed held in
    /// `size_props`, and lets an incomplete root Base Theme round-trip without
    /// silently materializing schema defaults into the file.
    pub fn mark_size_default_override(&mut self, prop_name: &str) {
        self.size_overrides.entry(DEFAULT_SIZE_KEY.to_string()).or_default().insert(prop_name.to_string());
    }

    /// Set the Common base for a prop and cascade the new value into every
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

    pub fn clear_size_default_to_value(&mut self, prop_name: &str, value: PropertyValue) {
        self.set_size_default(prop_name, value);
    }

    /// Clear a Common size value back to the schema seed. For a root Base Theme
    /// there is no inherited value underneath, so reset means "restore the
    /// schema/default seed" while the property remains an override of the empty
    /// base.
    pub fn clear_size_default(&mut self, prop_name: &str, tokens: &TokenStore) {
        let default_value = self
            .size_prop_defs
            .iter()
            .find(|prop| prop.name == prop_name)
            .map(|prop| resolve_size_base(prop, tokens))
            .unwrap_or_else(|| default_for_type("float"));
        self.clear_size_default_to_value(prop_name, default_value);
    }

    /// Set a per-size override for a prop (marks the prop as overridden for
    /// that size; future "default" edits won't cascade into it).
    pub fn set_size_override(&mut self, size: &str, prop_name: &str, value: PropertyValue) {
        if let Some(props) = self.size_props.get_mut(size) {
            props.insert(prop_name.to_string(), value.clone());
        }
        let base_value = self.size_props.get(DEFAULT_SIZE_KEY).and_then(|base| base.get(prop_name));
        if base_value == Some(&value) {
            if let Some(overrides) = self.size_overrides.get_mut(size) {
                overrides.remove(prop_name);
            }
        } else {
            self.size_overrides.entry(size.to_string()).or_default().insert(prop_name.to_string());
        }
    }

    /// Clear a per-size override and re-inherit from the Common base.
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
    /// Common value for the given prop (so live parent edits won't reach it).
    pub fn parent_prop_is_overridden(&self, prop_name: &str) -> bool {
        self.parent_overrides.contains(prop_name)
    }

    /// Mark a prop's Common value as locally overridden in this child — call
    /// when the user explicitly edits a child's Common prop.
    pub fn mark_parent_override(&mut self, prop_name: &str) {
        self.parent_overrides.insert(prop_name.to_string());
    }

    /// Drop the local parent-level override and re-inherit `value` from the
    /// parent's current Common value. Also cascades within the child via
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

/// Cross-component live cascade for a Common-base edit: every component whose
/// `parent_key` matches `parent_key` and which has NOT locally overridden this
/// prop gets its own Common value updated (which then cascades within that child to
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
    common_variant: Option<&str>,
    variant: &str,
    state: &str,
    tokens: &TokenStore,
) -> PropertyValue {
    if let Some(variant_defaults) = prop_def.defaults.values.get(variant) {
        if let Some(token_or_value) = variant_defaults.get_state(state) {
            return resolve_token_or_value(token_or_value, &prop_def.prop_type, tokens);
        }
        if state != NORMAL_STATE_KEY {
            if let Some(token_or_value) = variant_defaults.get_state(NORMAL_STATE_KEY) {
                return resolve_token_or_value(token_or_value, &prop_def.prop_type, tokens);
            }
        }
    }

    if let Some(common_variant) = common_variant {
        if variant != common_variant {
            if let Some(common_defaults) = prop_def.defaults.values.get(common_variant) {
                if let Some(token_or_value) = common_defaults.get_state(NORMAL_STATE_KEY) {
                    return resolve_token_or_value(token_or_value, &prop_def.prop_type, tokens);
                }
            }
        }
    }

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
    if let Some(base) = prop_def
        .defaults
        .values
        .get(COMMON_SIZE_KEY)
        .or_else(|| prop_def.defaults.values.get(LEGACY_COMMON_SIZE_KEY))
    {
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
    if let Some(base) = prop_def
        .defaults
        .values
        .get(COMMON_SIZE_KEY)
        .or_else(|| prop_def.defaults.values.get(LEGACY_COMMON_SIZE_KEY))
    {
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

    // Handle CSS/Slint 8-char RRGGBBAA format.
    if hex.len() == 8 {
        if let (Ok(r), Ok(g), Ok(b), Ok(a)) = (
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
    fn first_variant_is_common_variant_for_override_tracking() {
        let plugin: PluginDefinition =
            serde_json::from_str(include_str!("../../defaults/components/button.schema.json")).unwrap();
        let data = ComponentThemeData::from_plugin(&plugin, &TokenStore::default());

        assert_eq!(data.common_variant_key(), Some("primary"));
        for prop in &plugin.variant_props {
            assert!(
                !data.variant_prop_is_overridden("primary", "normal", &prop.name),
                "primary/normal/{} should be the common baseline",
                prop.name
            );
        }

        assert!(
            data.variant_prop_is_overridden("secondary", "normal", "background"),
            "secondary background differs from primary"
        );
        assert!(
            !data.variant_prop_is_overridden("secondary", "normal", "borderColor"),
            "same-as-primary values should not be marked as overrides"
        );
        assert!(
            !data.variant_prop_is_overridden("primary", "focused", "background"),
            "focused background equals normal and should inherit"
        );
        assert!(
            data.variant_prop_is_overridden("primary", "focused", "borderColor"),
            "focused border color differs from normal"
        );
        assert!(
            data.variant_prop_is_overridden("primary", "pressed", "background"),
            "pressed background differs from normal"
        );
        assert!(
            !data.variant_prop_is_overridden("primary", "disabled", "background"),
            "disabled background equals normal and should inherit"
        );
        assert!(
            data.variant_prop_is_overridden("primary", "disabled", "opacity"),
            "disabled opacity differs from normal"
        );
    }

    #[test]
    fn state_overrides_reset_to_normal_and_inheriting_states_follow_normal() {
        let plugin: PluginDefinition =
            serde_json::from_str(include_str!("../../defaults/components/button.schema.json")).unwrap();
        let mut data = ComponentThemeData::from_plugin(&plugin, &TokenStore::default());

        assert!(data.variant_prop_is_overridden("primary", "focused", "borderColor"));
        data.clear_variant_override("primary", "focused", "borderColor");
        assert!(!data.variant_prop_is_overridden("primary", "focused", "borderColor"));
        assert_eq!(
            data.variant_props["primary"]["focused"]["borderColor"],
            data.variant_props["primary"]["normal"]["borderColor"]
        );

        data.set_variant_override(
            "primary",
            "normal",
            "background",
            PropertyValue::Token("color.danger".to_string()),
        );
        assert_eq!(
            data.variant_props["primary"]["focused"]["background"],
            PropertyValue::Token("color.danger".to_string()),
            "focused background inherits primary normal"
        );
        assert_eq!(
            data.variant_props["primary"]["disabled"]["background"],
            PropertyValue::Token("color.danger".to_string()),
            "disabled background inherits primary normal"
        );
        assert_eq!(
            data.variant_props["primary"]["pressed"]["background"],
            PropertyValue::Token("color.primary.dark".to_string()),
            "pressed background keeps its state override"
        );
        assert_eq!(
            data.variant_props["secondary"]["normal"]["background"],
            PropertyValue::Token("color.secondary".to_string()),
            "secondary normal keeps its variant override"
        );
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

        // Only props whose explicit lg value differs from the Default base
        // should start as overridden. Same-as-default declarations are pruned.
        let lg_init: HashSet<String> = data.size_overrides["lg"].iter().cloned().collect();
        assert_eq!(lg_init.len(), 4);
        assert!(lg_init.contains("border-radius"));
        assert!(lg_init.contains("padding-horizontal"));
        assert!(lg_init.contains("field-height"));
        assert!(lg_init.contains("font-size"));

        // Clear four of them, one at a time.
        for name in ["border-radius", "padding-horizontal", "field-height", "font-size"] {
            data.clear_size_override("lg", name);
            assert!(!data.size_overrides["lg"].contains(name), "{} should be removed", name);
        }
        assert_eq!(data.size_overrides["lg"].len(), 0);

        // Clearing a non-overridden same-as-default prop should not add it back.
        data.clear_size_override("lg", "border-width");
        let after: HashSet<String> = data.size_overrides["lg"].iter().cloned().collect();
        assert_eq!(after.len(), 0, "expected no overrides, got {:?}", after);
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
