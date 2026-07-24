// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{HashMap, HashSet};

/// A value that can be stored in the token system
#[derive(Debug, Clone, PartialEq)]
pub enum TokenValue {
    Color(Color),
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
    /// Reference to another token path (e.g. "radius.md")
    Ref(String),
}

/// Simple RGBA color
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self { Self { r, g, b, a: 255 } }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self { Self { r, g, b, a } }

    /// Parse a hex color string like "#FF6B00" or "#FF6B00FF"
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.trim_start_matches('#');
        match hex.len() {
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Self::rgb(r, g, b))
            }
            8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
                Some(Self::rgba(r, g, b, a))
            }
            _ => None,
        }
    }

    /// Convert to Slint color
    pub fn to_slint(&self) -> slint::Color { slint::Color::from_argb_u8(self.a, self.r, self.g, self.b) }

    /// Lighten the color by a percentage (0.0 - 1.0)
    pub fn lighten(&self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        Self {
            r: (self.r as f32 + (255.0 - self.r as f32) * amount) as u8,
            g: (self.g as f32 + (255.0 - self.g as f32) * amount) as u8,
            b: (self.b as f32 + (255.0 - self.b as f32) * amount) as u8,
            a: self.a,
        }
    }

    /// Darken the color by a percentage (0.0 - 1.0)
    pub fn darken(&self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        Self {
            r: (self.r as f32 * (1.0 - amount)) as u8,
            g: (self.g as f32 * (1.0 - amount)) as u8,
            b: (self.b as f32 * (1.0 - amount)) as u8,
            a: self.a,
        }
    }

    /// Set the alpha value (0.0 - 1.0)
    pub fn with_alpha(&self, alpha: f32) -> Self {
        Self { r: self.r, g: self.g, b: self.b, a: (alpha.clamp(0.0, 1.0) * 255.0) as u8 }
    }

    /// Mix two colors together with a weight (0.0 = self, 1.0 = other)
    pub fn mix(&self, other: &Color, weight: f32) -> Self {
        let weight = weight.clamp(0.0, 1.0);
        let inv_weight = 1.0 - weight;
        Self {
            r: (self.r as f32 * inv_weight + other.r as f32 * weight) as u8,
            g: (self.g as f32 * inv_weight + other.g as f32 * weight) as u8,
            b: (self.b as f32 * inv_weight + other.b as f32 * weight) as u8,
            a: (self.a as f32 * inv_weight + other.a as f32 * weight) as u8,
        }
    }
}

/// Dynamic token storage using nested HashMaps.
/// Two-level addressing: category.key (e.g., "fontSize.md", "color.primary")
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TokenSet {
    tokens: HashMap<String, HashMap<String, TokenValue>>,
}

impl TokenSet {
    pub fn new() -> Self { Self::default() }

    /// Get a token value: tokens.get("fontSize", "md")
    pub fn get(&self, category: &str, key: &str) -> Option<&TokenValue> {
        self.tokens.get(category)?.get(key)
    }

    /// Set a token value: tokens.set("fontSize", "xxl", TokenValue::Float(24.0))
    pub fn set(&mut self, category: &str, key: &str, value: TokenValue) {
        self.tokens.entry(category.to_string()).or_default().insert(key.to_string(), value);
    }

    /// Get a color token, returning None if not found or wrong type
    pub fn get_color(&self, category: &str, key: &str) -> Option<Color> {
        match self.get_resolved(category, key)? {
            TokenValue::Color(c) => Some(*c),
            _ => None,
        }
    }

    /// Get a float token, returning None if not found or wrong type
    pub fn get_float(&self, category: &str, key: &str) -> Option<f32> {
        match self.get_resolved(category, key)? {
            TokenValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Get an int token, returning None if not found or wrong type
    pub fn get_int(&self, category: &str, key: &str) -> Option<i32> {
        match self.get_resolved(category, key)? {
            TokenValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Get a bool token, returning None if not found or wrong type
    pub fn get_bool(&self, category: &str, key: &str) -> Option<bool> {
        match self.get_resolved(category, key)? {
            TokenValue::Bool(flag) => Some(*flag),
            _ => None,
        }
    }

    /// Get a string token, returning None if not found or wrong type
    pub fn get_string(&self, category: &str, key: &str) -> Option<&str> {
        match self.get_resolved(category, key)? {
            TokenValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Resolve a token reference like `radius.default` or `md` (with the caller supplying
    /// the fallback category) to its final token value. Returns `None` on miss
    /// — use `resolve_reference_strict` if you want unresolved references to be
    /// surfaced to the user instead of silently falling back.
    pub fn resolve_reference(&self, reference: &str, fallback_category: Option<&str>) -> Option<&TokenValue> {
        let (category, key) = match reference.split_once('.') {
            Some((category, key)) => (category, key),
            None => (fallback_category?, reference),
        };

        self.get_resolved(category, key)
    }

    /// Like `resolve_reference`, but logs a warning to stderr when the lookup
    /// fails so that token typos in theme JSON don't silently disappear.
    pub fn resolve_reference_strict(
        &self,
        reference: &str,
        fallback_category: Option<&str>,
    ) -> Option<&TokenValue> {
        let resolved = self.resolve_reference(reference, fallback_category);
        if resolved.is_none() {
            eprintln!(
                "Theme: unresolved token reference {reference:?} (fallback category: {fallback_category:?})"
            );
        }
        resolved
    }

    fn get_resolved(&self, category: &str, key: &str) -> Option<&TokenValue> {
        let mut visited = HashSet::new();
        self.get_resolved_inner(category, key, &mut visited)
    }

    fn get_resolved_inner(
        &self,
        category: &str,
        key: &str,
        visited: &mut HashSet<(String, String)>,
    ) -> Option<&TokenValue> {
        if !visited.insert((category.to_string(), key.to_string())) {
            return None;
        }

        let value = self.get(category, key)?;
        match value {
            TokenValue::Ref(reference) => {
                let (next_category, next_key) = parse_token_reference(reference, category)?;
                self.get_resolved_inner(&next_category, &next_key, visited)
            }
            _ => Some(value),
        }
    }

    pub fn categories(&self) -> &HashMap<String, HashMap<String, TokenValue>> { &self.tokens }

    pub fn merge_from(&mut self, other: &TokenSet) {
        for (category, values) in &other.tokens {
            let entry = self.tokens.entry(category.clone()).or_default();
            for (key, value) in values {
                entry.insert(key.clone(), value.clone());
            }
        }
    }
}

fn parse_token_reference(reference: &str, fallback_category: &str) -> Option<(String, String)> {
    let reference = reference.trim();
    if reference.is_empty() {
        return None;
    }

    if let Some((category, key)) = reference.split_once('.') {
        return Some((category.to_string(), key.to_string()));
    }

    Some((fallback_category.to_string(), reference.to_string()))
}

/// Theme tokens for light and dark schemes
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeTokens {
    pub light: TokenSet,
    pub dark: TokenSet,
}

impl ThemeTokens {
    pub fn new() -> Self { Self::default() }

    /// Get tokens for a specific color scheme
    pub fn for_scheme(&self, scheme: super::ColorScheme) -> &TokenSet {
        match scheme {
            super::ColorScheme::Light => &self.light,
            super::ColorScheme::Dark => &self.dark,
        }
    }

    pub fn merged_with_parent(&self, parent: &ThemeTokens) -> ThemeTokens {
        let mut light = parent.light.clone();
        light.merge_from(&self.light);

        let mut dark = parent.dark.clone();
        dark.merge_from(&parent.dark);
        dark.merge_from(&self.light);
        dark.merge_from(&self.dark);

        ThemeTokens { light, dark }
    }

    /// Return every (category, key) present in `light` but missing in `dark`,
    /// or vice versa. Used by parity tests / strict theme loaders to fail fast
    /// when a contributor adds a token to one scheme and forgets the other.
    pub fn parity_diff(&self) -> Vec<(String, String, &'static str)> {
        let mut diff = Vec::new();

        let key_set = |set: &TokenSet| -> HashSet<(String, String)> {
            set.tokens
                .iter()
                .flat_map(|(cat, keys)| keys.keys().map(move |k| (cat.clone(), k.clone())))
                .collect()
        };

        let light_keys = key_set(&self.light);
        let dark_keys = key_set(&self.dark);

        for (cat, key) in light_keys.difference(&dark_keys) {
            diff.push((cat.clone(), key.clone(), "missing-in-dark"));
        }
        for (cat, key) in dark_keys.difference(&light_keys) {
            diff.push((cat.clone(), key.clone(), "missing-in-light"));
        }

        diff
    }
}

#[cfg(test)]
mod parity_tests {
    use super::*;

    /// Catches U1-style bugs — a token added to one scheme without the other.
    #[test]
    fn default_tokens_have_dark_light_parity() {
        let tokens = crate::theme::create_schema_fallback_tokens();
        let diff = tokens.parity_diff();
        assert!(
            diff.is_empty(),
            "token parity violations between light and dark: {diff:?}\n\
             Add the missing entries (or change the test to allow the asymmetry if intentional)."
        );
    }
}
