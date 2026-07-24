// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bidirectional token-key/UI-index lookup tables.
//!
//! The theme-editor UI presents tokens as combo boxes — selection is an index,
//! storage uses a string key. These helpers translate between the two without
//! risking the index↔key tables drifting apart (H16 collapsed three pairs of
//! 50-line manual matches into the single ordered lists below).
//!
//! Extracted from main.rs (U2) so the entry point doesn't have to carry the
//! token catalog alongside its event-wiring code.

pub const NUMBER_TOKENS: &[&str] = &[
    "fontSize.xs",
    "fontSize.caption",
    "fontSize.sm",
    "fontSize.md",
    "fontSize.lg",
    "fontSize.helper",
    "fontWeight.normal",
    "fontWeight.medium",
    "fontWeight.semibold",
    "fontWeight.bold",
    "borderWidth.none",
    "borderWidth.sm",
    "borderWidth.focus",
    "spacing.xs",
    "spacing.sm",
    "spacing.md",
    "spacing.lg",
    "spacing.xl",
    "radius.sm",
    "radius.default",
    "radius.lg",
    "radius.full",
    "controlSize.sm",
    "controlSize.md",
    "controlSize.lg",
    "choiceControlSize.sm",
    "choiceControlSize.md",
    "choiceControlSize.lg",
    "switchSize.sm",
    "switchSize.md",
    "switchSize.lg",
    "iconSize.sm",
    "iconSize.md",
    "iconSize.lg",
    "controlRadius.sm",
    "controlRadius.md",
    "controlRadius.lg",
    "controlPaddingInline.sm",
    "controlPaddingInline.md",
    "controlPaddingInline.lg",
];

pub const FONT_TOKENS: &[&str] = &["font.primary", "font.secondary", "font.tertiary"];

pub const COLOR_TOKENS: &[&str] = &[
    "color.primary",
    "color.primary.light",
    "color.primary.dark",
    "color.secondary",
    "color.secondary.dark",
    "color.danger",
    "color.danger.light",
    "color.danger.dark",
    "color.foreground",
    "color.foreground.light",
    "color.surface",
    "color.background",
    "color.muted",
    "color.border",
    "color.transparent",
    "color.white",
    "color.success",
    "color.warning",
];

/// Color aliases — accepted on input, normalized to a canonical entry in
/// `COLOR_TOKENS` before lookup.
pub const COLOR_TOKEN_ALIASES: &[(&str, &str)] =
    &[("color.primary-pressed", "color.primary.dark"), ("color.text-muted", "color.muted")];

pub fn lookup_token_index(table: &[&str], key: &str) -> Option<i32> {
    table.iter().position(|&k| k == key).map(|i| i as i32)
}

pub fn number_token_key_from_index(index: i32) -> &'static str {
    NUMBER_TOKENS.get(index as usize).copied().unwrap_or("fontSize.md")
}

pub fn number_token_index_from_key(key: Option<&str>) -> i32 {
    // Default falls back to "fontSize.md" for legacy compatibility.
    key.and_then(|k| lookup_token_index(NUMBER_TOKENS, k))
        .unwrap_or_else(|| lookup_token_index(NUMBER_TOKENS, "fontSize.md").unwrap_or(0))
}

pub fn font_token_key_from_index(index: i32) -> &'static str {
    FONT_TOKENS.get(index as usize).copied().unwrap_or("font.primary")
}

pub fn font_token_index_from_key(key: Option<&str>) -> i32 {
    key.and_then(|k| lookup_token_index(FONT_TOKENS, k)).unwrap_or(0)
}

pub fn color_token_key_from_index(index: i32) -> &'static str {
    COLOR_TOKENS.get(index as usize).copied().unwrap_or("color.primary")
}

pub fn color_token_index_from_key(key: Option<&str>) -> i32 {
    key.and_then(|k| {
        let canonical = COLOR_TOKEN_ALIASES
            .iter()
            .find_map(|(alias, target)| (*alias == k).then_some(*target))
            .unwrap_or(k);
        lookup_token_index(COLOR_TOKENS, canonical)
    })
    .unwrap_or(0)
}

pub fn actual_font_family_index_from_value(value: &str) -> i32 {
    match value {
        "Montserrat" => 0,
        "Arial" => 1,
        "Verdana" => 2,
        "Trebuchet MS" => 3,
        "Georgia" => 4,
        "Palatino" => 5,
        "Times New Roman" => 6,
        "Courier New" => 7,
        "Menlo" => 8,
        "Monaco" => 9,
        _ => 0,
    }
}

pub fn radius_default_index_from_key(key: &str) -> i32 {
    match key {
        "sm" => 0,
        "md" => 1,
        "lg" => 2,
        "full" => 3,
        _ => 1,
    }
}

pub fn radius_default_key_from_index(index: i32) -> &'static str {
    match index {
        0 => "sm",
        1 => "md",
        2 => "lg",
        3 => "full",
        _ => "md",
    }
}
