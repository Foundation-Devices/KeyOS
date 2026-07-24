// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Small color and float comparison helpers shared between the editor UI
//! event handlers and the theme export logic. Extracted from main.rs (U2)
//! so the theme-editor entry point isn't carrying these alongside event wiring.

/// Tolerance-aware float equality used to decide whether a token value still
/// matches its default (i.e. shouldn't be emitted as an override).
pub fn check_float_equal(value: f32, default: f32) -> bool { (value - default).abs() < 0.001 }

/// Channel-wise color equality.
pub fn check_color_equal(value: slint::Color, default: slint::Color) -> bool {
    value.red() == default.red()
        && value.green() == default.green()
        && value.blue() == default.blue()
        && value.alpha() == default.alpha()
}

/// Return the alpha channel as a 0-100 percentage for the Slint color picker.
pub fn color_alpha_percent(color: slint::Color) -> f32 { (color.alpha() as f32 / 255.0) * 100.0 }

/// Parse a hex color string (with or without `#`) into a Slint `Color`.
/// Accepts 3-character shorthand and full 6/8-character forms. On failure
/// returns the editor's "missing color" sentinel `#1a9a9a` so the UI shows
/// something visibly wrong instead of crashing.
pub fn parse_hex_color(hex: &str) -> slint::Color {
    let hex = hex.trim().trim_start_matches('#');

    // 3-char shorthand: "fff" -> "ffffff"
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
            return slint::Color::from_argb_u8(a, r, g, b);
        }
    }

    if hex.len() >= 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        ) {
            return slint::Color::from_rgb_u8(r, g, b);
        }
    }

    slint::Color::from_rgb_u8(26, 154, 154)
}

/// Format a Slint `Color` as a lowercase hex string (no `#` prefix).
pub fn format_hex_color(color: slint::Color) -> slint::SharedString {
    if color.alpha() == 255 {
        format!("{:02x}{:02x}{:02x}", color.red(), color.green(), color.blue()).into()
    } else {
        format!("{:02x}{:02x}{:02x}{:02x}", color.red(), color.green(), color.blue(), color.alpha()).into()
    }
}
