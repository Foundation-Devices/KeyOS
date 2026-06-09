// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{HashMap, HashSet};

use super::tokens::{Color, ThemeTokens, TokenSet, TokenValue};
use super::{ColorScheme, ComponentState};

/// A resolved style value that can be any supported type.
#[derive(Debug, Clone, PartialEq)]
pub enum StyleValue {
    Color(Color),
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
}

/// Style properties contain resolved native values ready for Slint.
/// Standard fields can also carry token references in `token_refs`, which are resolved
/// when a consumer asks for a scheme-specific component style.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleProps {
    pub background: Option<Color>,
    pub foreground: Option<Color>,
    pub border_color: Option<Color>,
    pub border_width: Option<f32>,
    pub border_radius: Option<f32>,
    pub padding_horizontal: Option<f32>,
    pub padding_vertical: Option<f32>,
    pub min_height: Option<f32>,
    pub min_width: Option<f32>,
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub font_weight: Option<i32>,
    pub icon_size: Option<f32>,
    pub opacity: Option<f32>,
    pub touch_expansion: Option<f32>,

    /// Component-specific props (e.g., track-background for Toggle).
    pub extra: HashMap<String, StyleValue>,

    /// Raw token references keyed by normalized property name.
    pub token_refs: HashMap<String, String>,
}

impl StyleProps {
    pub fn new() -> Self { Self::default() }

    /// Merge another StyleProps into this one.
    /// Values from `other` override values in `self` if they are Some.
    pub fn merge(&mut self, other: &StyleProps) {
        merge_copy_prop(
            &mut self.background,
            &mut self.token_refs,
            "background",
            other.background,
            &other.token_refs,
        );
        merge_copy_prop(
            &mut self.foreground,
            &mut self.token_refs,
            "foreground",
            other.foreground,
            &other.token_refs,
        );
        merge_copy_prop(
            &mut self.border_color,
            &mut self.token_refs,
            "border_color",
            other.border_color,
            &other.token_refs,
        );
        merge_copy_prop(
            &mut self.border_width,
            &mut self.token_refs,
            "border_width",
            other.border_width,
            &other.token_refs,
        );
        merge_copy_prop(
            &mut self.border_radius,
            &mut self.token_refs,
            "border_radius",
            other.border_radius,
            &other.token_refs,
        );
        merge_copy_prop(
            &mut self.padding_horizontal,
            &mut self.token_refs,
            "padding_horizontal",
            other.padding_horizontal,
            &other.token_refs,
        );
        merge_copy_prop(
            &mut self.padding_vertical,
            &mut self.token_refs,
            "padding_vertical",
            other.padding_vertical,
            &other.token_refs,
        );
        merge_copy_prop(
            &mut self.min_height,
            &mut self.token_refs,
            "min_height",
            other.min_height,
            &other.token_refs,
        );
        merge_copy_prop(
            &mut self.min_width,
            &mut self.token_refs,
            "min_width",
            other.min_width,
            &other.token_refs,
        );
        merge_string_prop(
            &mut self.font_family,
            &mut self.token_refs,
            "font_family",
            &other.font_family,
            &other.token_refs,
        );
        merge_copy_prop(
            &mut self.font_size,
            &mut self.token_refs,
            "font_size",
            other.font_size,
            &other.token_refs,
        );
        merge_copy_prop(
            &mut self.font_weight,
            &mut self.token_refs,
            "font_weight",
            other.font_weight,
            &other.token_refs,
        );
        merge_copy_prop(
            &mut self.icon_size,
            &mut self.token_refs,
            "icon_size",
            other.icon_size,
            &other.token_refs,
        );
        merge_copy_prop(&mut self.opacity, &mut self.token_refs, "opacity", other.opacity, &other.token_refs);
        merge_copy_prop(
            &mut self.touch_expansion,
            &mut self.token_refs,
            "touch_expansion",
            other.touch_expansion,
            &other.token_refs,
        );

        let mut extra_keys: HashSet<String> = other.extra.keys().cloned().collect();
        extra_keys.extend(other.token_refs.keys().filter(|key| !is_standard_prop(key)).cloned());

        for key in extra_keys {
            if let Some(path) = other.token_refs.get(&key) {
                self.extra.remove(&key);
                self.token_refs.insert(key, path.clone());
            } else if let Some(value) = other.extra.get(&key) {
                self.extra.insert(key.clone(), value.clone());
                self.token_refs.remove(&key);
            }
        }
    }

    pub fn merged_with(&self, other: &StyleProps) -> StyleProps {
        let mut merged = self.clone();
        merged.merge(other);
        merged
    }

    fn resolved_for_tokens(&self, tokens: &TokenSet) -> StyleProps {
        let mut resolved = self.clone();
        resolved.token_refs.clear();

        if let Some(path) = self.token_refs.get("background") {
            resolved.background = resolve_color_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("foreground") {
            resolved.foreground = resolve_color_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("border_color") {
            resolved.border_color = resolve_color_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("border_width") {
            resolved.border_width = resolve_float_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("border_radius") {
            resolved.border_radius = resolve_float_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("padding_horizontal") {
            resolved.padding_horizontal = resolve_float_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("padding_vertical") {
            resolved.padding_vertical = resolve_float_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("min_height") {
            resolved.min_height = resolve_float_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("min_width") {
            resolved.min_width = resolve_float_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("font_family") {
            resolved.font_family = resolve_string_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("font_size") {
            resolved.font_size = resolve_float_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("font_weight") {
            resolved.font_weight = resolve_int_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("icon_size") {
            resolved.icon_size = resolve_float_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("opacity") {
            resolved.opacity = resolve_float_token(tokens, path);
        }
        if let Some(path) = self.token_refs.get("touch_expansion") {
            resolved.touch_expansion = resolve_float_token(tokens, path);
        }

        for (key, path) in &self.token_refs {
            if is_standard_prop(key) {
                continue;
            }

            resolved.extra.remove(key);
            if let Some(value) = resolve_style_value_token(tokens, path) {
                resolved.extra.insert(key.clone(), value);
            }
        }

        resolved
    }

    /// Get an extra property as a color.
    pub fn get_extra_color(&self, key: &str) -> Option<Color> {
        match self.extra.get(key)? {
            StyleValue::Color(c) => Some(*c),
            _ => None,
        }
    }

    /// Get an extra property as a float.
    pub fn get_extra_float(&self, key: &str) -> Option<f32> {
        match self.extra.get(key)? {
            StyleValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Set an extra color property.
    pub fn set_extra_color(&mut self, key: &str, value: Color) {
        self.extra.insert(key.to_string(), StyleValue::Color(value));
    }

    /// Set an extra float property.
    pub fn set_extra_float(&mut self, key: &str, value: f32) {
        self.extra.insert(key.to_string(), StyleValue::Float(value));
    }
}

fn merge_copy_prop<T: Copy>(
    field: &mut Option<T>,
    token_refs: &mut HashMap<String, String>,
    key: &str,
    other_value: Option<T>,
    other_token_refs: &HashMap<String, String>,
) {
    if let Some(path) = other_token_refs.get(key) {
        *field = None;
        token_refs.insert(key.to_string(), path.clone());
    } else if other_value.is_some() {
        *field = other_value;
        token_refs.remove(key);
    }
}

fn merge_string_prop(
    field: &mut Option<String>,
    token_refs: &mut HashMap<String, String>,
    key: &str,
    other_value: &Option<String>,
    other_token_refs: &HashMap<String, String>,
) {
    if let Some(path) = other_token_refs.get(key) {
        *field = None;
        token_refs.insert(key.to_string(), path.clone());
    } else if other_value.is_some() {
        *field = other_value.clone();
        token_refs.remove(key);
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

fn resolve_token_value<'a>(tokens: &'a TokenSet, path: &str) -> Option<&'a TokenValue> {
    tokens.resolve_reference(path, None)
}

fn resolve_color_token(tokens: &TokenSet, path: &str) -> Option<Color> {
    match resolve_token_value(tokens, path)? {
        TokenValue::Color(color) => Some(*color),
        _ => None,
    }
}

fn resolve_float_token(tokens: &TokenSet, path: &str) -> Option<f32> {
    match resolve_token_value(tokens, path)? {
        TokenValue::Float(number) => Some(*number),
        TokenValue::Int(number) => Some(*number as f32),
        _ => None,
    }
}

fn resolve_int_token(tokens: &TokenSet, path: &str) -> Option<i32> {
    match resolve_token_value(tokens, path)? {
        TokenValue::Int(number) => Some(*number),
        TokenValue::Float(number) => Some(*number as i32),
        _ => None,
    }
}

fn resolve_string_token(tokens: &TokenSet, path: &str) -> Option<String> {
    match resolve_token_value(tokens, path)? {
        TokenValue::String(text) => Some(text.clone()),
        _ => None,
    }
}

fn resolve_style_value_token(tokens: &TokenSet, path: &str) -> Option<StyleValue> {
    match resolve_token_value(tokens, path)? {
        TokenValue::Color(color) => Some(StyleValue::Color(*color)),
        TokenValue::Float(number) => Some(StyleValue::Float(*number)),
        TokenValue::Int(number) => Some(StyleValue::Int(*number)),
        TokenValue::Bool(flag) => Some(StyleValue::Bool(*flag)),
        TokenValue::String(text) => Some(StyleValue::String(text.clone())),
        TokenValue::Ref(_) => None,
    }
}

/// Theme for a single variant of a component.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VariantTheme {
    /// Default state styles (inherits from component base).
    pub default: StyleProps,

    /// Size-specific styles (e.g., "sm", "md", "lg").
    pub sizes: HashMap<String, StyleProps>,

    /// State overrides (inherit from variant default).
    pub states: HashMap<String, StyleProps>,
}

impl VariantTheme {
    pub fn new() -> Self { Self::default() }

    pub fn inherit_from(&mut self, parent: &VariantTheme) {
        self.default = parent.default.merged_with(&self.default);

        let mut merged_sizes = parent.sizes.clone();
        for (key, child_style) in &self.sizes {
            let merged = merged_sizes.get(key).cloned().unwrap_or_default().merged_with(child_style);
            merged_sizes.insert(key.clone(), merged);
        }
        self.sizes = merged_sizes;

        let mut merged_states = parent.states.clone();
        for (key, child_style) in &self.states {
            let merged = merged_states.get(key).cloned().unwrap_or_default().merged_with(child_style);
            merged_states.insert(key.clone(), merged);
        }
        self.states = merged_states;
    }
}

/// Theme for a component with base styles and variants.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComponentTheme {
    /// Base styles shared across all variants of this component.
    pub base: StyleProps,

    /// Variants for this component (e.g., "primary", "secondary").
    pub variants: HashMap<String, VariantTheme>,
}

impl ComponentTheme {
    pub fn new() -> Self { Self::default() }

    pub fn inherit_from(&mut self, parent: &ComponentTheme) {
        self.base = parent.base.merged_with(&self.base);

        let mut merged_variants = parent.variants.clone();
        for (key, child_variant) in &self.variants {
            let mut merged = child_variant.clone();
            if let Some(parent_variant) = merged_variants.get(key) {
                merged.inherit_from(parent_variant);
            }
            merged_variants.insert(key.clone(), merged);
        }
        self.variants = merged_variants;
    }
}

/// The complete theme with tokens and component styles.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Theme {
    /// Global design tokens (can differ by color scheme).
    pub tokens: ThemeTokens,

    /// Per-component style definitions.
    pub components: HashMap<String, ComponentTheme>,
}

impl Theme {
    pub fn new() -> Self { Self::default() }

    pub fn inherit_from(&mut self, parent: &Theme) {
        self.tokens = self.tokens.merged_with_parent(&parent.tokens);

        let mut merged_components = parent.components.clone();
        for (name, child_component) in &self.components {
            let mut merged = child_component.clone();
            if let Some(parent_component) = merged_components.get(name) {
                merged.inherit_from(parent_component);
            }
            merged_components.insert(name.clone(), merged);
        }
        self.components = merged_components;
    }

    pub fn with_parent(mut self, parent: &Theme) -> Self {
        self.inherit_from(parent);
        self
    }

    /// Resolve styles for a component, applying the inheritance chain:
    /// Component Base -> Variant Default -> Size Styles -> State Overrides.
    pub fn resolve_style_for_state_key(
        &self,
        component_name: &str,
        variant_name: &str,
        size: Option<&str>,
        state_key: &str,
    ) -> StyleProps {
        self.resolve_style_for_state_key_with_scheme(
            component_name,
            variant_name,
            size,
            state_key,
            ColorScheme::Light,
        )
    }

    pub fn resolve_style_for_state_key_with_scheme(
        &self,
        component_name: &str,
        variant_name: &str,
        size: Option<&str>,
        state_key: &str,
        scheme: ColorScheme,
    ) -> StyleProps {
        let mut resolved = StyleProps::new();

        match self.components.get(component_name) {
            Some(component) => {
                resolved.merge(&component.base);

                match component.variants.get(variant_name) {
                    Some(variant) => {
                        resolved.merge(&variant.default);

                        if let Some(size_key) = size {
                            if let Some(size_styles) = variant.sizes.get(size_key) {
                                resolved.merge(size_styles);
                            } else {
                                eprintln!(
                                    "Theme: no size styles for {component_name}/{variant_name}/{size_key}; \
                                     using variant default"
                                );
                            }
                        }

                        if !state_key.is_empty() && state_key != "default" && state_key != "normal" {
                            if let Some(state_overrides) = variant.states.get(state_key) {
                                resolved.merge(state_overrides);
                            } else {
                                eprintln!(
                                    "Theme: no state overrides for {component_name}/{variant_name}/{state_key}; \
                                     using variant default"
                                );
                            }
                        }
                    }
                    None => eprintln!(
                        "Theme: no variant {variant_name:?} for component {component_name:?}; \
                         returning empty StyleProps"
                    ),
                }
            }
            None => eprintln!(
                "Theme: no styles registered for component {component_name:?}; returning empty StyleProps"
            ),
        }

        resolved.resolved_for_tokens(self.tokens.for_scheme(scheme))
    }

    pub fn resolve_style(
        &self,
        component_name: &str,
        variant_name: &str,
        size: Option<&str>,
        state: ComponentState,
    ) -> StyleProps {
        self.resolve_style_for_state_key(component_name, variant_name, size, state.key())
    }

    /// Get resolved props for a component (public API).
    pub fn get_component_style(
        &self,
        component: &str,
        variant: &str,
        size: Option<&str>,
        state: ComponentState,
        scheme: ColorScheme,
    ) -> StyleProps {
        self.resolve_style_for_state_key_with_scheme(component, variant, size, state.key(), scheme)
    }

    pub fn get_component_style_by_state_key(
        &self,
        component: &str,
        variant: &str,
        size: Option<&str>,
        state_key: &str,
        scheme: ColorScheme,
    ) -> StyleProps {
        self.resolve_style_for_state_key_with_scheme(component, variant, size, state_key, scheme)
    }
}

/// Get a design token value directly.
pub fn get_token(tokens: &ThemeTokens, category: &str, key: &str, scheme: ColorScheme) -> Option<TokenValue> {
    tokens.for_scheme(scheme).get(category, key).cloned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenReference(String);

impl TokenReference {
    pub fn new(path: impl Into<String>) -> Self { Self(path.into()) }

    pub fn as_str(&self) -> &str { &self.0 }
}

pub fn token_ref(path: impl Into<String>) -> TokenReference { TokenReference::new(path) }

pub fn color(hex: &str) -> Color {
    Color::from_hex(hex).unwrap_or_else(|| panic!("invalid color literal: {hex}"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum ThemeValue {
    Color(Color),
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
    TokenRef(String),
}

impl From<Color> for ThemeValue {
    fn from(value: Color) -> Self { Self::Color(value) }
}

impl From<TokenReference> for ThemeValue {
    fn from(value: TokenReference) -> Self { Self::TokenRef(value.0) }
}

impl From<String> for ThemeValue {
    fn from(value: String) -> Self { Self::String(value) }
}

impl From<&str> for ThemeValue {
    fn from(value: &str) -> Self { Self::String(value.to_string()) }
}

impl From<f32> for ThemeValue {
    fn from(value: f32) -> Self { Self::Float(value) }
}

impl From<f64> for ThemeValue {
    fn from(value: f64) -> Self { Self::Float(value as f32) }
}

impl From<i32> for ThemeValue {
    fn from(value: i32) -> Self { Self::Int(value) }
}

impl From<i64> for ThemeValue {
    fn from(value: i64) -> Self { Self::Int(value as i32) }
}

impl From<usize> for ThemeValue {
    fn from(value: usize) -> Self { Self::Int(value as i32) }
}

impl From<bool> for ThemeValue {
    fn from(value: bool) -> Self { Self::Bool(value) }
}

fn expect_color(prop_name: &str, value: ThemeValue) -> Color {
    match value {
        ThemeValue::Color(color) => color,
        ThemeValue::String(text) => Color::from_hex(&text)
            .unwrap_or_else(|| panic!("property '{prop_name}' expected a color, got '{text}'")),
        ThemeValue::TokenRef(path) => {
            panic!("property '{prop_name}' cannot use unresolved token reference '{path}'")
        }
        _ => panic!("property '{prop_name}' expected a color"),
    }
}

fn expect_float(prop_name: &str, value: ThemeValue) -> f32 {
    match value {
        ThemeValue::Float(number) => number,
        ThemeValue::Int(number) => number as f32,
        ThemeValue::TokenRef(path) => {
            panic!("property '{prop_name}' cannot use unresolved token reference '{path}'")
        }
        _ => panic!("property '{prop_name}' expected a numeric value"),
    }
}

fn expect_int(prop_name: &str, value: ThemeValue) -> i32 {
    match value {
        ThemeValue::Int(number) => number,
        ThemeValue::Float(number) => number as i32,
        ThemeValue::TokenRef(path) => {
            panic!("property '{prop_name}' cannot use unresolved token reference '{path}'")
        }
        _ => panic!("property '{prop_name}' expected an integer value"),
    }
}

fn expect_string(prop_name: &str, value: ThemeValue) -> String {
    match value {
        ThemeValue::String(text) => text,
        ThemeValue::TokenRef(path) => {
            panic!("property '{prop_name}' cannot use unresolved token reference '{path}'")
        }
        _ => panic!("property '{prop_name}' expected a string value"),
    }
}

fn normalize_prop_name(prop_name: &str) -> String { prop_name.trim().to_ascii_lowercase().replace('-', "_") }

pub fn set_token(theme: &mut Theme, scheme: ColorScheme, path: &str, value: impl Into<ThemeValue>) {
    let (category, key) = split_token_path(path);
    let token_value = match value.into() {
        ThemeValue::Color(color) => TokenValue::Color(color),
        ThemeValue::Float(number) => TokenValue::Float(number),
        ThemeValue::Int(number) => TokenValue::Int(number),
        ThemeValue::Bool(flag) => TokenValue::Bool(flag),
        ThemeValue::String(text) => TokenValue::String(text),
        ThemeValue::TokenRef(path) => TokenValue::Ref(path),
    };

    match scheme {
        ColorScheme::Light => theme.tokens.light.set(category, key, token_value),
        ColorScheme::Dark => theme.tokens.dark.set(category, key, token_value),
    }
}

pub fn set_style_prop(style: &mut StyleProps, prop_name: &str, value: impl Into<ThemeValue>) {
    let normalized = normalize_prop_name(prop_name);
    let value = value.into();

    if let ThemeValue::TokenRef(path) = &value {
        match normalized.as_str() {
            "background" => style.background = None,
            "foreground" => style.foreground = None,
            "border_color" => style.border_color = None,
            "border_width" => style.border_width = None,
            "border_radius" => style.border_radius = None,
            "padding_horizontal" => style.padding_horizontal = None,
            "padding_vertical" => style.padding_vertical = None,
            "min_height" => style.min_height = None,
            "min_width" => style.min_width = None,
            "font_family" => style.font_family = None,
            "font_size" => style.font_size = None,
            "font_weight" => style.font_weight = None,
            "icon_size" => style.icon_size = None,
            "opacity" => style.opacity = None,
            "touch_expansion" => style.touch_expansion = None,
            _ => {
                style.extra.remove(&normalized);
            }
        }
        style.token_refs.insert(normalized, path.clone());
        return;
    }

    match normalized.as_str() {
        "background" => {
            style.background = Some(expect_color(prop_name, value));
            style.token_refs.remove("background");
        }
        "foreground" => {
            style.foreground = Some(expect_color(prop_name, value));
            style.token_refs.remove("foreground");
        }
        "border_color" => {
            style.border_color = Some(expect_color(prop_name, value));
            style.token_refs.remove("border_color");
        }
        "border_width" => {
            style.border_width = Some(expect_float(prop_name, value));
            style.token_refs.remove("border_width");
        }
        "border_radius" => {
            style.border_radius = Some(expect_float(prop_name, value));
            style.token_refs.remove("border_radius");
        }
        "padding_horizontal" => {
            style.padding_horizontal = Some(expect_float(prop_name, value));
            style.token_refs.remove("padding_horizontal");
        }
        "padding_vertical" => {
            style.padding_vertical = Some(expect_float(prop_name, value));
            style.token_refs.remove("padding_vertical");
        }
        "min_height" => {
            style.min_height = Some(expect_float(prop_name, value));
            style.token_refs.remove("min_height");
        }
        "min_width" => {
            style.min_width = Some(expect_float(prop_name, value));
            style.token_refs.remove("min_width");
        }
        "font_family" => {
            style.font_family = Some(expect_string(prop_name, value));
            style.token_refs.remove("font_family");
        }
        "font_size" => {
            style.font_size = Some(expect_float(prop_name, value));
            style.token_refs.remove("font_size");
        }
        "font_weight" => {
            style.font_weight = Some(expect_int(prop_name, value));
            style.token_refs.remove("font_weight");
        }
        "icon_size" => {
            style.icon_size = Some(expect_float(prop_name, value));
            style.token_refs.remove("icon_size");
        }
        "opacity" => {
            style.opacity = Some(expect_float(prop_name, value));
            style.token_refs.remove("opacity");
        }
        "touch_expansion" => {
            style.touch_expansion = Some(expect_float(prop_name, value));
            style.token_refs.remove("touch_expansion");
        }
        _ => {
            let extra_value = match value {
                ThemeValue::Color(color) => StyleValue::Color(color),
                ThemeValue::Float(number) => StyleValue::Float(number),
                ThemeValue::Int(number) => StyleValue::Int(number),
                ThemeValue::Bool(flag) => StyleValue::Bool(flag),
                ThemeValue::String(text) => StyleValue::String(text),
                ThemeValue::TokenRef(path) => {
                    panic!("property '{prop_name}' cannot use unresolved token reference '{path}'")
                }
            };
            style.token_refs.remove(&normalized);
            style.extra.insert(normalized, extra_value);
        }
    }
}

pub fn component_mut<'a>(theme: &'a mut Theme, component_name: &str) -> &'a mut ComponentTheme {
    theme.components.entry(component_name.to_string()).or_default()
}

pub fn variant_mut<'a>(component: &'a mut ComponentTheme, variant_name: &str) -> &'a mut VariantTheme {
    component.variants.entry(variant_name.to_string()).or_default()
}

pub fn size_style_mut<'a>(variant: &'a mut VariantTheme, size_name: &str) -> &'a mut StyleProps {
    variant.sizes.entry(size_name.to_string()).or_default()
}

pub fn state_style_mut<'a>(variant: &'a mut VariantTheme, state_key: &str) -> &'a mut StyleProps {
    // Warn if state_key isn't one of the known ComponentState values — common typos
    // like "focussed" silently create a new entry that no component ever reads.
    if !is_known_state_key(state_key) {
        eprintln!(
            "Theme: state_style_mut called with unknown state key {state_key:?}. \
             Valid keys: default, focused, loading, pressed, disabled, confirmed, visited"
        );
    }
    variant.states.entry(state_key.to_string()).or_default()
}

fn is_known_state_key(key: &str) -> bool {
    matches!(
        key,
        "default" | "focused" | "loading" | "pressed" | "disabled" | "confirmed" | "visited"
    )
}

fn split_token_path(path: &str) -> (&str, &str) {
    path.split_once('.').unwrap_or_else(|| panic!("token path '{path}' must look like 'category.key'"))
}

#[allow(dead_code)]
pub fn get_token_set(tokens: &ThemeTokens, scheme: ColorScheme) -> &TokenSet { tokens.for_scheme(scheme) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_style_props_merge() {
        let mut base = StyleProps {
            background: Some(Color::rgb(255, 0, 0)),
            foreground: Some(Color::rgb(255, 255, 255)),
            border_radius: Some(8.0),
            font_family: Some("Arial".to_string()),
            ..Default::default()
        };

        let override_props = StyleProps {
            background: Some(Color::rgb(0, 255, 0)),
            font_size: Some(16.0),
            ..Default::default()
        };

        base.merge(&override_props);

        assert_eq!(base.background, Some(Color::rgb(0, 255, 0)));
        assert_eq!(base.foreground, Some(Color::rgb(255, 255, 255)));
        assert_eq!(base.font_size, Some(16.0));
        assert_eq!(base.border_radius, Some(8.0));
        assert_eq!(base.font_family.as_deref(), Some("Arial"));
    }

    #[test]
    fn test_theme_resolve_style() {
        let mut theme = Theme::new();

        let mut button = ComponentTheme::new();
        button.base = StyleProps { border_radius: Some(16.0), font_weight: Some(500), ..Default::default() };

        let mut primary = VariantTheme::new();
        primary.default = StyleProps {
            background: Some(Color::rgb(26, 154, 154)),
            foreground: Some(Color::rgb(255, 255, 255)),
            ..Default::default()
        };

        primary.sizes.insert(
            "sm".to_string(),
            StyleProps {
                font_size: Some(14.0),
                padding_horizontal: Some(12.0),
                min_height: Some(36.0),
                ..Default::default()
            },
        );
        primary.sizes.insert(
            "md".to_string(),
            StyleProps {
                font_size: Some(16.0),
                padding_horizontal: Some(16.0),
                min_height: Some(44.0),
                ..Default::default()
            },
        );

        primary
            .states
            .insert("disabled".to_string(), StyleProps { opacity: Some(0.5), ..Default::default() });

        button.variants.insert("primary".to_string(), primary);
        theme.components.insert("button".to_string(), button);

        let style = theme.resolve_style("button", "primary", Some("md"), ComponentState::Default);
        assert_eq!(style.background, Some(Color::rgb(26, 154, 154)));
        assert_eq!(style.foreground, Some(Color::rgb(255, 255, 255)));
        assert_eq!(style.border_radius, Some(16.0));
        assert_eq!(style.font_weight, Some(500));
        assert_eq!(style.font_size, Some(16.0));
        assert_eq!(style.padding_horizontal, Some(16.0));
        assert_eq!(style.min_height, Some(44.0));
        assert_eq!(style.opacity, None);

        let disabled_style = theme.resolve_style("button", "primary", Some("md"), ComponentState::Disabled);
        assert_eq!(disabled_style.opacity, Some(0.5));
        assert_eq!(disabled_style.background, Some(Color::rgb(26, 154, 154)));
    }

    #[test]
    fn test_theme_inheritance_merges_tokens_and_components() {
        let mut parent = Theme::new();
        set_token(&mut parent, ColorScheme::Light, "color.primary", color("#112233"));
        set_token(&mut parent, ColorScheme::Dark, "color.primary", color("#223344"));

        {
            let component = component_mut(&mut parent, "button");
            let variant = variant_mut(component, "primary");
            set_style_prop(&mut variant.default, "background", color("#112233"));
            set_style_prop(size_style_mut(variant, "sm"), "font_size", 14.0);
        }

        let mut child = Theme::new();
        set_token(&mut child, ColorScheme::Light, "color.primary", color("#abcdef"));
        {
            let component = component_mut(&mut child, "button");
            let variant = variant_mut(component, "primary");
            set_style_prop(&mut variant.default, "foreground", color("#ffffff"));
            set_style_prop(state_style_mut(variant, "pressed"), "opacity", 0.72);
        }

        let merged = child.with_parent(&parent);

        assert_eq!(merged.tokens.light.get_color("color", "primary"), Some(color("#abcdef")));
        assert_eq!(merged.tokens.dark.get_color("color", "primary"), Some(color("#abcdef")));

        let default_style = merged.resolve_style_for_state_key("button", "primary", Some("sm"), "normal");
        assert_eq!(default_style.background, Some(color("#112233")));
        assert_eq!(default_style.foreground, Some(color("#ffffff")));
        assert_eq!(default_style.font_size, Some(14.0));

        let pressed_style = merged.resolve_style_for_state_key("button", "primary", Some("sm"), "pressed");
        assert_eq!(pressed_style.opacity, Some(0.72));
    }

    #[test]
    fn test_style_props_resolve_token_refs_by_scheme() {
        let mut theme = Theme::new();
        set_token(&mut theme, ColorScheme::Light, "color.primary", color("#112233"));
        set_token(&mut theme, ColorScheme::Dark, "color.primary", color("#abcdef"));
        set_token(&mut theme, ColorScheme::Light, "font.primary", "Arial");
        set_token(&mut theme, ColorScheme::Dark, "font.primary", "Verdana");
        set_token(&mut theme, ColorScheme::Light, "fontSize.md", 16.0);
        set_token(&mut theme, ColorScheme::Dark, "fontSize.md", 18.0);

        let component = component_mut(&mut theme, "button");
        let variant = variant_mut(component, "primary");
        set_style_prop(&mut variant.default, "background", token_ref("color.primary"));
        set_style_prop(size_style_mut(variant, "md"), "font_family", token_ref("font.primary"));
        set_style_prop(size_style_mut(variant, "md"), "font_size", token_ref("fontSize.md"));

        let light = theme.get_component_style(
            "button",
            "primary",
            Some("md"),
            ComponentState::Default,
            ColorScheme::Light,
        );
        assert_eq!(light.background, Some(color("#112233")));
        assert_eq!(light.font_family.as_deref(), Some("Arial"));
        assert_eq!(light.font_size, Some(16.0));

        let dark = theme.get_component_style(
            "button",
            "primary",
            Some("md"),
            ComponentState::Default,
            ColorScheme::Dark,
        );
        assert_eq!(dark.background, Some(color("#abcdef")));
        assert_eq!(dark.font_family.as_deref(), Some("Verdana"));
        assert_eq!(dark.font_size, Some(18.0));
    }

    #[test]
    fn test_define_theme_macro_supports_extends() {
        crate::define_theme! {
            fn parent_theme() -> Theme {
                tokens {
                    light {
                        "color.primary" = color("#102030");
                        "fontSize.sm" = 14.0;
                    }
                }
                component "button" {
                    variant "primary" {
                        default {
                            "background" = color("#102030");
                        }
                        size "sm" {
                            "font_size" = token_ref("fontSize.sm");
                        }
                    }
                }
            }
        }

        crate::define_theme! {
            fn child_theme() -> Theme {
                extends parent_theme();
                tokens {
                    dark {
                        "color.primary" = color("#abcdef");
                    }
                }
                component "button" {
                    variant "primary" {
                        state "pressed" {
                            "opacity" = 0.4;
                        }
                    }
                }
            }
        }

        let theme = child_theme();
        assert_eq!(theme.tokens.light.get_color("color", "primary"), Some(color("#102030")));
        assert_eq!(theme.tokens.dark.get_color("color", "primary"), Some(color("#abcdef")));

        let default_style = theme.get_component_style(
            "button",
            "primary",
            Some("sm"),
            ComponentState::Default,
            ColorScheme::Light,
        );
        assert_eq!(default_style.background, Some(color("#102030")));
        assert_eq!(default_style.font_size, Some(14.0));

        let pressed_style = theme.get_component_style_by_state_key(
            "button",
            "primary",
            Some("sm"),
            "pressed",
            ColorScheme::Light,
        );
        assert_eq!(pressed_style.opacity, Some(0.4));
    }
}
