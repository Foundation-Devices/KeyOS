// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::tokens::{Color, ThemeTokens, TokenSet, TokenValue};

/// Create schema fallback tokens with light and dark schemes.
pub fn create_schema_fallback_tokens() -> ThemeTokens {
    ThemeTokens { light: create_light_tokens(), dark: create_dark_tokens() }
}

fn create_light_tokens() -> TokenSet {
    let mut tokens = TokenSet::new();
    let primary = Color::from_hex("#009DB9").unwrap();
    let secondary = Color::from_hex("#D5D4D5").unwrap();
    let secondary_pressed = Color::from_hex("#E3E2E2").unwrap();
    let danger = Color::from_hex("#FF3333").unwrap();

    // Colors
    tokens.set("color", "primary", TokenValue::Color(primary));
    tokens.set("color", "primary.light", TokenValue::Color(Color::from_hex("#33B1C7").unwrap()));
    tokens.set("color", "primary.dark", TokenValue::Color(Color::from_hex("#006F83").unwrap()));
    tokens.set("color", "secondary", TokenValue::Color(secondary));
    tokens.set("color", "secondary.dark", TokenValue::Color(secondary_pressed));
    tokens.set("color", "background", TokenValue::Color(Color::from_hex("#FFFFFF").unwrap()));
    tokens.set("color", "surface", TokenValue::Color(Color::from_hex("#FFFFFF").unwrap()));
    tokens.set("color", "foreground", TokenValue::Color(Color::from_hex("#231F20").unwrap()));
    tokens.set(
        "color",
        "foreground.light",
        TokenValue::Color(Color::from_hex("#231F20").unwrap().with_alpha(0.1)),
    );
    tokens.set("color", "muted", TokenValue::Color(Color::from_hex("#959394").unwrap()));
    tokens.set("color", "border", TokenValue::Color(Color::from_hex("#D5D4D5").unwrap()));
    tokens.set("color", "danger", TokenValue::Color(danger));
    tokens.set("color", "danger.light", TokenValue::Color(Color::from_hex("#FF5C5C").unwrap()));
    tokens.set("color", "danger.dark", TokenValue::Color(Color::from_hex("#B52424").unwrap()));
    tokens.set("color", "warning", TokenValue::Color(Color::from_hex("#D97706").unwrap()));
    tokens.set("color", "success", TokenValue::Color(Color::from_hex("#16A34A").unwrap()));
    tokens.set("color", "info", TokenValue::Color(Color::from_hex("#2563EB").unwrap()));
    tokens.set("color", "transparent", TokenValue::Color(Color::rgba(0, 0, 0, 0)));
    tokens.set("color", "white", TokenValue::Color(Color::rgb(255, 255, 255)));

    // Radius
    tokens.set("radius", "none", TokenValue::Float(0.0));
    tokens.set("radius", "sm", TokenValue::Float(8.0));
    tokens.set("radius", "md", TokenValue::Float(16.0));
    tokens.set("radius", "default", TokenValue::Ref("radius.lg".to_string()));
    tokens.set("radius", "lg", TokenValue::Float(24.0));
    tokens.set("radius", "xl", TokenValue::Float(32.0));
    tokens.set("radius", "full", TokenValue::Float(9999.0));

    // Border Width
    tokens.set("borderWidth", "none", TokenValue::Float(0.0));
    tokens.set("borderWidth", "thin", TokenValue::Float(1.0));
    tokens.set("borderWidth", "medium", TokenValue::Float(2.0));
    tokens.set("borderWidth", "thick", TokenValue::Float(3.0));

    // Spacing
    tokens.set("spacing", "xs", TokenValue::Float(4.0));
    tokens.set("spacing", "sm", TokenValue::Float(8.0));
    tokens.set("spacing", "md", TokenValue::Float(12.0));
    tokens.set("spacing", "lg", TokenValue::Float(16.0));
    tokens.set("spacing", "xl", TokenValue::Float(24.0));
    tokens.set("spacing", "2xl", TokenValue::Float(32.0));

    // Reusable control-scale tokens for Prime-sized components
    tokens.set("controlSize", "sm", TokenValue::Float(48.0));
    tokens.set("controlSize", "md", TokenValue::Float(56.0));
    tokens.set("controlSize", "lg", TokenValue::Float(64.0));
    tokens.set("iconSize", "sm", TokenValue::Float(20.0));
    tokens.set("iconSize", "md", TokenValue::Float(24.0));
    tokens.set("iconSize", "lg", TokenValue::Float(28.0));
    tokens.set("controlRadius", "sm", TokenValue::Float(20.0));
    tokens.set("controlRadius", "md", TokenValue::Float(24.0));
    tokens.set("controlRadius", "lg", TokenValue::Float(28.0));
    tokens.set("controlPaddingInline", "sm", TokenValue::Float(14.0));
    tokens.set("controlPaddingInline", "md", TokenValue::Float(16.0));
    tokens.set("controlPaddingInline", "lg", TokenValue::Float(20.0));

    // Font Size
    tokens.set("fontSize", "xs", TokenValue::Float(18.0));
    tokens.set("fontSize", "sm", TokenValue::Float(20.0));
    tokens.set("fontSize", "md", TokenValue::Float(22.0));
    tokens.set("fontSize", "lg", TokenValue::Float(24.0));
    tokens.set("fontSize", "xl", TokenValue::Float(26.0));
    tokens.set("fontSize", "2xl", TokenValue::Float(32.0));

    // Font families
    tokens.set("font", "primary", TokenValue::String("Montserrat".to_string()));
    tokens.set("font", "secondary", TokenValue::String("Montserrat".to_string()));
    tokens.set("font", "tertiary", TokenValue::String("Montserrat".to_string()));

    // Font Weight
    tokens.set("fontWeight", "normal", TokenValue::Int(400));
    tokens.set("fontWeight", "medium", TokenValue::Int(500));
    tokens.set("fontWeight", "bold", TokenValue::Int(700));

    // Line Height
    tokens.set("lineHeight", "tight", TokenValue::Float(1.2));
    tokens.set("lineHeight", "normal", TokenValue::Float(1.5));
    tokens.set("lineHeight", "loose", TokenValue::Float(1.75));

    // Effects
    tokens.set("opacity", "disabled", TokenValue::Float(0.5));
    tokens.set("transition", "fast", TokenValue::Float(100.0));
    tokens.set("transition", "normal", TokenValue::Float(200.0));
    tokens.set("transition", "slow", TokenValue::Float(300.0));

    tokens
}

fn create_dark_tokens() -> TokenSet {
    let mut tokens = TokenSet::new();
    let primary = Color::from_hex("#33B1C7").unwrap();
    let secondary = Color::from_hex("#5A595A").unwrap();
    let danger = Color::from_hex("#FF3333").unwrap();

    // Colors - dark mode overrides
    tokens.set("color", "primary", TokenValue::Color(primary));
    tokens.set("color", "primary.light", TokenValue::Color(Color::from_hex("#54BDD0").unwrap()));
    tokens.set("color", "primary.dark", TokenValue::Color(Color::from_hex("#8AD2DF").unwrap()));
    tokens.set("color", "secondary", TokenValue::Color(secondary));
    tokens.set("color", "secondary.dark", TokenValue::Color(secondary));
    tokens.set("color", "background", TokenValue::Color(Color::from_hex("#231F20").unwrap()));
    tokens.set("color", "surface", TokenValue::Color(Color::from_hex("#231F20").unwrap()));
    tokens.set("color", "foreground", TokenValue::Color(Color::from_hex("#FFFFFF").unwrap()));
    tokens.set(
        "color",
        "foreground.light",
        TokenValue::Color(Color::from_hex("#FFFFFF").unwrap().with_alpha(0.1)),
    );
    tokens.set("color", "muted", TokenValue::Color(Color::from_hex("#959394").unwrap()));
    tokens.set("color", "border", TokenValue::Color(Color::from_hex("#454444").unwrap()));
    tokens.set("color", "danger", TokenValue::Color(danger));
    tokens.set("color", "danger.light", TokenValue::Color(Color::from_hex("#FF5C5C").unwrap()));
    tokens.set("color", "danger.dark", TokenValue::Color(Color::from_hex("#FF7676").unwrap()));
    tokens.set("color", "warning", TokenValue::Color(Color::from_hex("#F59E0B").unwrap()));
    tokens.set("color", "success", TokenValue::Color(Color::from_hex("#22C55E").unwrap()));
    tokens.set("color", "info", TokenValue::Color(Color::from_hex("#3B82F6").unwrap()));
    tokens.set("color", "transparent", TokenValue::Color(Color::rgba(0, 0, 0, 0)));
    tokens.set("color", "white", TokenValue::Color(Color::rgb(255, 255, 255)));

    // Radius - same as light
    tokens.set("radius", "none", TokenValue::Float(0.0));
    tokens.set("radius", "sm", TokenValue::Float(8.0));
    tokens.set("radius", "md", TokenValue::Float(16.0));
    tokens.set("radius", "default", TokenValue::Ref("radius.lg".to_string()));
    tokens.set("radius", "lg", TokenValue::Float(24.0));
    tokens.set("radius", "xl", TokenValue::Float(32.0));
    tokens.set("radius", "full", TokenValue::Float(9999.0));

    // Border Width - same as light
    tokens.set("borderWidth", "none", TokenValue::Float(0.0));
    tokens.set("borderWidth", "thin", TokenValue::Float(1.0));
    tokens.set("borderWidth", "medium", TokenValue::Float(2.0));
    tokens.set("borderWidth", "thick", TokenValue::Float(3.0));

    // Spacing - same as light
    tokens.set("spacing", "xs", TokenValue::Float(4.0));
    tokens.set("spacing", "sm", TokenValue::Float(8.0));
    tokens.set("spacing", "md", TokenValue::Float(12.0));
    tokens.set("spacing", "lg", TokenValue::Float(16.0));
    tokens.set("spacing", "xl", TokenValue::Float(24.0));
    tokens.set("spacing", "2xl", TokenValue::Float(32.0));

    // Reusable control-scale tokens - same as light
    tokens.set("controlSize", "sm", TokenValue::Float(48.0));
    tokens.set("controlSize", "md", TokenValue::Float(56.0));
    tokens.set("controlSize", "lg", TokenValue::Float(64.0));
    tokens.set("iconSize", "sm", TokenValue::Float(20.0));
    tokens.set("iconSize", "md", TokenValue::Float(24.0));
    tokens.set("iconSize", "lg", TokenValue::Float(28.0));
    tokens.set("controlRadius", "sm", TokenValue::Float(20.0));
    tokens.set("controlRadius", "md", TokenValue::Float(24.0));
    tokens.set("controlRadius", "lg", TokenValue::Float(28.0));
    tokens.set("controlPaddingInline", "sm", TokenValue::Float(14.0));
    tokens.set("controlPaddingInline", "md", TokenValue::Float(16.0));
    tokens.set("controlPaddingInline", "lg", TokenValue::Float(20.0));

    // Font Size - same as light
    tokens.set("fontSize", "xs", TokenValue::Float(18.0));
    tokens.set("fontSize", "sm", TokenValue::Float(20.0));
    tokens.set("fontSize", "md", TokenValue::Float(22.0));
    tokens.set("fontSize", "lg", TokenValue::Float(24.0));
    tokens.set("fontSize", "xl", TokenValue::Float(26.0));
    tokens.set("fontSize", "2xl", TokenValue::Float(32.0));

    // Font families - same as light
    tokens.set("font", "primary", TokenValue::String("Montserrat".to_string()));
    tokens.set("font", "secondary", TokenValue::String("Montserrat".to_string()));
    tokens.set("font", "tertiary", TokenValue::String("Montserrat".to_string()));

    // Font Weight - same as light
    tokens.set("fontWeight", "normal", TokenValue::Int(400));
    tokens.set("fontWeight", "medium", TokenValue::Int(500));
    tokens.set("fontWeight", "bold", TokenValue::Int(700));

    // Line Height - same as light
    tokens.set("lineHeight", "tight", TokenValue::Float(1.2));
    tokens.set("lineHeight", "normal", TokenValue::Float(1.5));
    tokens.set("lineHeight", "loose", TokenValue::Float(1.75));

    // Effects - same as light
    tokens.set("opacity", "disabled", TokenValue::Float(0.5));
    tokens.set("transition", "fast", TokenValue::Float(100.0));
    tokens.set("transition", "normal", TokenValue::Float(200.0));
    tokens.set("transition", "slow", TokenValue::Float(300.0));

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_tokens() {
        let tokens = create_schema_fallback_tokens();

        // Test light mode colors
        let primary = tokens.light.get_color("color", "primary").unwrap();
        assert_eq!(primary.r, 0x00);
        assert_eq!(primary.g, 0x9D);
        assert_eq!(primary.b, 0xB9);

        // Test dark mode colors
        let primary_dark = tokens.dark.get_color("color", "primary").unwrap();
        assert_eq!(primary_dark.r, 0x33);
        assert_eq!(primary_dark.g, 0xB1);
        assert_eq!(primary_dark.b, 0xC7);

        // Test radius tokens
        assert_eq!(tokens.light.get_float("radius", "md"), Some(16.0));
        assert_eq!(tokens.light.get_float("radius", "default"), Some(24.0));

        // Test font size tokens
        assert_eq!(tokens.light.get_float("fontSize", "xs"), Some(18.0));
        assert_eq!(tokens.light.get_float("fontSize", "sm"), Some(20.0));
        assert_eq!(tokens.light.get_float("fontSize", "md"), Some(22.0));
        assert_eq!(tokens.light.get_float("fontSize", "lg"), Some(24.0));
        assert_eq!(tokens.light.get_float("fontSize", "xl"), Some(26.0));

        // Test font family tokens
        assert_eq!(tokens.light.get_string("font", "primary"), Some("Montserrat"));

        // Test font weight tokens
        assert_eq!(tokens.light.get_int("fontWeight", "medium"), Some(500));
    }
}
