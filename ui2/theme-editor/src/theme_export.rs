// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use components::theme as export_theme;
use serde::Serialize;

use crate::plugin::{
    self, ensure_default_files, load_all_plugins, ComponentThemeData, PluginDefinition, TokenStore,
};

const BASE_THEME_NAME: &str = "Base Theme";
const LEGACY_DEFAULT_THEME_NAME: &str = "Default Theme";
const BASE_THEME_ID: &str = "base_theme";
const LEGACY_DEFAULT_THEME_ID: &str = "default_theme";

#[derive(Clone)]
pub struct ThemeMeta {
    pub name: String,
    pub is_builtin: bool,
    pub parent_name: Option<String>,
}

#[derive(Clone)]
pub struct ThemeRecord {
    pub meta: ThemeMeta,
    pub tokens: TokenTheme,
    pub component_themes: HashMap<String, ComponentThemeData>,
}

#[derive(Clone, PartialEq)]
pub struct TokenTheme {
    pub light_primary: slint::Color,
    pub light_primary_pressed: slint::Color,
    pub light_secondary: slint::Color,
    pub light_danger: slint::Color,
    pub light_surface: slint::Color,
    pub light_background: slint::Color,
    pub light_text: slint::Color,
    pub light_text_muted: slint::Color,
    pub dark_primary: slint::Color,
    pub dark_primary_pressed: slint::Color,
    pub dark_secondary: slint::Color,
    pub dark_danger: slint::Color,
    pub dark_surface: slint::Color,
    pub dark_background: slint::Color,
    pub dark_text: slint::Color,
    pub dark_text_muted: slint::Color,
    pub spacing_xs: f32,
    pub spacing_sm: f32,
    pub spacing_md: f32,
    pub spacing_lg: f32,
    pub spacing_xl: f32,
    pub control_size_sm: f32,
    pub control_size_md: f32,
    pub control_size_lg: f32,
    pub choice_control_size_sm: f32,
    pub choice_control_size_md: f32,
    pub choice_control_size_lg: f32,
    pub switch_size_sm: f32,
    pub switch_size_md: f32,
    pub switch_size_lg: f32,
    pub icon_size_sm: f32,
    pub icon_size_md: f32,
    pub icon_size_lg: f32,
    pub border_width_none: f32,
    pub border_width_sm: f32,
    pub border_width_focus: f32,
    pub control_radius_sm: f32,
    pub control_radius_md: f32,
    pub control_radius_lg: f32,
    pub control_padding_inline_sm: f32,
    pub control_padding_inline_md: f32,
    pub control_padding_inline_lg: f32,
    pub font_size_xs: f32,
    pub font_size_caption: f32,
    pub font_size_sm: f32,
    pub font_size_md: f32,
    pub font_size_lg: f32,
    pub font_size_helper: f32,
    pub font_weight_normal: i32,
    pub font_weight_medium: i32,
    pub font_weight_semibold: i32,
    pub font_weight_bold: i32,
    pub radius_sm: f32,
    pub radius_md: f32,
    pub radius_lg: f32,
    pub radius_full: f32,
    pub radius_default_key: String,
    pub font_primary: String,
    pub font_secondary: String,
    pub font_tertiary: String,
}

#[derive(Serialize)]
struct ThemeManifest {
    themes: Vec<ThemeManifestEntry>,
}

#[derive(Serialize)]
struct ThemeManifestEntry {
    id: String,
    display_name: String,
    module_name: String,
    function_name: String,
}

impl TokenTheme {
    pub fn base_theme() -> Self {
        Self {
            light_primary: slint::Color::from_rgb_u8(0, 157, 185),
            light_primary_pressed: slint::Color::from_rgb_u8(0, 111, 131),
            light_secondary: slint::Color::from_rgb_u8(213, 212, 213),
            light_danger: slint::Color::from_rgb_u8(255, 51, 51),
            light_surface: slint::Color::from_rgb_u8(255, 255, 255),
            light_background: slint::Color::from_rgb_u8(255, 255, 255),
            light_text: slint::Color::from_rgb_u8(35, 31, 32),
            light_text_muted: slint::Color::from_rgb_u8(149, 147, 148),
            dark_primary: slint::Color::from_rgb_u8(51, 177, 199),
            dark_primary_pressed: slint::Color::from_rgb_u8(138, 210, 223),
            dark_secondary: slint::Color::from_rgb_u8(90, 89, 90),
            dark_danger: slint::Color::from_rgb_u8(255, 51, 51),
            dark_surface: slint::Color::from_rgb_u8(35, 31, 32),
            dark_background: slint::Color::from_rgb_u8(35, 31, 32),
            dark_text: slint::Color::from_rgb_u8(255, 255, 255),
            dark_text_muted: slint::Color::from_rgb_u8(149, 147, 148),
            spacing_xs: 4.0,
            spacing_sm: 8.0,
            spacing_md: 12.0,
            spacing_lg: 16.0,
            spacing_xl: 24.0,
            control_size_sm: 48.0,
            control_size_md: 56.0,
            control_size_lg: 64.0,
            choice_control_size_sm: 24.0,
            choice_control_size_md: 28.0,
            choice_control_size_lg: 32.0,
            switch_size_sm: 20.0,
            switch_size_md: 24.0,
            switch_size_lg: 28.0,
            icon_size_sm: 20.0,
            icon_size_md: 24.0,
            icon_size_lg: 28.0,
            border_width_none: 0.0,
            border_width_sm: 1.0,
            border_width_focus: 2.0,
            control_radius_sm: 20.0,
            control_radius_md: 24.0,
            control_radius_lg: 28.0,
            control_padding_inline_sm: 14.0,
            control_padding_inline_md: 16.0,
            control_padding_inline_lg: 20.0,
            font_size_xs: 12.0,
            font_size_caption: 13.0,
            font_size_sm: 20.0,
            font_size_md: 22.0,
            font_size_lg: 24.0,
            font_size_helper: 14.0,
            font_weight_normal: 400,
            font_weight_medium: 500,
            font_weight_semibold: 600,
            font_weight_bold: 700,
            radius_sm: 8.0,
            radius_md: 16.0,
            radius_lg: 24.0,
            radius_full: 9999.0,
            radius_default_key: "lg".to_string(),
            font_primary: "Montserrat".to_string(),
            font_secondary: "Montserrat".to_string(),
            font_tertiary: "Montserrat".to_string(),
        }
    }
}

pub fn write_theme_crate_outputs(output_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    ensure_default_files()?;
    let loaded_plugins =
        load_all_plugins().map_err(|err| format!("failed to load theme-editor plugins: {err}"))?;
    write_theme_crate_outputs_with_plugins(output_dir, &loaded_plugins)
}

pub fn write_theme_crate_outputs_with_plugins(
    output_dir: &Path,
    loaded_plugins: &[(plugin::BuiltinComponentSpec, PluginDefinition)],
) -> Result<(), Box<dyn std::error::Error>> {
    let generated_dir = output_dir.join("src").join("generated");
    fs::create_dir_all(&generated_dir)?;

    let records = built_in_theme_records(loaded_plugins);
    let mut manifest = ThemeManifest { themes: Vec::new() };
    let mut mod_rs = String::from("use crate::ThemeDescriptor;\n\n");

    for (idx, record) in records.iter().enumerate() {
        let module_name = theme_record_function_name(record);
        let rust = generate_theme_rust_export(&records, idx, "crate::generated::base_theme::base_theme")
            .ok_or_else(|| format!("failed to generate export for {}", record.meta.name))?;
        fs::write(generated_dir.join(format!("{module_name}.rs")), rust)?;
        mod_rs.push_str(&format!("pub mod {module_name};\n"));
        manifest.themes.push(ThemeManifestEntry {
            id: module_name.clone(),
            display_name: record.meta.name.clone(),
            module_name: module_name.clone(),
            function_name: module_name.clone(),
        });
    }

    mod_rs.push_str("\npub const BUILTIN_THEMES: &[ThemeDescriptor] = &[\n");
    for entry in &manifest.themes {
        mod_rs.push_str(&format!(
            "    ThemeDescriptor {{ id: {:?}, display_name: {:?}, module_name: {:?}, function_name: {:?} }},\n",
            entry.id, entry.display_name, entry.module_name, entry.function_name
        ));
    }
    mod_rs.push_str("];\n");

    fs::write(generated_dir.join("mod.rs"), mod_rs)?;
    fs::write(output_dir.join("theme_manifest.json"), serde_json::to_string_pretty(&manifest)?)?;

    Ok(())
}

pub fn built_in_theme_records(
    loaded_plugins: &[(plugin::BuiltinComponentSpec, PluginDefinition)],
) -> Vec<ThemeRecord> {
    vec![build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), loaded_plugins)]
}

fn is_base_theme_name(name: &str) -> bool {
    name == BASE_THEME_NAME
        || name == LEGACY_DEFAULT_THEME_NAME
        || theme_function_name(name) == BASE_THEME_ID
        || theme_function_name(name) == LEGACY_DEFAULT_THEME_ID
}

fn is_builtin_base_theme(theme: &ThemeRecord) -> bool {
    theme.meta.is_builtin && is_base_theme_name(&theme.meta.name)
}

fn theme_record_function_name(theme: &ThemeRecord) -> String {
    if is_builtin_base_theme(theme) {
        BASE_THEME_ID.to_string()
    } else {
        theme_function_name(&theme.meta.name)
    }
}

fn format_hex_color(color: slint::Color) -> String {
    if color.alpha() == 255 {
        format!("{:02x}{:02x}{:02x}", color.red(), color.green(), color.blue())
    } else {
        format!("{:02x}{:02x}{:02x}{:02x}", color.red(), color.green(), color.blue(), color.alpha())
    }
}

fn slint_color_to_token(color: slint::Color) -> components::theme::Color {
    components::theme::Color::rgba(color.red(), color.green(), color.blue(), color.alpha())
}

fn token_store_from_theme(theme: &TokenTheme) -> TokenStore {
    use plugin::TokenValue;

    let mut categories = HashMap::new();
    let primary_light = slint_color_to_token(theme.light_primary).lighten(0.2).to_slint();
    let secondary_dark = slint_color_to_token(theme.light_secondary).darken(0.1).to_slint();
    let danger_light = slint_color_to_token(theme.light_danger).lighten(0.15).to_slint();
    let danger_dark = slint_color_to_token(theme.light_danger).darken(0.1).to_slint();
    let foreground_light = slint_color_to_token(theme.light_text).with_alpha(0.1).to_slint();

    categories.insert(
        "color".to_string(),
        HashMap::from([
            (
                "primary".to_string(),
                TokenValue::String(format!("#{}", format_hex_color(theme.light_primary))),
            ),
            (
                "primary.light".to_string(),
                TokenValue::String(format!("#{}", format_hex_color(primary_light))),
            ),
            (
                "primary.dark".to_string(),
                TokenValue::String(format!("#{}", format_hex_color(theme.light_primary_pressed))),
            ),
            (
                "secondary".to_string(),
                TokenValue::String(format!("#{}", format_hex_color(theme.light_secondary))),
            ),
            (
                "secondary.dark".to_string(),
                TokenValue::String(format!("#{}", format_hex_color(secondary_dark))),
            ),
            ("danger".to_string(), TokenValue::String(format!("#{}", format_hex_color(theme.light_danger)))),
            (
                "surface".to_string(),
                TokenValue::String(format!("#{}", format_hex_color(theme.light_surface))),
            ),
            (
                "background".to_string(),
                TokenValue::String(format!("#{}", format_hex_color(theme.light_background))),
            ),
            (
                "foreground".to_string(),
                TokenValue::String(format!("#{}", format_hex_color(theme.light_text))),
            ),
            (
                "foreground.light".to_string(),
                TokenValue::String(format!("#{}", format_hex_color(foreground_light))),
            ),
            ("danger.light".to_string(), TokenValue::String(format!("#{}", format_hex_color(danger_light)))),
            ("danger.dark".to_string(), TokenValue::String(format!("#{}", format_hex_color(danger_dark)))),
            ("success".to_string(), TokenValue::String("#16a34a".to_string())),
            ("warning".to_string(), TokenValue::String("#d97706".to_string())),
            ("transparent".to_string(), TokenValue::String("#00000000".to_string())),
            ("white".to_string(), TokenValue::String("#ffffff".to_string())),
            (
                "muted".to_string(),
                TokenValue::String(format!("#{}", format_hex_color(theme.light_text_muted))),
            ),
            ("border".to_string(), TokenValue::String("#e0e0e0".to_string())),
        ]),
    );
    categories.insert(
        "spacing".to_string(),
        HashMap::from([
            ("xs".to_string(), TokenValue::Number(theme.spacing_xs as f64)),
            ("sm".to_string(), TokenValue::Number(theme.spacing_sm as f64)),
            ("md".to_string(), TokenValue::Number(theme.spacing_md as f64)),
            ("lg".to_string(), TokenValue::Number(theme.spacing_lg as f64)),
            ("xl".to_string(), TokenValue::Number(theme.spacing_xl as f64)),
        ]),
    );
    categories.insert(
        "font".to_string(),
        HashMap::from([
            ("primary".to_string(), TokenValue::String(theme.font_primary.clone())),
            ("secondary".to_string(), TokenValue::String(theme.font_secondary.clone())),
            ("tertiary".to_string(), TokenValue::String(theme.font_tertiary.clone())),
        ]),
    );
    categories.insert(
        "controlSize".to_string(),
        HashMap::from([
            ("sm".to_string(), TokenValue::Number(theme.control_size_sm as f64)),
            ("md".to_string(), TokenValue::Number(theme.control_size_md as f64)),
            ("lg".to_string(), TokenValue::Number(theme.control_size_lg as f64)),
        ]),
    );
    categories.insert(
        "iconSize".to_string(),
        HashMap::from([
            ("sm".to_string(), TokenValue::Number(theme.icon_size_sm as f64)),
            ("md".to_string(), TokenValue::Number(theme.icon_size_md as f64)),
            ("lg".to_string(), TokenValue::Number(theme.icon_size_lg as f64)),
        ]),
    );
    categories.insert(
        "choiceControlSize".to_string(),
        HashMap::from([
            ("sm".to_string(), TokenValue::Number(theme.choice_control_size_sm as f64)),
            ("md".to_string(), TokenValue::Number(theme.choice_control_size_md as f64)),
            ("lg".to_string(), TokenValue::Number(theme.choice_control_size_lg as f64)),
        ]),
    );
    categories.insert(
        "switchSize".to_string(),
        HashMap::from([
            ("sm".to_string(), TokenValue::Number(theme.switch_size_sm as f64)),
            ("md".to_string(), TokenValue::Number(theme.switch_size_md as f64)),
            ("lg".to_string(), TokenValue::Number(theme.switch_size_lg as f64)),
        ]),
    );
    categories.insert(
        "borderWidth".to_string(),
        HashMap::from([
            ("none".to_string(), TokenValue::Number(theme.border_width_none as f64)),
            ("sm".to_string(), TokenValue::Number(theme.border_width_sm as f64)),
            ("focus".to_string(), TokenValue::Number(theme.border_width_focus as f64)),
        ]),
    );
    categories.insert(
        "controlRadius".to_string(),
        HashMap::from([
            ("sm".to_string(), TokenValue::Number(theme.control_radius_sm as f64)),
            ("md".to_string(), TokenValue::Number(theme.control_radius_md as f64)),
            ("lg".to_string(), TokenValue::Number(theme.control_radius_lg as f64)),
        ]),
    );
    categories.insert(
        "controlPaddingInline".to_string(),
        HashMap::from([
            ("sm".to_string(), TokenValue::Number(theme.control_padding_inline_sm as f64)),
            ("md".to_string(), TokenValue::Number(theme.control_padding_inline_md as f64)),
            ("lg".to_string(), TokenValue::Number(theme.control_padding_inline_lg as f64)),
        ]),
    );
    categories.insert(
        "fontSize".to_string(),
        HashMap::from([
            ("xs".to_string(), TokenValue::Number(theme.font_size_xs as f64)),
            ("caption".to_string(), TokenValue::Number(theme.font_size_caption as f64)),
            ("sm".to_string(), TokenValue::Number(theme.font_size_sm as f64)),
            ("md".to_string(), TokenValue::Number(theme.font_size_md as f64)),
            ("lg".to_string(), TokenValue::Number(theme.font_size_lg as f64)),
            ("helper".to_string(), TokenValue::Number(theme.font_size_helper as f64)),
        ]),
    );
    categories.insert(
        "radius".to_string(),
        HashMap::from([
            ("sm".to_string(), TokenValue::Number(theme.radius_sm as f64)),
            ("md".to_string(), TokenValue::Number(theme.radius_md as f64)),
            ("default".to_string(), TokenValue::String(format!("radius.{}", theme.radius_default_key))),
            ("lg".to_string(), TokenValue::Number(theme.radius_lg as f64)),
            ("full".to_string(), TokenValue::Number(theme.radius_full as f64)),
        ]),
    );
    categories.insert(
        "fontWeight".to_string(),
        HashMap::from([
            ("normal".to_string(), TokenValue::Int(400)),
            ("medium".to_string(), TokenValue::Int(500)),
            ("semibold".to_string(), TokenValue::Int(600)),
            ("bold".to_string(), TokenValue::Int(700)),
        ]),
    );
    categories
        .insert("opacity".to_string(), HashMap::from([("disabled".to_string(), TokenValue::Number(0.5))]));

    TokenStore { categories }
}

fn build_component_theme_store(
    loaded_plugins: &[(plugin::BuiltinComponentSpec, PluginDefinition)],
    tokens: &TokenTheme,
) -> HashMap<String, ComponentThemeData> {
    let token_store = token_store_from_theme(tokens);
    loaded_plugins
        .iter()
        .map(|(spec, plugin)| (spec.key.to_string(), ComponentThemeData::from_plugin(plugin, &token_store)))
        .collect()
}

fn build_theme_record(
    name: &str,
    is_builtin: bool,
    parent_name: Option<&str>,
    tokens: TokenTheme,
    loaded_plugins: &[(plugin::BuiltinComponentSpec, PluginDefinition)],
) -> ThemeRecord {
    ThemeRecord {
        meta: ThemeMeta { name: name.to_string(), is_builtin, parent_name: parent_name.map(str::to_string) },
        component_themes: build_component_theme_store(loaded_plugins, &tokens),
        tokens,
    }
}

fn find_theme_index_by_name(themes: &[ThemeRecord], name: &str) -> Option<usize> {
    themes.iter().position(|theme| theme.meta.name == name)
}

fn explicit_parent_theme_index(themes: &[ThemeRecord], theme_idx: usize) -> Option<usize> {
    themes
        .get(theme_idx)
        .and_then(|theme| theme.meta.parent_name.as_deref())
        .and_then(|name| find_theme_index_by_name(themes, name))
}

fn editor_tokens_to_export_tokens(tokens: &TokenTheme) -> export_theme::ThemeTokens {
    let mut export_tokens = export_theme::create_schema_fallback_tokens();

    let set_shared = |set: &mut export_theme::TokenSet| {
        set.set("spacing", "xs", export_theme::TokenValue::Float(tokens.spacing_xs));
        set.set("spacing", "sm", export_theme::TokenValue::Float(tokens.spacing_sm));
        set.set("spacing", "md", export_theme::TokenValue::Float(tokens.spacing_md));
        set.set("spacing", "lg", export_theme::TokenValue::Float(tokens.spacing_lg));
        set.set("spacing", "xl", export_theme::TokenValue::Float(tokens.spacing_xl));
        set.set("controlSize", "sm", export_theme::TokenValue::Float(tokens.control_size_sm));
        set.set("controlSize", "md", export_theme::TokenValue::Float(tokens.control_size_md));
        set.set("controlSize", "lg", export_theme::TokenValue::Float(tokens.control_size_lg));
        set.set("iconSize", "sm", export_theme::TokenValue::Float(tokens.icon_size_sm));
        set.set("iconSize", "md", export_theme::TokenValue::Float(tokens.icon_size_md));
        set.set("iconSize", "lg", export_theme::TokenValue::Float(tokens.icon_size_lg));
        set.set("controlRadius", "sm", export_theme::TokenValue::Float(tokens.control_radius_sm));
        set.set("controlRadius", "md", export_theme::TokenValue::Float(tokens.control_radius_md));
        set.set("controlRadius", "lg", export_theme::TokenValue::Float(tokens.control_radius_lg));
        set.set(
            "controlPaddingInline",
            "sm",
            export_theme::TokenValue::Float(tokens.control_padding_inline_sm),
        );
        set.set(
            "controlPaddingInline",
            "md",
            export_theme::TokenValue::Float(tokens.control_padding_inline_md),
        );
        set.set(
            "controlPaddingInline",
            "lg",
            export_theme::TokenValue::Float(tokens.control_padding_inline_lg),
        );
        set.set("fontSize", "xs", export_theme::TokenValue::Float(tokens.font_size_xs));
        set.set("fontSize", "caption", export_theme::TokenValue::Float(tokens.font_size_caption));
        set.set("fontSize", "sm", export_theme::TokenValue::Float(tokens.font_size_sm));
        set.set("fontSize", "md", export_theme::TokenValue::Float(tokens.font_size_md));
        set.set("fontSize", "lg", export_theme::TokenValue::Float(tokens.font_size_lg));
        set.set("fontSize", "helper", export_theme::TokenValue::Float(tokens.font_size_helper));
        set.set("borderWidth", "none", export_theme::TokenValue::Float(tokens.border_width_none));
        set.set("borderWidth", "sm", export_theme::TokenValue::Float(tokens.border_width_sm));
        set.set("borderWidth", "focus", export_theme::TokenValue::Float(tokens.border_width_focus));
        set.set("choiceControlSize", "sm", export_theme::TokenValue::Float(tokens.choice_control_size_sm));
        set.set("choiceControlSize", "md", export_theme::TokenValue::Float(tokens.choice_control_size_md));
        set.set("choiceControlSize", "lg", export_theme::TokenValue::Float(tokens.choice_control_size_lg));
        set.set("switchSize", "sm", export_theme::TokenValue::Float(tokens.switch_size_sm));
        set.set("switchSize", "md", export_theme::TokenValue::Float(tokens.switch_size_md));
        set.set("switchSize", "lg", export_theme::TokenValue::Float(tokens.switch_size_lg));
        set.set("radius", "sm", export_theme::TokenValue::Float(tokens.radius_sm));
        set.set("radius", "md", export_theme::TokenValue::Float(tokens.radius_md));
        set.set(
            "radius",
            "default",
            export_theme::TokenValue::Ref(format!("radius.{}", tokens.radius_default_key)),
        );
        set.set("radius", "lg", export_theme::TokenValue::Float(tokens.radius_lg));
        set.set("radius", "full", export_theme::TokenValue::Float(tokens.radius_full));
        set.set("font", "primary", export_theme::TokenValue::String(tokens.font_primary.clone()));
        set.set("font", "secondary", export_theme::TokenValue::String(tokens.font_secondary.clone()));
        set.set("font", "tertiary", export_theme::TokenValue::String(tokens.font_tertiary.clone()));
        set.set("fontWeight", "normal", export_theme::TokenValue::Int(tokens.font_weight_normal));
        set.set("fontWeight", "medium", export_theme::TokenValue::Int(tokens.font_weight_medium));
        set.set("fontWeight", "semibold", export_theme::TokenValue::Int(tokens.font_weight_semibold));
        set.set("fontWeight", "bold", export_theme::TokenValue::Int(tokens.font_weight_bold));
    };

    set_shared(&mut export_tokens.light);
    set_shared(&mut export_tokens.dark);

    let set_scheme_colors = |set: &mut export_theme::TokenSet,
                             primary: slint::Color,
                             primary_pressed: slint::Color,
                             secondary: slint::Color,
                             danger: slint::Color,
                             surface: slint::Color,
                             background: slint::Color,
                             text: slint::Color,
                             text_muted: slint::Color| {
        let primary_light = slint_color_to_token(primary).lighten(0.2);
        let secondary_dark = slint_color_to_token(secondary).darken(0.1);
        let danger_light = slint_color_to_token(danger).lighten(0.15);
        let danger_dark = slint_color_to_token(danger).darken(0.1);
        let foreground_light = slint_color_to_token(text).with_alpha(0.1);
        set.set("color", "primary", export_theme::TokenValue::Color(slint_color_to_export_color(primary)));
        set.set("color", "primary.light", export_theme::TokenValue::Color(primary_light));
        set.set(
            "color",
            "primary.dark",
            export_theme::TokenValue::Color(slint_color_to_export_color(primary_pressed)),
        );
        set.set(
            "color",
            "primary_pressed",
            export_theme::TokenValue::Color(slint_color_to_export_color(primary_pressed)),
        );
        set.set(
            "color",
            "secondary",
            export_theme::TokenValue::Color(slint_color_to_export_color(secondary)),
        );
        set.set("color", "secondary.dark", export_theme::TokenValue::Color(secondary_dark));
        set.set("color", "danger", export_theme::TokenValue::Color(slint_color_to_export_color(danger)));
        set.set("color", "danger.light", export_theme::TokenValue::Color(danger_light));
        set.set("color", "danger.dark", export_theme::TokenValue::Color(danger_dark));
        set.set("color", "surface", export_theme::TokenValue::Color(slint_color_to_export_color(surface)));
        set.set(
            "color",
            "background",
            export_theme::TokenValue::Color(slint_color_to_export_color(background)),
        );
        set.set("color", "foreground", export_theme::TokenValue::Color(slint_color_to_export_color(text)));
        set.set("color", "foreground.light", export_theme::TokenValue::Color(foreground_light));
        set.set("color", "text", export_theme::TokenValue::Color(slint_color_to_export_color(text)));
        set.set("color", "muted", export_theme::TokenValue::Color(slint_color_to_export_color(text_muted)));
        set.set(
            "color",
            "text_muted",
            export_theme::TokenValue::Color(slint_color_to_export_color(text_muted)),
        );
    };

    set_scheme_colors(
        &mut export_tokens.light,
        tokens.light_primary,
        tokens.light_primary_pressed,
        tokens.light_secondary,
        tokens.light_danger,
        tokens.light_surface,
        tokens.light_background,
        tokens.light_text,
        tokens.light_text_muted,
    );
    set_scheme_colors(
        &mut export_tokens.dark,
        tokens.dark_primary,
        tokens.dark_primary_pressed,
        tokens.dark_secondary,
        tokens.dark_danger,
        tokens.dark_surface,
        tokens.dark_background,
        tokens.dark_text,
        tokens.dark_text_muted,
    );

    export_tokens
}

fn theme_record_to_export_theme(record: &ThemeRecord) -> export_theme::Theme {
    let mut theme = export_theme::Theme {
        tokens: editor_tokens_to_export_tokens(&record.tokens),
        components: HashMap::new(),
    };

    let mut component_keys: Vec<_> = record.component_themes.keys().cloned().collect();
    component_keys.sort();
    for key in component_keys {
        if let Some(data) = record.component_themes.get(&key) {
            theme.components.insert(key, component_theme_data_to_export_component(data));
        }
    }

    theme
}

fn slint_color_to_export_color(color: slint::Color) -> export_theme::Color {
    export_theme::Color::rgba(color.red(), color.green(), color.blue(), color.alpha())
}

fn component_theme_data_to_export_component(data: &ComponentThemeData) -> export_theme::ComponentTheme {
    let mut component = export_theme::ComponentTheme::new();
    let mut variant_names: Vec<_> = data.variant_props.keys().cloned().collect();
    variant_names.sort();
    for variant_name in variant_names {
        let mut variant = export_theme::VariantTheme::new();
        if let Some(states) = data.variant_props.get(&variant_name) {
            let mut state_names: Vec<_> = states.keys().cloned().collect();
            state_names.sort();
            for state_name in state_names {
                let Some(props) = states.get(&state_name) else { continue };
                let mut style = export_theme::StyleProps::new();
                for (prop_name, value) in props {
                    match value {
                        plugin::PropertyValue::Token(token) => {
                            export_theme::set_style_prop(
                                &mut style,
                                prop_name,
                                export_theme::token_ref(token.clone()),
                            );
                        }
                        plugin::PropertyValue::Color(color) => {
                            export_theme::set_style_prop(
                                &mut style,
                                prop_name,
                                slint_color_to_export_color(*color),
                            );
                        }
                        plugin::PropertyValue::Float(number) => {
                            export_theme::set_style_prop(&mut style, prop_name, *number);
                        }
                        plugin::PropertyValue::Int(number) => {
                            export_theme::set_style_prop(&mut style, prop_name, *number);
                        }
                        plugin::PropertyValue::Bool(flag) => {
                            export_theme::set_style_prop(&mut style, prop_name, *flag);
                        }
                        plugin::PropertyValue::String(text) => {
                            export_theme::set_style_prop(&mut style, prop_name, text.clone());
                        }
                    }
                }
                if state_name == "default" || state_name == "normal" {
                    variant.default = style;
                } else {
                    variant.states.insert(state_name, style);
                }
            }
        }
        if let Some(sizes) = data.size_props.get("default") {
            let mut style = export_theme::StyleProps::new();
            for (prop_name, value) in sizes {
                match value {
                    plugin::PropertyValue::Token(token) => {
                        export_theme::set_style_prop(
                            &mut style,
                            prop_name,
                            export_theme::token_ref(token.clone()),
                        );
                    }
                    plugin::PropertyValue::Color(color) => {
                        export_theme::set_style_prop(
                            &mut style,
                            prop_name,
                            slint_color_to_export_color(*color),
                        );
                    }
                    plugin::PropertyValue::Float(number) => {
                        export_theme::set_style_prop(&mut style, prop_name, *number);
                    }
                    plugin::PropertyValue::Int(number) => {
                        export_theme::set_style_prop(&mut style, prop_name, *number);
                    }
                    plugin::PropertyValue::Bool(flag) => {
                        export_theme::set_style_prop(&mut style, prop_name, *flag);
                    }
                    plugin::PropertyValue::String(text) => {
                        export_theme::set_style_prop(&mut style, prop_name, text.clone());
                    }
                }
            }
            variant.sizes.insert("default".to_string(), style);
        }
        component.variants.insert(variant_name, variant);
    }
    component
}

#[derive(Clone, PartialEq)]
enum ExportLiteral {
    Color(slint::Color),
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
    TokenRef(String),
}

impl ExportLiteral {
    fn to_macro_expr(&self) -> String {
        match self {
            ExportLiteral::Color(color) => format!("color({})", rust_string_literal(&color_literal(*color))),
            ExportLiteral::Float(number) => format_rust_float(*number),
            ExportLiteral::Int(number) => number.to_string(),
            ExportLiteral::Bool(flag) => flag.to_string(),
            ExportLiteral::String(text) => rust_string_literal(text),
            ExportLiteral::TokenRef(path) => format!("token_ref({})", rust_string_literal(path)),
        }
    }
}

fn token_literal(value: &export_theme::TokenValue) -> ExportLiteral {
    match value {
        export_theme::TokenValue::Color(color) => ExportLiteral::Color(color.to_slint()),
        export_theme::TokenValue::Float(number) => ExportLiteral::Float(*number),
        export_theme::TokenValue::Int(number) => ExportLiteral::Int(*number),
        export_theme::TokenValue::Bool(flag) => ExportLiteral::Bool(*flag),
        export_theme::TokenValue::String(text) => ExportLiteral::String(text.clone()),
        export_theme::TokenValue::Ref(path) => ExportLiteral::TokenRef(path.clone()),
    }
}

fn style_value_literal(value: &export_theme::StyleValue) -> ExportLiteral {
    match value {
        export_theme::StyleValue::Color(color) => ExportLiteral::Color(color.to_slint()),
        export_theme::StyleValue::Float(number) => ExportLiteral::Float(*number),
        export_theme::StyleValue::Int(number) => ExportLiteral::Int(*number),
        export_theme::StyleValue::Bool(flag) => ExportLiteral::Bool(*flag),
        export_theme::StyleValue::String(text) => ExportLiteral::String(text.clone()),
    }
}

fn style_prop_literal(style: &export_theme::StyleProps, key: &str) -> Option<ExportLiteral> {
    if let Some(path) = style.token_refs.get(key) {
        return Some(ExportLiteral::TokenRef(path.clone()));
    }
    match key {
        "background" => style.background.map(|v| ExportLiteral::Color(v.to_slint())),
        "foreground" => style.foreground.map(|v| ExportLiteral::Color(v.to_slint())),
        "border_color" => style.border_color.map(|v| ExportLiteral::Color(v.to_slint())),
        "border_width" => style.border_width.map(ExportLiteral::Float),
        "border_radius" => style.border_radius.map(ExportLiteral::Float),
        "padding_horizontal" => style.padding_horizontal.map(ExportLiteral::Float),
        "padding_vertical" => style.padding_vertical.map(ExportLiteral::Float),
        "min_height" => style.min_height.map(ExportLiteral::Float),
        "min_width" => style.min_width.map(ExportLiteral::Float),
        "font_family" => style.font_family.clone().map(ExportLiteral::String),
        "font_size" => style.font_size.map(ExportLiteral::Float),
        "font_weight" => style.font_weight.map(ExportLiteral::Int),
        "icon_size" => style.icon_size.map(ExportLiteral::Float),
        "opacity" => style.opacity.map(ExportLiteral::Float),
        "touch_expansion" => style.touch_expansion.map(ExportLiteral::Float),
        key => style.extra.get(key).map(style_value_literal),
    }
}

fn color_literal(color: slint::Color) -> String {
    if color.alpha() == 255 {
        format!("#{:02x}{:02x}{:02x}", color.red(), color.green(), color.blue())
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", color.red(), color.green(), color.blue(), color.alpha())
    }
}

fn format_rust_float(value: f32) -> String {
    if value.fract().abs() < f32::EPSILON {
        return format!("{value:.1}");
    }
    let mut text = format!("{value:.4}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.push('0');
    }
    text
}

fn rust_string_literal(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn theme_function_name(name: &str) -> String {
    let mut ident = String::new();
    let mut last_was_underscore = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if ident.is_empty() && ch.is_ascii_digit() {
                ident.push('_');
            }
            ident.push(ch.to_ascii_lowercase());
            last_was_underscore = false;
        } else if !last_was_underscore {
            ident.push('_');
            last_was_underscore = true;
        }
    }
    while ident.ends_with('_') {
        ident.pop();
    }
    if ident.is_empty() {
        ident.push_str("theme_export");
    }
    ident
}

fn push_line(out: &mut String, indent: usize, line: impl AsRef<str>) {
    out.push_str(&" ".repeat(indent));
    out.push_str(line.as_ref());
    out.push('\n');
}

fn style_prop_lines(
    style: &export_theme::StyleProps,
    parent: Option<&export_theme::StyleProps>,
) -> Vec<String> {
    let mut lines = Vec::new();
    let push_if_changed = |lines: &mut Vec<String>,
                           key: &str,
                           current: Option<ExportLiteral>,
                           parent: Option<ExportLiteral>| {
        if current != parent {
            if let Some(value) = current {
                lines.push(format!("\"{key}\" = {};", value.to_macro_expr()));
            }
        }
    };
    for key in [
        "background",
        "foreground",
        "border_color",
        "border_width",
        "border_radius",
        "padding_horizontal",
        "padding_vertical",
        "min_height",
        "min_width",
        "font_family",
        "font_size",
        "font_weight",
        "icon_size",
        "opacity",
        "touch_expansion",
    ] {
        push_if_changed(
            &mut lines,
            key,
            style_prop_literal(style, key),
            parent.and_then(|style| style_prop_literal(style, key)),
        );
    }
    let mut extra_keys: HashSet<String> = style.extra.keys().cloned().collect();
    extra_keys.extend(style.token_refs.keys().cloned());
    let mut extra_keys: Vec<_> = extra_keys.into_iter().collect();
    extra_keys.sort();
    for key in extra_keys {
        if matches!(
            key.as_str(),
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
        ) {
            continue;
        }
        push_if_changed(
            &mut lines,
            &key,
            style_prop_literal(style, &key),
            parent.and_then(|style| style_prop_literal(style, &key)),
        );
    }
    lines
}

fn emit_style_block(
    out: &mut String,
    indent: usize,
    header: &str,
    style: &export_theme::StyleProps,
    parent: Option<&export_theme::StyleProps>,
) -> bool {
    let lines = style_prop_lines(style, parent);
    if lines.is_empty() {
        return false;
    }
    push_line(out, indent, format!("{header} {{"));
    for line in lines {
        push_line(out, indent + 4, line);
    }
    push_line(out, indent, "}");
    true
}

fn emit_variant_block(
    out: &mut String,
    indent: usize,
    variant_name: &str,
    variant: &export_theme::VariantTheme,
    parent: Option<&export_theme::VariantTheme>,
) -> bool {
    let mut inner = String::new();
    emit_style_block(&mut inner, indent + 4, "default", &variant.default, parent.map(|v| &v.default));
    let mut size_names: Vec<_> = variant.sizes.keys().cloned().collect();
    size_names.sort();
    for size_name in size_names {
        if let Some(style) = variant.sizes.get(&size_name) {
            emit_style_block(
                &mut inner,
                indent + 4,
                &format!("size {}", rust_string_literal(&size_name)),
                style,
                parent.and_then(|variant| variant.sizes.get(&size_name)),
            );
        }
    }
    let mut state_names: Vec<_> = variant.states.keys().cloned().collect();
    state_names.sort();
    for state_name in state_names {
        if let Some(style) = variant.states.get(&state_name) {
            emit_style_block(
                &mut inner,
                indent + 4,
                &format!("state {}", rust_string_literal(&state_name)),
                style,
                parent.and_then(|variant| variant.states.get(&state_name)),
            );
        }
    }
    if inner.is_empty() {
        return false;
    }
    push_line(out, indent, format!("variant {} {{", rust_string_literal(variant_name)));
    out.push_str(&inner);
    push_line(out, indent, "}");
    true
}

fn emit_component_block(
    out: &mut String,
    indent: usize,
    component_name: &str,
    component: &export_theme::ComponentTheme,
    parent: Option<&export_theme::ComponentTheme>,
) -> bool {
    let mut inner = String::new();
    emit_style_block(&mut inner, indent + 4, "base", &component.base, parent.map(|c| &c.base));
    let mut variant_names: Vec<_> = component.variants.keys().cloned().collect();
    variant_names.sort();
    for variant_name in variant_names {
        if let Some(variant) = component.variants.get(&variant_name) {
            emit_variant_block(
                &mut inner,
                indent + 4,
                &variant_name,
                variant,
                parent.and_then(|component| component.variants.get(&variant_name)),
            );
        }
    }
    if inner.is_empty() {
        return false;
    }
    push_line(out, indent, format!("component {} {{", rust_string_literal(component_name)));
    out.push_str(&inner);
    push_line(out, indent, "}");
    true
}

fn should_inline_parent_theme(themes: &[ThemeRecord], theme_idx: usize, selected_theme_idx: usize) -> bool {
    if theme_idx == selected_theme_idx {
        return true;
    }
    !themes.get(theme_idx).map(is_builtin_base_theme).unwrap_or(false)
}

fn export_parent_reference<'a>(
    themes: &'a [ThemeRecord],
    export_themes: &'a HashMap<usize, export_theme::Theme>,
    theme_idx: usize,
    selected_theme_idx: usize,
) -> Option<(String, &'a export_theme::Theme)> {
    let parent_idx = explicit_parent_theme_index(themes, theme_idx)?;
    let parent_record = themes.get(parent_idx)?;
    let parent_theme = export_themes.get(&parent_idx)?;
    let parent_expr = if should_inline_parent_theme(themes, parent_idx, selected_theme_idx) {
        format!("{}()", theme_record_function_name(parent_record))
    } else {
        "base_theme()".to_string()
    };
    Some((parent_expr, parent_theme))
}

fn export_needs_base_theme_import(themes: &[ThemeRecord], theme_idx: usize) -> bool {
    let mut current = explicit_parent_theme_index(themes, theme_idx);
    while let Some(idx) = current {
        let Some(record) = themes.get(idx) else { break };
        if is_builtin_base_theme(record) {
            return true;
        }
        current = explicit_parent_theme_index(themes, idx);
    }
    false
}

fn theme_export_chain(themes: &[ThemeRecord], theme_idx: usize) -> Vec<usize> {
    fn visit(themes: &[ThemeRecord], theme_idx: usize, visited: &mut HashSet<usize>, chain: &mut Vec<usize>) {
        if !visited.insert(theme_idx) {
            return;
        }
        if let Some(parent_idx) = explicit_parent_theme_index(themes, theme_idx) {
            visit(themes, parent_idx, visited, chain);
        }
        chain.push(theme_idx);
    }
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    visit(themes, theme_idx, &mut visited, &mut chain);
    chain
}

fn emit_theme_function(
    out: &mut String,
    function_name: &str,
    is_public: bool,
    theme: &export_theme::Theme,
    parent: Option<(&str, &export_theme::Theme)>,
) {
    out.push_str("define_theme! {\n");
    let visibility = if is_public { "pub " } else { "" };
    push_line(out, 4, format!("{visibility}fn {function_name}() -> Theme {{"));
    if let Some((parent_expr, _)) = parent {
        push_line(out, 8, format!("extends {parent_expr};"));
    }
    let mut scheme_entries = Vec::new();
    for (scheme_name, token_set, parent_token_set) in [
        ("light", &theme.tokens.light, parent.map(|(_, theme)| &theme.tokens.light)),
        ("dark", &theme.tokens.dark, parent.map(|(_, theme)| &theme.tokens.dark)),
    ] {
        let mut token_entries = Vec::new();
        for (category, keys) in token_set.categories() {
            for (key, value) in keys {
                token_entries.push((format!("{category}.{key}"), token_literal(value)));
            }
        }
        token_entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut changed = Vec::new();
        for (path, value) in token_entries {
            let parent_value = parent_token_set
                .and_then(|tokens| path.split_once('.').and_then(|(category, key)| tokens.get(category, key)))
                .map(token_literal);
            if Some(value.clone()) != parent_value {
                changed.push((path, value));
            }
        }
        scheme_entries.push((scheme_name, changed));
    }
    if scheme_entries.iter().any(|(_, entries)| !entries.is_empty()) {
        push_line(out, 8, "tokens {");
        for (scheme_name, entries) in scheme_entries {
            if entries.is_empty() {
                continue;
            }
            push_line(out, 12, format!("{scheme_name} {{"));
            for (path, value) in entries {
                push_line(out, 16, format!("{} = {};", rust_string_literal(&path), value.to_macro_expr()));
            }
            push_line(out, 12, "}");
        }
        push_line(out, 8, "}");
    }
    let mut component_names: Vec<_> = theme.components.keys().cloned().collect();
    component_names.sort();
    for component_name in component_names {
        if let Some(component) = theme.components.get(&component_name) {
            emit_component_block(
                out,
                8,
                &component_name,
                component,
                parent.and_then(|(_, theme)| theme.components.get(&component_name)),
            );
        }
    }
    push_line(out, 4, "}");
    out.push_str("}\n\n");
}

pub fn generate_theme_rust_export(
    themes: &[ThemeRecord],
    theme_idx: usize,
    base_theme_import_path: &str,
) -> Option<String> {
    let theme = themes.get(theme_idx)?;
    let chain = theme_export_chain(themes, theme_idx);
    if chain.is_empty() {
        return None;
    }
    let mut export_themes = HashMap::new();
    for idx in &chain {
        if let Some(record) = themes.get(*idx) {
            export_themes.insert(*idx, theme_record_to_export_theme(record));
        }
    }
    let mut rust_code = String::new();
    rust_code
        .push_str(&format!("//! Theme: {}\n//! Generated by Prime UI Theme Editor\n\n", theme.meta.name));
    rust_code.push_str("use components::{color, define_theme, token_ref, Theme};\n");
    if export_needs_base_theme_import(themes, theme_idx) && !is_builtin_base_theme(theme) {
        rust_code.push_str(&format!("use {base_theme_import_path};\n"));
    }
    rust_code.push('\n');
    for idx in chain.iter().copied().filter(|idx| should_inline_parent_theme(themes, *idx, theme_idx)) {
        let record = themes.get(idx)?;
        let function_name = theme_record_function_name(record);
        let parent = export_parent_reference(themes, &export_themes, idx, theme_idx);
        let export_theme = export_themes.get(&idx)?;
        emit_theme_function(
            &mut rust_code,
            &function_name,
            idx == theme_idx,
            export_theme,
            parent.as_ref().map(|(expr, theme)| (expr.as_str(), *theme)),
        );
    }
    Some(rust_code)
}
