// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Runtime helpers for pushing a resolved [`ExportTheme`] into an app's Slint
//! `Theme` global.
//!
//! The Slint `Theme` global is a per-app generated Rust type (it comes out of
//! `slint::include_modules!()`), so a library can't call its setters directly.
//! Instead, this module exposes plain-data token extraction helpers, and the
//! [`crate::apply_theme!`] macro calls the app's generated `Theme` setters at
//! the call site. Apps never write the wall of setters themselves. (Button
//! variant/size styling now lives in the generated ButtonTheme global, which
//! cascades from the color-* + token surface, so no button data helpers are
//! needed here.)

use crate::{ColorScheme, ExportTheme, ThemeColor, TokenValue};

/// Resolve a color token (e.g. `color.primary`), falling back to `fallback`
/// when the token is missing or the wrong type.
pub fn token_color(
    theme: &ExportTheme,
    scheme: ColorScheme,
    category: &str,
    key: &str,
    fallback: ThemeColor,
) -> ThemeColor {
    match crate::get_token(&theme.tokens, category, key, scheme) {
        Some(TokenValue::Color(color)) => color,
        Some(TokenValue::String(text)) => ThemeColor::from_hex(&text).unwrap_or(fallback),
        _ => fallback,
    }
}

/// Resolve a numeric token (e.g. `typography.font-size-title`) as pixels,
/// falling back to `fallback` when the token is missing or the wrong type.
pub fn token_length(
    theme: &ExportTheme,
    scheme: ColorScheme,
    category: &str,
    key: &str,
    fallback: f32,
) -> f32 {
    match crate::get_token(&theme.tokens, category, key, scheme) {
        Some(TokenValue::Float(value)) => value as f32,
        Some(TokenValue::Int(value)) => value as f32,
        _ => fallback,
    }
}

/// Resolve a string token (e.g. `font.primary`), falling back to `fallback`
/// when the token is missing or the wrong type.
pub fn token_string(
    theme: &ExportTheme,
    scheme: ColorScheme,
    category: &str,
    key: &str,
    fallback: &str,
) -> String {
    match crate::get_token(&theme.tokens, category, key, scheme) {
        Some(TokenValue::String(text)) => text,
        _ => fallback.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use components::theme::create_schema_fallback_theme;

    use super::*;

    #[test]
    fn token_length_falls_back_when_missing() {
        let theme = create_schema_fallback_theme();
        let value = token_length(&theme, ColorScheme::Light, "typography", "does-not-exist", 17.0);
        assert_eq!(value, 17.0);
    }

    #[test]
    fn token_color_falls_back_when_missing() {
        let theme = create_schema_fallback_theme();
        let fallback = ThemeColor::rgb(1, 2, 3);
        let value = token_color(&theme, ColorScheme::Light, "color", "does-not-exist", fallback);
        assert_eq!(value, fallback);
    }
}
