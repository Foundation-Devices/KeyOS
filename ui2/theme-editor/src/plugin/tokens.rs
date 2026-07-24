#![allow(dead_code)]
// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Token resolution system.
//!
//! Tokens are design system values like colors, spacing, font sizes, etc.
//! They are loaded from `~/.foundation/theme-editor/tokens.json` and used
//! to resolve token references in plugin defaults.
//!
//! Example tokens.json:
//! ```json
//! {
//!     "color": { "primary": "#0066cc", "secondary": "#e0e0e0" },
//!     "spacing": { "xs": 4, "sm": 8, "md": 12 },
//!     "fontSize": { "sm": 14, "md": 16, "lg": 18 }
//! }
//! ```

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Token value types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TokenValue {
    /// String value (typically a color like "#ff0000")
    String(String),
    /// Numeric value (spacing, font size, etc.)
    Number(f64),
    /// Integer value
    Int(i64),
    /// Boolean value
    Bool(bool),
}

impl TokenValue {
    /// Convert to f32 for numeric values
    pub fn as_float(&self) -> Option<f32> {
        match self {
            TokenValue::Number(n) => Some(*n as f32),
            TokenValue::Int(i) => Some(*i as f32),
            _ => None,
        }
    }

    /// Convert to i32 for integer values
    pub fn as_int(&self) -> Option<i32> {
        match self {
            TokenValue::Int(i) => Some(*i as i32),
            TokenValue::Number(n) => Some(*n as i32),
            _ => None,
        }
    }

    /// Get as bool
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            TokenValue::Bool(flag) => Some(*flag),
            _ => None,
        }
    }

    /// Get as string
    pub fn as_string(&self) -> Option<&str> {
        match self {
            TokenValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// Convert to slint::Color if this is a color string
    pub fn as_color(&self) -> Option<slint::Color> {
        match self {
            TokenValue::String(s) => parse_hex_color(s),
            _ => None,
        }
    }
}

/// Parse a hex color string (with or without #) into a Slint Color
fn parse_hex_color(hex: &str) -> Option<slint::Color> {
    let hex = hex.trim().trim_start_matches('#');

    // Handle 3-char shorthand (e.g., "fff" -> "ffffff")
    let hex = if hex.len() == 3 {
        hex.chars().flat_map(|c| std::iter::repeat(c).take(2)).collect::<String>()
    } else {
        hex.to_string()
    };

    if hex.len() == 8 {
        if let (Ok(r), Ok(g), Ok(b), Ok(a)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
            u8::from_str_radix(&hex[6..8], 16),
        ) {
            return Some(slint::Color::from_argb_u8(a, r, g, b));
        }
    }

    if hex.len() >= 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        ) {
            return Some(slint::Color::from_rgb_u8(r, g, b));
        }
    }

    None
}

/// Token store - holds all design tokens organized by category.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TokenStore {
    /// Tokens organized by category: { "color": { "primary": "#fff" }, ... }
    pub categories: HashMap<String, HashMap<String, TokenValue>>,
}

impl TokenStore {
    /// Load tokens from JSON string
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> { serde_json::from_str(json) }

    /// Resolve a token reference like "color.primary" to its value
    pub fn resolve(&self, reference: &str) -> Option<&TokenValue> {
        let parts: Vec<&str> = reference.splitn(2, '.').collect();
        if parts.len() == 2 {
            self.categories.get(parts[0])?.get(parts[1])
        } else {
            None
        }
    }

    /// Resolve a token reference to a color
    pub fn resolve_color(&self, reference: &str) -> Option<slint::Color> {
        self.resolve_resolved(reference)?.as_color()
    }

    /// Resolve a token reference to a float
    pub fn resolve_float(&self, reference: &str) -> Option<f32> {
        self.resolve_resolved(reference)?.as_float()
    }

    /// Resolve a token reference to an int
    pub fn resolve_int(&self, reference: &str) -> Option<i32> { self.resolve_resolved(reference)?.as_int() }

    /// Resolve a token reference to a bool
    pub fn resolve_bool(&self, reference: &str) -> Option<bool> {
        self.resolve_resolved(reference)?.as_bool()
    }

    /// Resolve a token reference to a string
    pub fn resolve_string(&self, reference: &str) -> Option<&str> {
        self.resolve_resolved(reference)?.as_string()
    }

    /// Resolve a reference and follow string aliases like `"radius.default" -> "radius.md"`.
    fn resolve_resolved(&self, reference: &str) -> Option<&TokenValue> {
        let (category, key) = split_reference(reference)?;
        let mut visited = HashSet::new();
        visited.insert((category.to_string(), key.to_string()));

        let value = self.resolve(reference)?;
        match value {
            TokenValue::String(s) if is_token_reference(s) => {
                let (next_category, next_key) = split_reference(s)?;
                self.resolve_inner(&next_category, &next_key, &mut visited)
            }
            _ => Some(value),
        }
    }

    fn resolve_inner(
        &self,
        category: &str,
        key: &str,
        visited: &mut HashSet<(String, String)>,
    ) -> Option<&TokenValue> {
        if !visited.insert((category.to_string(), key.to_string())) {
            return None;
        }

        let value = self.categories.get(category)?.get(key)?;
        match value {
            TokenValue::String(s) if is_token_reference(s) => {
                let (next_category, next_key) = split_reference(s)?;
                self.resolve_inner(&next_category, &next_key, visited)
            }
            _ => Some(value),
        }
    }

    /// Create default tokens
    pub fn default_tokens() -> Self {
        let json = r##"{
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
                "xs": 12,
                "caption": 13,
                "sm": 20,
                "md": 22,
                "lg": 24,
                "helper": 14
            },
            "borderWidth": {
                "none": 0,
                "sm": 1,
                "focus": 2
            },
            "choiceControlSize": {
                "sm": 24,
                "md": 28,
                "lg": 32
            },
            "switchSize": {
                "sm": 20,
                "md": 24,
                "lg": 28
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
        }"##;

        Self::from_json(json).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_token() {
        let store = TokenStore::default_tokens();

        // Resolve color
        let color = store.resolve_color("color.primary");
        assert!(color.is_some());

        // Resolve spacing
        let spacing = store.resolve_float("spacing.md");
        assert_eq!(spacing, Some(12.0));

        // Resolve aliased radius token
        let radius_default = store.resolve_float("radius.default");
        assert_eq!(radius_default, Some(24.0));

        // Resolve font weight
        let weight = store.resolve_int("fontWeight.medium");
        assert_eq!(weight, Some(500));
    }

    #[test]
    fn test_parse_hex() {
        let color = parse_hex_color("#ff0000").unwrap();
        assert_eq!(color.red(), 255);
        assert_eq!(color.green(), 0);
        assert_eq!(color.blue(), 0);

        // Without #
        let color2 = parse_hex_color("00ff00").unwrap();
        assert_eq!(color2.green(), 255);

        // Shorthand
        let color3 = parse_hex_color("#f00").unwrap();
        assert_eq!(color3.red(), 255);

        // CSS/Slint 8-char form: #RRGGBBAA
        let color4 = parse_hex_color("#231f201a").unwrap();
        assert_eq!(color4.red(), 0x23);
        assert_eq!(color4.green(), 0x1f);
        assert_eq!(color4.blue(), 0x20);
        assert_eq!(color4.alpha(), 0x1a);
    }
}

fn is_token_reference(value: &str) -> bool {
    let value = value.trim();
    value.contains('.') && !value.starts_with('#')
}

fn split_reference(reference: &str) -> Option<(&str, &str)> { reference.trim().split_once('.') }
