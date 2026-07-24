#![allow(dead_code)]
// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

mod color_utils;
mod icons;
mod token_lookup;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use color_utils::{
    check_color_equal, check_float_equal, color_alpha_percent, format_hex_color, parse_hex_color,
};
use components::theme as export_theme;
use icons::IconRegistry;
use plugin::{
    builtin_component_specs, ensure_default_files, load_all_plugins, ComponentThemeData, PluginDefinition,
    PropertyValue, TokenStore,
};
use serde_json::{json, Value};
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use theme_editor::{plugin, theme_export};
use token_lookup::{
    actual_font_family_index_from_value, color_token_index_from_key, color_token_key_from_index,
    font_token_index_from_key, font_token_key_from_index, number_token_index_from_key,
    number_token_key_from_index, radius_default_index_from_key, radius_default_key_from_index,
};

// One TokenTheme for the editor — defined in theme_export.rs, used directly
// here. The previous private duplicate in main.rs has been removed (M29 / U3).
use crate::theme_export::TokenTheme;

slint::include_modules!();

const BASE_THEME_NAME: &str = "Base Theme";
const LEGACY_DEFAULT_THEME_NAME: &str = "Default Theme";
const BASE_THEME_ID: &str = "base_theme";
const LEGACY_DEFAULT_THEME_ID: &str = "default_theme";
const TOKEN_THEME_KEYS: &[&str] = &[
    "colors.light.primary",
    "colors.light.primary-pressed",
    "colors.light.secondary",
    "colors.light.danger",
    "colors.light.surface",
    "colors.light.background",
    "colors.light.text",
    "colors.light.text-muted",
    "colors.light.transparent",
    "colors.dark.primary",
    "colors.dark.primary-pressed",
    "colors.dark.secondary",
    "colors.dark.danger",
    "colors.dark.surface",
    "colors.dark.background",
    "colors.dark.text",
    "colors.dark.text-muted",
    "colors.dark.transparent",
    "spacing.xs",
    "spacing.sm",
    "spacing.md",
    "spacing.lg",
    "spacing.xl",
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
    "borderWidth.none",
    "borderWidth.sm",
    "borderWidth.focus",
    "controlRadius.sm",
    "controlRadius.md",
    "controlRadius.lg",
    "controlPaddingInline.sm",
    "controlPaddingInline.md",
    "controlPaddingInline.lg",
    "typography.font-size-xs",
    "typography.font-size-caption",
    "typography.font-size-sm",
    "typography.font-size-md",
    "typography.font-size-lg",
    "typography.font-size-helper",
    "typography.font-primary",
    "typography.font-secondary",
    "typography.font-tertiary",
    "fontWeight.normal",
    "fontWeight.medium",
    "fontWeight.semibold",
    "fontWeight.bold",
    "radius.sm",
    "radius.md",
    "radius.lg",
    "radius.full",
    "radius.default",
];

/// Theme metadata for UI display
#[derive(Clone)]
struct ThemeMeta {
    name: String,
    is_builtin: bool,
    parent_name: Option<String>,
}

#[derive(Clone)]
struct ThemeRecord {
    meta: ThemeMeta,
    tokens: TokenTheme,
    token_overrides: HashSet<String>,
    component_themes: HashMap<String, ComponentThemeData>,
}

/// Undo/redo snapshots capture the complete in-memory theme graph so edits
/// that also update dependent parent references are restored atomically.
type ThemeEditSnapshot = Vec<ThemeRecord>;

impl From<&ThemeMeta> for theme_export::ThemeMeta {
    fn from(meta: &ThemeMeta) -> Self {
        Self { name: meta.name.clone(), is_builtin: meta.is_builtin, parent_name: meta.parent_name.clone() }
    }
}

// TokenTheme is now the same type on both sides (main.rs uses
// theme_export::TokenTheme directly), so the previous field-by-field From impl
// has been replaced with a plain clone.

impl From<&ThemeRecord> for theme_export::ThemeRecord {
    fn from(record: &ThemeRecord) -> Self {
        Self {
            meta: (&record.meta).into(),
            tokens: record.tokens.clone(),
            component_themes: record.component_themes.clone(),
        }
    }
}

/// Update the theme list in the UI
fn update_theme_list_ui(app: &AppWindow, themes: &[ThemeRecord]) {
    let theme_list: Vec<ThemeInfo> = themes
        .iter()
        .map(|t| ThemeInfo { name: t.meta.name.clone().into(), is_builtin: t.meta.is_builtin })
        .collect();
    app.set_theme_list(slint::ModelRc::new(slint::VecModel::from(theme_list)));

    let theme_names: Vec<slint::SharedString> = themes.iter().map(|t| t.meta.name.clone().into()).collect();
    app.set_theme_name_list(slint::ModelRc::new(slint::VecModel::from(theme_names)));
}

fn update_parent_theme_ui(app: &AppWindow, themes: &[ThemeRecord], current_idx: usize) {
    let (options, selected_index) = parent_theme_options(themes, current_idx);
    let options: Vec<slint::SharedString> = options.into_iter().map(Into::into).collect();
    app.set_parent_theme_name_list(slint::ModelRc::new(slint::VecModel::from(options)));
    app.set_selected_parent_theme(selected_index);
}

fn parent_theme_options(themes: &[ThemeRecord], current_idx: usize) -> (Vec<String>, i32) {
    let current_name = themes.get(current_idx).map(|theme| theme.meta.name.as_str()).unwrap_or_default();
    let current_parent = themes.get(current_idx).and_then(|theme| theme.meta.parent_name.as_deref());

    let mut options = vec!["None".to_string()];
    let mut selected_index = 0i32;

    for theme in themes {
        if theme.meta.name == current_name {
            continue;
        }

        if current_parent == Some(theme.meta.name.as_str()) {
            selected_index = options.len() as i32;
        }

        options.push(theme.meta.name.clone());
    }

    (options, selected_index)
}

fn find_theme_index_by_name(themes: &[ThemeRecord], name: &str) -> Option<usize> {
    themes.iter().position(|theme| theme.meta.name == name)
}

fn is_base_theme_name(name: &str) -> bool {
    name == BASE_THEME_NAME
        || name == LEGACY_DEFAULT_THEME_NAME
        || normalize_theme_identifier(name) == BASE_THEME_ID
        || normalize_theme_identifier(name) == LEGACY_DEFAULT_THEME_ID
}

fn is_builtin_base_theme(theme: &ThemeRecord) -> bool {
    theme.meta.is_builtin && is_base_theme_name(&theme.meta.name)
}

fn theme_record_json_id(theme: &ThemeRecord) -> String {
    if is_builtin_base_theme(theme) {
        BASE_THEME_ID.to_string()
    } else {
        normalize_theme_identifier(&theme.meta.name)
    }
}

fn theme_record_function_name(theme: &ThemeRecord) -> String {
    if is_builtin_base_theme(theme) {
        BASE_THEME_ID.to_string()
    } else {
        theme_function_name(&theme.meta.name)
    }
}

fn all_token_override_keys() -> HashSet<String> {
    TOKEN_THEME_KEYS.iter().map(|key| (*key).to_string()).collect()
}

fn json_path_exists(value: &Value, path: &str) -> bool {
    path.split('.').try_fold(value, |current, segment| current.get(segment)).is_some()
}

fn collect_token_override_keys(value: &Value) -> HashSet<String> {
    TOKEN_THEME_KEYS
        .iter()
        .filter(|path| json_path_exists(value, path))
        .map(|path| (*path).to_string())
        .collect()
}

fn color_token_override_path(token_name: &str, is_dark: bool) -> Option<String> {
    match token_name {
        "primary" | "primary-pressed" | "secondary" | "danger" | "surface" | "background" | "text"
        | "text-muted" => Some(format!("colors.{}.{}", if is_dark { "dark" } else { "light" }, token_name)),
        _ => None,
    }
}

fn theme_parent_would_cycle(themes: &[ThemeRecord], theme_idx: usize, candidate_parent: &str) -> bool {
    let mut visited = HashSet::new();
    let mut current = find_theme_index_by_name(themes, candidate_parent);

    while let Some(index) = current {
        if !visited.insert(index) {
            return true;
        }
        if index == theme_idx {
            return true;
        }
        current =
            themes[index].meta.parent_name.as_deref().and_then(|name| find_theme_index_by_name(themes, name));
    }

    false
}

fn update_theme_component_list_ui(app: &AppWindow) {
    let specs = sorted_theme_component_specs();
    let component_list: Vec<ThemeComponentInfo> = specs
        .iter()
        .map(|spec| ThemeComponentInfo { name: spec.component.into(), key: spec.key.into() })
        .collect();
    let component_name_list: Vec<slint::SharedString> =
        specs.iter().map(|spec| spec.component.into()).collect();
    app.set_theme_component_list(slint::ModelRc::new(slint::VecModel::from(component_list)));
    app.set_theme_component_name_list(slint::ModelRc::new(slint::VecModel::from(component_name_list)));
}

fn theme_component_index(component_key: &str) -> i32 {
    sorted_theme_component_specs().iter().position(|spec| spec.key == component_key).unwrap_or(0) as i32
}

fn sorted_theme_component_specs() -> Vec<&'static plugin::BuiltinComponentSpec> {
    let mut specs: Vec<&'static plugin::BuiltinComponentSpec> = builtin_component_specs().iter().collect();
    specs.sort_by(|a, b| a.component.cmp(b.component));
    specs
}

fn slint_color_to_token(color: slint::Color) -> components::theme::Color {
    components::theme::Color::rgba(color.red(), color.green(), color.blue(), color.alpha())
}

fn current_semantic_palette(
    app: &AppWindow,
) -> (
    bool,
    slint::Color,
    slint::Color,
    slint::Color,
    slint::Color,
    slint::Color,
    slint::Color,
    slint::Color,
    slint::Color,
    slint::Color,
    slint::Color,
) {
    let is_dark = app.get_dark_mode();
    let primary = if is_dark { app.get_dark_token_primary() } else { app.get_light_token_primary() };
    let primary_pressed =
        if is_dark { app.get_dark_token_primary_pressed() } else { app.get_light_token_primary_pressed() };
    let secondary = if is_dark { app.get_dark_token_secondary() } else { app.get_light_token_secondary() };
    let danger = if is_dark { app.get_dark_token_danger() } else { app.get_light_token_danger() };
    let surface = if is_dark { app.get_dark_token_surface() } else { app.get_light_token_surface() };
    let background = if is_dark { app.get_dark_token_background() } else { app.get_light_token_background() };
    let foreground = if is_dark { app.get_dark_token_text() } else { app.get_light_token_text() };
    let muted = if is_dark { app.get_dark_token_text_muted() } else { app.get_light_token_text_muted() };

    let secondary_token = slint_color_to_token(secondary);
    let border = if is_dark { secondary } else { secondary_token.darken(0.08).to_slint() };
    let secondary_pressed = if is_dark {
        secondary_token.lighten(0.14).to_slint()
    } else {
        secondary_token.darken(0.1).to_slint()
    };

    (
        is_dark,
        primary,
        primary_pressed,
        secondary,
        secondary_pressed,
        danger,
        surface,
        background,
        foreground,
        muted,
        border,
    )
}

/// Initialize the shared Theme global: semantic palette + design tokens. (Button
/// styling is no longer pushed here — it reads its generated ButtonTheme global,
/// which cascades from these values like every other component.)
fn init_theme_global(app: &AppWindow, tokens: &TokenTheme) {
    let theme_global = app.global::<Theme>();
    let (
        is_dark,
        primary,
        primary_pressed,
        secondary,
        secondary_pressed,
        danger,
        surface,
        background,
        foreground,
        muted,
        border,
    ) = current_semantic_palette(app);

    theme_global.set_is_dark(is_dark);
    theme_global.set_palette_primary(primary);
    theme_global.set_palette_primary_pressed(primary_pressed);
    theme_global.set_palette_secondary(secondary);
    theme_global.set_palette_secondary_pressed(secondary_pressed);
    theme_global.set_palette_danger(danger);
    theme_global.set_palette_surface(surface);
    theme_global.set_palette_background(background);
    theme_global.set_palette_foreground(foreground);
    theme_global.set_palette_muted(muted);
    theme_global.set_palette_border(border);

    // Button styling is not pushed here. Like every other component, Button
    // reads its generated ButtonTheme global, whose entries are bindings over
    // the color-* + token surface set by init_theme_global_tokens below, so it
    // cascades to the active scheme automatically.
    init_theme_global_tokens(app, tokens);
}

/// Push the shared design tokens (font.*, fontSize.*, controlSize.*, iconSize.*,
/// radius.*, spacing.*, controlRadius.*, controlPaddingInline.*, fontWeight.*)
/// from the active theme into the `Theme` global, so components can read them
/// instead of baking in local constants. Size/spacing tokens are
/// scheme-independent; colors are handled separately by `init_theme_global`.
/// Any token the store can't resolve keeps its Slint-side fallback (which
/// mirrors Base Theme).
fn init_theme_global_tokens(app: &AppWindow, tokens: &TokenTheme) {
    let tg = app.global::<Theme>();
    let store = token_store_from_theme(tokens);

    // Prefer the live Tokens-panel values (app props); fall back to the
    // persisted theme record, then to the Slint-side default (left untouched
    // when a resolver returns None).
    let num = |key: &str| {
        resolve_number_token_from_app(app, key).or_else(|| resolve_number_token_from_store(&store, key))
    };
    let font = |key: &str| {
        resolve_font_token_from_app(app, key).or_else(|| resolve_font_token_from_store(&store, key))
    };
    let col = |key: &str| {
        resolve_color_token_from_app(app, key).or_else(|| resolve_color_token_from_store(&store, key))
    };

    // Font families
    if let Some(v) = font("font.primary") {
        tg.set_font_primary(v.into());
    }
    if let Some(v) = font("font.secondary") {
        tg.set_font_secondary(v.into());
    }
    if let Some(v) = font("font.tertiary") {
        tg.set_font_tertiary(v.into());
    }

    // Font scale
    if let Some(v) = num("fontSize.xs") {
        tg.set_font_size_xs(v);
    }
    if let Some(v) = num("fontSize.caption") {
        tg.set_font_size_caption(v);
    }
    if let Some(v) = num("fontSize.helper") {
        tg.set_font_size_helper(v);
    }
    if let Some(v) = num("fontSize.sm") {
        tg.set_font_size_sm(v);
    }
    if let Some(v) = num("fontSize.md") {
        tg.set_font_size_md(v);
    }
    if let Some(v) = num("fontSize.lg") {
        tg.set_font_size_lg(v);
    }

    // Font weights
    if let Some(v) = num("fontWeight.normal") {
        tg.set_font_weight_normal(v as i32);
    }
    if let Some(v) = num("fontWeight.medium") {
        tg.set_font_weight_medium(v as i32);
    }
    if let Some(v) = num("fontWeight.semibold") {
        tg.set_font_weight_semibold(v as i32);
    }
    if let Some(v) = num("fontWeight.bold") {
        tg.set_font_weight_bold(v as i32);
    }

    // Border widths
    if let Some(v) = num("borderWidth.none") {
        tg.set_border_width_none(v);
    }
    if let Some(v) = num("borderWidth.sm") {
        tg.set_border_width_sm(v);
    }
    if let Some(v) = num("borderWidth.focus") {
        tg.set_border_width_focus(v);
    }

    // Control sizing (row / touch-target heights)
    if let Some(v) = num("controlSize.sm") {
        tg.set_control_size_sm(v);
    }
    if let Some(v) = num("controlSize.md") {
        tg.set_control_size_md(v);
    }
    if let Some(v) = num("controlSize.lg") {
        tg.set_control_size_lg(v);
    }

    // Choice control sizing
    if let Some(v) = num("choiceControlSize.sm") {
        tg.set_choice_control_size_sm(v);
    }
    if let Some(v) = num("choiceControlSize.md") {
        tg.set_choice_control_size_md(v);
    }
    if let Some(v) = num("choiceControlSize.lg") {
        tg.set_choice_control_size_lg(v);
    }

    // Switch sizing
    if let Some(v) = num("switchSize.sm") {
        tg.set_switch_size_sm(v);
    }
    if let Some(v) = num("switchSize.md") {
        tg.set_switch_size_md(v);
    }
    if let Some(v) = num("switchSize.lg") {
        tg.set_switch_size_lg(v);
    }

    // Icon sizing
    if let Some(v) = num("iconSize.sm") {
        tg.set_icon_size_sm(v);
    }
    if let Some(v) = num("iconSize.md") {
        tg.set_icon_size_md(v);
    }
    if let Some(v) = num("iconSize.lg") {
        tg.set_icon_size_lg(v);
    }

    // Corner radius
    if let Some(v) = num("radius.sm") {
        tg.set_radius_sm(v);
    }
    if let Some(v) = num("radius.md") {
        tg.set_radius_md(v);
    }
    if let Some(v) = num("radius.lg") {
        tg.set_radius_lg(v);
    }
    if let Some(v) = num("radius.default") {
        tg.set_radius_default(v);
    }
    if let Some(v) = num("radius.full") {
        tg.set_radius_full(v);
    }

    // Control radius (pill controls)
    if let Some(v) = num("controlRadius.sm") {
        tg.set_control_radius_sm(v);
    }
    if let Some(v) = num("controlRadius.md") {
        tg.set_control_radius_md(v);
    }
    if let Some(v) = num("controlRadius.lg") {
        tg.set_control_radius_lg(v);
    }

    // Control inline padding
    if let Some(v) = num("controlPaddingInline.sm") {
        tg.set_control_padding_inline_sm(v);
    }
    if let Some(v) = num("controlPaddingInline.md") {
        tg.set_control_padding_inline_md(v);
    }
    if let Some(v) = num("controlPaddingInline.lg") {
        tg.set_control_padding_inline_lg(v);
    }

    // Spacing scale
    if let Some(v) = num("spacing.xs") {
        tg.set_spacing_xs(v);
    }
    if let Some(v) = num("spacing.sm") {
        tg.set_spacing_sm(v);
    }
    if let Some(v) = num("spacing.md") {
        tg.set_spacing_md(v);
    }
    if let Some(v) = num("spacing.lg") {
        tg.set_spacing_lg(v);
    }
    if let Some(v) = num("spacing.xl") {
        tg.set_spacing_xl(v);
    }

    // Full color tokens (for per-component variant styling, e.g. button).
    if let Some(v) = col("color.primary") {
        tg.set_color_primary(v);
    }
    if let Some(v) = col("color.primary.light") {
        tg.set_color_primary_light(v);
    }
    if let Some(v) = col("color.primary.dark") {
        tg.set_color_primary_dark(v);
    }
    if let Some(v) = col("color.secondary") {
        tg.set_color_secondary(v);
    }
    if let Some(v) = col("color.secondary.dark") {
        tg.set_color_secondary_dark(v);
    }
    if let Some(v) = col("color.foreground") {
        tg.set_color_foreground(v);
    }
    if let Some(v) = col("color.foreground.light") {
        tg.set_color_foreground_light(v);
    }
    if let Some(v) = col("color.muted") {
        tg.set_color_muted(v);
    }
    if let Some(v) = col("color.border") {
        tg.set_color_border(v);
    }
    if let Some(v) = col("color.surface") {
        tg.set_color_surface(v);
    }
    if let Some(v) = col("color.background") {
        tg.set_color_background(v);
    }
    if let Some(v) = col("color.danger") {
        tg.set_color_danger(v);
    }
    if let Some(v) = col("color.danger.light") {
        tg.set_color_danger_light(v);
    }
    if let Some(v) = col("color.danger.dark") {
        tg.set_color_danger_dark(v);
    }
    if let Some(v) = col("color.success") {
        tg.set_color_success(v);
    }
    if let Some(v) = col("color.warning") {
        tg.set_color_warning(v);
    }
    if let Some(v) = col("color.info") {
        tg.set_color_info(v);
    }
    if let Some(v) = col("color.white") {
        tg.set_color_white(v);
    }
    if let Some(v) = col("color.transparent") {
        tg.set_color_transparent(v);
    }
}

fn resolve_number_token_from_app(app: &AppWindow, key: &str) -> Option<f32> {
    match key {
        "fontSize.xs" => Some(app.get_token_font_size_xs()),
        "fontSize.caption" => Some(app.get_token_font_size_caption()),
        "fontSize.sm" => Some(app.get_token_font_size_sm()),
        "fontSize.md" => Some(app.get_token_font_size_md()),
        "fontSize.lg" => Some(app.get_token_font_size_lg()),
        "fontSize.helper" => Some(app.get_token_font_size_helper()),
        // Font weights are stored in the theme record rather than mirrored by
        // editable Slint properties, so let the token-store fallback below
        // resolve them from Base Theme.
        "fontWeight.normal" | "fontWeight.medium" | "fontWeight.semibold" | "fontWeight.bold" => None,
        "borderWidth.none" => Some(app.get_token_border_width_none()),
        "borderWidth.sm" => Some(app.get_token_border_width_sm()),
        "borderWidth.focus" => Some(app.get_token_border_width_focus()),
        "spacing.xs" => Some(app.get_token_spacing_xs()),
        "spacing.sm" => Some(app.get_token_spacing_sm()),
        "spacing.md" => Some(app.get_token_spacing_md()),
        "spacing.lg" => Some(app.get_token_spacing_lg()),
        "spacing.xl" => Some(app.get_token_spacing_xl()),
        "radius.sm" => Some(app.get_token_radius_sm()),
        "radius.default" => Some(match app.get_token_radius_default_index() {
            0 => app.get_token_radius_sm(),
            1 => app.get_token_radius_md(),
            2 => app.get_token_radius_lg(),
            _ => app.get_token_radius_full(),
        }),
        "radius.lg" => Some(app.get_token_radius_lg()),
        "radius.full" => Some(app.get_token_radius_full()),
        "controlSize.sm" => Some(app.get_token_control_size_sm()),
        "controlSize.md" => Some(app.get_token_control_size_md()),
        "controlSize.lg" => Some(app.get_token_control_size_lg()),
        "choiceControlSize.sm" => Some(app.get_token_choice_control_size_sm()),
        "choiceControlSize.md" => Some(app.get_token_choice_control_size_md()),
        "choiceControlSize.lg" => Some(app.get_token_choice_control_size_lg()),
        "switchSize.sm" => Some(app.get_token_switch_size_sm()),
        "switchSize.md" => Some(app.get_token_switch_size_md()),
        "switchSize.lg" => Some(app.get_token_switch_size_lg()),
        "iconSize.sm" => Some(app.get_token_icon_size_sm()),
        "iconSize.md" => Some(app.get_token_icon_size_md()),
        "iconSize.lg" => Some(app.get_token_icon_size_lg()),
        "controlRadius.sm" => Some(app.get_token_control_radius_sm()),
        "controlRadius.md" => Some(app.get_token_control_radius_md()),
        "controlRadius.lg" => Some(app.get_token_control_radius_lg()),
        "controlPaddingInline.sm" => Some(app.get_token_control_padding_inline_sm()),
        "controlPaddingInline.md" => Some(app.get_token_control_padding_inline_md()),
        "controlPaddingInline.lg" => Some(app.get_token_control_padding_inline_lg()),
        _ => None,
    }
}

fn resolve_font_token_from_app(app: &AppWindow, key: &str) -> Option<String> {
    match key {
        "font.primary" => Some(app.get_token_font_primary().to_string()),
        "font.secondary" => Some(app.get_token_font_secondary().to_string()),
        "font.tertiary" => Some(app.get_token_font_tertiary().to_string()),
        _ => None,
    }
}

fn resolve_color_token_from_app(app: &AppWindow, key: &str) -> Option<slint::Color> {
    let is_dark = app.get_dark_mode();
    let primary = if is_dark { app.get_dark_token_primary() } else { app.get_light_token_primary() };
    let primary_pressed =
        if is_dark { app.get_dark_token_primary_pressed() } else { app.get_light_token_primary_pressed() };
    let secondary = if is_dark { app.get_dark_token_secondary() } else { app.get_light_token_secondary() };
    let danger = if is_dark { app.get_dark_token_danger() } else { app.get_light_token_danger() };
    let surface = if is_dark { app.get_dark_token_surface() } else { app.get_light_token_surface() };
    let background = if is_dark { app.get_dark_token_background() } else { app.get_light_token_background() };
    let foreground = if is_dark { app.get_dark_token_text() } else { app.get_light_token_text() };
    let muted = if is_dark { app.get_dark_token_text_muted() } else { app.get_light_token_text_muted() };

    let primary_token = slint_color_to_token(primary);
    let secondary_token = slint_color_to_token(secondary);
    let danger_token = slint_color_to_token(danger);

    match key {
        "color.primary" => Some(primary),
        "color.primary.light" => Some(if is_dark {
            primary_token.lighten(0.12).to_slint()
        } else {
            primary_token.lighten(0.08).to_slint()
        }),
        "color.primary.dark" | "color.primary-pressed" => Some(primary_pressed),
        "color.secondary" => Some(secondary),
        "color.secondary.dark" => Some(if is_dark {
            secondary_token.lighten(0.14).to_slint()
        } else {
            secondary_token.darken(0.1).to_slint()
        }),
        "color.danger" => Some(danger),
        "color.danger.light" => Some(if is_dark {
            danger_token.lighten(0.08).to_slint()
        } else {
            danger_token.lighten(0.1).to_slint()
        }),
        "color.danger.dark" => Some(danger_token.darken(0.12).to_slint()),
        "color.foreground" => Some(foreground),
        "color.foreground.light" => Some(slint_color_to_token(foreground).with_alpha(0.1).to_slint()),
        "color.surface" => Some(surface),
        "color.background" => Some(background),
        "color.muted" | "color.text-muted" => Some(muted),
        "color.border" => Some(if is_dark { secondary } else { secondary_token.darken(0.08).to_slint() }),
        "color.transparent" => Some(slint::Color::from_argb_u8(0, 0, 0, 0)),
        "color.white" => Some(slint::Color::from_rgb_u8(255, 255, 255)),
        "color.success" => Some(if is_dark {
            slint::Color::from_rgb_u8(74, 222, 128)
        } else {
            slint::Color::from_rgb_u8(21, 128, 61)
        }),
        "color.warning" => Some(if is_dark {
            slint::Color::from_rgb_u8(251, 191, 36)
        } else {
            slint::Color::from_rgb_u8(180, 83, 9)
        }),
        _ => None,
    }
}

fn resolve_property_value_from_app(app: &AppWindow, value: &PropertyValue) -> PropertyValue {
    match value {
        PropertyValue::Token(key) => {
            if key.starts_with("color.") {
                resolve_color_token_from_app(app, key).map(PropertyValue::Color).unwrap_or_else(|| {
                    PropertyValue::Color(if app.get_dark_mode() {
                        app.get_dark_token_text()
                    } else {
                        app.get_light_token_text()
                    })
                })
            } else if key.starts_with("font.") {
                resolve_font_token_from_app(app, key)
                    .map(PropertyValue::String)
                    .unwrap_or_else(|| PropertyValue::String(String::new()))
            } else {
                resolve_number_token_from_app(app, key)
                    .map(PropertyValue::Float)
                    .unwrap_or(PropertyValue::Float(0.0))
            }
        }
        _ => value.clone(),
    }
}

fn token_store_from_theme(theme: &TokenTheme) -> TokenStore {
    use crate::plugin::TokenValue;

    let mut categories = HashMap::new();
    let primary_light = slint_color_to_token(theme.light_primary).lighten(0.2).to_slint();
    let secondary_dark = slint_color_to_token(theme.light_secondary).darken(0.1).to_slint();
    let danger_light = slint_color_to_token(theme.light_danger).lighten(0.15).to_slint();
    let danger_dark = slint_color_to_token(theme.light_danger).darken(0.1).to_slint();
    let foreground_light = slint_color_to_token(theme.light_text).with_alpha(0.1).to_slint();

    let mut color = HashMap::new();
    color.insert(
        "primary".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(theme.light_primary))),
    );
    color.insert(
        "primary.light".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(primary_light))),
    );
    color.insert(
        "primary.dark".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(theme.light_primary_pressed))),
    );
    color.insert(
        "secondary".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(theme.light_secondary))),
    );
    color.insert(
        "secondary.dark".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(secondary_dark))),
    );
    color.insert(
        "danger".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(theme.light_danger))),
    );
    color.insert(
        "surface".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(theme.light_surface))),
    );
    color.insert(
        "background".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(theme.light_background))),
    );
    color.insert(
        "foreground".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(theme.light_text))),
    );
    color.insert(
        "foreground.light".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(foreground_light))),
    );
    color.insert(
        "danger.light".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(danger_light))),
    );
    color
        .insert("danger.dark".to_string(), TokenValue::String(format!("#{}", format_hex_color(danger_dark))));
    color.insert("success".to_string(), TokenValue::String("#16a34a".to_string()));
    color.insert("warning".to_string(), TokenValue::String("#d97706".to_string()));
    color.insert("transparent".to_string(), TokenValue::String("#00000000".to_string()));
    color.insert("white".to_string(), TokenValue::String("#ffffff".to_string()));
    color.insert(
        "muted".to_string(),
        TokenValue::String(format!("#{}", format_hex_color(theme.light_text_muted))),
    );
    color.insert("border".to_string(), TokenValue::String("#e0e0e0".to_string()));
    categories.insert("color".to_string(), color);

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
        "borderWidth".to_string(),
        HashMap::from([
            ("none".to_string(), TokenValue::Number(theme.border_width_none as f64)),
            ("sm".to_string(), TokenValue::Number(theme.border_width_sm as f64)),
            ("focus".to_string(), TokenValue::Number(theme.border_width_focus as f64)),
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
            ("normal".to_string(), TokenValue::Int(theme.font_weight_normal as i64)),
            ("medium".to_string(), TokenValue::Int(theme.font_weight_medium as i64)),
            ("semibold".to_string(), TokenValue::Int(theme.font_weight_semibold as i64)),
            ("bold".to_string(), TokenValue::Int(theme.font_weight_bold as i64)),
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
    let component_plugins: HashMap<String, PluginDefinition> =
        loaded_plugins.iter().map(|(spec, plugin)| (spec.key.to_string(), plugin.clone())).collect();
    let mut record = ThemeRecord {
        meta: ThemeMeta { name: name.to_string(), is_builtin, parent_name: parent_name.map(str::to_string) },
        component_themes: build_component_theme_store(loaded_plugins, &tokens),
        token_overrides: if parent_name.is_none() { all_token_override_keys() } else { HashSet::new() },
        tokens,
    };
    reconcile_theme_component_parent_overrides(&component_plugins, &mut record);
    record
}

fn clear_component_override_ownership(record: &mut ThemeRecord) {
    for component in record.component_themes.values_mut() {
        component.clear_local_override_flags();
    }
}

fn reconcile_component_parent_size_overrides(
    component_plugins: &HashMap<String, PluginDefinition>,
    component_themes: &mut HashMap<String, ComponentThemeData>,
) {
    let mut inherited_props = Vec::new();
    for (component_key, data) in component_themes.iter() {
        let Some(parent_component) = data.parent_key.as_deref() else {
            continue;
        };
        let Some((parent_key, parent)) = component_themes.iter().find(|(key, _)| {
            component_plugins.get(key.as_str()).map(|plugin| plugin.component.as_str())
                == Some(parent_component)
        }) else {
            continue;
        };
        let Some(parent_common) = parent.size_props.get(crate::plugin::DEFAULT_SIZE_KEY) else {
            continue;
        };
        let Some(child_common) = data.size_props.get(crate::plugin::DEFAULT_SIZE_KEY) else {
            continue;
        };
        for prop_name in child_common.keys() {
            if parent_common.contains_key(prop_name) {
                inherited_props.push((component_key.clone(), parent_key.clone(), prop_name.clone()));
            }
        }
    }

    for (component_key, parent_key, prop_name) in inherited_props {
        let child_value = component_themes
            .get(&component_key)
            .and_then(|data| data.size_props.get(crate::plugin::DEFAULT_SIZE_KEY))
            .and_then(|props| props.get(&prop_name))
            .cloned();
        let parent_value = component_themes
            .get(&parent_key)
            .and_then(|data| data.size_props.get(crate::plugin::DEFAULT_SIZE_KEY))
            .and_then(|props| props.get(&prop_name))
            .cloned();
        let is_overridden = match (child_value.as_ref(), parent_value.as_ref()) {
            (Some(child), Some(parent)) => !property_value_matches(child, parent),
            _ => false,
        };
        if let Some(child) = component_themes.get_mut(&component_key) {
            if is_overridden {
                child.mark_parent_override(&prop_name);
            } else {
                child.parent_overrides.remove(&prop_name);
            }
        }
    }
}

fn reconcile_component_parent_variant_overrides(
    component_plugins: &HashMap<String, PluginDefinition>,
    component_themes: &mut HashMap<String, ComponentThemeData>,
) {
    let mut updates = Vec::new();
    for (component_key, data) in component_themes.iter() {
        if data.parent_key.is_none() {
            continue;
        }
        let Some(parent) = component_parent_data(component_plugins, component_themes, data) else {
            continue;
        };
        for (variant_key, states) in &data.variant_props {
            for (state_key, props) in states {
                for (prop_name, value) in props {
                    if !data.variant_prop_is_overridden(variant_key, state_key, prop_name) {
                        continue;
                    }
                    let Some(fallback) = variant_prop_fallback_value(
                        data,
                        None,
                        Some(parent),
                        variant_key,
                        state_key,
                        prop_name,
                    ) else {
                        continue;
                    };
                    if property_value_matches(value, &fallback) {
                        updates.push((
                            component_key.clone(),
                            variant_key.clone(),
                            state_key.clone(),
                            prop_name.clone(),
                            fallback,
                        ));
                    }
                }
            }
        }
    }

    for (component_key, variant_key, state_key, prop_name, value) in updates {
        if let Some(child) = component_themes.get_mut(&component_key) {
            child.set_variant_resolved_value(&variant_key, &state_key, &prop_name, value, false);
        }
    }
}

fn cascade_component_parent_size_values(
    component_plugins: &HashMap<String, PluginDefinition>,
    component_themes: &mut HashMap<String, ComponentThemeData>,
) {
    let mut updates = Vec::new();
    for (component_key, plugin) in component_plugins {
        let Some(data) = component_themes.get(component_key) else {
            continue;
        };
        let Some(common_props) = data.size_props.get(crate::plugin::DEFAULT_SIZE_KEY) else {
            continue;
        };
        for (prop_name, value) in common_props {
            updates.push((plugin.component.clone(), prop_name.clone(), value.clone()));
        }
    }

    for (component_name, prop_name, value) in updates {
        crate::plugin::cascade_default_to_children(component_themes, &component_name, &prop_name, &value);
    }
}

fn reconcile_theme_component_parent_overrides(
    component_plugins: &HashMap<String, PluginDefinition>,
    record: &mut ThemeRecord,
) {
    cascade_component_parent_size_values(component_plugins, &mut record.component_themes);
    reconcile_component_parent_size_overrides(component_plugins, &mut record.component_themes);
    reconcile_component_parent_variant_overrides(component_plugins, &mut record.component_themes);
}

fn load_theme_record_into_ui(
    app: &AppWindow,
    theme: &ThemeRecord,
    component_plugins: &HashMap<String, PluginDefinition>,
    component_key: &str,
) {
    load_theme_record_into_ui_with_parent(app, theme, None, component_plugins, component_key);
}

fn load_theme_record_into_ui_with_parent(
    app: &AppWindow,
    theme: &ThemeRecord,
    parent_theme: Option<&ThemeRecord>,
    component_plugins: &HashMap<String, PluginDefinition>,
    component_key: &str,
) {
    push_tokens_to_ui(app, &theme.tokens);
    init_theme_global(app, &theme.tokens);
    load_theme_editor_component_with_parent(
        app,
        component_key,
        component_plugins,
        &theme.component_themes,
        parent_theme.map(|parent| &parent.component_themes),
    );
    bump_preview_version(app);
}

/// Pull token values from UI into storage
fn pull_tokens_from_ui(app: &AppWindow, tokens: &mut TokenTheme) {
    tokens.light_primary = app.get_light_token_primary();
    tokens.light_primary_pressed = app.get_light_token_primary_pressed();
    tokens.light_secondary = app.get_light_token_secondary();
    tokens.light_danger = app.get_light_token_danger();
    tokens.light_surface = app.get_light_token_surface();
    tokens.light_background = app.get_light_token_background();
    tokens.light_text = app.get_light_token_text();
    tokens.light_text_muted = app.get_light_token_text_muted();
    tokens.dark_primary = app.get_dark_token_primary();
    tokens.dark_primary_pressed = app.get_dark_token_primary_pressed();
    tokens.dark_secondary = app.get_dark_token_secondary();
    tokens.dark_danger = app.get_dark_token_danger();
    tokens.dark_surface = app.get_dark_token_surface();
    tokens.dark_background = app.get_dark_token_background();
    tokens.dark_text = app.get_dark_token_text();
    tokens.dark_text_muted = app.get_dark_token_text_muted();
    tokens.spacing_xs = app.get_token_spacing_xs();
    tokens.spacing_sm = app.get_token_spacing_sm();
    tokens.spacing_md = app.get_token_spacing_md();
    tokens.spacing_lg = app.get_token_spacing_lg();
    tokens.spacing_xl = app.get_token_spacing_xl();
    tokens.control_size_sm = app.get_token_control_size_sm();
    tokens.control_size_md = app.get_token_control_size_md();
    tokens.control_size_lg = app.get_token_control_size_lg();
    tokens.choice_control_size_sm = app.get_token_choice_control_size_sm();
    tokens.choice_control_size_md = app.get_token_choice_control_size_md();
    tokens.choice_control_size_lg = app.get_token_choice_control_size_lg();
    tokens.switch_size_sm = app.get_token_switch_size_sm();
    tokens.switch_size_md = app.get_token_switch_size_md();
    tokens.switch_size_lg = app.get_token_switch_size_lg();
    tokens.icon_size_sm = app.get_token_icon_size_sm();
    tokens.icon_size_md = app.get_token_icon_size_md();
    tokens.icon_size_lg = app.get_token_icon_size_lg();
    tokens.border_width_none = app.get_token_border_width_none();
    tokens.border_width_sm = app.get_token_border_width_sm();
    tokens.border_width_focus = app.get_token_border_width_focus();
    tokens.control_radius_sm = app.get_token_control_radius_sm();
    tokens.control_radius_md = app.get_token_control_radius_md();
    tokens.control_radius_lg = app.get_token_control_radius_lg();
    tokens.control_padding_inline_sm = app.get_token_control_padding_inline_sm();
    tokens.control_padding_inline_md = app.get_token_control_padding_inline_md();
    tokens.control_padding_inline_lg = app.get_token_control_padding_inline_lg();
    tokens.font_size_xs = app.get_token_font_size_xs();
    tokens.font_size_caption = app.get_token_font_size_caption();
    tokens.font_size_sm = app.get_token_font_size_sm();
    tokens.font_size_md = app.get_token_font_size_md();
    tokens.font_size_lg = app.get_token_font_size_lg();
    tokens.font_size_helper = app.get_token_font_size_helper();
    tokens.font_primary = app.get_token_font_primary().to_string();
    tokens.font_secondary = app.get_token_font_secondary().to_string();
    tokens.font_tertiary = app.get_token_font_tertiary().to_string();
    tokens.radius_sm = app.get_token_radius_sm();
    tokens.radius_md = app.get_token_radius_md();
    tokens.radius_lg = app.get_token_radius_lg();
    tokens.radius_full = app.get_token_radius_full();
    tokens.radius_default_key =
        radius_default_key_from_index(app.get_token_radius_default_index()).to_string();
}

/// Push token values from storage to UI
fn push_tokens_to_ui(app: &AppWindow, tokens: &TokenTheme) {
    app.set_theme_font_family(tokens.font_primary.clone().into());
    app.set_light_token_primary(tokens.light_primary);
    app.set_light_token_primary_pressed(tokens.light_primary_pressed);
    app.set_light_token_secondary(tokens.light_secondary);
    app.set_light_token_danger(tokens.light_danger);
    app.set_light_token_surface(tokens.light_surface);
    app.set_light_token_background(tokens.light_background);
    app.set_light_token_text(tokens.light_text);
    app.set_light_token_text_muted(tokens.light_text_muted);
    app.set_dark_token_primary(tokens.dark_primary);
    app.set_dark_token_primary_pressed(tokens.dark_primary_pressed);
    app.set_dark_token_secondary(tokens.dark_secondary);
    app.set_dark_token_danger(tokens.dark_danger);
    app.set_dark_token_surface(tokens.dark_surface);
    app.set_dark_token_background(tokens.dark_background);
    app.set_dark_token_text(tokens.dark_text);
    app.set_dark_token_text_muted(tokens.dark_text_muted);
    app.set_token_spacing_xs(tokens.spacing_xs);
    app.set_token_spacing_sm(tokens.spacing_sm);
    app.set_token_spacing_md(tokens.spacing_md);
    app.set_token_spacing_lg(tokens.spacing_lg);
    app.set_token_spacing_xl(tokens.spacing_xl);
    app.set_token_control_size_sm(tokens.control_size_sm);
    app.set_token_control_size_md(tokens.control_size_md);
    app.set_token_control_size_lg(tokens.control_size_lg);
    app.set_token_choice_control_size_sm(tokens.choice_control_size_sm);
    app.set_token_choice_control_size_md(tokens.choice_control_size_md);
    app.set_token_choice_control_size_lg(tokens.choice_control_size_lg);
    app.set_token_switch_size_sm(tokens.switch_size_sm);
    app.set_token_switch_size_md(tokens.switch_size_md);
    app.set_token_switch_size_lg(tokens.switch_size_lg);
    app.set_token_icon_size_sm(tokens.icon_size_sm);
    app.set_token_icon_size_md(tokens.icon_size_md);
    app.set_token_icon_size_lg(tokens.icon_size_lg);
    app.set_token_border_width_none(tokens.border_width_none);
    app.set_token_border_width_sm(tokens.border_width_sm);
    app.set_token_border_width_focus(tokens.border_width_focus);
    app.set_token_control_radius_sm(tokens.control_radius_sm);
    app.set_token_control_radius_md(tokens.control_radius_md);
    app.set_token_control_radius_lg(tokens.control_radius_lg);
    app.set_token_control_padding_inline_sm(tokens.control_padding_inline_sm);
    app.set_token_control_padding_inline_md(tokens.control_padding_inline_md);
    app.set_token_control_padding_inline_lg(tokens.control_padding_inline_lg);
    app.set_token_font_size_xs(tokens.font_size_xs);
    app.set_token_font_size_caption(tokens.font_size_caption);
    app.set_token_font_size_sm(tokens.font_size_sm);
    app.set_token_font_size_md(tokens.font_size_md);
    app.set_token_font_size_lg(tokens.font_size_lg);
    app.set_token_font_size_helper(tokens.font_size_helper);
    app.set_token_font_primary(tokens.font_primary.clone().into());
    app.set_token_font_secondary(tokens.font_secondary.clone().into());
    app.set_token_font_tertiary(tokens.font_tertiary.clone().into());
    app.set_token_font_primary_index(actual_font_family_index_from_value(&tokens.font_primary));
    app.set_token_font_secondary_index(actual_font_family_index_from_value(&tokens.font_secondary));
    app.set_token_font_tertiary_index(actual_font_family_index_from_value(&tokens.font_tertiary));
    app.set_token_radius_sm(tokens.radius_sm);
    app.set_token_radius_md(tokens.radius_md);
    app.set_token_radius_lg(tokens.radius_lg);
    app.set_token_radius_full(tokens.radius_full);
    app.set_token_radius_default_index(radius_default_index_from_key(&tokens.radius_default_key));
}

// ============================================================================
// Plugin-based Dynamic UI Functions
// ============================================================================

/// Capitalize the first letter of a string
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Populate the variant list model from plugin definition
fn populate_variant_list(app: &AppWindow, plugin: &PluginDefinition) {
    let variants: Vec<VariantInfo> = plugin
        .variants
        .iter()
        .map(|v| VariantInfo { name: capitalize(v).into(), key: v.clone().into() })
        .collect();
    app.set_variant_list(slint::ModelRc::new(slint::VecModel::from(variants)));

    let variant_names: Vec<slint::SharedString> =
        plugin.variants.iter().map(|v| capitalize(v).into()).collect();
    app.set_variant_name_list(slint::ModelRc::new(slint::VecModel::from(variant_names)));
}

/// Populate the state list model from plugin definition
fn populate_state_list(app: &AppWindow, plugin: &PluginDefinition) {
    let states: Vec<StateInfo> = plugin
        .states
        .iter()
        .map(|s| StateInfo { name: capitalize(s).into(), key: s.clone().into() })
        .collect();
    app.set_state_list(slint::ModelRc::new(slint::VecModel::from(states)));

    let state_names: Vec<slint::SharedString> = plugin.states.iter().map(|s| capitalize(s).into()).collect();
    app.set_state_name_list(slint::ModelRc::new(slint::VecModel::from(state_names)));
}

/// True for every component: the size editor exposes a virtual Common base
/// row (index 0) ahead of the concrete sizes. Button is no longer special — it
/// uses the same generic ComponentThemeData path as every other component.
fn component_supports_default_size(_plugin: &PluginDefinition) -> bool { true }

/// Populate the size list model from a plugin. For components that support it
/// (everything except Button), index 0 is the virtual Common base (the
/// shared value for every size); indices 1.. are the concrete sizes. Editing
/// the Common base cascades into every concrete size that is currently
/// inheriting (no per-size override).
fn populate_size_list(app: &AppWindow, plugin: &PluginDefinition) {
    let supports_default = component_supports_default_size(plugin);
    let mut sizes: Vec<SizeInfo> = Vec::new();
    let mut size_names: Vec<slint::SharedString> = Vec::new();
    if supports_default {
        sizes.push(SizeInfo { name: "Common".into(), key: crate::plugin::DEFAULT_SIZE_KEY.into() });
        size_names.push("Common".into());
    }
    let matrix_sizes: Vec<SizeInfo> =
        plugin.sizes.iter().map(|s| SizeInfo { name: capitalize(s).into(), key: s.clone().into() }).collect();
    sizes.extend(matrix_sizes.iter().cloned());
    size_names.extend(plugin.sizes.iter().map(|s| capitalize(s).into()));
    app.set_size_list(slint::ModelRc::new(slint::VecModel::from(sizes)));
    app.set_size_name_list(slint::ModelRc::new(slint::VecModel::from(size_names)));
    app.set_matrix_size_list(slint::ModelRc::new(slint::VecModel::from(matrix_sizes)));
    app.set_matrix_size_index_offset(if supports_default { 1 } else { 0 });
}

/// Map a UI size index to a `ComponentThemeData::size_props` key. For plugins
/// that expose the Common row, UI index 0 → the Common key, 1..N → plugin.sizes[idx-1].
/// For Button (no Common row), the mapping is the identity.
fn ui_size_index_to_key<'a>(plugin: &'a PluginDefinition, ui_index: i32) -> Option<&'a str> {
    if !component_supports_default_size(plugin) {
        return plugin.sizes.get(ui_index.max(0) as usize).map(|s| s.as_str());
    }
    if ui_index <= 0 {
        Some(crate::plugin::DEFAULT_SIZE_KEY)
    } else {
        plugin.sizes.get((ui_index - 1) as usize).map(|s| s.as_str())
    }
}

/// Populate the variant property definitions from plugin
fn populate_variant_prop_defs(app: &AppWindow, plugin: &PluginDefinition) {
    let props: Vec<PropertyDef> = plugin
        .variant_props
        .iter()
        .map(|p| PropertyDef {
            name: p.name.clone().into(),
            display_name: p.display_name().to_string().into(),
            prop_type: p.prop_type.clone().into(),
            min_value: p.min_value(),
            max_value: p.max_value(),
            step: p.step_value(),
        })
        .collect();
    app.set_variant_prop_defs(slint::ModelRc::new(slint::VecModel::from(props)));
}

/// Populate the size property definitions from plugin
fn populate_size_prop_defs(app: &AppWindow, plugin: &PluginDefinition) {
    let props: Vec<PropertyDef> = plugin
        .size_props
        .iter()
        .map(|p| PropertyDef {
            name: p.name.clone().into(),
            display_name: p.display_name().to_string().into(),
            prop_type: p.prop_type.clone().into(),
            min_value: p.min_value(),
            max_value: p.max_value(),
            step: p.step_value(),
        })
        .collect();
    app.set_size_prop_defs(slint::ModelRc::new(slint::VecModel::from(props)));
}

fn make_property_value_data(
    value: &PropertyValue,
    is_overridden: bool,
    is_token_mode: bool,
    selected_token: i32,
    selected_actual_index: i32,
) -> PropertyValueData {
    let string_value = match value {
        PropertyValue::String(s) => s.clone().into(),
        _ => "".into(),
    };

    PropertyValueData {
        color_value: value.as_color().unwrap_or_else(|| slint::Color::from_rgb_u8(0, 0, 0)),
        float_value: value.as_float().unwrap_or(0.0),
        int_value: value.as_int().unwrap_or(0),
        bool_value: matches!(value, PropertyValue::Bool(true)),
        string_value,
        is_overridden,
        is_token_mode,
        selected_token,
        selected_actual_index,
    }
}

fn property_value_to_slint(app: &AppWindow, value: &PropertyValue) -> PropertyValueData {
    property_value_to_slint_with_override(app, value, true)
}

fn property_value_to_slint_with_override(
    app: &AppWindow,
    value: &PropertyValue,
    is_overridden: bool,
) -> PropertyValueData {
    if let Some(token_key) = value.token_key() {
        let resolved = resolve_property_value_from_app(app, value);
        let selected_token = if token_key.starts_with("color.") {
            color_token_index_from_key(Some(token_key))
        } else if token_key.starts_with("font.") {
            font_token_index_from_key(Some(token_key))
        } else {
            number_token_index_from_key(Some(token_key))
        };
        let selected_actual_index = match &resolved {
            PropertyValue::String(text) => actual_font_family_index_from_value(text),
            _ => 0,
        };
        return make_property_value_data(
            &resolved,
            is_overridden,
            true,
            selected_token,
            selected_actual_index,
        );
    }

    make_property_value_data(value, is_overridden, false, 0, 0)
}

fn default_property_value_data() -> PropertyValueData {
    PropertyValueData {
        color_value: slint::Color::from_argb_u8(0, 0, 0, 0),
        float_value: 0.0,
        int_value: 0,
        bool_value: false,
        string_value: "".into(),
        is_overridden: false,
        is_token_mode: false,
        selected_token: 0,
        selected_actual_index: 0,
    }
}

fn preview_variant_property_value(
    app: &AppWindow,
    component_themes: &HashMap<String, ComponentThemeData>,
    component_key: &str,
    variant: &str,
    state: &str,
    prop_name: &str,
) -> PropertyValueData {
    component_themes
        .get(component_key)
        .and_then(|theme| theme.variant_props.get(variant))
        .and_then(|states| states.get(state))
        .and_then(|props| props.get(prop_name))
        .map(|value| property_value_to_slint(app, value))
        .unwrap_or_else(default_property_value_data)
}

fn preview_size_property_value(
    app: &AppWindow,
    component_themes: &HashMap<String, ComponentThemeData>,
    component_key: &str,
    size: &str,
    prop_name: &str,
) -> PropertyValueData {
    component_themes
        .get(component_key)
        .and_then(|theme| theme.size_props.get(size))
        .and_then(|props| props.get(prop_name))
        .map(|value| property_value_to_slint(app, value))
        .unwrap_or_else(default_property_value_data)
}

fn update_generic_variant_values(
    app: &AppWindow,
    plugin: &PluginDefinition,
    data: &ComponentThemeData,
    variant: &str,
) {
    update_generic_variant_values_with_parent(app, plugin, data, None, None, variant);
}

fn component_parent_data<'a>(
    component_plugins: &HashMap<String, PluginDefinition>,
    component_themes: &'a HashMap<String, ComponentThemeData>,
    data: &ComponentThemeData,
) -> Option<&'a ComponentThemeData> {
    let parent_component = data.parent_key.as_deref()?;
    component_themes.iter().find_map(|(key, theme_data)| {
        component_plugins
            .get(key.as_str())
            .filter(|plugin| plugin.component == parent_component)
            .map(|_| theme_data)
    })
}

fn variant_prop_value<'a>(
    data: &'a ComponentThemeData,
    variant: &str,
    state: &str,
    prop_name: &str,
) -> Option<&'a PropertyValue> {
    data.variant_props
        .get(variant)
        .and_then(|states| states.get(state))
        .and_then(|props| props.get(prop_name))
}

fn variant_prop_fallback_value(
    data: &ComponentThemeData,
    theme_parent_data: Option<&ComponentThemeData>,
    component_parent_data: Option<&ComponentThemeData>,
    variant: &str,
    state: &str,
    prop_name: &str,
) -> Option<PropertyValue> {
    if state != crate::plugin::NORMAL_STATE_KEY {
        let normal_value =
            variant_prop_value(data, variant, crate::plugin::NORMAL_STATE_KEY, prop_name).cloned();
        if let Some(normal_value) = normal_value.as_ref() {
            if variant_prop_is_overridden_for_ui(
                data,
                theme_parent_data,
                component_parent_data,
                variant,
                crate::plugin::NORMAL_STATE_KEY,
                prop_name,
                normal_value,
            ) {
                return Some(normal_value.clone());
            }
        }

        if let Some(parent_value) =
            component_parent_data.and_then(|parent| variant_prop_value(parent, variant, state, prop_name))
        {
            return Some(parent_value.clone());
        }

        if let Some(parent_value) =
            theme_parent_data.and_then(|parent| variant_prop_value(parent, variant, state, prop_name))
        {
            return Some(parent_value.clone());
        }

        return normal_value;
    }

    if !data.variant_is_common(variant) {
        let common_normal_value = data
            .common_variant_key()
            .and_then(|common| variant_prop_value(data, common, crate::plugin::NORMAL_STATE_KEY, prop_name))
            .cloned();
        if let (Some(common_variant), Some(common_normal_value)) =
            (data.common_variant_key(), common_normal_value.as_ref())
        {
            if variant_prop_is_overridden_for_ui(
                data,
                theme_parent_data,
                component_parent_data,
                common_variant,
                crate::plugin::NORMAL_STATE_KEY,
                prop_name,
                common_normal_value,
            ) {
                return Some(common_normal_value.clone());
            }
        }

        if let Some(parent_value) =
            component_parent_data.and_then(|parent| variant_prop_value(parent, variant, state, prop_name))
        {
            return Some(parent_value.clone());
        }

        if let Some(parent_value) =
            theme_parent_data.and_then(|parent| variant_prop_value(parent, variant, state, prop_name))
        {
            return Some(parent_value.clone());
        }

        return common_normal_value;
    }

    component_parent_data
        .and_then(|parent| variant_prop_value(parent, variant, state, prop_name))
        .or_else(|| {
            theme_parent_data.and_then(|parent| variant_prop_value(parent, variant, state, prop_name))
        })
        .cloned()
}

fn variant_prop_is_overridden_for_ui(
    data: &ComponentThemeData,
    theme_parent_data: Option<&ComponentThemeData>,
    component_parent_data: Option<&ComponentThemeData>,
    variant: &str,
    state: &str,
    prop_name: &str,
    value: &PropertyValue,
) -> bool {
    if let Some(fallback) =
        variant_prop_fallback_value(data, theme_parent_data, component_parent_data, variant, state, prop_name)
    {
        return !property_value_matches(value, &fallback);
    }

    if data.variant_state_is_root(variant, state) {
        theme_parent_data.is_none() && component_parent_data.is_none()
    } else {
        data.variant_prop_is_overridden(variant, state, prop_name)
    }
}

fn update_generic_variant_values_with_parent(
    app: &AppWindow,
    plugin: &PluginDefinition,
    data: &ComponentThemeData,
    theme_parent_data: Option<&ComponentThemeData>,
    component_parent_data: Option<&ComponentThemeData>,
    variant: &str,
) {
    let mut values = Vec::new();
    for state in &plugin.states {
        values.extend(
            plugin.variant_props.iter().zip(data.get_variant_state_values(variant, state).iter()).map(
                |(prop, value)| {
                    let is_overridden = variant_prop_is_overridden_for_ui(
                        data,
                        theme_parent_data,
                        component_parent_data,
                        variant,
                        state,
                        &prop.name,
                        value,
                    );
                    property_value_to_slint_with_override(app, value, is_overridden)
                },
            ),
        );
    }

    app.set_current_variant_values(slint::ModelRc::new(slint::VecModel::from(values)));
    app.set_variant_values_version(app.get_variant_values_version() + 1);
}

fn update_generic_size_values(
    app: &AppWindow,
    plugin: &PluginDefinition,
    data: &ComponentThemeData,
    size: &str,
) {
    update_generic_size_values_with_parent(app, plugin, data, None, None, size);
}

fn size_prop_is_overridden_for_ui(
    data: &ComponentThemeData,
    theme_parent_data: Option<&ComponentThemeData>,
    component_parent_data: Option<&ComponentThemeData>,
    size: &str,
    prop_name: &str,
    value: &PropertyValue,
) -> bool {
    if size == crate::plugin::DEFAULT_SIZE_KEY {
        let has_component_parent = component_parent_data.is_some();
        if let Some(component_parent_value) = component_parent_data
            .and_then(|parent| parent.size_props.get(crate::plugin::DEFAULT_SIZE_KEY))
            .and_then(|props| props.get(prop_name))
        {
            return !property_value_matches(value, component_parent_value);
        }

        theme_parent_data
            .and_then(|parent| parent.size_props.get(crate::plugin::DEFAULT_SIZE_KEY))
            .and_then(|props| props.get(prop_name))
            .map(|parent| parent != value)
            .unwrap_or_else(|| {
                if has_component_parent {
                    true
                } else if data.parent_key.is_some() {
                    data.parent_prop_is_overridden(prop_name)
                } else {
                    true
                }
            })
    } else {
        data.size_prop_is_overridden(size, prop_name)
    }
}

fn update_generic_size_values_with_parent(
    app: &AppWindow,
    plugin: &PluginDefinition,
    data: &ComponentThemeData,
    theme_parent_data: Option<&ComponentThemeData>,
    component_parent_data: Option<&ComponentThemeData>,
    size: &str,
) {
    // Common view: a child component (`parent_key` set) shows each prop as
    //   overridden iff the user has locally edited the prop's Common value (the
    //   `parent_overrides` set tracks that); other props are inherited live
    //   from the parent and render greyed.
    // Common view on a top-level component (no parent): every prop is shown
    //   as the source-of-truth (always overridden / normal styling).
    // Concrete-size view: a prop is overridden iff it actually overrides the
    //   component's Common base.
    let has_parent = data.parent_key.is_some();
    let values: Vec<PropertyValueData> = plugin
        .size_props
        .iter()
        .zip(data.get_size_values(size).iter())
        .map(|(prop, value)| {
            let is_overridden = size_prop_is_overridden_for_ui(
                data,
                theme_parent_data,
                component_parent_data,
                size,
                &prop.name,
                value,
            );
            property_value_to_slint_with_override(app, value, is_overridden)
        })
        .collect();

    app.set_current_size_values(slint::ModelRc::new(slint::VecModel::from(values)));
    app.set_size_values_version(app.get_size_values_version() + 1);
    // Tell the UI whether the active component has a parent so the Common-row
    // reset (↺) can be enabled.
    app.set_theme_component_has_parent(has_parent);
}

fn property_value_from_ui(prop_def: &plugin::PropDefinition, value: &PropertyValueData) -> PropertyValue {
    match prop_def.prop_type.as_str() {
        "color" => {
            if value.is_token_mode {
                PropertyValue::Token(color_token_key_from_index(value.selected_token).to_string())
            } else {
                PropertyValue::Color(value.color_value)
            }
        }
        "int" => {
            if value.is_token_mode {
                PropertyValue::Token(number_token_key_from_index(value.selected_token).to_string())
            } else {
                PropertyValue::Int(value.int_value)
            }
        }
        "bool" => PropertyValue::Bool(value.bool_value),
        "string" => {
            if value.is_token_mode {
                PropertyValue::Token(font_token_key_from_index(value.selected_token).to_string())
            } else {
                PropertyValue::String(value.string_value.to_string())
            }
        }
        _ => {
            if value.is_token_mode {
                PropertyValue::Token(number_token_key_from_index(value.selected_token).to_string())
            } else {
                PropertyValue::Float(value.float_value)
            }
        }
    }
}

fn populate_plugin_ui(app: &AppWindow, plugin: &PluginDefinition) {
    populate_variant_list(app, plugin);
    populate_state_list(app, plugin);
    populate_size_list(app, plugin);
    populate_variant_prop_defs(app, plugin);
    populate_size_prop_defs(app, plugin);
}

/// Remember the outgoing component's (variant, size) selection and restore the
/// incoming component's, so switching components preserves where you were
/// (first view of a component defaults to 0/0). Indices are clamped to the
/// target component's actual counts by load_theme_editor_component.
fn remember_component_selection(
    app: &AppWindow,
    selections: &RefCell<HashMap<String, (i32, i32, i32)>>,
    old_key: &str,
    new_key: &str,
) {
    if old_key != new_key {
        selections.borrow_mut().insert(
            old_key.to_string(),
            (
                app.get_theme_selected_variant_index(),
                app.get_theme_selected_size_index(),
                app.get_theme_preview_state_index(),
            ),
        );
    }
    let (variant, size, state) = selections.borrow().get(new_key).copied().unwrap_or((0, 0, 0));
    app.set_theme_selected_variant_index(variant);
    app.set_theme_selected_size_index(size);
    app.set_theme_preview_state_index(state);
}

fn load_theme_editor_component(
    app: &AppWindow,
    component_key: &str,
    component_plugins: &HashMap<String, PluginDefinition>,
    generic_component_themes: &HashMap<String, ComponentThemeData>,
) {
    load_theme_editor_component_with_parent(
        app,
        component_key,
        component_plugins,
        generic_component_themes,
        None,
    );
}

fn load_theme_editor_component_with_parent(
    app: &AppWindow,
    component_key: &str,
    component_plugins: &HashMap<String, PluginDefinition>,
    generic_component_themes: &HashMap<String, ComponentThemeData>,
    parent_component_themes: Option<&HashMap<String, ComponentThemeData>>,
) {
    let Some(plugin) = component_plugins.get(component_key) else {
        return;
    };

    populate_plugin_ui(app, plugin);
    app.set_theme_selected_component_index(theme_component_index(component_key));
    app.set_theme_selected_component_key(component_key.into());
    app.set_theme_selected_component_name(plugin.component.clone().into());

    if let Some(theme_data) = generic_component_themes.get(component_key) {
        let parent_theme_data = parent_component_themes.and_then(|themes| themes.get(component_key));
        let component_parent_theme_data =
            component_parent_data(component_plugins, generic_component_themes, theme_data);
        let variant_index = (app.get_theme_selected_variant_index().max(0) as usize)
            .min(plugin.variants.len().saturating_sub(1));
        // UI size list = [Common?, plugin.sizes...]. Clamp against that count
        // (Common is prepended for non-Button components), not plugin.sizes.
        let ui_size_count = plugin.sizes.len() + if component_supports_default_size(plugin) { 1 } else { 0 };
        let size_index =
            (app.get_theme_selected_size_index().max(0) as usize).min(ui_size_count.saturating_sub(1));

        app.set_theme_selected_variant_index(variant_index as i32);
        app.set_theme_selected_size_index(size_index as i32);
        let state_index =
            (app.get_theme_preview_state_index().max(0) as usize).min(plugin.states.len().saturating_sub(1));
        app.set_theme_preview_state_index(state_index as i32);

        if let Some(variant_key) = plugin.variants.get(variant_index) {
            update_generic_variant_values_with_parent(
                app,
                plugin,
                theme_data,
                parent_theme_data,
                component_parent_theme_data,
                variant_key,
            );
        } else {
            app.set_current_variant_values(slint::ModelRc::new(slint::VecModel::from(Vec::<
                PropertyValueData,
            >::new())));
            app.set_variant_values_version(app.get_variant_values_version() + 1);
        }
        // Use the UI→size-key mapping so UI index 0 ("Common") resolves to the
        // virtual Common base, not plugin.sizes[0].
        if let Some(size_key) = ui_size_index_to_key(plugin, size_index as i32) {
            update_generic_size_values_with_parent(
                app,
                plugin,
                theme_data,
                parent_theme_data,
                component_parent_theme_data,
                size_key,
            );
        } else {
            app.set_current_size_values(slint::ModelRc::new(slint::VecModel::from(
                Vec::<PropertyValueData>::new(),
            )));
            app.set_size_values_version(app.get_size_values_version() + 1);
        }
    }
}

fn color_hex(color: slint::Color) -> String { format!("#{}", format_hex_color(color)) }

fn property_value_to_json(value: &PropertyValue) -> Value {
    match value {
        PropertyValue::Token(key) => Value::String(key.clone()),
        PropertyValue::Color(color) => Value::String(color_hex(*color)),
        PropertyValue::Float(number) => json!(number),
        PropertyValue::Int(number) => json!(number),
        PropertyValue::Bool(flag) => json!(flag),
        PropertyValue::String(text) => json!(text),
    }
}

fn property_value_from_json(value: &Value, prop_type: &str) -> Option<PropertyValue> {
    match prop_type {
        "color" => value.as_str().map(|text| {
            if text.contains('.') && !text.starts_with('#') {
                PropertyValue::Token(text.to_string())
            } else {
                PropertyValue::Color(parse_hex_color(text))
            }
        }),
        "int" => value
            .as_i64()
            .map(|number| PropertyValue::Int(number as i32))
            .or_else(|| value.as_f64().map(|number| PropertyValue::Int(number as i32)))
            .or_else(|| {
                value.as_str().and_then(|text| {
                    if text.contains('.') && !text.starts_with('#') {
                        Some(PropertyValue::Token(text.to_string()))
                    } else {
                        None
                    }
                })
            }),
        "bool" => value.as_bool().map(PropertyValue::Bool),
        "string" => value.as_str().map(|text| {
            if text.contains('.') {
                PropertyValue::Token(text.to_string())
            } else {
                PropertyValue::String(text.to_string())
            }
        }),
        _ => value
            .as_f64()
            .map(|number| PropertyValue::Float(number as f32))
            .or_else(|| value.as_i64().map(|number| PropertyValue::Float(number as f32)))
            .or_else(|| {
                value.as_str().and_then(|text| {
                    if text.contains('.') && !text.starts_with('#') {
                        Some(PropertyValue::Token(text.to_string()))
                    } else {
                        None
                    }
                })
            }),
    }
}

fn export_token_theme_json(tokens: &TokenTheme) -> Value {
    json!({
        "colors": {
            "light": {
                "primary": color_hex(tokens.light_primary),
                "primary-pressed": color_hex(tokens.light_primary_pressed),
                "secondary": color_hex(tokens.light_secondary),
                "danger": color_hex(tokens.light_danger),
                "surface": color_hex(tokens.light_surface),
                "background": color_hex(tokens.light_background),
                "text": color_hex(tokens.light_text),
                "text-muted": color_hex(tokens.light_text_muted),
                "transparent": "#00000000",
            },
            "dark": {
                "primary": color_hex(tokens.dark_primary),
                "primary-pressed": color_hex(tokens.dark_primary_pressed),
                "secondary": color_hex(tokens.dark_secondary),
                "danger": color_hex(tokens.dark_danger),
                "surface": color_hex(tokens.dark_surface),
                "background": color_hex(tokens.dark_background),
                "text": color_hex(tokens.dark_text),
                "text-muted": color_hex(tokens.dark_text_muted),
                "transparent": "#00000000",
            }
        },
        "spacing": {
            "xs": tokens.spacing_xs,
            "sm": tokens.spacing_sm,
            "md": tokens.spacing_md,
            "lg": tokens.spacing_lg,
            "xl": tokens.spacing_xl,
        },
        "controlSize": {
            "sm": tokens.control_size_sm,
            "md": tokens.control_size_md,
            "lg": tokens.control_size_lg,
        },
        "choiceControlSize": {
            "sm": tokens.choice_control_size_sm,
            "md": tokens.choice_control_size_md,
            "lg": tokens.choice_control_size_lg,
        },
        "switchSize": {
            "sm": tokens.switch_size_sm,
            "md": tokens.switch_size_md,
            "lg": tokens.switch_size_lg,
        },
        "iconSize": {
            "sm": tokens.icon_size_sm,
            "md": tokens.icon_size_md,
            "lg": tokens.icon_size_lg,
        },
        "borderWidth": {
            "none": tokens.border_width_none,
            "sm": tokens.border_width_sm,
            "focus": tokens.border_width_focus,
        },
        "controlRadius": {
            "sm": tokens.control_radius_sm,
            "md": tokens.control_radius_md,
            "lg": tokens.control_radius_lg,
        },
        "controlPaddingInline": {
            "sm": tokens.control_padding_inline_sm,
            "md": tokens.control_padding_inline_md,
            "lg": tokens.control_padding_inline_lg,
        },
        "typography": {
            "font-size-xs": tokens.font_size_xs,
            "font-size-caption": tokens.font_size_caption,
            "font-size-helper": tokens.font_size_helper,
            "font-size-sm": tokens.font_size_sm,
            "font-size-md": tokens.font_size_md,
            "font-size-lg": tokens.font_size_lg,
            "font-primary": tokens.font_primary,
            "font-secondary": tokens.font_secondary,
            "font-tertiary": tokens.font_tertiary,
        },
        "fontWeight": {
            "normal": tokens.font_weight_normal,
            "medium": tokens.font_weight_medium,
            "semibold": tokens.font_weight_semibold,
            "bold": tokens.font_weight_bold,
        },
        "radius": {
            "sm": tokens.radius_sm,
            "md": tokens.radius_md,
            "default": format!("radius.{}", tokens.radius_default_key),
            "lg": tokens.radius_lg,
            "full": tokens.radius_full,
        }
    })
}

/// Allowed key map for token-theme JSON imports. A typo (e.g. `font.primry`)
/// emits a warning to stderr so the editor surfaces silent data loss instead of
/// pretending the import succeeded.
const TOKEN_THEME_ALLOWED_KEYS: &[(&str, &[&str])] = &[
    ("colors", &["light", "dark"]),
    (
        "colors.light",
        &[
            "primary",
            "primary-pressed",
            "secondary",
            "danger",
            "surface",
            "background",
            "text",
            "text-muted",
            "transparent",
        ],
    ),
    (
        "colors.dark",
        &[
            "primary",
            "primary-pressed",
            "secondary",
            "danger",
            "surface",
            "background",
            "text",
            "text-muted",
            "transparent",
        ],
    ),
    ("spacing", &["xs", "sm", "md", "lg", "xl"]),
    ("controlSize", &["sm", "md", "lg"]),
    ("choiceControlSize", &["sm", "md", "lg"]),
    ("switchSize", &["sm", "md", "lg"]),
    ("iconSize", &["sm", "md", "lg"]),
    ("borderWidth", &["none", "sm", "focus"]),
    ("controlRadius", &["sm", "md", "lg"]),
    ("controlPaddingInline", &["sm", "md", "lg"]),
    ("fontWeight", &["normal", "medium", "semibold", "bold"]),
    (
        "typography",
        &[
            "font-size-xs",
            "font-size-caption",
            "font-size-sm",
            "font-size-md",
            "font-size-lg",
            // Named text-role sizes (M23). Carried by SDK theme JSON and applied
            // by apps; the editor doesn't expose them for editing yet, but they
            // are valid keys, so don't warn about them.
            "font-size-title",
            "font-size-body",
            "font-size-subtitle",
            "font-size-label",
            "font-size-helper",
            "font-primary",
            "font-secondary",
            "font-tertiary",
        ],
    ),
    ("radius", &["sm", "md", "default", "lg", "full"]),
];

fn warn_unknown_token_keys(value: &Value) {
    const TOP_LEVEL: &[&str] = &[
        "colors",
        "spacing",
        "controlSize",
        "choiceControlSize",
        "switchSize",
        "iconSize",
        "borderWidth",
        "controlRadius",
        "controlPaddingInline",
        "fontWeight",
        "typography",
        "radius",
    ];

    if let Some(map) = value.as_object() {
        for key in map.keys() {
            if !TOP_LEVEL.contains(&key.as_str()) {
                eprintln!("warning: ignoring unknown token-theme key '{key}'");
            }
        }
    }

    for (path, allowed) in TOKEN_THEME_ALLOWED_KEYS {
        let section = path.split('.').fold(Some(value), |acc, segment| acc.and_then(|v| v.get(segment)));
        if let Some(Value::Object(map)) = section {
            for key in map.keys() {
                if !allowed.contains(&key.as_str()) {
                    eprintln!("warning: ignoring unknown token-theme key '{path}.{key}'");
                }
            }
        }
    }
}

fn import_token_theme_json(tokens: &mut TokenTheme, value: &Value) {
    warn_unknown_token_keys(value);

    let parse_color_field = |parent: &Value, key: &str, current: slint::Color| {
        parent.get(key).and_then(Value::as_str).map(parse_hex_color).unwrap_or(current)
    };
    let parse_float_field = |parent: &Value, key: &str, current: f32| {
        parent.get(key).and_then(Value::as_f64).map(|number| number as f32).unwrap_or(current)
    };
    let parse_int_field = |parent: &Value, key: &str, current: i32| {
        parent.get(key).and_then(Value::as_i64).map(|number| number as i32).unwrap_or(current)
    };
    let parse_string_field = |parent: &Value, key: &str, current: &str| {
        parent.get(key).and_then(Value::as_str).unwrap_or(current).to_string()
    };

    if let Some(light) = value.get("colors").and_then(|colors| colors.get("light")) {
        tokens.light_primary = parse_color_field(light, "primary", tokens.light_primary);
        tokens.light_primary_pressed =
            parse_color_field(light, "primary-pressed", tokens.light_primary_pressed);
        tokens.light_secondary = parse_color_field(light, "secondary", tokens.light_secondary);
        tokens.light_danger = parse_color_field(light, "danger", tokens.light_danger);
        tokens.light_surface = parse_color_field(light, "surface", tokens.light_surface);
        tokens.light_background = parse_color_field(light, "background", tokens.light_background);
        tokens.light_text = parse_color_field(light, "text", tokens.light_text);
        tokens.light_text_muted = parse_color_field(light, "text-muted", tokens.light_text_muted);
    }
    if let Some(dark) = value.get("colors").and_then(|colors| colors.get("dark")) {
        tokens.dark_primary = parse_color_field(dark, "primary", tokens.dark_primary);
        tokens.dark_primary_pressed = parse_color_field(dark, "primary-pressed", tokens.dark_primary_pressed);
        tokens.dark_secondary = parse_color_field(dark, "secondary", tokens.dark_secondary);
        tokens.dark_danger = parse_color_field(dark, "danger", tokens.dark_danger);
        tokens.dark_surface = parse_color_field(dark, "surface", tokens.dark_surface);
        tokens.dark_background = parse_color_field(dark, "background", tokens.dark_background);
        tokens.dark_text = parse_color_field(dark, "text", tokens.dark_text);
        tokens.dark_text_muted = parse_color_field(dark, "text-muted", tokens.dark_text_muted);
    }
    if let Some(spacing) = value.get("spacing") {
        tokens.spacing_xs = parse_float_field(spacing, "xs", tokens.spacing_xs);
        tokens.spacing_sm = parse_float_field(spacing, "sm", tokens.spacing_sm);
        tokens.spacing_md = parse_float_field(spacing, "md", tokens.spacing_md);
        tokens.spacing_lg = parse_float_field(spacing, "lg", tokens.spacing_lg);
        tokens.spacing_xl = parse_float_field(spacing, "xl", tokens.spacing_xl);
    }
    if let Some(control_size) = value.get("controlSize") {
        tokens.control_size_sm = parse_float_field(control_size, "sm", tokens.control_size_sm);
        tokens.control_size_md = parse_float_field(control_size, "md", tokens.control_size_md);
        tokens.control_size_lg = parse_float_field(control_size, "lg", tokens.control_size_lg);
    }
    if let Some(choice_control_size) = value.get("choiceControlSize") {
        tokens.choice_control_size_sm =
            parse_float_field(choice_control_size, "sm", tokens.choice_control_size_sm);
        tokens.choice_control_size_md =
            parse_float_field(choice_control_size, "md", tokens.choice_control_size_md);
        tokens.choice_control_size_lg =
            parse_float_field(choice_control_size, "lg", tokens.choice_control_size_lg);
    }
    if let Some(switch_size) = value.get("switchSize") {
        tokens.switch_size_sm = parse_float_field(switch_size, "sm", tokens.switch_size_sm);
        tokens.switch_size_md = parse_float_field(switch_size, "md", tokens.switch_size_md);
        tokens.switch_size_lg = parse_float_field(switch_size, "lg", tokens.switch_size_lg);
    }
    if let Some(icon_size) = value.get("iconSize") {
        tokens.icon_size_sm = parse_float_field(icon_size, "sm", tokens.icon_size_sm);
        tokens.icon_size_md = parse_float_field(icon_size, "md", tokens.icon_size_md);
        tokens.icon_size_lg = parse_float_field(icon_size, "lg", tokens.icon_size_lg);
    }
    if let Some(border_width) = value.get("borderWidth") {
        tokens.border_width_none = parse_float_field(border_width, "none", tokens.border_width_none);
        tokens.border_width_sm = parse_float_field(border_width, "sm", tokens.border_width_sm);
        tokens.border_width_focus = parse_float_field(border_width, "focus", tokens.border_width_focus);
    }
    if let Some(control_radius) = value.get("controlRadius") {
        tokens.control_radius_sm = parse_float_field(control_radius, "sm", tokens.control_radius_sm);
        tokens.control_radius_md = parse_float_field(control_radius, "md", tokens.control_radius_md);
        tokens.control_radius_lg = parse_float_field(control_radius, "lg", tokens.control_radius_lg);
    }
    if let Some(control_padding_inline) = value.get("controlPaddingInline") {
        tokens.control_padding_inline_sm =
            parse_float_field(control_padding_inline, "sm", tokens.control_padding_inline_sm);
        tokens.control_padding_inline_md =
            parse_float_field(control_padding_inline, "md", tokens.control_padding_inline_md);
        tokens.control_padding_inline_lg =
            parse_float_field(control_padding_inline, "lg", tokens.control_padding_inline_lg);
    }
    if let Some(font_weight) = value.get("fontWeight") {
        tokens.font_weight_normal = parse_int_field(font_weight, "normal", tokens.font_weight_normal);
        tokens.font_weight_medium = parse_int_field(font_weight, "medium", tokens.font_weight_medium);
        tokens.font_weight_semibold = parse_int_field(font_weight, "semibold", tokens.font_weight_semibold);
        tokens.font_weight_bold = parse_int_field(font_weight, "bold", tokens.font_weight_bold);
    }
    if let Some(typography) = value.get("typography") {
        tokens.font_size_xs = parse_float_field(typography, "font-size-xs", tokens.font_size_xs);
        tokens.font_size_caption =
            parse_float_field(typography, "font-size-caption", tokens.font_size_caption);
        tokens.font_size_sm = parse_float_field(typography, "font-size-sm", tokens.font_size_sm);
        tokens.font_size_md = parse_float_field(typography, "font-size-md", tokens.font_size_md);
        tokens.font_size_lg = parse_float_field(typography, "font-size-lg", tokens.font_size_lg);
        tokens.font_size_helper = parse_float_field(typography, "font-size-helper", tokens.font_size_helper);
        tokens.font_primary = parse_string_field(typography, "font-primary", &tokens.font_primary);
        tokens.font_secondary = parse_string_field(typography, "font-secondary", &tokens.font_secondary);
        tokens.font_tertiary = parse_string_field(typography, "font-tertiary", &tokens.font_tertiary);
    }
    if let Some(radius) = value.get("radius") {
        tokens.radius_sm = parse_float_field(radius, "sm", tokens.radius_sm);
        tokens.radius_md = parse_float_field(radius, "md", tokens.radius_md);
        tokens.radius_lg = parse_float_field(radius, "lg", tokens.radius_lg);
        tokens.radius_full = parse_float_field(radius, "full", tokens.radius_full);
        if let Some(default_key) = radius.get("default").and_then(Value::as_str) {
            tokens.radius_default_key =
                default_key.strip_prefix("radius.").unwrap_or(default_key).to_string();
        }
    }
}

fn export_component_theme_json(data: &ComponentThemeData) -> Value {
    let variant_props = data
        .variant_props
        .iter()
        .map(|(variant_key, state_map)| {
            let states = state_map
                .iter()
                .map(|(state_key, prop_map)| {
                    let props = prop_map
                        .iter()
                        .map(|(prop_name, prop_value)| {
                            (prop_name.clone(), property_value_to_json(prop_value))
                        })
                        .collect::<serde_json::Map<String, Value>>();
                    (state_key.clone(), Value::Object(props))
                })
                .collect::<serde_json::Map<String, Value>>();
            (variant_key.clone(), Value::Object(states))
        })
        .collect::<serde_json::Map<String, Value>>();

    let size_props = data
        .size_props
        .iter()
        .map(|(size_key, prop_map)| {
            let props = prop_map
                .iter()
                .map(|(prop_name, prop_value)| (prop_name.clone(), property_value_to_json(prop_value)))
                .collect::<serde_json::Map<String, Value>>();
            (crate::plugin::serialize_size_key(size_key).to_string(), Value::Object(props))
        })
        .collect::<serde_json::Map<String, Value>>();

    json!({
        "variantProps": variant_props,
        "sizeProps": size_props,
    })
}

fn import_component_theme_json(data: &mut ComponentThemeData, plugin: &PluginDefinition, value: &Value) {
    let variant_prop_types = plugin
        .variant_props
        .iter()
        .map(|prop| (prop.name.as_str(), prop.prop_type.as_str()))
        .collect::<HashMap<_, _>>();
    let size_prop_types = plugin
        .size_props
        .iter()
        .map(|prop| (prop.name.as_str(), prop.prop_type.as_str()))
        .collect::<HashMap<_, _>>();

    if let Some(variants) = value.get("variantProps").and_then(Value::as_object) {
        for (variant_key, state_value) in variants {
            if !data.variant_props.contains_key(variant_key) {
                continue;
            }
            let Some(state_value) = state_value.as_object() else {
                continue;
            };
            for (state_key, prop_value) in state_value {
                if !data.variant_props.get(variant_key).and_then(|states| states.get(state_key)).is_some() {
                    continue;
                }
                let Some(prop_value) = prop_value.as_object() else {
                    continue;
                };
                for (prop_name, raw_value) in prop_value {
                    if let Some(prop_type) = variant_prop_types.get(prop_name.as_str()) {
                        if let Some(value) = property_value_from_json(raw_value, prop_type) {
                            data.set_variant_import_override(variant_key, state_key, prop_name, value);
                        }
                    }
                }
            }
        }
    }

    if let Some(sizes) = value.get("sizeProps").and_then(Value::as_object) {
        for (raw_size_key, prop_value) in sizes {
            let size_key = crate::plugin::normalize_size_key(raw_size_key).to_string();
            if !data.size_props.contains_key(&size_key) {
                continue;
            }
            let Some(prop_value) = prop_value.as_object() else {
                continue;
            };
            for (prop_name, raw_value) in prop_value {
                if let Some(prop_type) = size_prop_types.get(prop_name.as_str()) {
                    if let Some(value) = property_value_from_json(raw_value, prop_type) {
                        if size_key == crate::plugin::DEFAULT_SIZE_KEY {
                            data.set_size_import_override(crate::plugin::DEFAULT_SIZE_KEY, prop_name, value);
                        } else {
                            data.set_size_import_override(&size_key, prop_name, value);
                        }
                    }
                }
            }
        }
    }
}

fn export_theme_record_json(
    themes: &[ThemeRecord],
    theme_idx: usize,
    component_plugins: &HashMap<String, PluginDefinition>,
) -> Value {
    let record = themes
        .get(theme_idx)
        .unwrap_or_else(|| panic!("theme index {theme_idx} out of range for JSON export"));
    let parent = explicit_parent_theme_index(themes, theme_idx).and_then(|idx| themes.get(idx));

    let mut json = serde_json::Map::new();
    // Schema version is read by `import_theme_record_json` to refuse newer
    // schemas rather than silently misinterpret them. Bump whenever fields are
    // added or renamed.
    json.insert("version".to_string(), Value::Number(THEME_RECORD_SCHEMA_VERSION.into()));
    json.insert("id".to_string(), Value::String(theme_record_json_id(record)));
    json.insert("name".to_string(), Value::String(record.meta.name.clone()));

    if let Some(parent) = parent {
        json.insert("parent".to_string(), Value::String(theme_record_json_id(parent)));
    }

    if let Some(tokens) = export_token_theme_json_patch(record, parent) {
        json.insert("tokens".to_string(), tokens);
    }

    if let Some(components) = export_components_json_patch(record, parent, component_plugins) {
        json.insert("components".to_string(), Value::Object(components));
    }

    Value::Object(json)
}

fn component_key_by_name<'a>(
    component_plugins: &'a HashMap<String, PluginDefinition>,
    component_name: &str,
) -> Option<&'a str> {
    component_plugins
        .iter()
        .find_map(|(key, plugin)| (plugin.component == component_name).then_some(key.as_str()))
}

fn component_variant_value_is_defined(
    record: &ThemeRecord,
    component_plugins: &HashMap<String, PluginDefinition>,
    component_key: &str,
    variant: &str,
    state: &str,
    prop_name: &str,
    visited: &mut HashSet<String>,
) -> bool {
    if !visited.insert(component_key.to_string()) {
        return false;
    }
    let Some(component) = record.component_themes.get(component_key) else {
        return false;
    };

    if component.variant_prop_is_overridden(variant, state, prop_name) {
        return true;
    }
    if state != crate::plugin::NORMAL_STATE_KEY
        && component_variant_value_is_defined(
            record,
            component_plugins,
            component_key,
            variant,
            crate::plugin::NORMAL_STATE_KEY,
            prop_name,
            &mut HashSet::new(),
        )
    {
        return true;
    }
    if let Some(common_variant) = component.common_variant_key() {
        if variant != common_variant
            && component_variant_value_is_defined(
                record,
                component_plugins,
                component_key,
                common_variant,
                crate::plugin::NORMAL_STATE_KEY,
                prop_name,
                &mut HashSet::new(),
            )
        {
            return true;
        }
    }
    component
        .parent_key
        .as_deref()
        .and_then(|parent_name| component_key_by_name(component_plugins, parent_name))
        .is_some_and(|parent_key| {
            component_variant_value_is_defined(
                record,
                component_plugins,
                parent_key,
                variant,
                state,
                prop_name,
                visited,
            )
        })
}

fn component_size_value_is_defined(
    record: &ThemeRecord,
    component_plugins: &HashMap<String, PluginDefinition>,
    component_key: &str,
    size: &str,
    prop_name: &str,
    visited: &mut HashSet<String>,
) -> bool {
    if !visited.insert(component_key.to_string()) {
        return false;
    }
    let Some(component) = record.component_themes.get(component_key) else {
        return false;
    };
    if component.size_prop_is_overridden(size, prop_name)
        || (size != crate::plugin::DEFAULT_SIZE_KEY
            && component.size_prop_is_overridden(crate::plugin::DEFAULT_SIZE_KEY, prop_name))
    {
        return true;
    }
    component
        .parent_key
        .as_deref()
        .and_then(|parent_name| component_key_by_name(component_plugins, parent_name))
        .is_some_and(|parent_key| {
            component_size_value_is_defined(record, component_plugins, parent_key, size, prop_name, visited)
        })
}

fn validate_theme_record_for_save(
    record: &ThemeRecord,
    component_plugins: &HashMap<String, PluginDefinition>,
) -> Result<(), Vec<String>> {
    if record.meta.parent_name.is_some() {
        return Ok(());
    }

    let mut missing = Vec::new();
    for token_key in TOKEN_THEME_KEYS {
        if !record.token_overrides.contains(*token_key) {
            missing.push(format!("token {token_key}"));
        }
    }

    for (component_key, component) in &record.component_themes {
        for (variant_key, states) in &component.variant_props {
            for (state_key, props) in states {
                for prop in &component.variant_prop_defs {
                    if !props.contains_key(&prop.name)
                        || !component_variant_value_is_defined(
                            record,
                            component_plugins,
                            component_key,
                            variant_key,
                            state_key,
                            &prop.name,
                            &mut HashSet::new(),
                        )
                    {
                        missing.push(format!(
                            "component {component_key} variant {variant_key}/{state_key} property {}",
                            prop.name
                        ));
                    }
                }
            }
        }

        for (size_key, props) in &component.size_props {
            for prop in &component.size_prop_defs {
                if !props.contains_key(&prop.name)
                    || !component_size_value_is_defined(
                        record,
                        component_plugins,
                        component_key,
                        size_key,
                        &prop.name,
                        &mut HashSet::new(),
                    )
                {
                    missing.push(format!(
                        "component {component_key} size {} property {}",
                        crate::plugin::serialize_size_key(size_key),
                        prop.name
                    ));
                }
            }
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

fn title_case_identifier(value: &str) -> String {
    let mut spaced = String::new();
    let mut prev_was_word = false;
    for ch in value.chars() {
        if ch == '-' || ch == '_' || ch == '/' {
            spaced.push(' ');
            prev_was_word = false;
            continue;
        }

        if ch.is_ascii_uppercase() && prev_was_word {
            spaced.push(' ');
        }
        spaced.push(ch);
        prev_was_word = ch.is_ascii_lowercase() || ch.is_ascii_digit();
    }

    spaced
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            let Some(first) = chars.next() else { return String::new() };
            format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_missing_base_values(missing: &[String]) -> String {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for item in missing {
        if let Some(token) = item.strip_prefix("token ") {
            grouped.entry("Tokens".to_string()).or_default().push(token.to_string());
            continue;
        }

        let Some(rest) = item.strip_prefix("component ") else {
            grouped.entry("Other".to_string()).or_default().push(item.clone());
            continue;
        };

        if let Some((component, rest)) = rest.split_once(" variant ") {
            if let Some((variant_state, prop)) = rest.split_once(" property ") {
                if let Some((variant, state)) = variant_state.split_once('/') {
                    grouped.entry(title_case_identifier(component)).or_default().push(format!(
                        "Variants.{}.{}.{}",
                        title_case_identifier(variant),
                        title_case_identifier(state),
                        title_case_identifier(prop)
                    ));
                    continue;
                }
            }
        }

        if let Some((component, rest)) = rest.split_once(" size ") {
            if let Some((size, prop)) = rest.split_once(" property ") {
                let prop = title_case_identifier(prop);
                let path = if size == crate::plugin::DEFAULT_SIZE_KEY || size == "common" {
                    format!("Sizes.{prop}")
                } else {
                    format!("Sizes.{}.{}", title_case_identifier(size), prop)
                };
                grouped.entry(title_case_identifier(component)).or_default().push(path);
                continue;
            }
        }

        grouped.entry("Other".to_string()).or_default().push(item.clone());
    }

    let mut message = String::from("The following components/props are missing values:\n\n");
    for (group, mut props) in grouped {
        props.sort();
        props.dedup();
        message.push_str(&group);
        message.push('\n');
        for prop in props {
            message.push_str("  ");
            message.push_str(&prop);
            message.push('\n');
        }
        message.push('\n');
    }

    message.trim_end().to_string()
}

/// Current schema version emitted by `export_theme_record_json`. Theme files
/// can omit `version` (they're treated as legacy `1`) but if a version is
/// present and unknown to us we refuse the import instead of mangling
/// half-recognised fields.
const THEME_RECORD_SCHEMA_VERSION: u32 = 1;

fn import_theme_record_json(
    value: &Value,
    loaded_plugins: &[(plugin::BuiltinComponentSpec, PluginDefinition)],
    existing_themes: &[ThemeRecord],
) -> Result<ThemeRecord, String> {
    if let Some(version) = value.get("version").and_then(Value::as_u64) {
        if version > THEME_RECORD_SCHEMA_VERSION as u64 {
            return Err(format!(
                "Theme export schema version {version} is newer than supported version \
                 {THEME_RECORD_SCHEMA_VERSION}; update the theme editor to import this file"
            ));
        }
    }

    let name = value.get("name").and_then(Value::as_str).unwrap_or("Imported Theme");
    let resolved_parent_name = value
        .get("parent")
        .and_then(Value::as_str)
        .and_then(|raw_parent| resolve_theme_parent_name(existing_themes, raw_parent));
    let id = value.get("id").and_then(Value::as_str).map(normalize_theme_identifier);
    let is_root_base_theme = resolved_parent_name.is_none()
        && (is_base_theme_name(name)
            || id.as_deref() == Some(BASE_THEME_ID)
            || id.as_deref() == Some(LEGACY_DEFAULT_THEME_ID));

    let mut record = resolved_parent_name
        .as_deref()
        .and_then(|parent_name| find_theme_index_by_name(existing_themes, parent_name))
        .and_then(|idx| existing_themes.get(idx))
        .cloned()
        .unwrap_or_else(|| build_theme_record(name, false, None, TokenTheme::base_theme(), loaded_plugins));
    record.meta = ThemeMeta {
        name: if is_root_base_theme { BASE_THEME_NAME.to_string() } else { name.to_string() },
        is_builtin: false,
        parent_name: resolved_parent_name.clone(),
    };
    record.token_overrides.clear();
    // Schema defaults seed the editor controls, but they are not values owned
    // by this file. Rebuild ownership solely from the JSON below so saving an
    // incomplete Base Theme draft cannot materialize missing schema defaults.
    clear_component_override_ownership(&mut record);

    if let Some(tokens_value) = value.get("tokens") {
        record.token_overrides.extend(collect_token_override_keys(tokens_value));
        import_token_theme_json(&mut record.tokens, tokens_value);
    }

    if let Some(components) = value.get("components").and_then(Value::as_object) {
        for (component_key, component_value) in components {
            let Some((_, plugin)) =
                loaded_plugins.iter().find(|(spec, _)| spec.key == component_key.as_str())
            else {
                continue;
            };
            if let Some(data) = record.component_themes.get_mut(component_key) {
                import_component_theme_json(data, plugin, component_value);
            }
        }
    }

    let component_plugins: HashMap<String, PluginDefinition> =
        loaded_plugins.iter().map(|(spec, plugin)| (spec.key.to_string(), plugin.clone())).collect();
    reconcile_theme_component_parent_overrides(&component_plugins, &mut record);

    Ok(record)
}

fn resolve_theme_parent_name(themes: &[ThemeRecord], raw_parent: &str) -> Option<String> {
    let normalized_parent = normalize_theme_identifier(raw_parent);
    if normalized_parent == BASE_THEME_ID
        || normalized_parent == LEGACY_DEFAULT_THEME_ID
        || is_base_theme_name(raw_parent)
    {
        if let Some(theme) = themes.iter().find(|theme| is_builtin_base_theme(theme)) {
            return Some(theme.meta.name.clone());
        }
    }

    find_theme_index_by_name(themes, raw_parent)
        .and_then(|idx| themes.get(idx))
        .map(|theme| theme.meta.name.clone())
        .or_else(|| {
            let normalized = normalize_theme_identifier(raw_parent);
            themes
                .iter()
                .find(|theme| normalize_theme_identifier(&theme.meta.name) == normalized)
                .map(|theme| theme.meta.name.clone())
        })
}

fn export_token_theme_json_patch(record: &ThemeRecord, parent: Option<&ThemeRecord>) -> Option<Value> {
    let tokens = &record.tokens;
    let parent = parent.map(|theme| &theme.tokens);
    let token_overrides = &record.token_overrides;
    let owns = |path: &str| token_overrides.contains(path);
    let mut tokens_json = serde_json::Map::new();

    let mut colors = serde_json::Map::new();
    let mut light = serde_json::Map::new();
    let mut dark = serde_json::Map::new();

    insert_color_if_changed(
        &mut light,
        "primary",
        tokens.light_primary,
        parent.map(|parent| parent.light_primary),
        owns("colors.light.primary"),
    );
    insert_color_if_changed(
        &mut light,
        "primary-pressed",
        tokens.light_primary_pressed,
        parent.map(|parent| parent.light_primary_pressed),
        owns("colors.light.primary-pressed"),
    );
    insert_color_if_changed(
        &mut light,
        "secondary",
        tokens.light_secondary,
        parent.map(|parent| parent.light_secondary),
        owns("colors.light.secondary"),
    );
    insert_color_if_changed(
        &mut light,
        "danger",
        tokens.light_danger,
        parent.map(|parent| parent.light_danger),
        owns("colors.light.danger"),
    );
    insert_color_if_changed(
        &mut light,
        "surface",
        tokens.light_surface,
        parent.map(|parent| parent.light_surface),
        owns("colors.light.surface"),
    );
    insert_color_if_changed(
        &mut light,
        "background",
        tokens.light_background,
        parent.map(|parent| parent.light_background),
        owns("colors.light.background"),
    );
    insert_color_if_changed(
        &mut light,
        "text",
        tokens.light_text,
        parent.map(|parent| parent.light_text),
        owns("colors.light.text"),
    );
    insert_color_if_changed(
        &mut light,
        "text-muted",
        tokens.light_text_muted,
        parent.map(|parent| parent.light_text_muted),
        owns("colors.light.text-muted"),
    );
    if owns("colors.light.transparent") {
        light.insert("transparent".to_string(), Value::String("#00000000".to_string()));
    }

    insert_color_if_changed(
        &mut dark,
        "primary",
        tokens.dark_primary,
        parent.map(|parent| parent.dark_primary),
        owns("colors.dark.primary"),
    );
    insert_color_if_changed(
        &mut dark,
        "primary-pressed",
        tokens.dark_primary_pressed,
        parent.map(|parent| parent.dark_primary_pressed),
        owns("colors.dark.primary-pressed"),
    );
    insert_color_if_changed(
        &mut dark,
        "secondary",
        tokens.dark_secondary,
        parent.map(|parent| parent.dark_secondary),
        owns("colors.dark.secondary"),
    );
    insert_color_if_changed(
        &mut dark,
        "danger",
        tokens.dark_danger,
        parent.map(|parent| parent.dark_danger),
        owns("colors.dark.danger"),
    );
    insert_color_if_changed(
        &mut dark,
        "surface",
        tokens.dark_surface,
        parent.map(|parent| parent.dark_surface),
        owns("colors.dark.surface"),
    );
    insert_color_if_changed(
        &mut dark,
        "background",
        tokens.dark_background,
        parent.map(|parent| parent.dark_background),
        owns("colors.dark.background"),
    );
    insert_color_if_changed(
        &mut dark,
        "text",
        tokens.dark_text,
        parent.map(|parent| parent.dark_text),
        owns("colors.dark.text"),
    );
    insert_color_if_changed(
        &mut dark,
        "text-muted",
        tokens.dark_text_muted,
        parent.map(|parent| parent.dark_text_muted),
        owns("colors.dark.text-muted"),
    );
    if owns("colors.dark.transparent") {
        dark.insert("transparent".to_string(), Value::String("#00000000".to_string()));
    }

    if !light.is_empty() {
        colors.insert("light".to_string(), Value::Object(light));
    }
    if !dark.is_empty() {
        colors.insert("dark".to_string(), Value::Object(dark));
    }
    if !colors.is_empty() {
        tokens_json.insert("colors".to_string(), Value::Object(colors));
    }

    let mut spacing = serde_json::Map::new();
    insert_float_if_changed(
        &mut spacing,
        "xs",
        tokens.spacing_xs,
        parent.map(|parent| parent.spacing_xs),
        owns("spacing.xs"),
    );
    insert_float_if_changed(
        &mut spacing,
        "sm",
        tokens.spacing_sm,
        parent.map(|parent| parent.spacing_sm),
        owns("spacing.sm"),
    );
    insert_float_if_changed(
        &mut spacing,
        "md",
        tokens.spacing_md,
        parent.map(|parent| parent.spacing_md),
        owns("spacing.md"),
    );
    insert_float_if_changed(
        &mut spacing,
        "lg",
        tokens.spacing_lg,
        parent.map(|parent| parent.spacing_lg),
        owns("spacing.lg"),
    );
    insert_float_if_changed(
        &mut spacing,
        "xl",
        tokens.spacing_xl,
        parent.map(|parent| parent.spacing_xl),
        owns("spacing.xl"),
    );
    if !spacing.is_empty() {
        tokens_json.insert("spacing".to_string(), Value::Object(spacing));
    }

    let mut control_size = serde_json::Map::new();
    insert_float_if_changed(
        &mut control_size,
        "sm",
        tokens.control_size_sm,
        parent.map(|parent| parent.control_size_sm),
        owns("controlSize.sm"),
    );
    insert_float_if_changed(
        &mut control_size,
        "md",
        tokens.control_size_md,
        parent.map(|parent| parent.control_size_md),
        owns("controlSize.md"),
    );
    insert_float_if_changed(
        &mut control_size,
        "lg",
        tokens.control_size_lg,
        parent.map(|parent| parent.control_size_lg),
        owns("controlSize.lg"),
    );
    if !control_size.is_empty() {
        tokens_json.insert("controlSize".to_string(), Value::Object(control_size));
    }

    let mut choice_control_size = serde_json::Map::new();
    insert_float_if_changed(
        &mut choice_control_size,
        "sm",
        tokens.choice_control_size_sm,
        parent.map(|parent| parent.choice_control_size_sm),
        owns("choiceControlSize.sm"),
    );
    insert_float_if_changed(
        &mut choice_control_size,
        "md",
        tokens.choice_control_size_md,
        parent.map(|parent| parent.choice_control_size_md),
        owns("choiceControlSize.md"),
    );
    insert_float_if_changed(
        &mut choice_control_size,
        "lg",
        tokens.choice_control_size_lg,
        parent.map(|parent| parent.choice_control_size_lg),
        owns("choiceControlSize.lg"),
    );
    if !choice_control_size.is_empty() {
        tokens_json.insert("choiceControlSize".to_string(), Value::Object(choice_control_size));
    }

    let mut switch_size = serde_json::Map::new();
    insert_float_if_changed(
        &mut switch_size,
        "sm",
        tokens.switch_size_sm,
        parent.map(|parent| parent.switch_size_sm),
        owns("switchSize.sm"),
    );
    insert_float_if_changed(
        &mut switch_size,
        "md",
        tokens.switch_size_md,
        parent.map(|parent| parent.switch_size_md),
        owns("switchSize.md"),
    );
    insert_float_if_changed(
        &mut switch_size,
        "lg",
        tokens.switch_size_lg,
        parent.map(|parent| parent.switch_size_lg),
        owns("switchSize.lg"),
    );
    if !switch_size.is_empty() {
        tokens_json.insert("switchSize".to_string(), Value::Object(switch_size));
    }

    let mut icon_size = serde_json::Map::new();
    insert_float_if_changed(
        &mut icon_size,
        "sm",
        tokens.icon_size_sm,
        parent.map(|parent| parent.icon_size_sm),
        owns("iconSize.sm"),
    );
    insert_float_if_changed(
        &mut icon_size,
        "md",
        tokens.icon_size_md,
        parent.map(|parent| parent.icon_size_md),
        owns("iconSize.md"),
    );
    insert_float_if_changed(
        &mut icon_size,
        "lg",
        tokens.icon_size_lg,
        parent.map(|parent| parent.icon_size_lg),
        owns("iconSize.lg"),
    );
    if !icon_size.is_empty() {
        tokens_json.insert("iconSize".to_string(), Value::Object(icon_size));
    }

    let mut border_width = serde_json::Map::new();
    insert_float_if_changed(
        &mut border_width,
        "none",
        tokens.border_width_none,
        parent.map(|parent| parent.border_width_none),
        owns("borderWidth.none"),
    );
    insert_float_if_changed(
        &mut border_width,
        "sm",
        tokens.border_width_sm,
        parent.map(|parent| parent.border_width_sm),
        owns("borderWidth.sm"),
    );
    insert_float_if_changed(
        &mut border_width,
        "focus",
        tokens.border_width_focus,
        parent.map(|parent| parent.border_width_focus),
        owns("borderWidth.focus"),
    );
    if !border_width.is_empty() {
        tokens_json.insert("borderWidth".to_string(), Value::Object(border_width));
    }

    let mut control_radius = serde_json::Map::new();
    insert_float_if_changed(
        &mut control_radius,
        "sm",
        tokens.control_radius_sm,
        parent.map(|parent| parent.control_radius_sm),
        owns("controlRadius.sm"),
    );
    insert_float_if_changed(
        &mut control_radius,
        "md",
        tokens.control_radius_md,
        parent.map(|parent| parent.control_radius_md),
        owns("controlRadius.md"),
    );
    insert_float_if_changed(
        &mut control_radius,
        "lg",
        tokens.control_radius_lg,
        parent.map(|parent| parent.control_radius_lg),
        owns("controlRadius.lg"),
    );
    if !control_radius.is_empty() {
        tokens_json.insert("controlRadius".to_string(), Value::Object(control_radius));
    }

    let mut control_padding_inline = serde_json::Map::new();
    insert_float_if_changed(
        &mut control_padding_inline,
        "sm",
        tokens.control_padding_inline_sm,
        parent.map(|parent| parent.control_padding_inline_sm),
        owns("controlPaddingInline.sm"),
    );
    insert_float_if_changed(
        &mut control_padding_inline,
        "md",
        tokens.control_padding_inline_md,
        parent.map(|parent| parent.control_padding_inline_md),
        owns("controlPaddingInline.md"),
    );
    insert_float_if_changed(
        &mut control_padding_inline,
        "lg",
        tokens.control_padding_inline_lg,
        parent.map(|parent| parent.control_padding_inline_lg),
        owns("controlPaddingInline.lg"),
    );
    if !control_padding_inline.is_empty() {
        tokens_json.insert("controlPaddingInline".to_string(), Value::Object(control_padding_inline));
    }

    let mut typography = serde_json::Map::new();
    insert_float_if_changed(
        &mut typography,
        "font-size-xs",
        tokens.font_size_xs,
        parent.map(|parent| parent.font_size_xs),
        owns("typography.font-size-xs"),
    );
    insert_float_if_changed(
        &mut typography,
        "font-size-caption",
        tokens.font_size_caption,
        parent.map(|parent| parent.font_size_caption),
        owns("typography.font-size-caption"),
    );
    insert_float_if_changed(
        &mut typography,
        "font-size-sm",
        tokens.font_size_sm,
        parent.map(|parent| parent.font_size_sm),
        owns("typography.font-size-sm"),
    );
    insert_float_if_changed(
        &mut typography,
        "font-size-md",
        tokens.font_size_md,
        parent.map(|parent| parent.font_size_md),
        owns("typography.font-size-md"),
    );
    insert_float_if_changed(
        &mut typography,
        "font-size-lg",
        tokens.font_size_lg,
        parent.map(|parent| parent.font_size_lg),
        owns("typography.font-size-lg"),
    );
    insert_float_if_changed(
        &mut typography,
        "font-size-helper",
        tokens.font_size_helper,
        parent.map(|parent| parent.font_size_helper),
        owns("typography.font-size-helper"),
    );
    insert_string_if_changed(
        &mut typography,
        "font-primary",
        &tokens.font_primary,
        parent.map(|parent| parent.font_primary.as_str()),
        owns("typography.font-primary"),
    );
    insert_string_if_changed(
        &mut typography,
        "font-secondary",
        &tokens.font_secondary,
        parent.map(|parent| parent.font_secondary.as_str()),
        owns("typography.font-secondary"),
    );
    insert_string_if_changed(
        &mut typography,
        "font-tertiary",
        &tokens.font_tertiary,
        parent.map(|parent| parent.font_tertiary.as_str()),
        owns("typography.font-tertiary"),
    );
    if !typography.is_empty() {
        tokens_json.insert("typography".to_string(), Value::Object(typography));
    }

    let mut font_weight = serde_json::Map::new();
    insert_int_if_changed(
        &mut font_weight,
        "normal",
        tokens.font_weight_normal,
        parent.map(|parent| parent.font_weight_normal),
        owns("fontWeight.normal"),
    );
    insert_int_if_changed(
        &mut font_weight,
        "medium",
        tokens.font_weight_medium,
        parent.map(|parent| parent.font_weight_medium),
        owns("fontWeight.medium"),
    );
    insert_int_if_changed(
        &mut font_weight,
        "semibold",
        tokens.font_weight_semibold,
        parent.map(|parent| parent.font_weight_semibold),
        owns("fontWeight.semibold"),
    );
    insert_int_if_changed(
        &mut font_weight,
        "bold",
        tokens.font_weight_bold,
        parent.map(|parent| parent.font_weight_bold),
        owns("fontWeight.bold"),
    );
    if !font_weight.is_empty() {
        tokens_json.insert("fontWeight".to_string(), Value::Object(font_weight));
    }

    let mut radius = serde_json::Map::new();
    insert_float_if_changed(
        &mut radius,
        "sm",
        tokens.radius_sm,
        parent.map(|parent| parent.radius_sm),
        owns("radius.sm"),
    );
    insert_float_if_changed(
        &mut radius,
        "md",
        tokens.radius_md,
        parent.map(|parent| parent.radius_md),
        owns("radius.md"),
    );
    insert_float_if_changed(
        &mut radius,
        "lg",
        tokens.radius_lg,
        parent.map(|parent| parent.radius_lg),
        owns("radius.lg"),
    );
    insert_float_if_changed(
        &mut radius,
        "full",
        tokens.radius_full,
        parent.map(|parent| parent.radius_full),
        owns("radius.full"),
    );
    let current_default_radius = format!("radius.{}", tokens.radius_default_key);
    let parent_default_radius = parent.map(|parent| format!("radius.{}", parent.radius_default_key));
    insert_string_if_changed(
        &mut radius,
        "default",
        &current_default_radius,
        parent_default_radius.as_deref(),
        owns("radius.default"),
    );
    if !radius.is_empty() {
        tokens_json.insert("radius".to_string(), Value::Object(radius));
    }

    (!tokens_json.is_empty()).then_some(Value::Object(tokens_json))
}

fn export_components_json_patch(
    record: &ThemeRecord,
    parent: Option<&ThemeRecord>,
    component_plugins: &HashMap<String, PluginDefinition>,
) -> Option<serde_json::Map<String, Value>> {
    let mut components = serde_json::Map::new();

    let mut component_keys: Vec<_> = record.component_themes.keys().cloned().collect();
    component_keys.sort();
    for key in component_keys {
        let Some(current) = record.component_themes.get(&key) else {
            continue;
        };
        let parent_component = parent.and_then(|theme| theme.component_themes.get(&key));
        let component_parent = component_parent_data(component_plugins, &record.component_themes, current);
        if let Some(component_json) =
            export_component_theme_json_patch(current, parent_component, component_parent)
        {
            components.insert(key, component_json);
        }
    }

    (!components.is_empty()).then_some(components)
}

fn export_component_theme_json_patch(
    data: &ComponentThemeData,
    parent: Option<&ComponentThemeData>,
    component_parent: Option<&ComponentThemeData>,
) -> Option<Value> {
    let mut variant_props = serde_json::Map::new();
    let mut variant_keys: Vec<_> = data.variant_props.keys().cloned().collect();
    variant_keys.sort();
    for variant_key in variant_keys {
        let Some(state_map) = data.variant_props.get(&variant_key) else {
            continue;
        };
        let mut states = serde_json::Map::new();
        let mut state_keys: Vec<_> = state_map.keys().cloned().collect();
        state_keys.sort();
        for state_key in state_keys {
            let Some(prop_map) = state_map.get(&state_key) else {
                continue;
            };
            let mut props = serde_json::Map::new();
            let mut prop_names: Vec<_> = prop_map.keys().cloned().collect();
            prop_names.sort();
            for prop_name in prop_names {
                let Some(value) = prop_map.get(&prop_name) else {
                    continue;
                };
                if let Some(fallback) = variant_prop_fallback_value(
                    data,
                    parent,
                    component_parent,
                    &variant_key,
                    &state_key,
                    &prop_name,
                ) {
                    if property_value_matches(value, &fallback) {
                        continue;
                    }
                } else if !data.variant_state_is_root(&variant_key, &state_key)
                    && !data.variant_prop_is_overridden(&variant_key, &state_key, &prop_name)
                {
                    continue;
                } else if data.variant_state_is_root(&variant_key, &state_key)
                    && !data.variant_prop_is_overridden(&variant_key, &state_key, &prop_name)
                {
                    continue;
                }
                props.insert(prop_name, property_value_to_json(value));
            }
            if !props.is_empty() {
                states.insert(state_key, Value::Object(props));
            }
        }
        if !states.is_empty() {
            variant_props.insert(variant_key, Value::Object(states));
        }
    }

    let mut size_props = serde_json::Map::new();
    let mut size_keys: Vec<_> = data.size_props.keys().cloned().collect();
    size_keys.sort();
    for size_key in size_keys {
        let Some(prop_map) = data.size_props.get(&size_key) else {
            continue;
        };
        let parent_map = parent.and_then(|parent| parent.size_props.get(&size_key));
        let mut props = serde_json::Map::new();
        let mut prop_names: Vec<_> = prop_map.keys().cloned().collect();
        prop_names.sort();
        for prop_name in prop_names {
            let Some(value) = prop_map.get(&prop_name) else {
                continue;
            };
            if size_key == crate::plugin::DEFAULT_SIZE_KEY {
                let component_parent_value = component_parent
                    .and_then(|parent| parent.size_props.get(crate::plugin::DEFAULT_SIZE_KEY))
                    .and_then(|props| props.get(&prop_name));
                let parent_value =
                    component_parent_value.or_else(|| parent_map.and_then(|props| props.get(&prop_name)));
                if parent_value.map(|parent| property_value_matches(value, parent)).unwrap_or(false) {
                    continue;
                }
                if parent_value.is_none() && !data.size_prop_is_overridden(&size_key, &prop_name) {
                    continue;
                }
            } else {
                if !data.size_prop_is_overridden(&size_key, &prop_name) {
                    continue;
                }
            }
            props.insert(prop_name, property_value_to_json(value));
        }
        if !props.is_empty() {
            size_props.insert(crate::plugin::serialize_size_key(&size_key).to_string(), Value::Object(props));
        }
    }

    let mut component = serde_json::Map::new();
    if !variant_props.is_empty() {
        component.insert("variantProps".to_string(), Value::Object(variant_props));
    }
    if !size_props.is_empty() {
        component.insert("sizeProps".to_string(), Value::Object(size_props));
    }

    (!component.is_empty()).then_some(Value::Object(component))
}

fn insert_color_if_changed(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    current: slint::Color,
    parent: Option<slint::Color>,
    force: bool,
) {
    if force || parent.is_some_and(|parent| parent != current) {
        map.insert(key.to_string(), Value::String(color_hex(current)));
    }
}

fn insert_float_if_changed(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    current: f32,
    parent: Option<f32>,
    force: bool,
) {
    if force || parent.is_some_and(|parent| (parent - current).abs() >= f32::EPSILON) {
        map.insert(key.to_string(), json!(current));
    }
}

fn insert_int_if_changed(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    current: i32,
    parent: Option<i32>,
    force: bool,
) {
    if force || parent.is_some_and(|parent| parent != current) {
        map.insert(key.to_string(), json!(current));
    }
}

fn insert_string_if_changed(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    current: &str,
    parent: Option<&str>,
    force: bool,
) {
    if force || parent.is_some_and(|parent| parent != current) {
        map.insert(key.to_string(), Value::String(current.to_string()));
    }
}

fn insert_button_color_prop(
    map: &mut serde_json::Map<String, Value>,
    value_key: &str,
    token_key: &str,
    current_color: slint::Color,
    current_token: Option<&str>,
    parent_color: Option<slint::Color>,
    parent_token: Option<&str>,
) {
    if current_token != parent_token {
        map.insert(
            token_key.to_string(),
            current_token.map(|token| Value::String(token.to_string())).unwrap_or(Value::Null),
        );
    }
    if current_token.is_none() {
        insert_color_if_changed(map, value_key, current_color, parent_color, false);
    }
}

fn insert_button_float_prop(
    map: &mut serde_json::Map<String, Value>,
    value_key: &str,
    token_key: &str,
    current_value: f32,
    current_token: Option<&str>,
    parent_value: Option<f32>,
    parent_token: Option<&str>,
) {
    if current_token != parent_token {
        map.insert(
            token_key.to_string(),
            current_token.map(|token| Value::String(token.to_string())).unwrap_or(Value::Null),
        );
    }
    if current_token.is_none() {
        insert_float_if_changed(map, value_key, current_value, parent_value, false);
    }
}

fn insert_button_string_prop(
    map: &mut serde_json::Map<String, Value>,
    value_key: &str,
    token_key: &str,
    current_value: &str,
    current_token: Option<&str>,
    parent_value: Option<&str>,
    parent_token: Option<&str>,
) {
    if current_token != parent_token {
        map.insert(
            token_key.to_string(),
            current_token.map(|token| Value::String(token.to_string())).unwrap_or(Value::Null),
        );
    }
    if current_token.is_none() {
        insert_string_if_changed(map, value_key, current_value, parent_value, false);
    }
}

fn resolve_number_token_from_store(tokens: &TokenStore, key: &str) -> Option<f32> {
    tokens.resolve_float(normalize_export_token_key(key))
}

fn resolve_font_token_from_store(tokens: &TokenStore, key: &str) -> Option<String> {
    tokens.resolve_string(normalize_export_token_key(key)).map(str::to_string)
}

fn resolve_color_token_from_store(tokens: &TokenStore, key: &str) -> Option<slint::Color> {
    tokens.resolve_color(normalize_export_token_key(key))
}

/// SDK source-of-truth theme JSON, embedded at compile time. Lets the editor
/// seed the shared `~/.foundation/themes/json` cache (and fall back to these)
/// without needing to know the SDK layout at runtime. These are the very files
/// the `foundation` CLI seeds and apps compile from, so the editor's built-in
/// themes never drift from what ships.
const SEED_BUILTIN_THEMES: &[(&str, &str)] = &[(
    "base_theme.json",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../sdk/crates/foundation-themes/themes/base_theme.json"
    )),
)];

/// Shared, user-scoped theme cache the editor reads built-in themes from:
/// `~/.foundation/themes/json` (the same location the `foundation` CLI seeds and
/// apps compile from). `None` if the home directory can't be determined.
fn user_themes_json_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".foundation").join("themes").join("json"))
}

/// Copy any bundled SDK theme the cache is missing into `dir`, never clobbering
/// existing files (user edits win). Returns whether `dir` then holds at least
/// one theme JSON.
fn seed_user_themes_dir(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    for (name, contents) in SEED_BUILTIN_THEMES {
        let dest = dir.join(name);
        if !dest.exists() {
            let _ = std::fs::write(&dest, contents);
        }
    }
    dir_has_theme_json(dir)
}

fn dir_has_theme_json(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
        })
        .unwrap_or(false)
}

/// Built-in theme JSON as `(file_name, parsed)` pairs. Prefers the shared
/// `~/.foundation/themes/json` cache (seeding it from the embedded SDK themes on
/// first use); if that cache can't be used, falls back to parsing the embedded
/// copies directly so the editor always has its base themes.
fn builtin_theme_sources() -> Vec<(String, Value)> {
    if let Some(dir) = user_themes_json_dir() {
        if seed_user_themes_dir(&dir) {
            if let Ok(sources) = read_theme_jsons_from_dir(&dir) {
                if !sources.is_empty() {
                    return sources;
                }
            }
        }
    }

    SEED_BUILTIN_THEMES
        .iter()
        .filter_map(|(name, contents)| {
            serde_json::from_str::<Value>(contents).ok().map(|value| (name.to_string(), value))
        })
        .collect()
}

/// Read every `*.json` in `dir` as `(file_name, parsed)`, sorted by path.
/// Malformed files are skipped rather than failing the whole editor.
fn read_theme_jsons_from_dir(dir: &Path) -> std::io::Result<Vec<(String, Value)>> {
    let mut entries = std::fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    let mut sources = Vec::new();
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else { continue };
        let Ok(value) = serde_json::from_str::<Value>(&contents) else { continue };
        let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("theme.json").to_string();
        sources.push((file_name, value));
    }
    Ok(sources)
}

fn load_builtin_theme_records(
    loaded_plugins: &[(plugin::BuiltinComponentSpec, PluginDefinition)],
) -> Result<Vec<ThemeRecord>, String> {
    let sources = builtin_theme_sources();

    let mut theme_defs = Vec::new();
    for (file_name, value) in sources {
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Path::new(&file_name).file_stem().and_then(|stem| stem.to_str()).map(str::to_string))
            .unwrap_or_else(|| "theme".to_string());
        let name = value.get("name").and_then(Value::as_str).unwrap_or(&id).to_string();
        let parent = value.get("parent").and_then(Value::as_str).map(str::to_string);
        let sort_order = value.get("sort_order").and_then(Value::as_i64).unwrap_or(i64::MAX);
        theme_defs.push((sort_order, name.clone(), id, name, parent, value));
    }

    theme_defs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let builtin_names = theme_defs
        .iter()
        .map(|(_, _, id, name, _, _)| (normalize_theme_identifier(id), name.clone()))
        .collect::<HashMap<_, _>>();

    let mut records = Vec::new();
    for (_, _, id, name, parent, value) in theme_defs {
        let mut record = import_theme_record_json(&value, loaded_plugins, &records)?;
        record.meta.name = name;
        record.meta.is_builtin = true;
        record.meta.parent_name = parent.and_then(|parent_id| {
            builtin_names.get(&normalize_theme_identifier(&parent_id)).cloned().or(Some(parent_id))
        });
        if record.meta.name.is_empty() {
            record.meta.name = id;
        }
        records.push(record);
    }

    if records.is_empty() {
        return Err("no built-in theme-editor themes found".to_string());
    }

    Ok(records)
}

fn normalize_theme_identifier(value: &str) -> String {
    let mut ident = String::new();
    let mut last_was_underscore = true;
    for ch in value.chars() {
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
        "theme".to_string()
    } else {
        ident
    }
}

fn base_theme_index(themes: &[ThemeRecord]) -> Option<usize> {
    themes.iter().position(is_builtin_base_theme).or_else(|| (!themes.is_empty()).then_some(0))
}

fn explicit_parent_theme_index(themes: &[ThemeRecord], theme_idx: usize) -> Option<usize> {
    themes
        .get(theme_idx)
        .and_then(|theme| theme.meta.parent_name.as_deref())
        .and_then(|name| find_theme_index_by_name(themes, name))
}

fn explicit_parent_theme<'a>(themes: &'a [ThemeRecord], theme_idx: usize) -> Option<&'a ThemeRecord> {
    explicit_parent_theme_index(themes, theme_idx).and_then(|idx| themes.get(idx))
}

fn reset_theme_record_from_baseline(themes: &mut [ThemeRecord], theme_idx: usize) {
    let Some(current_meta) = themes.get(theme_idx).map(|theme| theme.meta.clone()) else {
        return;
    };

    let baseline_idx = explicit_parent_theme_index(themes, theme_idx)
        .or_else(|| base_theme_index(themes))
        .filter(|baseline_idx| *baseline_idx != theme_idx);

    let Some(baseline) = baseline_idx.and_then(|idx| themes.get(idx).cloned()) else {
        return;
    };

    let mut reset_theme = baseline;
    reset_theme.meta = current_meta;
    reset_theme.token_overrides =
        if reset_theme.meta.parent_name.is_none() { all_token_override_keys() } else { HashSet::new() };
    if reset_theme.meta.parent_name.is_some() {
        clear_component_override_ownership(&mut reset_theme);
    }
    themes[theme_idx] = reset_theme;
}

fn slint_color_to_export_color(color: slint::Color) -> export_theme::Color {
    export_theme::Color::rgba(color.red(), color.green(), color.blue(), color.alpha())
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

fn set_export_style_literal(style: &mut export_theme::StyleProps, prop_name: &str, value: &ExportLiteral) {
    match value {
        ExportLiteral::Color(color) => {
            export_theme::set_style_prop(style, prop_name, slint_color_to_export_color(*color))
        }
        ExportLiteral::Float(number) => export_theme::set_style_prop(style, prop_name, *number),
        ExportLiteral::Int(number) => export_theme::set_style_prop(style, prop_name, *number),
        ExportLiteral::Bool(flag) => export_theme::set_style_prop(style, prop_name, *flag),
        ExportLiteral::String(text) => export_theme::set_style_prop(style, prop_name, text.clone()),
        ExportLiteral::TokenRef(path) => {
            export_theme::set_style_prop(style, prop_name, export_theme::token_ref(path.clone()))
        }
    }
}

fn style_prop_literal(style: &export_theme::StyleProps, prop_name: &str) -> Option<ExportLiteral> {
    let normalized = prop_name.trim().to_ascii_lowercase().replace('-', "_");

    if let Some(path) = style.token_refs.get(&normalized) {
        return Some(ExportLiteral::TokenRef(path.clone()));
    }

    match normalized.as_str() {
        "background" => style.background.map(|value| ExportLiteral::Color(value.to_slint())),
        "foreground" => style.foreground.map(|value| ExportLiteral::Color(value.to_slint())),
        "border_color" => style.border_color.map(|value| ExportLiteral::Color(value.to_slint())),
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
        _ => style.extra.get(&normalized).map(style_value_literal),
    }
}

fn resolve_export_literal_against_store(
    tokens: &TokenStore,
    literal: &ExportLiteral,
) -> Option<PropertyValue> {
    match literal {
        ExportLiteral::Color(color) => Some(PropertyValue::Color(*color)),
        ExportLiteral::Float(number) => Some(PropertyValue::Float(*number)),
        ExportLiteral::Int(number) => Some(PropertyValue::Int(*number)),
        ExportLiteral::Bool(flag) => Some(PropertyValue::Bool(*flag)),
        ExportLiteral::String(text) => Some(PropertyValue::String(text.clone())),
        ExportLiteral::TokenRef(path) => {
            resolve_property_value_from_store(tokens, &PropertyValue::Token(path.clone()))
        }
    }
}

fn property_value_matches(a: &PropertyValue, b: &PropertyValue) -> bool {
    match (a, b) {
        (PropertyValue::Color(a), PropertyValue::Color(b)) => a == b,
        (PropertyValue::Float(a), PropertyValue::Float(b)) => (*a - *b).abs() < f32::EPSILON,
        (PropertyValue::Float(a), PropertyValue::Int(b)) => (*a - *b as f32).abs() < f32::EPSILON,
        (PropertyValue::Int(a), PropertyValue::Float(b)) => (*a as f32 - *b).abs() < f32::EPSILON,
        (PropertyValue::Int(a), PropertyValue::Int(b)) => a == b,
        (PropertyValue::Bool(a), PropertyValue::Bool(b)) => a == b,
        (PropertyValue::String(a), PropertyValue::String(b)) => a == b,
        (PropertyValue::Token(a), PropertyValue::Token(b)) => a == b,
        _ => false,
    }
}

fn normalize_export_token_key(key: &str) -> &str {
    match key {
        "color.primary-pressed" | "color.primary_pressed" => "color.primary.dark",
        "color.primary-hover" | "color.primary_hover" => "color.primary.light",
        "color.text" => "color.foreground",
        "color.text-muted" | "color.text_muted" => "color.muted",
        _ => key,
    }
}

fn resolve_property_value_from_store(tokens: &TokenStore, value: &PropertyValue) -> Option<PropertyValue> {
    match value {
        PropertyValue::Token(key) => {
            let normalized_key = normalize_export_token_key(key);
            if let Some(color) = tokens.resolve_color(normalized_key) {
                Some(PropertyValue::Color(color))
            } else if let Some(flag) = tokens.resolve_bool(normalized_key) {
                Some(PropertyValue::Bool(flag))
            } else if let Some(number) = tokens.resolve_int(normalized_key) {
                Some(PropertyValue::Int(number))
            } else if let Some(number) = tokens.resolve_float(normalized_key) {
                Some(PropertyValue::Float(number))
            } else if let Some(text) = tokens.resolve_string(normalized_key) {
                Some(PropertyValue::String(text.to_string()))
            } else {
                None
            }
        }
        _ => Some(value.clone()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportStylePropKind {
    Color,
    Float,
    Int,
    String,
    Other,
}

fn export_style_prop_kind(prop_name: &str) -> ExportStylePropKind {
    match prop_name.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "background" | "foreground" | "border_color" => ExportStylePropKind::Color,
        "border_width" | "border_radius" | "padding_horizontal" | "padding_vertical" | "min_height"
        | "min_width" | "font_size" | "icon_size" | "opacity" | "touch_expansion" => {
            ExportStylePropKind::Float
        }
        "font_weight" => ExportStylePropKind::Int,
        "font_family" => ExportStylePropKind::String,
        _ => ExportStylePropKind::Other,
    }
}

fn apply_property_value_to_export_style(
    style: &mut export_theme::StyleProps,
    prop_name: &str,
    value: &PropertyValue,
    _tokens: &TokenStore,
) {
    match (export_style_prop_kind(prop_name), value) {
        (_, PropertyValue::Token(key)) => export_theme::set_style_prop(
            style,
            prop_name,
            export_theme::token_ref(normalize_export_token_key(key)),
        ),
        (ExportStylePropKind::Color, PropertyValue::Color(color)) => {
            export_theme::set_style_prop(style, prop_name, slint_color_to_export_color(*color))
        }
        (ExportStylePropKind::Float, PropertyValue::Float(number)) => {
            export_theme::set_style_prop(style, prop_name, *number)
        }
        (ExportStylePropKind::Float, PropertyValue::Int(number)) => {
            export_theme::set_style_prop(style, prop_name, *number)
        }
        (ExportStylePropKind::Int, PropertyValue::Int(number)) => {
            export_theme::set_style_prop(style, prop_name, *number)
        }
        (ExportStylePropKind::Int, PropertyValue::Float(number)) => {
            export_theme::set_style_prop(style, prop_name, *number)
        }
        (ExportStylePropKind::String, PropertyValue::String(text)) if text.trim().is_empty() => {
            eprintln!("[export] skipping empty string for typed property '{}'", prop_name);
        }
        (ExportStylePropKind::String, PropertyValue::String(text)) => {
            export_theme::set_style_prop(style, prop_name, text.clone())
        }
        (ExportStylePropKind::Other, PropertyValue::Color(color)) => {
            export_theme::set_style_prop(style, prop_name, slint_color_to_export_color(*color))
        }
        (ExportStylePropKind::Other, PropertyValue::Float(number)) => {
            export_theme::set_style_prop(style, prop_name, *number)
        }
        (ExportStylePropKind::Other, PropertyValue::Int(number)) => {
            export_theme::set_style_prop(style, prop_name, *number)
        }
        (ExportStylePropKind::Other, PropertyValue::Bool(flag)) => {
            export_theme::set_style_prop(style, prop_name, *flag)
        }
        (ExportStylePropKind::Other, PropertyValue::String(text)) => {
            export_theme::set_style_prop(style, prop_name, text.clone())
        }
        (_, resolved_value) => {
            eprintln!("[export] skipping invalid value for property '{}': {:?}", prop_name, resolved_value);
        }
    }
}

fn component_theme_data_to_export_component(
    data: &ComponentThemeData,
    tokens: &TokenStore,
) -> export_theme::ComponentTheme {
    let mut component = export_theme::ComponentTheme::new();

    let mut shared_sizes = HashMap::new();
    let mut size_keys: Vec<_> = data.size_props.keys().cloned().collect();
    size_keys.sort();
    for size_key in size_keys {
        let Some(prop_map) = data.size_props.get(&size_key) else {
            continue;
        };
        let mut style = export_theme::StyleProps::new();
        let mut prop_names: Vec<_> = prop_map.keys().cloned().collect();
        prop_names.sort();
        for prop_name in prop_names {
            if let Some(value) = prop_map.get(&prop_name) {
                apply_property_value_to_export_style(&mut style, &prop_name, value, tokens);
            }
        }
        shared_sizes.insert(size_key, style);
    }

    let mut variant_keys: Vec<_> = data.variant_props.keys().cloned().collect();
    variant_keys.sort();
    for variant_key in variant_keys {
        let mut variant = export_theme::VariantTheme::new();
        variant.sizes = shared_sizes.clone();

        if let Some(state_map) = data.variant_props.get(&variant_key) {
            if let Some(normal_props) = state_map.get("normal") {
                let mut default_style = export_theme::StyleProps::new();
                let mut prop_names: Vec<_> = normal_props.keys().cloned().collect();
                prop_names.sort();
                for prop_name in prop_names {
                    if let Some(value) = normal_props.get(&prop_name) {
                        apply_property_value_to_export_style(&mut default_style, &prop_name, value, tokens);
                    }
                }
                variant.default = default_style;
            }

            let mut state_keys: Vec<_> = state_map.keys().cloned().collect();
            state_keys.sort();
            for state_key in state_keys {
                if state_key == "normal" {
                    continue;
                }
                let Some(prop_map) = state_map.get(&state_key) else {
                    continue;
                };
                let mut style = export_theme::StyleProps::new();
                let mut prop_names: Vec<_> = prop_map.keys().cloned().collect();
                prop_names.sort();
                for prop_name in prop_names {
                    if let Some(value) = prop_map.get(&prop_name) {
                        apply_property_value_to_export_style(&mut style, &prop_name, value, tokens);
                    }
                }
                variant.states.insert(state_key, style);
            }
        }

        component.variants.insert(variant_key, variant);
    }

    component
}

fn theme_record_to_export_theme(record: &ThemeRecord) -> export_theme::Theme {
    let token_store = token_store_from_theme(&record.tokens);
    let mut theme = export_theme::Theme {
        tokens: editor_tokens_to_export_tokens(&record.tokens),
        components: HashMap::new(),
    };

    let mut component_keys: Vec<_> = record.component_themes.keys().cloned().collect();
    component_keys.sort();
    for component_key in component_keys {
        if let Some(data) = record.component_themes.get(&component_key) {
            theme
                .components
                .insert(component_key, component_theme_data_to_export_component(data, &token_store));
        }
    }

    theme
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
            ExportLiteral::Color(color) => {
                format!("color({})", rust_string_literal(&color_literal(*color)))
            }
            ExportLiteral::Float(number) => format_rust_float(*number),
            ExportLiteral::Int(number) => number.to_string(),
            ExportLiteral::Bool(flag) => flag.to_string(),
            ExportLiteral::String(text) => rust_string_literal(text),
            ExportLiteral::TokenRef(path) => format!("token_ref({})", rust_string_literal(path)),
        }
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

/// Hard cap on sanitized identifier length. Rust identifiers can theoretically
/// be much longer, but a single function name here is something a human reads
/// in a generated module — keep them bounded.
const MAX_THEME_FUNCTION_NAME_LEN: usize = 96;

fn theme_function_name(name: &str) -> String {
    let mut ident = String::new();
    let mut last_was_underscore = true;

    for ch in name.chars() {
        if ident.len() >= MAX_THEME_FUNCTION_NAME_LEN {
            break;
        }
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

const BASE_THEME_IMPORT_PATH: &str = "ui2::themes::base_theme::base_theme";

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
    extra_keys.extend(
        style
            .token_refs
            .keys()
            .filter(|key| {
                !matches!(
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
                )
            })
            .cloned(),
    );

    let mut extra_keys: Vec<_> = extra_keys.into_iter().collect();
    extra_keys.sort();
    for key in extra_keys {
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
    emit_style_block(
        &mut inner,
        indent + 4,
        "default",
        &variant.default,
        parent.map(|variant| &variant.default),
    );

    let mut size_names: Vec<_> = variant.sizes.keys().cloned().collect();
    size_names.sort();
    for size_name in size_names {
        let Some(style) = variant.sizes.get(&size_name) else {
            continue;
        };
        emit_style_block(
            &mut inner,
            indent + 4,
            &format!("size {}", rust_string_literal(&size_name)),
            style,
            parent.and_then(|variant| variant.sizes.get(&size_name)),
        );
    }

    let mut state_names: Vec<_> = variant.states.keys().cloned().collect();
    state_names.sort();
    for state_name in state_names {
        let Some(style) = variant.states.get(&state_name) else {
            continue;
        };
        emit_style_block(
            &mut inner,
            indent + 4,
            &format!("state {}", rust_string_literal(&state_name)),
            style,
            parent.and_then(|variant| variant.states.get(&state_name)),
        );
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
    emit_style_block(
        &mut inner,
        indent + 4,
        "base",
        &component.base,
        parent.map(|component| &component.base),
    );

    let mut variant_names: Vec<_> = component.variants.keys().cloned().collect();
    variant_names.sort();
    for variant_name in variant_names {
        let Some(variant) = component.variants.get(&variant_name) else {
            continue;
        };
        emit_variant_block(
            &mut inner,
            indent + 4,
            &variant_name,
            variant,
            parent.and_then(|component| component.variants.get(&variant_name)),
        );
    }

    if inner.is_empty() {
        return false;
    }

    push_line(out, indent, format!("component {} {{", rust_string_literal(component_name)));
    out.push_str(&inner);
    push_line(out, indent, "}");
    true
}

fn emit_theme_function(
    out: &mut String,
    _theme_name: &str,
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

    let mut scheme_entries: Vec<(&str, Vec<(String, ExportLiteral)>)> = Vec::new();
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

        let mut changed_entries = Vec::new();
        for (path, value) in token_entries {
            let parent_value = parent_token_set
                .and_then(|tokens| path.split_once('.').and_then(|(category, key)| tokens.get(category, key)))
                .map(token_literal);
            if Some(value.clone()) != parent_value {
                changed_entries.push((path, value));
            }
        }
        scheme_entries.push((scheme_name, changed_entries));
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
        let Some(component) = theme.components.get(&component_name) else {
            continue;
        };
        emit_component_block(
            out,
            8,
            &component_name,
            component,
            parent.and_then(|(_, theme)| theme.components.get(&component_name)),
        );
    }

    push_line(out, 4, "}");
    out.push_str("}\n\n");
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
    } else if is_builtin_base_theme(parent_record) {
        "base_theme()".to_string()
    } else {
        format!("{}()", theme_record_function_name(parent_record))
    };

    Some((parent_expr, parent_theme))
}

fn export_needs_base_theme_import(themes: &[ThemeRecord], theme_idx: usize) -> bool {
    let mut current = explicit_parent_theme_index(themes, theme_idx);
    while let Some(idx) = current {
        let Some(record) = themes.get(idx) else {
            break;
        };
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

fn generate_theme_rust_export(themes: &[ThemeRecord], theme_idx: usize) -> Option<String> {
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
        rust_code.push_str(&format!("use {BASE_THEME_IMPORT_PATH};\n"));
    }
    rust_code.push('\n');

    for idx in chain.iter().copied().filter(|idx| should_inline_parent_theme(themes, *idx, theme_idx)) {
        let Some(record) = themes.get(idx) else {
            continue;
        };
        let function_name = theme_record_function_name(record);
        let parent = export_parent_reference(themes, &export_themes, idx, theme_idx);

        let Some(export_theme) = export_themes.get(&idx) else {
            continue;
        };
        emit_theme_function(
            &mut rust_code,
            &record.meta.name,
            &function_name,
            idx == theme_idx,
            export_theme,
            parent.as_ref().map(|(expr, theme)| (expr.as_str(), *theme)),
        );
    }

    Some(rust_code)
}

fn theme_editor_icon_dir() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources/icons") }

fn sidebar_component_key(category: i32, component: i32) -> Option<&'static str> {
    match (category, component) {
        (0, 0) => Some("button"),
        (0, 1) => Some("icon_button"),
        (0, 15) => Some("icon"),
        (0, 2) => Some("chip"),
        (0, 3) => Some("input"),
        (0, 4) => Some("search"),
        (0, 5) => Some("dropdown"),
        (0, 6) => Some("checkbox"),
        (0, 7) => Some("radio"),
        (0, 14) => Some("radio_group"),
        (0, 8) => Some("switch"),
        (0, 9) => Some("tabs"),
        (0, 10) => Some("slide_to_confirm"),
        (0, 11) => Some("link"),
        (0, 12) => Some("slider"),
        (0, 13) => Some("textarea"),
        (1, 0) => Some("card"),
        (1, 1) => Some("menu"),
        (1, 2) => Some("menu_item"),
        (1, 3) => Some("accordion"),
        (1, 4) => Some("image"),
        (1, 5) => Some("divider"),
        (2, 0) => Some("dialog"),
        (2, 1) => Some("sheet"),
        (2, 2) => Some("toast"),
        (3, 0) => Some("progress_bar"),
        (3, 1) => Some("spinner"),
        _ => None,
    }
}

fn update_icon_results_ui(app: &AppWindow, registry: &IconRegistry, query: &str) {
    app.set_icon_results(slint::ModelRc::new(slint::VecModel::from(registry.filter(query))));
}

fn bump_preview_version(app: &AppWindow) {
    app.set_theme_preview_version(app.get_theme_preview_version() + 1);
}

fn clear_native_window_max_constraints(app: &AppWindow) {
    let _ = app.window().with_winit_window(|window: &winit::window::Window| {
        window.set_max_inner_size(None::<winit::dpi::LogicalSize<f64>>);
    });
}

fn install_winit_window_sync(app: &AppWindow) {
    app.window().on_winit_window_event(|slint_window, event| {
        let _ = slint_window.with_winit_window(|window: &winit::window::Window| match event {
            winit::event::WindowEvent::Resized(size) => {
                let logical = size.to_logical::<f64>(window.scale_factor());
                slint_window.set_size(slint::LogicalSize::new(logical.width as f32, logical.height as f32));
            }
            winit::event::WindowEvent::ScaleFactorChanged { .. } => {
                let size = window.inner_size();
                let logical = size.to_logical::<f64>(window.scale_factor());
                slint_window.set_size(slint::LogicalSize::new(logical.width as f32, logical.height as f32));
            }
            _ => {}
        });
        EventResult::Propagate
    });
}

fn initial_theme_path_arg() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    let first = args.next()?;
    if matches!(first.as_str(), "-h" | "--help") {
        println!("usage: foundation-theme-editor [theme.json]");
        std::process::exit(0);
    }
    Some(PathBuf::from(first))
}

fn load_theme_path_into_slot(
    app: &AppWindow,
    path: &Path,
    themes: &mut [ThemeRecord],
    curr_idx: usize,
    component_key: &str,
    component_plugins: &HashMap<String, PluginDefinition>,
    loaded_plugins: &[(plugin::BuiltinComponentSpec, PluginDefinition)],
) -> Result<(), String> {
    let content = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    let value = serde_json::from_str::<Value>(&content).map_err(|err| err.to_string())?;
    let mut theme = import_theme_record_json(&value, loaded_plugins, themes)?;
    theme.meta.is_builtin = false;

    let fallback_name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or("App Theme");
    if theme.meta.name.trim().is_empty() || theme.meta.name == "Untitled" {
        theme.meta.name = fallback_name.to_string();
    }

    if curr_idx < themes.len() {
        themes[curr_idx] = theme;
    }
    if let Some(theme) = themes.get(curr_idx) {
        load_theme_record_into_ui_with_parent(
            app,
            theme,
            explicit_parent_theme(themes, curr_idx),
            component_plugins,
            component_key,
        );
        app.set_theme_name_draft(theme.meta.name.clone().into());
    }
    update_theme_list_ui(app, themes);
    update_parent_theme_ui(app, themes, curr_idx);
    Ok(())
}

fn default_save_file_name(theme_name: &str) -> String {
    let stem = if theme_name.trim().is_empty() {
        "untitled".to_string()
    } else {
        theme_name.replace(' ', "_").to_lowercase()
    };
    format!("{stem}.json")
}

fn update_undo_redo_ui(app: &AppWindow, undo_stack: &[ThemeEditSnapshot], redo_stack: &[ThemeEditSnapshot]) {
    app.set_can_undo_theme_edit(!undo_stack.is_empty());
    app.set_can_redo_theme_edit(!redo_stack.is_empty());
}

fn record_theme_edit(
    app: &AppWindow,
    themes: &[ThemeRecord],
    theme_idx: usize,
    undo_stack: &mut Vec<ThemeEditSnapshot>,
    redo_stack: &mut Vec<ThemeEditSnapshot>,
) {
    if themes.get(theme_idx).is_some() {
        undo_stack.push(themes.to_vec());
        redo_stack.clear();
        update_undo_redo_ui(app, undo_stack, redo_stack);
    }
}

fn refresh_theme_values_preserving_panel_state(
    app: &AppWindow,
    theme: &ThemeRecord,
    parent_theme: Option<&ThemeRecord>,
    component_plugins: &HashMap<String, PluginDefinition>,
    component_key: &str,
) {
    push_tokens_to_ui(app, &theme.tokens);
    init_theme_global(app, &theme.tokens);

    let Some(plugin) = component_plugins.get(component_key) else {
        bump_preview_version(app);
        return;
    };
    let Some(theme_data) = theme.component_themes.get(component_key) else {
        bump_preview_version(app);
        return;
    };
    let parent_theme_data = parent_theme.and_then(|parent| parent.component_themes.get(component_key));
    let component_parent_theme_data =
        component_parent_data(component_plugins, &theme.component_themes, theme_data);

    let variant_index =
        (app.get_theme_selected_variant_index().max(0) as usize).min(plugin.variants.len().saturating_sub(1));
    if let Some(variant_key) = plugin.variants.get(variant_index) {
        update_generic_variant_values_with_parent(
            app,
            plugin,
            theme_data,
            parent_theme_data,
            component_parent_theme_data,
            variant_key,
        );
    }

    let ui_size_count = plugin.sizes.len() + if component_supports_default_size(plugin) { 1 } else { 0 };
    let size_index =
        (app.get_theme_selected_size_index().max(0) as usize).min(ui_size_count.saturating_sub(1));
    if let Some(size_key) = ui_size_index_to_key(plugin, size_index as i32) {
        update_generic_size_values_with_parent(
            app,
            plugin,
            theme_data,
            parent_theme_data,
            component_parent_theme_data,
            size_key,
        );
    }

    bump_preview_version(app);
}

fn restore_theme_edit_snapshot(
    app: &AppWindow,
    themes: &mut Vec<ThemeRecord>,
    theme_idx: usize,
    component_plugins: &HashMap<String, PluginDefinition>,
    component_key: &str,
    snapshot: ThemeEditSnapshot,
) {
    *themes = snapshot;
    if theme_idx >= themes.len() {
        return;
    }
    refresh_theme_values_preserving_panel_state(
        app,
        &themes[theme_idx],
        explicit_parent_theme(themes, theme_idx),
        component_plugins,
        component_key,
    );
    update_theme_list_ui(app, themes);
    update_parent_theme_ui(app, themes, theme_idx);
    app.set_theme_name_draft(themes[theme_idx].meta.name.clone().into());
    bump_preview_version(app);
}

fn main() {
    slint::BackendSelector::new().backend_name("winit".into()).select().unwrap();
    let initial_theme_path = initial_theme_path_arg();

    let app = AppWindow::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(1440.0, 960.0));
    install_winit_window_sync(&app);
    let app_weak = app.as_weak();
    slint::Timer::single_shot(Duration::ZERO, {
        let app_weak = app_weak.clone();
        move || {
            if let Some(app) = app_weak.upgrade() {
                clear_native_window_max_constraints(&app);
            }
        }
    });
    slint::Timer::single_shot(Duration::from_millis(250), move || {
        if let Some(app) = app_weak.upgrade() {
            clear_native_window_max_constraints(&app);
        }
    });
    let icon_registry = Rc::new(IconRegistry::load_from_dir(&theme_editor_icon_dir()));
    update_icon_results_ui(&app, &icon_registry, "");

    // ===== PLUGIN LOADING =====
    // Ensure default plugin files exist
    if let Err(e) = ensure_default_files() {
        eprintln!("Warning: Could not create default plugin files: {}", e);
    }

    let loaded_plugins = match load_all_plugins() {
        Ok(plugins) => plugins,
        Err(e) => {
            eprintln!("Warning: Could not load component plugins: {}", e);
            Vec::new()
        }
    };
    let component_plugins: Rc<HashMap<String, PluginDefinition>> =
        Rc::new(loaded_plugins.iter().map(|(spec, plugin)| (spec.key.to_string(), plugin.clone())).collect());
    let loaded_plugins_for_theme_io = loaded_plugins.clone();
    update_theme_component_list_ui(&app);

    // Theme model: exactly two records — the built-in Base Theme (the base /
    // inheritance source) and a single editable "working" theme. The working
    // theme starts as a copy of Base; every token/component edit lands on it,
    // and it can be saved to JSON at any time. Base Theme is the only built-in
    // theme.
    const WORKING_THEME_NAME: &str = "Untitled";
    let theme_records = Rc::new(RefCell::new({
        let loaded = load_builtin_theme_records(&loaded_plugins).unwrap_or_else(|error| {
            eprintln!("Warning: Could not load built-in theme JSON files: {error}");
            vec![build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &loaded_plugins)]
        });

        // Collapse whatever was loaded down to the single built-in base.
        let base_idx = base_theme_index(&loaded).unwrap_or(0);
        let mut base_theme = loaded.into_iter().nth(base_idx).unwrap_or_else(|| {
            build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &loaded_plugins)
        });
        base_theme.meta.name = BASE_THEME_NAME.to_string();
        base_theme.meta.is_builtin = true;
        base_theme.meta.parent_name = None;

        // The working theme is a copy of Base, unnamed, based on it.
        let mut working = base_theme.clone();
        working.meta = ThemeMeta {
            name: WORKING_THEME_NAME.to_string(),
            is_builtin: false,
            parent_name: Some(BASE_THEME_NAME.to_string()),
        };
        working.token_overrides.clear();
        clear_component_override_ownership(&mut working);

        vec![base_theme, working]
    }));

    let current_theme_component = Rc::new(RefCell::new(String::from("button")));
    // Index 0 is the built-in Base theme; index 1 is the editable working theme.
    let working_theme_index = 1i32;
    let current_theme_index = Rc::new(RefCell::new(working_theme_index));
    let undo_stack: Rc<RefCell<Vec<ThemeEditSnapshot>>> = Rc::new(RefCell::new(Vec::new()));
    let redo_stack: Rc<RefCell<Vec<ThemeEditSnapshot>>> = Rc::new(RefCell::new(Vec::new()));

    // Initialize theme list in UI
    update_theme_list_ui(&app, &theme_records.borrow());
    update_parent_theme_ui(&app, &theme_records.borrow(), working_theme_index as usize);

    // Set up initial state
    app.set_dark_mode(false);
    app.set_selected_category(0);
    app.set_selected_component(0);
    app.set_btn_selected_variant(0);
    app.set_btn_selected_size(1); // Medium by default
    app.set_selected_theme(working_theme_index);
    app.set_theme_name_draft(WORKING_THEME_NAME.into());
    app.set_theme_selected_component_index(0);
    app.set_theme_selected_component_key("button".into());
    app.set_theme_selected_component_name("Button".into());
    app.set_theme_selected_variant_index(0);
    app.set_theme_selected_size_index(0);

    {
        let records = theme_records.borrow();
        if let Some(theme) = records.get(working_theme_index as usize) {
            load_theme_record_into_ui_with_parent(
                &app,
                theme,
                explicit_parent_theme(&records, working_theme_index as usize),
                component_plugins.as_ref(),
                "button",
            );
        }
    }
    bump_preview_version(&app);

    let fixed_theme_path = Rc::new(RefCell::new(initial_theme_path));
    if let Some(path) = fixed_theme_path.borrow().as_ref() {
        if path.exists() {
            let mut records = theme_records.borrow_mut();
            let component_key = current_theme_component.borrow().clone();
            if let Err(err) = load_theme_path_into_slot(
                &app,
                path,
                &mut records,
                working_theme_index as usize,
                component_key.as_str(),
                component_plugins.as_ref(),
                &loaded_plugins_for_theme_io,
            ) {
                eprintln!("Failed to load theme JSON {}: {}", path.display(), err);
            } else {
                eprintln!("Loaded theme from: {}", path.display());
            }
        } else if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            app.set_theme_name_draft(stem.into());
        }
    }

    // Handle theme toggle
    let app_weak = app.as_weak();
    let theme_records_for_toggle = theme_records.clone();
    let current_theme_for_toggle = current_theme_index.clone();
    let component_plugins_for_toggle = component_plugins.clone();
    let current_theme_component_for_toggle = current_theme_component.clone();
    app.on_toggle_theme(move || {
        let app = app_weak.unwrap();
        app.set_dark_mode(!app.get_dark_mode());
        let component_key = current_theme_component_for_toggle.borrow().clone();
        if let Ok(mut records) = theme_records_for_toggle.try_borrow_mut() {
            let curr_idx = *current_theme_for_toggle.borrow() as usize;
            let parent_component_themes =
                explicit_parent_theme(&records, curr_idx).map(|parent| parent.component_themes.clone());
            if let Some(theme) = records.get_mut(curr_idx) {
                init_theme_global(&app, &theme.tokens);
                load_theme_editor_component_with_parent(
                    &app,
                    component_key.as_str(),
                    component_plugins_for_toggle.as_ref(),
                    &theme.component_themes,
                    parent_component_themes.as_ref(),
                );
                bump_preview_version(&app);
            }
        }
    });

    // Per-component memory of the last-viewed (variant, size) selection, so
    // switching away from a component and back restores where you were.
    let component_selections: Rc<RefCell<HashMap<String, (i32, i32, i32)>>> =
        Rc::new(RefCell::new(HashMap::new()));

    // Handle component selection
    let app_weak = app.as_weak();
    let component_plugins_for_sidebar = component_plugins.clone();
    let theme_records_for_sidebar = theme_records.clone();
    let current_theme_for_sidebar = current_theme_index.clone();
    let current_theme_component_for_sidebar = current_theme_component.clone();
    let component_selections_sidebar = component_selections.clone();
    app.on_select_component(move |category, component| {
        let app = app_weak.unwrap();
        app.set_selected_category(category);
        app.set_selected_component(component);

        let Some(component_key) = sidebar_component_key(category, component) else {
            return;
        };
        let old_key = current_theme_component_for_sidebar.borrow().clone();
        remember_component_selection(&app, &component_selections_sidebar, &old_key, component_key);
        *current_theme_component_for_sidebar.borrow_mut() = component_key.to_string();

        if let Ok(records) = theme_records_for_sidebar.try_borrow() {
            let curr_idx = *current_theme_for_sidebar.borrow() as usize;
            if let Some(theme) = records.get(curr_idx) {
                load_theme_editor_component_with_parent(
                    &app,
                    component_key,
                    component_plugins_for_sidebar.as_ref(),
                    &theme.component_themes,
                    explicit_parent_theme(&records, curr_idx).map(|parent| &parent.component_themes),
                );
                bump_preview_version(&app);
            }
        }
    });

    let app_weak = app.as_weak();
    let component_plugins_for_selection = component_plugins.clone();
    let theme_records_for_selection = theme_records.clone();
    let current_theme_for_selection = current_theme_index.clone();
    let current_theme_component_for_selection = current_theme_component.clone();
    let component_selections_selection = component_selections.clone();
    app.on_component_theme_selected(move |component_key| {
        let app = app_weak.unwrap();
        let key = component_key.to_string();
        let old_key = current_theme_component_for_selection.borrow().clone();
        remember_component_selection(&app, &component_selections_selection, &old_key, &key);
        *current_theme_component_for_selection.borrow_mut() = key.clone();

        if let Ok(records) = theme_records_for_selection.try_borrow() {
            let curr_idx = *current_theme_for_selection.borrow() as usize;
            if let Some(theme) = records.get(curr_idx) {
                load_theme_editor_component_with_parent(
                    &app,
                    &key,
                    component_plugins_for_selection.as_ref(),
                    &theme.component_themes,
                    explicit_parent_theme(&records, curr_idx).map(|parent| &parent.component_themes),
                );
                bump_preview_version(&app);
            }
        }
    });

    let icon_registry_for_lookup = icon_registry.clone();
    app.on_icon_image(move |name| icon_registry_for_lookup.image(name.as_str()));

    let app_weak = app.as_weak();
    let icon_registry_for_filter = icon_registry.clone();
    app.on_filter_icons(move |query| {
        let app = app_weak.unwrap();
        update_icon_results_ui(&app, &icon_registry_for_filter, query.as_str());
    });

    let theme_records_for_preview_variant = theme_records.clone();
    let current_theme_for_preview_variant = current_theme_index.clone();
    let preview_variant_app = app.as_weak();
    app.on_preview_variant_prop(move |component_key, variant_key, state_key, prop_name, _version| {
        let records = theme_records_for_preview_variant.borrow();
        records
            .get(*current_theme_for_preview_variant.borrow() as usize)
            .map(|theme| {
                let app = preview_variant_app.unwrap();
                preview_variant_property_value(
                    &app,
                    &theme.component_themes,
                    component_key.as_str(),
                    variant_key.as_str(),
                    state_key.as_str(),
                    prop_name.as_str(),
                )
            })
            .unwrap_or_else(default_property_value_data)
    });

    let theme_records_for_preview_size = theme_records.clone();
    let current_theme_for_preview_size = current_theme_index.clone();
    let preview_size_app = app.as_weak();
    app.on_preview_size_prop(move |component_key, size_key, prop_name, _version| {
        let records = theme_records_for_preview_size.borrow();
        records
            .get(*current_theme_for_preview_size.borrow() as usize)
            .map(|theme| {
                let app = preview_size_app.unwrap();
                preview_size_property_value(
                    &app,
                    &theme.component_themes,
                    component_key.as_str(),
                    size_key.as_str(),
                    prop_name.as_str(),
                )
            })
            .unwrap_or_else(default_property_value_data)
    });

    let app_weak = app.as_weak();
    let theme_records_for_undo = theme_records.clone();
    let current_theme_for_undo = current_theme_index.clone();
    let component_plugins_for_undo = component_plugins.clone();
    let current_theme_component_for_undo = current_theme_component.clone();
    let undo_stack_for_undo = undo_stack.clone();
    let redo_stack_for_undo = redo_stack.clone();
    app.on_undo_theme_edit(move || {
        let app = app_weak.unwrap();
        let mut undo = undo_stack_for_undo.borrow_mut();
        let Some(snapshot) = undo.pop() else {
            update_undo_redo_ui(&app, &undo, &redo_stack_for_undo.borrow());
            return;
        };

        let mut themes = theme_records_for_undo.borrow_mut();
        let curr_idx = *current_theme_for_undo.borrow() as usize;
        if themes.get(curr_idx).is_some() {
            let mut redo = redo_stack_for_undo.borrow_mut();
            redo.push(themes.clone());
            let component_key = current_theme_component_for_undo.borrow().clone();
            restore_theme_edit_snapshot(
                &app,
                &mut themes,
                curr_idx,
                component_plugins_for_undo.as_ref(),
                component_key.as_str(),
                snapshot,
            );
            update_undo_redo_ui(&app, &undo, &redo);
        }
    });

    let app_weak = app.as_weak();
    let theme_records_for_redo = theme_records.clone();
    let current_theme_for_redo = current_theme_index.clone();
    let component_plugins_for_redo = component_plugins.clone();
    let current_theme_component_for_redo = current_theme_component.clone();
    let undo_stack_for_redo = undo_stack.clone();
    let redo_stack_for_redo = redo_stack.clone();
    app.on_redo_theme_edit(move || {
        let app = app_weak.unwrap();
        let mut redo = redo_stack_for_redo.borrow_mut();
        let Some(snapshot) = redo.pop() else {
            update_undo_redo_ui(&app, &undo_stack_for_redo.borrow(), &redo);
            return;
        };

        let mut themes = theme_records_for_redo.borrow_mut();
        let curr_idx = *current_theme_for_redo.borrow() as usize;
        if themes.get(curr_idx).is_some() {
            let mut undo = undo_stack_for_redo.borrow_mut();
            undo.push(themes.clone());
            let component_key = current_theme_component_for_redo.borrow().clone();
            restore_theme_edit_snapshot(
                &app,
                &mut themes,
                curr_idx,
                component_plugins_for_redo.as_ref(),
                component_key.as_str(),
                snapshot,
            );
            update_undo_redo_ui(&app, &undo, &redo);
        }
    });

    // Handle button variant change
    let app_weak = app.as_weak();
    let theme_records_for_variant = theme_records.clone();
    let current_theme_for_variant = current_theme_index.clone();
    let component_plugins_for_variant = component_plugins.clone();
    let current_theme_component_for_variant = current_theme_component.clone();
    app.on_btn_variant_changed(move |new_variant_index| {
        let app = app_weak.unwrap();

        app.set_theme_selected_variant_index(new_variant_index);
        app.set_theme_preview_state_index(0);
        let component_key = current_theme_component_for_variant.borrow().clone();
        if let Some(plugin) = component_plugins_for_variant.get(component_key.as_str()) {
            if let Ok(records) = theme_records_for_variant.try_borrow() {
                let curr_idx = *current_theme_for_variant.borrow() as usize;
                if let Some(theme) = records.get(curr_idx) {
                    if let Some(theme_data) = theme.component_themes.get(component_key.as_str()) {
                        if let Some(variant_key) = plugin.variants.get(new_variant_index as usize) {
                            let parent_theme_data = explicit_parent_theme(&records, curr_idx)
                                .and_then(|parent| parent.component_themes.get(component_key.as_str()));
                            let component_parent_theme_data = component_parent_data(
                                component_plugins_for_variant.as_ref(),
                                &theme.component_themes,
                                theme_data,
                            );
                            update_generic_variant_values_with_parent(
                                &app,
                                plugin,
                                theme_data,
                                parent_theme_data,
                                component_parent_theme_data,
                                variant_key,
                            );
                            bump_preview_version(&app);
                        }
                    }
                }
            }
        }
    });

    // Handle button size change
    let app_weak = app.as_weak();
    let theme_records_for_size = theme_records.clone();
    let current_theme_for_size = current_theme_index.clone();
    let component_plugins_for_size = component_plugins.clone();
    let current_theme_component_for_size = current_theme_component.clone();
    app.on_btn_size_changed(move |new_size_index| {
        let app = app_weak.unwrap();

        app.set_theme_selected_size_index(new_size_index);
        app.set_theme_preview_state_index(0);
        let component_key = current_theme_component_for_size.borrow().clone();
        if let Some(plugin) = component_plugins_for_size.get(component_key.as_str()) {
            if let Ok(records) = theme_records_for_size.try_borrow() {
                let curr_idx = *current_theme_for_size.borrow() as usize;
                if let Some(theme) = records.get(curr_idx) {
                    if let Some(theme_data) = theme.component_themes.get(component_key.as_str()) {
                        if let Some(size_key) = ui_size_index_to_key(plugin, new_size_index) {
                            let parent_theme_data = explicit_parent_theme(&records, curr_idx)
                                .and_then(|parent| parent.component_themes.get(component_key.as_str()));
                            let component_parent_theme_data = component_parent_data(
                                component_plugins_for_size.as_ref(),
                                &theme.component_themes,
                                theme_data,
                            );
                            update_generic_size_values_with_parent(
                                &app,
                                plugin,
                                theme_data,
                                parent_theme_data,
                                component_parent_theme_data,
                                size_key,
                            );
                            bump_preview_version(&app);
                        }
                    }
                }
            }
        }
    });

    let app_weak = app.as_weak();
    let theme_records_variant_edit = theme_records.clone();
    let current_theme_variant_edit = current_theme_index.clone();
    let component_plugins_variant_edit = component_plugins.clone();
    let current_theme_component_variant_edit = current_theme_component.clone();
    let undo_stack_variant_edit = undo_stack.clone();
    let redo_stack_variant_edit = redo_stack.clone();
    app.on_variant_prop_changed(move |state_index, prop_index, new_value| {
        let app = app_weak.unwrap();
        let component_key = current_theme_component_variant_edit.borrow().clone();

        let Some(plugin) = component_plugins_variant_edit.get(component_key.as_str()) else {
            return;
        };
        let Some(state_key) = plugin.states.get(state_index as usize) else {
            return;
        };
        let Some(prop) = plugin.variant_props.get(prop_index as usize) else {
            return;
        };
        let variant_index = app.get_theme_selected_variant_index().max(0) as usize;
        let Some(variant_key) = plugin.variants.get(variant_index) else {
            return;
        };

        if let Ok(mut records) = theme_records_variant_edit.try_borrow_mut() {
            let curr_idx = *current_theme_variant_edit.borrow() as usize;
            let value = property_value_from_ui(prop, &new_value);
            let already_matches = records
                .get(curr_idx)
                .and_then(|theme| theme.component_themes.get(component_key.as_str()))
                .and_then(|theme_data| theme_data.variant_props.get(variant_key))
                .and_then(|states| states.get(state_key))
                .and_then(|props| props.get(&prop.name))
                .map(|current| current == &value)
                .unwrap_or(false);

            if !already_matches {
                let mut undo = undo_stack_variant_edit.borrow_mut();
                let mut redo = redo_stack_variant_edit.borrow_mut();
                record_theme_edit(&app, &records, curr_idx, &mut undo, &mut redo);
            }

            let parent_theme_data = explicit_parent_theme(&records, curr_idx)
                .and_then(|parent| parent.component_themes.get(component_key.as_str()))
                .cloned();
            if let Some(theme) = records.get_mut(curr_idx) {
                if let Some(theme_data) = theme.component_themes.get_mut(component_key.as_str()) {
                    if !already_matches {
                        theme_data.set_variant_override(variant_key, state_key, &prop.name, value);
                    }
                }
                if !already_matches {
                    let component_parent_theme_data = theme
                        .component_themes
                        .get(component_key.as_str())
                        .and_then(|theme_data| {
                            component_parent_data(
                                component_plugins_variant_edit.as_ref(),
                                &theme.component_themes,
                                theme_data,
                            )
                        })
                        .cloned();
                    let fallback_value =
                        theme.component_themes.get(component_key.as_str()).and_then(|theme_data| {
                            variant_prop_fallback_value(
                                theme_data,
                                parent_theme_data.as_ref(),
                                component_parent_theme_data.as_ref(),
                                variant_key,
                                state_key,
                                &prop.name,
                            )
                        });
                    let stored_value = theme
                        .component_themes
                        .get(component_key.as_str())
                        .and_then(|theme_data| {
                            variant_prop_value(theme_data, variant_key, state_key, &prop.name)
                        })
                        .cloned();
                    if let (Some(fallback_value), Some(stored_value), Some(theme_data)) =
                        (fallback_value, stored_value, theme.component_themes.get_mut(component_key.as_str()))
                    {
                        let is_overridden = !property_value_matches(&stored_value, &fallback_value);
                        theme_data.set_variant_resolved_value(
                            variant_key,
                            state_key,
                            &prop.name,
                            stored_value,
                            is_overridden,
                        );
                    }
                }
                if let Some(theme_data) = theme.component_themes.get(component_key.as_str()) {
                    let component_parent_theme_data = component_parent_data(
                        component_plugins_variant_edit.as_ref(),
                        &theme.component_themes,
                        theme_data,
                    );
                    update_generic_variant_values_with_parent(
                        &app,
                        plugin,
                        theme_data,
                        parent_theme_data.as_ref(),
                        component_parent_theme_data,
                        variant_key,
                    );
                    bump_preview_version(&app);
                }
            }
        }
    });

    let app_weak = app.as_weak();
    let theme_records_clear_variant = theme_records.clone();
    let current_theme_clear_variant = current_theme_index.clone();
    let component_plugins_clear_variant = component_plugins.clone();
    let current_theme_component_clear_variant = current_theme_component.clone();
    let undo_stack_clear_variant = undo_stack.clone();
    let redo_stack_clear_variant = redo_stack.clone();
    app.on_clear_variant_override(move |state_index, prop_index| {
        let app = app_weak.unwrap();
        let component_key = current_theme_component_clear_variant.borrow().clone();
        let Some(plugin) = component_plugins_clear_variant.get(component_key.as_str()) else {
            return;
        };
        let Some(state_key) = plugin.states.get(state_index as usize) else {
            return;
        };
        let Some(prop) = plugin.variant_props.get(prop_index as usize) else {
            return;
        };
        let variant_index = app.get_theme_selected_variant_index().max(0) as usize;
        let Some(variant_key) = plugin.variants.get(variant_index) else {
            return;
        };

        if let Ok(mut records) = theme_records_clear_variant.try_borrow_mut() {
            let curr_idx = *current_theme_clear_variant.borrow() as usize;
            let parent_theme_data = explicit_parent_theme(&records, curr_idx)
                .and_then(|parent| parent.component_themes.get(component_key.as_str()))
                .cloned();
            let has_override = records
                .get(curr_idx)
                .and_then(|theme| theme.component_themes.get(component_key.as_str()))
                .map(|theme_data| {
                    let value = variant_prop_value(theme_data, variant_key, state_key, &prop.name);
                    value
                        .map(|value| {
                            let component_parent_theme_data = records.get(curr_idx).and_then(|theme| {
                                component_parent_data(
                                    component_plugins_clear_variant.as_ref(),
                                    &theme.component_themes,
                                    theme_data,
                                )
                            });
                            variant_prop_is_overridden_for_ui(
                                theme_data,
                                parent_theme_data.as_ref(),
                                component_parent_theme_data,
                                variant_key,
                                state_key,
                                &prop.name,
                                value,
                            )
                        })
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if !has_override {
                return;
            }
            {
                let mut undo = undo_stack_clear_variant.borrow_mut();
                let mut redo = redo_stack_clear_variant.borrow_mut();
                record_theme_edit(&app, &records, curr_idx, &mut undo, &mut redo);
            }
            if let Some(theme) = records.get_mut(curr_idx) {
                let token_store = token_store_from_theme(&theme.tokens);
                let component_parent_theme_data = theme
                    .component_themes
                    .get(component_key.as_str())
                    .and_then(|theme_data| {
                        component_parent_data(
                            component_plugins_clear_variant.as_ref(),
                            &theme.component_themes,
                            theme_data,
                        )
                    })
                    .cloned();
                let fallback_value =
                    theme.component_themes.get(component_key.as_str()).and_then(|theme_data| {
                        variant_prop_fallback_value(
                            theme_data,
                            parent_theme_data.as_ref(),
                            component_parent_theme_data.as_ref(),
                            variant_key,
                            state_key,
                            &prop.name,
                        )
                    });
                if let Some(theme_data) = theme.component_themes.get_mut(component_key.as_str()) {
                    if let Some(fallback_value) = fallback_value {
                        theme_data.set_variant_resolved_value(
                            variant_key,
                            state_key,
                            &prop.name,
                            fallback_value,
                            false,
                        );
                    } else if theme_data.variant_state_is_root(variant_key, state_key) {
                        theme_data.clear_variant_default(state_key, &prop.name, &token_store);
                    } else {
                        theme_data.clear_variant_override(variant_key, state_key, &prop.name);
                    }
                }
                if let Some(theme_data) = theme.component_themes.get(component_key.as_str()) {
                    let component_parent_theme_data = component_parent_data(
                        component_plugins_clear_variant.as_ref(),
                        &theme.component_themes,
                        theme_data,
                    );
                    update_generic_variant_values_with_parent(
                        &app,
                        plugin,
                        theme_data,
                        parent_theme_data.as_ref(),
                        component_parent_theme_data,
                        variant_key,
                    );
                    bump_preview_version(&app);
                }
            }
        }
    });

    let app_weak = app.as_weak();
    let theme_records_size_edit = theme_records.clone();
    let current_theme_size_edit = current_theme_index.clone();
    let component_plugins_size_edit = component_plugins.clone();
    let current_theme_component_size_edit = current_theme_component.clone();
    let undo_stack_size_edit = undo_stack.clone();
    let redo_stack_size_edit = redo_stack.clone();
    app.on_size_prop_changed(move |size_index, prop_index, new_value| {
        let app = app_weak.unwrap();
        let component_key = current_theme_component_size_edit.borrow().clone();

        let Some(plugin) = component_plugins_size_edit.get(component_key.as_str()) else {
            return;
        };
        let Some(prop) = plugin.size_props.get(prop_index as usize) else {
            return;
        };
        // UI index 0 = virtual Common base (cascades to every inheriting
        // size); 1..N = the concrete sizes from plugin.sizes.
        let Some(size_key) = ui_size_index_to_key(plugin, size_index) else {
            return;
        };
        let size_key = size_key.to_string();
        let value = property_value_from_ui(prop, &new_value);

        if let Ok(mut records) = theme_records_size_edit.try_borrow_mut() {
            let curr_idx = *current_theme_size_edit.borrow() as usize;
            let already_matches = records
                .get(curr_idx)
                .and_then(|theme| theme.component_themes.get(component_key.as_str()))
                .and_then(|theme_data| theme_data.size_props.get(&size_key))
                .and_then(|m| m.get(&prop.name))
                .map(|current| current == &value)
                .unwrap_or(false);
            if !already_matches {
                let mut undo = undo_stack_size_edit.borrow_mut();
                let mut redo = redo_stack_size_edit.borrow_mut();
                record_theme_edit(&app, &records, curr_idx, &mut undo, &mut redo);
            }

            let parent_theme_data = explicit_parent_theme(&records, curr_idx)
                .and_then(|parent| parent.component_themes.get(component_key.as_str()))
                .cloned();
            if let Some(theme) = records.get_mut(curr_idx) {
                let is_default_edit = size_key == crate::plugin::DEFAULT_SIZE_KEY;
                {
                    if let Some(theme_data) = theme.component_themes.get_mut(component_key.as_str()) {
                        // Belt-and-suspenders against the Slint sync echo: when
                        // the value we're being told to "set" equals what's
                        // already stored, this isn't a real user edit — it's a
                        // changed-value callback that Slint fired because some
                        // other prop's reset replaced the current_size_values
                        // model. Treat as no-op so we don't accidentally promote
                        // an inheriting prop to a (fake) local override.
                        if is_default_edit {
                            if !already_matches {
                                theme_data.set_size_default(&prop.name, value.clone());
                                theme_data.mark_size_default_override(&prop.name);
                            }
                            // If this is a child component AND it's a real edit,
                            // the Common value becomes a local override — parent
                            // edits won't reach it.
                            if !already_matches && theme_data.parent_key.is_some() {
                                theme_data.mark_parent_override(&prop.name);
                            }
                        } else if !already_matches {
                            theme_data.set_size_override(&size_key, &prop.name, value.clone());
                        }
                    }
                }
                // Cross-component cascade: a Common-base edit on the parent
                // also updates every descendant that is still inheriting.
                if is_default_edit {
                    let parent_component_name = plugin.component.clone();
                    crate::plugin::cascade_default_to_children(
                        &mut theme.component_themes,
                        &parent_component_name,
                        &prop.name,
                        &value,
                    );
                }
                if let Some(theme_data) = theme.component_themes.get(component_key.as_str()) {
                    let component_parent_theme_data = component_parent_data(
                        component_plugins_size_edit.as_ref(),
                        &theme.component_themes,
                        theme_data,
                    );
                    update_generic_size_values_with_parent(
                        &app,
                        plugin,
                        theme_data,
                        parent_theme_data.as_ref(),
                        component_parent_theme_data,
                        &size_key,
                    );
                    bump_preview_version(&app);
                }
            }
        }
    });

    // Reset (↺) on a per-size row: clear that size's override and re-inherit
    // the value from the Common base.
    let app_weak = app.as_weak();
    let theme_records_clear = theme_records.clone();
    let current_theme_clear = current_theme_index.clone();
    let component_plugins_clear = component_plugins.clone();
    let current_theme_component_clear = current_theme_component.clone();
    let undo_stack_clear = undo_stack.clone();
    let redo_stack_clear = redo_stack.clone();
    app.on_clear_size_override(move |size_index, prop_index| {
        let app = app_weak.unwrap();
        let component_key = current_theme_component_clear.borrow().clone();
        let Some(plugin) = component_plugins_clear.get(component_key.as_str()) else {
            return;
        };
        let Some(prop) = plugin.size_props.get(prop_index as usize) else {
            return;
        };
        let Some(size_key) = ui_size_index_to_key(plugin, size_index) else {
            return;
        };
        let size_key = size_key.to_string();
        if let Ok(mut records) = theme_records_clear.try_borrow_mut() {
            let curr_idx = *current_theme_clear.borrow() as usize;
            let parent_theme_data = explicit_parent_theme(&records, curr_idx)
                .and_then(|parent| parent.component_themes.get(component_key.as_str()))
                .cloned();
            let has_override = records
                .get(curr_idx)
                .and_then(|theme| theme.component_themes.get(component_key.as_str()))
                .map(|theme_data| {
                    if size_key == crate::plugin::DEFAULT_SIZE_KEY {
                        let value = theme_data
                            .size_props
                            .get(crate::plugin::DEFAULT_SIZE_KEY)
                            .and_then(|props| props.get(&prop.name));
                        value
                            .map(|value| {
                                let component_parent_theme_data = records.get(curr_idx).and_then(|theme| {
                                    component_parent_data(
                                        component_plugins_clear.as_ref(),
                                        &theme.component_themes,
                                        theme_data,
                                    )
                                });
                                size_prop_is_overridden_for_ui(
                                    theme_data,
                                    parent_theme_data.as_ref(),
                                    component_parent_theme_data,
                                    crate::plugin::DEFAULT_SIZE_KEY,
                                    &prop.name,
                                    value,
                                )
                            })
                            .unwrap_or(false)
                    } else {
                        theme_data.size_prop_is_overridden(&size_key, &prop.name)
                    }
                })
                .unwrap_or(false);
            if !has_override {
                return;
            }
            {
                let mut undo = undo_stack_clear.borrow_mut();
                let mut redo = redo_stack_clear.borrow_mut();
                record_theme_edit(&app, &records, curr_idx, &mut undo, &mut redo);
            }
            if let Some(theme) = records.get_mut(curr_idx) {
                if size_key == crate::plugin::DEFAULT_SIZE_KEY {
                    let token_store = token_store_from_theme(&theme.tokens);
                    // Common row reset: inherited component props reset to
                    // their component parent (Input). Child-only props reset to
                    // the selected Base theme, or to the schema seed when
                    // editing the Base theme against None.
                    let component_parent_value = theme
                        .component_themes
                        .get(component_key.as_str())
                        .and_then(|theme_data| {
                            component_parent_data(
                                component_plugins_clear.as_ref(),
                                &theme.component_themes,
                                theme_data,
                            )
                        })
                        .and_then(|parent| parent.size_props.get(crate::plugin::DEFAULT_SIZE_KEY))
                        .and_then(|props| props.get(&prop.name))
                        .cloned();
                    let has_component_parent_value = component_parent_value.is_some();
                    let parent_value = component_parent_value.or_else(|| {
                        parent_theme_data
                            .as_ref()
                            .and_then(|parent| parent.size_props.get(crate::plugin::DEFAULT_SIZE_KEY))
                            .and_then(|props| props.get(&prop.name))
                            .cloned()
                    });
                    if let Some(parent_value) = parent_value {
                        if let Some(theme_data) = theme.component_themes.get_mut(component_key.as_str()) {
                            if has_component_parent_value {
                                theme_data.clear_parent_override(&prop.name, parent_value);
                            } else {
                                theme_data.clear_size_default_to_value(&prop.name, parent_value);
                            }
                        }
                        if let Some(theme_data) = theme.component_themes.get(component_key.as_str()) {
                            let component_parent_theme_data = component_parent_data(
                                component_plugins_clear.as_ref(),
                                &theme.component_themes,
                                theme_data,
                            );
                            update_generic_size_values_with_parent(
                                &app,
                                plugin,
                                theme_data,
                                parent_theme_data.as_ref(),
                                component_parent_theme_data,
                                &size_key,
                            );
                            bump_preview_version(&app);
                        }
                    } else {
                        if let Some(theme_data) = theme.component_themes.get_mut(component_key.as_str()) {
                            theme_data.clear_size_default(&prop.name, &token_store);
                            theme_data.mark_size_default_override(&prop.name);
                        }
                        if let Some(theme_data) = theme.component_themes.get(component_key.as_str()) {
                            let component_parent_theme_data = component_parent_data(
                                component_plugins_clear.as_ref(),
                                &theme.component_themes,
                                theme_data,
                            );
                            update_generic_size_values_with_parent(
                                &app,
                                plugin,
                                theme_data,
                                parent_theme_data.as_ref(),
                                component_parent_theme_data,
                                &size_key,
                            );
                            bump_preview_version(&app);
                        }
                    }
                } else {
                    if let Some(theme_data) = theme.component_themes.get_mut(component_key.as_str()) {
                        let before: Vec<String> = theme_data
                            .size_overrides
                            .get(&size_key)
                            .map(|s| s.iter().cloned().collect())
                            .unwrap_or_default();
                        theme_data.clear_size_override(&size_key, &prop.name);
                        let after: Vec<String> = theme_data
                            .size_overrides
                            .get(&size_key)
                            .map(|s| s.iter().cloned().collect())
                            .unwrap_or_default();
                        eprintln!(
                            "[clear-size-override] {}/{}/{}  before={:?}  after={:?}",
                            component_key, size_key, prop.name, before, after
                        );
                        update_generic_size_values_with_parent(
                            &app,
                            plugin,
                            theme_data,
                            parent_theme_data.as_ref(),
                            None,
                            &size_key,
                        );
                        bump_preview_version(&app);
                    }
                }
            }
        }
    });

    // Keep token-backed runtime values in sync with shared token edits.
    let app_weak = app.as_weak();
    let theme_records_token_live = theme_records.clone();
    let current_theme_token_live = current_theme_index.clone();
    let component_plugins_token_live = component_plugins.clone();
    let current_theme_component_token_live = current_theme_component.clone();
    let undo_stack_token_live = undo_stack.clone();
    let redo_stack_token_live = redo_stack.clone();
    app.on_theme_tokens_edited(move || {
        let app = app_weak.unwrap();
        let curr_theme = *current_theme_token_live.borrow() as usize;

        if let Ok(mut themes) = theme_records_token_live.try_borrow_mut() {
            let Some(current) = themes.get(curr_theme) else {
                return;
            };
            let mut edited_tokens = current.tokens.clone();
            pull_tokens_from_ui(&app, &mut edited_tokens);
            if edited_tokens == current.tokens {
                return;
            }
            {
                let mut undo = undo_stack_token_live.borrow_mut();
                let mut redo = redo_stack_token_live.borrow_mut();
                record_theme_edit(&app, &themes, curr_theme, &mut undo, &mut redo);
            }
            let parent_component_themes =
                explicit_parent_theme(&themes, curr_theme).map(|parent| parent.component_themes.clone());
            if let Some(theme) = themes.get_mut(curr_theme) {
                theme.tokens = edited_tokens;
                app.set_theme_font_family(app.get_token_font_primary());
                init_theme_global(&app, &theme.tokens);
                let component_key = current_theme_component_token_live.borrow().clone();
                load_theme_editor_component_with_parent(
                    &app,
                    component_key.as_str(),
                    component_plugins_token_live.as_ref(),
                    &theme.component_themes,
                    parent_component_themes.as_ref(),
                );
                bump_preview_version(&app);
            }
        }
    });

    // Handle theme change (token themes)
    let app_weak = app.as_weak();
    let theme_records_on_theme_change = theme_records.clone();
    let current_theme_clone = current_theme_index.clone();
    let component_plugins_on_theme_change = component_plugins.clone();
    let current_theme_component_on_theme_change = current_theme_component.clone();
    app.on_theme_changed(move |new_theme_index| {
        let app = app_weak.unwrap();
        let mut themes = theme_records_on_theme_change.borrow_mut();
        let mut curr_idx = current_theme_clone.borrow_mut();

        // Skip if same theme
        if *curr_idx == new_theme_index {
            return;
        }

        // Save current theme's token values from UI
        if let Some(current_theme) = themes.get_mut(*curr_idx as usize) {
            pull_tokens_from_ui(&app, &mut current_theme.tokens);
        }

        // Update current theme index
        *curr_idx = new_theme_index;

        // Load new theme's token values to UI
        if let Some(new_theme) = themes.get(new_theme_index as usize) {
            let component_key = current_theme_component_on_theme_change.borrow().clone();
            load_theme_record_into_ui_with_parent(
                &app,
                new_theme,
                explicit_parent_theme(&themes, new_theme_index as usize),
                component_plugins_on_theme_change.as_ref(),
                component_key.as_str(),
            );
            update_parent_theme_ui(&app, &themes, new_theme_index as usize);
        }

        // Update UI selection AFTER load completes
        app.set_selected_theme(new_theme_index);
    });

    let app_weak = app.as_weak();
    let theme_records_parent_change = theme_records.clone();
    let current_theme_parent_change = current_theme_index.clone();
    let component_plugins_parent_change = component_plugins.clone();
    let current_theme_component_parent_change = current_theme_component.clone();
    let undo_stack_parent_change = undo_stack.clone();
    let redo_stack_parent_change = redo_stack.clone();
    app.on_theme_parent_changed(move |new_parent_name| {
        let app = app_weak.unwrap();
        let mut themes = theme_records_parent_change.borrow_mut();
        let curr_idx = *current_theme_parent_change.borrow() as usize;

        let Some(theme) = themes.get(curr_idx) else {
            return;
        };
        if theme.meta.is_builtin {
            update_parent_theme_ui(&app, &themes, curr_idx);
            return;
        }

        let selected_name = new_parent_name.trim();
        let next_parent = if selected_name.is_empty() || selected_name == "None" {
            None
        } else {
            Some(selected_name.to_string())
        };
        if theme.meta.parent_name == next_parent {
            return;
        }

        if let Some(parent_name) = next_parent.as_deref() {
            if theme_parent_would_cycle(&themes, curr_idx, parent_name) {
                eprintln!("Cannot assign parent theme '{parent_name}' because it would create a cycle");
                update_parent_theme_ui(&app, &themes, curr_idx);
                return;
            }
        }

        {
            let mut undo = undo_stack_parent_change.borrow_mut();
            let mut redo = redo_stack_parent_change.borrow_mut();
            record_theme_edit(&app, &themes, curr_idx, &mut undo, &mut redo);
        }
        if let Some(theme) = themes.get_mut(curr_idx) {
            theme.meta.parent_name = next_parent;
            if theme.meta.parent_name.is_none() {
                theme.token_overrides = all_token_override_keys();
            }
        }

        update_parent_theme_ui(&app, &themes, curr_idx);
        if let Some(theme) = themes.get(curr_idx) {
            let component_key = current_theme_component_parent_change.borrow().clone();
            load_theme_record_into_ui_with_parent(
                &app,
                theme,
                explicit_parent_theme(&themes, curr_idx),
                component_plugins_parent_change.as_ref(),
                component_key.as_str(),
            );
        }
    });

    // Handle token value changes (immediate sync to Rust storage)
    let app_weak = app.as_weak();
    let theme_records_for_token_change = theme_records.clone();
    let current_theme_clone = current_theme_index.clone();
    let component_plugins_token_change = component_plugins.clone();
    let current_theme_component_token_change = current_theme_component.clone();
    let undo_stack_token_change = undo_stack.clone();
    let redo_stack_token_change = redo_stack.clone();
    app.on_token_value_changed(move |token_name, new_color, is_dark| {
        let app = app_weak.unwrap();
        let curr_idx = *current_theme_clone.borrow() as usize;
        let component_key = current_theme_component_token_change.borrow().clone();

        let mut themes = theme_records_for_token_change.borrow_mut();

        let token_name = token_name.as_str();
        let current_color = themes.get(curr_idx).and_then(|theme| {
            if is_dark {
                match token_name {
                    "primary" => Some(theme.tokens.dark_primary),
                    "primary-pressed" => Some(theme.tokens.dark_primary_pressed),
                    "secondary" => Some(theme.tokens.dark_secondary),
                    "danger" => Some(theme.tokens.dark_danger),
                    "surface" => Some(theme.tokens.dark_surface),
                    "background" => Some(theme.tokens.dark_background),
                    "text" => Some(theme.tokens.dark_text),
                    "text-muted" => Some(theme.tokens.dark_text_muted),
                    _ => None,
                }
            } else {
                match token_name {
                    "primary" => Some(theme.tokens.light_primary),
                    "primary-pressed" => Some(theme.tokens.light_primary_pressed),
                    "secondary" => Some(theme.tokens.light_secondary),
                    "danger" => Some(theme.tokens.light_danger),
                    "surface" => Some(theme.tokens.light_surface),
                    "background" => Some(theme.tokens.light_background),
                    "text" => Some(theme.tokens.light_text),
                    "text-muted" => Some(theme.tokens.light_text_muted),
                    _ => None,
                }
            }
        });
        if current_color == Some(new_color) {
            return;
        }
        {
            let mut undo = undo_stack_token_change.borrow_mut();
            let mut redo = redo_stack_token_change.borrow_mut();
            record_theme_edit(&app, &themes, curr_idx, &mut undo, &mut redo);
        }
        let parent_component_themes =
            explicit_parent_theme(&themes, curr_idx).map(|parent| parent.component_themes.clone());
        if let Some(theme) = themes.get_mut(curr_idx) {
            if is_dark {
                match token_name {
                    "primary" => theme.tokens.dark_primary = new_color,
                    "primary-pressed" => theme.tokens.dark_primary_pressed = new_color,
                    "secondary" => theme.tokens.dark_secondary = new_color,
                    "danger" => theme.tokens.dark_danger = new_color,
                    "surface" => theme.tokens.dark_surface = new_color,
                    "background" => theme.tokens.dark_background = new_color,
                    "text" => theme.tokens.dark_text = new_color,
                    "text-muted" => theme.tokens.dark_text_muted = new_color,
                    _ => {}
                }
            } else {
                match token_name {
                    "primary" => theme.tokens.light_primary = new_color,
                    "primary-pressed" => theme.tokens.light_primary_pressed = new_color,
                    "secondary" => theme.tokens.light_secondary = new_color,
                    "danger" => theme.tokens.light_danger = new_color,
                    "surface" => theme.tokens.light_surface = new_color,
                    "background" => theme.tokens.light_background = new_color,
                    "text" => theme.tokens.light_text = new_color,
                    "text-muted" => theme.tokens.light_text_muted = new_color,
                    _ => {}
                }
            }
            if let Some(path) = color_token_override_path(token_name, is_dark) {
                theme.token_overrides.insert(path);
            }

            init_theme_global(&app, &theme.tokens);
            load_theme_editor_component_with_parent(
                &app,
                component_key.as_str(),
                component_plugins_token_change.as_ref(),
                &theme.component_themes,
                parent_component_themes.as_ref(),
            );
            bump_preview_version(&app);
        }
    });

    // Handle "New Theme": confirm discard, then revert the working theme to its
    // base values and clear its name.
    let app_weak = app.as_weak();
    let theme_records_new = theme_records.clone();
    let current_theme_new = current_theme_index.clone();
    let current_theme_component_new = current_theme_component.clone();
    let component_plugins_new = component_plugins.clone();
    let undo_stack_new = undo_stack.clone();
    let redo_stack_new = redo_stack.clone();
    app.on_new_theme(move || {
        let app = app_weak.unwrap();

        let confirmed = rfd::MessageDialog::new()
            .set_title("New Theme")
            .set_description("Discard all changes and start a new theme from the base?")
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if confirmed != rfd::MessageDialogResult::Yes {
            return;
        }

        let mut themes = theme_records_new.borrow_mut();
        let curr_idx = *current_theme_new.borrow() as usize;
        {
            let mut undo = undo_stack_new.borrow_mut();
            let mut redo = redo_stack_new.borrow_mut();
            record_theme_edit(&app, &themes, curr_idx, &mut undo, &mut redo);
        }

        // Revert the working theme to its base values.
        reset_theme_record_from_baseline(&mut themes, curr_idx);
        if let Some(theme) = themes.get_mut(curr_idx) {
            theme.meta.name = WORKING_THEME_NAME.to_string();
            theme.meta.is_builtin = false;
        }
        app.set_theme_name_draft(WORKING_THEME_NAME.into());

        if let Some(theme) = themes.get(curr_idx) {
            let component_key = current_theme_component_new.borrow().clone();
            load_theme_record_into_ui_with_parent(
                &app,
                theme,
                explicit_parent_theme(&themes, curr_idx),
                component_plugins_new.as_ref(),
                component_key.as_str(),
            );
            update_theme_list_ui(&app, &themes);
            update_parent_theme_ui(&app, &themes, curr_idx);
        }

        eprintln!("Started a new theme from the base");
    });

    // Handle JSON export
    let app_weak = app.as_weak();
    let theme_records_export = theme_records.clone();
    let current_theme_clone = current_theme_index.clone();
    let fixed_theme_path_export = fixed_theme_path.clone();
    let component_plugins_export = component_plugins.clone();
    app.on_export_json(move || {
        let app = app_weak.unwrap();
        let mut themes = theme_records_export.borrow_mut();
        let curr_idx = *current_theme_clone.borrow() as usize;

        if let Some(current_theme) = themes.get_mut(curr_idx) {
            pull_tokens_from_ui(&app, &mut current_theme.tokens);
        }

        if curr_idx >= themes.len() {
            return;
        }

        if let Err(missing) =
            validate_theme_record_for_save(&themes[curr_idx], component_plugins_export.as_ref())
        {
            let message = format_missing_base_values(&missing);
            eprintln!("Saving incomplete Base Theme draft:\n{message}");
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Warning)
                .set_title("Incomplete Base Theme")
                .set_description(format!(
                    "{message}\n\nThe draft will be saved, but it cannot be compiled until every value is defined."
                ))
                .show();
        }

        let json = serde_json::to_string_pretty(&export_theme_record_json(
            &themes,
            curr_idx,
            component_plugins_export.as_ref(),
        ))
        .unwrap_or_else(|_| "{}".to_string());
        let current_name = themes[curr_idx].meta.name.clone();
        let fixed_path = fixed_theme_path_export.borrow().clone();
        let path = if let Some(path) = fixed_path.clone() {
            path
        } else {
            let default_name = default_save_file_name(&current_name);
            let file_dialog = rfd::FileDialog::new()
                .set_title("Save Theme")
                .set_file_name(&default_name)
                .add_filter("JSON", &["json"]);

            let Some(path) = file_dialog.save_file() else {
                eprintln!("Save cancelled");
                return;
            };
            path
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("Failed to create theme directory: {}", e);
                return;
            }
        }
        if let Err(e) = std::fs::write(&path, &json) {
            eprintln!("Failed to save theme: {}", e);
            return;
        }
        eprintln!("Saved theme to: {}", path.display());

        // Save As names a new theme after the chosen file. Direct file editing
        // keeps the document's canonical name (especially "Base Theme").
        if fixed_path.is_none() {
            if let Some(file_stem) = path.file_stem().and_then(|s| s.to_str()) {
                let name = file_stem.to_string();
                if let Some(theme) = themes.get_mut(curr_idx) {
                    theme.meta.name = name.clone();
                }
                app.set_theme_name_draft(name.into());
                update_theme_list_ui(&app, &themes);
            }
        }
    });

    // Handle "Load Theme": read a JSON theme into the working slot.
    let app_weak = app.as_weak();
    let theme_records_import = theme_records.clone();
    let current_theme_import = current_theme_index.clone();
    let current_theme_component_import = current_theme_component.clone();
    let component_plugins_import = component_plugins.clone();
    let loaded_plugins_for_import = loaded_plugins_for_theme_io.clone();
    let undo_stack_import = undo_stack.clone();
    let redo_stack_import = redo_stack.clone();
    app.on_import_json(move || {
        let app = app_weak.unwrap();

        let file_dialog = rfd::FileDialog::new().set_title("Load Theme").add_filter("JSON", &["json"]);
        let Some(path) = file_dialog.pick_file() else {
            eprintln!("Load cancelled");
            return;
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                eprintln!("Failed to read file: {}", e);
                return;
            }
        };

        let mut themes = theme_records_import.borrow_mut();
        let curr_idx = *current_theme_import.borrow() as usize;
        let parsed = serde_json::from_str::<Value>(&content)
            .map_err(|err| err.to_string())
            .and_then(|value| import_theme_record_json(&value, &loaded_plugins_for_import, &themes));
        match parsed {
            Ok(mut theme) => {
                theme.meta.is_builtin = false;
                // Prefer the theme's own name; fall back to the file name.
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
                    .filter(|s| !s.trim().is_empty());
                let name = if !theme.meta.name.trim().is_empty() && theme.meta.name != WORKING_THEME_NAME {
                    theme.meta.name.clone()
                } else {
                    stem.unwrap_or_else(|| WORKING_THEME_NAME.to_string())
                };
                theme.meta.name = name.clone();
                if curr_idx < themes.len() {
                    {
                        let mut undo = undo_stack_import.borrow_mut();
                        let mut redo = redo_stack_import.borrow_mut();
                        record_theme_edit(&app, &themes, curr_idx, &mut undo, &mut redo);
                    }
                    themes[curr_idx] = theme;
                }
                app.set_theme_name_draft(name.into());
                if let Some(theme) = themes.get(curr_idx) {
                    let component_key = current_theme_component_import.borrow().clone();
                    load_theme_record_into_ui_with_parent(
                        &app,
                        theme,
                        explicit_parent_theme(&themes, curr_idx),
                        component_plugins_import.as_ref(),
                        component_key.as_str(),
                    );
                }
                update_theme_list_ui(&app, &themes);
                update_parent_theme_ui(&app, &themes, curr_idx);
                eprintln!("Loaded theme from: {}", path.display());
            }
            Err(err) => {
                eprintln!("Failed to load theme JSON: {}", err);
                rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("Couldn't load theme")
                    .set_description(
                        "That file isn't a valid theme JSON. Pick a theme exported from this editor.",
                    )
                    .set_buttons(rfd::MessageButtons::Ok)
                    .show();
            }
        }
    });

    // Handle base-theme file picker: load a JSON theme as a selectable base and
    // point the working theme at it (keeping the working theme's own edits).
    let app_weak = app.as_weak();
    let theme_records_base = theme_records.clone();
    let current_theme_base = current_theme_index.clone();
    let loaded_plugins_for_base = loaded_plugins_for_theme_io.clone();
    let undo_stack_base = undo_stack.clone();
    let redo_stack_base = redo_stack.clone();
    app.on_pick_base_theme(move || {
        let app = app_weak.unwrap();

        let file_dialog = rfd::FileDialog::new().set_title("Choose Base Theme").add_filter("JSON", &["json"]);
        let Some(path) = file_dialog.pick_file() else {
            eprintln!("Base theme selection cancelled");
            return;
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                eprintln!("Failed to read file: {}", e);
                return;
            }
        };

        let mut themes = theme_records_base.borrow_mut();
        let curr_idx = *current_theme_base.borrow() as usize;
        let parsed = serde_json::from_str::<Value>(&content)
            .map_err(|err| err.to_string())
            .and_then(|value| import_theme_record_json(&value, &loaded_plugins_for_base, &themes));
        match parsed {
            Ok(mut base) => {
                base.meta.is_builtin = false;
                base.meta.parent_name = None;
                // Name the base after the chosen file so it's recognisable in the
                // dropdown (a theme's internal name is often "Untitled").
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
                    .filter(|s| !s.trim().is_empty());
                let raw = stem
                    .or_else(|| (!base.meta.name.trim().is_empty()).then(|| base.meta.name.clone()))
                    .unwrap_or_else(|| "Base Theme".to_string());
                let mut name = raw.clone();
                let mut counter = 1;
                while themes.iter().enumerate().any(|(i, t)| i != curr_idx && t.meta.name == name) {
                    counter += 1;
                    name = format!("{} {}", raw, counter);
                }
                base.meta.name = name.clone();
                {
                    let mut undo = undo_stack_base.borrow_mut();
                    let mut redo = redo_stack_base.borrow_mut();
                    record_theme_edit(&app, &themes, curr_idx, &mut undo, &mut redo);
                }
                themes.push(base);
                if let Some(working) = themes.get_mut(curr_idx) {
                    working.meta.parent_name = Some(name.clone());
                }
                update_theme_list_ui(&app, &themes);
                update_parent_theme_ui(&app, &themes, curr_idx);
                eprintln!("Loaded base theme '{}' from: {}", name, path.display());
            }
            Err(err) => {
                eprintln!("Failed to load base theme JSON: {}", err);
                rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("Couldn't load base theme")
                    .set_description(
                        "That file isn't a valid theme JSON. Pick a theme exported from this editor.",
                    )
                    .set_buttons(rfd::MessageButtons::Ok)
                    .show();
            }
        }
    });

    // Handle theme rename
    let app_weak = app.as_weak();
    let theme_records_rename = theme_records.clone();
    let current_theme_clone = current_theme_index.clone();
    let undo_stack_rename = undo_stack.clone();
    let redo_stack_rename = redo_stack.clone();
    app.on_rename_theme(move |new_name| {
        let app = app_weak.unwrap();
        let mut metas = theme_records_rename.borrow_mut();
        let curr_idx = *current_theme_clone.borrow() as usize;

        let Some(current) = metas.get(curr_idx) else {
            return;
        };
        if current.meta.is_builtin {
            eprintln!("Cannot rename built-in theme");
            return;
        }

        let name_str = new_name.to_string();
        if !name_str.is_empty() && current.meta.name != name_str {
            let old_name = current.meta.name.clone();
            {
                let mut undo = undo_stack_rename.borrow_mut();
                let mut redo = redo_stack_rename.borrow_mut();
                record_theme_edit(&app, &metas, curr_idx, &mut undo, &mut redo);
            }
            if let Some(meta) = metas.get_mut(curr_idx) {
                meta.meta.name = name_str.clone();
            }
            for theme in metas.iter_mut() {
                if theme.meta.parent_name.as_deref() == Some(old_name.as_str()) {
                    theme.meta.parent_name = Some(name_str.clone());
                }
            }
            update_theme_list_ui(&app, &metas);
            update_parent_theme_ui(&app, &metas, curr_idx);
            eprintln!("Renamed theme to: {}", name_str);
        }
    });

    // Handle hex color parsing
    app.on_parse_hex_color(|hex| parse_hex_color(&hex));

    // Handle hex color formatting
    app.on_format_hex_color(|color| format_hex_color(color));

    // Handle color alpha formatting for the shared color picker
    app.on_color_alpha_percent(color_alpha_percent);

    // Handle float comparison
    app.on_check_float_equal(|value, default| check_float_equal(value, default));

    // Debug callback for visibility state
    app.on_debug_visibility(|label, show_all, is_overridden, should_show| {
        eprintln!(
            "[visibility] {}: show_all={}, is_overridden={}, should_show={}",
            label, show_all, is_overridden, should_show
        );
    });

    // Handle color comparison
    app.on_check_color_equal(|value, default| check_color_equal(value, default));

    app.run().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::TokenValue;

    fn component_plugin_map(
        plugins: &[(plugin::BuiltinComponentSpec, PluginDefinition)],
    ) -> HashMap<String, PluginDefinition> {
        plugins.iter().map(|(spec, plugin)| (spec.key.to_string(), plugin.clone())).collect()
    }

    #[test]
    fn json_color_format_preserves_alpha() {
        assert_eq!(color_hex(slint::Color::from_argb_u8(0x80, 0x11, 0x22, 0x33)), "#11223380");
        assert_eq!(color_hex(slint::Color::from_rgb_u8(0x11, 0x22, 0x33)), "#112233");
    }

    #[test]
    fn builtin_theme_seed_and_read_roundtrip() {
        use std::time::{SystemTime, UNIX_EPOCH};

        // The embedded SDK themes resolve, parse, and carry the inheritance
        // metadata the editor relies on (compile already proved the paths).
        let embedded: HashMap<&str, Value> = SEED_BUILTIN_THEMES
            .iter()
            .map(|(name, contents)| {
                (*name, serde_json::from_str::<Value>(contents).expect("embedded theme JSON parses"))
            })
            .collect();
        assert_eq!(embedded.len(), 1, "only the base theme is bundled");
        assert!(embedded["base_theme.json"].get("parent").is_none(), "base theme is the root");

        // Seeding a fresh dir writes every bundled theme; it then reads back.
        let temp = std::env::temp_dir().join(format!(
            "theme-editor-themes-test-{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        assert!(seed_user_themes_dir(&temp), "seeding a fresh dir should populate it");
        let sources = read_theme_jsons_from_dir(&temp).expect("seeded themes should read back");
        assert_eq!(sources.len(), SEED_BUILTIN_THEMES.len());

        // User edits win: re-seeding never clobbers an existing file.
        let default_path = temp.join("base_theme.json");
        std::fs::write(&default_path, "{\"id\":\"base_theme\",\"name\":\"Edited\"}").unwrap();
        assert!(seed_user_themes_dir(&temp));
        let edited = std::fs::read_to_string(&default_path).unwrap();
        assert!(edited.contains("Edited"), "existing theme files must not be overwritten");

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn export_resolution_follows_token_aliases() {
        let mut tokens = TokenStore::default();
        tokens.categories.insert(
            "radius".to_string(),
            HashMap::from([
                ("default".to_string(), TokenValue::String("radius.md".to_string())),
                ("md".to_string(), TokenValue::Number(8.0)),
            ]),
        );

        let resolved =
            resolve_property_value_from_store(&tokens, &PropertyValue::Token("radius.default".into()));

        match resolved {
            Some(PropertyValue::Int(value)) => assert_eq!(value, 8),
            Some(PropertyValue::Float(value)) => assert!((value - 8.0).abs() < f32::EPSILON),
            other => panic!("expected numeric value, got {other:?}"),
        }
    }

    #[test]
    fn export_resolution_maps_legacy_color_aliases() {
        let mut tokens = TokenStore::default();
        tokens.categories.insert(
            "color".to_string(),
            HashMap::from([
                ("primary.dark".to_string(), TokenValue::String("#112233".to_string())),
                ("muted".to_string(), TokenValue::String("#445566".to_string())),
            ]),
        );

        let pressed =
            resolve_property_value_from_store(&tokens, &PropertyValue::Token("color.primary-pressed".into()));
        let muted =
            resolve_property_value_from_store(&tokens, &PropertyValue::Token("color.text-muted".into()));

        match pressed {
            Some(PropertyValue::Color(color)) => {
                assert_eq!(color, slint::Color::from_rgb_u8(0x11, 0x22, 0x33))
            }
            other => panic!("expected legacy pressed color to resolve, got {other:?}"),
        }

        match muted {
            Some(PropertyValue::Color(color)) => {
                assert_eq!(color, slint::Color::from_rgb_u8(0x44, 0x55, 0x66))
            }
            other => panic!("expected legacy muted color to resolve, got {other:?}"),
        }
    }

    #[test]
    fn export_preserves_token_refs_for_standard_props() {
        let mut style = export_theme::StyleProps::new();
        let tokens = TokenStore::default();

        apply_property_value_to_export_style(
            &mut style,
            "border-color",
            &PropertyValue::Token("color.primary-pressed".into()),
            &tokens,
        );
        apply_property_value_to_export_style(
            &mut style,
            "font-family",
            &PropertyValue::String(String::new()),
            &tokens,
        );

        assert!(style.border_color.is_none());
        assert_eq!(style.token_refs.get("border_color").map(String::as_str), Some("color.primary.dark"));
        assert!(style.font_family.is_none());
    }

    #[test]
    fn rust_export_uses_theme_named_entrypoint() {
        let theme =
            build_theme_record("Example Theme", false, Some(BASE_THEME_NAME), TokenTheme::base_theme(), &[]);
        let base_theme = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &[]);

        let rust_code =
            generate_theme_rust_export(&[base_theme, theme], 1).expect("theme export should be generated");

        assert!(rust_code.contains("pub fn example_theme() -> Theme"));
        assert!(!rust_code.contains("theme_example_theme()"));
        assert!(!rust_code.contains("pub fn exported_theme() -> Theme"));
        assert!(rust_code.contains("use ui2::themes::base_theme::base_theme;"));
        assert!(rust_code.contains("extends base_theme();"));
        assert!(!rust_code.contains("pub fn theme_base_theme() -> Theme"));
    }

    #[test]
    fn rust_export_for_base_theme_uses_direct_name() {
        let base_theme = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &[]);

        let rust_code =
            generate_theme_rust_export(&[base_theme], 0).expect("theme export should be generated");

        assert!(rust_code.contains("pub fn base_theme() -> Theme"));
        assert!(!rust_code.contains("theme_base_theme"));
        assert!(!rust_code.contains("use ui2::themes::base_theme::base_theme;"));
        assert!(!rust_code.contains("extends base_theme();"));
    }

    #[test]
    fn rust_export_inlines_custom_ancestors_with_unprefixed_private_names() {
        let base_theme = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &[]);
        let parent_theme =
            build_theme_record("Parent Theme", false, Some(BASE_THEME_NAME), TokenTheme::base_theme(), &[]);
        let child_theme =
            build_theme_record("Child Theme", false, Some("Parent Theme"), TokenTheme::base_theme(), &[]);

        let rust_code = generate_theme_rust_export(&[base_theme, parent_theme, child_theme], 2)
            .expect("theme export should be generated");

        assert!(rust_code.contains("pub fn child_theme() -> Theme"));
        assert!(rust_code.contains("fn parent_theme() -> Theme"));
        assert!(!rust_code.contains("theme_parent_theme"));
        assert!(!rust_code.contains("theme_child_theme"));
        assert!(rust_code.contains("use ui2::themes::base_theme::base_theme;"));
        assert!(rust_code.contains("extends parent_theme();"));
        assert!(rust_code.contains("extends base_theme();"));
    }

    #[test]
    fn rust_export_sanitizes_theme_names_without_theme_prefix() {
        let base_theme = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &[]);
        let theme = build_theme_record(
            "Solarized Dark!!!",
            false,
            Some(BASE_THEME_NAME),
            TokenTheme::base_theme(),
            &[],
        );

        let rust_code =
            generate_theme_rust_export(&[base_theme, theme], 1).expect("theme export should be generated");

        assert!(rust_code.contains("pub fn solarized_dark() -> Theme"));
        assert!(!rust_code.contains("theme_solarized_dark"));
    }

    #[test]
    fn rust_export_keeps_button_size_styles_token_driven() {
        let base_theme = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &[]);

        let mut custom_tokens = TokenTheme::base_theme();
        custom_tokens.font_primary = "Verdana".to_string();
        custom_tokens.font_secondary = "Trebuchet MS".to_string();
        custom_tokens.font_tertiary = "Georgia".to_string();

        let theme = build_theme_record("My Custom Theme", false, Some(BASE_THEME_NAME), custom_tokens, &[]);

        let rust_code =
            generate_theme_rust_export(&[base_theme, theme], 1).expect("theme export should be generated");

        assert!(rust_code.contains("tokens {"));
        assert!(rust_code.contains("light {"));
        assert!(rust_code.contains("\"font.primary\" = \"Verdana\";"));
        assert!(!rust_code.contains("\"font_family\" = \"Verdana\";"));
        assert!(!rust_code.contains("\"font_size\" = "));
        assert!(!rust_code.contains("\"padding_horizontal\" = "));
        assert!(!rust_code.contains("\"padding_vertical\" = "));
        assert!(!rust_code.contains("\"border_radius\" = "));
    }

    #[test]
    fn json_export_uses_normalized_parent_and_elides_matching_values() {
        let base_theme = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &[]);

        let mut child_tokens = TokenTheme::base_theme();
        child_tokens.font_primary = "Verdana".to_string();
        let child_theme =
            build_theme_record("Example Theme", false, Some(BASE_THEME_NAME), child_tokens, &[]);

        let json = export_theme_record_json(&[base_theme, child_theme], 1, &HashMap::new());

        assert_eq!(json.get("parent").and_then(Value::as_str), Some("base_theme"));
        assert_eq!(json.get("id").and_then(Value::as_str), Some("example_theme"));

        let tokens =
            json.get("tokens").and_then(Value::as_object).expect("export should contain token overrides");
        assert!(!tokens.contains_key("colors"));
        assert!(!tokens.contains_key("spacing"));
        assert!(!tokens.contains_key("radius"));
        assert_eq!(
            tokens
                .get("typography")
                .and_then(Value::as_object)
                .and_then(|typography| typography.get("font-primary"))
                .and_then(Value::as_str),
            Some("Verdana")
        );
        assert_eq!(
            tokens.get("typography").and_then(Value::as_object).map(|typography| typography.len()),
            Some(1)
        );
        assert!(json.get("components").is_none());
    }

    #[test]
    fn json_import_resolves_normalized_parent_ids_and_applies_patches() {
        let base_theme = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &[]);
        let imported = import_theme_record_json(
            &json!({
                "id": "example_theme",
                "name": "Example Theme",
                "parent": "base_theme",
                "tokens": {
                    "typography": {
                        "font-primary": "Verdana"
                    }
                }
            }),
            &[],
            &[base_theme.clone()],
        )
        .expect("import should succeed");

        assert_eq!(imported.meta.parent_name.as_deref(), Some(BASE_THEME_NAME));
        assert_eq!(imported.tokens.font_primary, "Verdana");
        assert_eq!(imported.tokens.font_secondary, base_theme.tokens.font_secondary);
    }

    #[test]
    fn imported_token_presence_is_exported_as_override_even_when_value_matches_parent() {
        let base_theme = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &[]);
        let imported = import_theme_record_json(
            &json!({
                "id": "example_theme",
                "name": "Example Theme",
                "parent": "base_theme",
                "tokens": {
                    "typography": {
                        "font-primary": "Montserrat"
                    }
                }
            }),
            &[],
            std::slice::from_ref(&base_theme),
        )
        .expect("import should succeed");

        assert!(imported.token_overrides.contains("typography.font-primary"));
        let exported = export_theme_record_json(&[base_theme, imported], 1, &HashMap::new());
        assert_eq!(
            exported
                .get("tokens")
                .and_then(|tokens| tokens.get("typography"))
                .and_then(|typography| typography.get("font-primary"))
                .and_then(Value::as_str),
            Some("Montserrat")
        );
    }

    #[test]
    fn component_json_reads_legacy_default_and_writes_common_size_key() {
        let plugin: PluginDefinition = serde_json::from_str(
            r##"{
                "component": "Test",
                "variants": ["default"],
                "states": ["normal"],
                "sizes": ["sm", "md"],
                "sizeProps": [
                    { "name": "border-radius", "type": "float",
                      "defaults": { "default": 8.0 } }
                ]
            }"##,
        )
        .unwrap();
        let mut data = ComponentThemeData::from_plugin(&plugin, &TokenStore::default());

        import_component_theme_json(
            &mut data,
            &plugin,
            &json!({
                "sizeProps": {
                    "default": {
                        "border-radius": 12.0
                    }
                }
            }),
        );

        assert_eq!(data.size_props[crate::plugin::DEFAULT_SIZE_KEY]["border-radius"].as_float(), Some(12.0));

        let exported = export_component_theme_json(&data);
        assert!(exported
            .get("sizeProps")
            .and_then(Value::as_object)
            .is_some_and(|sizes| sizes.contains_key("common")));
        assert!(exported
            .get("sizeProps")
            .and_then(Value::as_object)
            .is_some_and(|sizes| !sizes.contains_key("default")));
    }

    #[test]
    fn base_save_validation_requires_root_token_ownership() {
        let mut base = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &[]);
        let component_plugins = HashMap::new();
        let validation = validate_theme_record_for_save(&base, &component_plugins);
        assert!(validation.is_ok(), "unexpected incomplete Base Theme values: {validation:?}");

        base.token_overrides.remove("spacing.md");
        let missing = validate_theme_record_for_save(&base, &component_plugins)
            .expect_err("root base should require all tokens");
        assert!(missing.iter().any(|item| item == "token spacing.md"));

        let child =
            build_theme_record("App Theme", false, Some(BASE_THEME_NAME), TokenTheme::base_theme(), &[]);
        assert!(
            validate_theme_record_for_save(&child, &component_plugins).is_ok(),
            "app themes can save sparse overrides"
        );
    }

    #[test]
    fn base_save_validation_requires_resolved_component_values() {
        let plugin: PluginDefinition =
            serde_json::from_str(include_str!("../defaults/components/button.schema.json")).unwrap();
        let spec =
            plugin::BuiltinComponentSpec { key: "button", component: "Button", source_file: "button.slint" };
        let plugins = vec![(spec, plugin)];
        let component_plugins = plugins
            .iter()
            .map(|(spec, plugin)| (spec.key.to_string(), plugin.clone()))
            .collect::<HashMap<_, _>>();
        let base_json: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../sdk/crates/foundation-themes/themes/base_theme.json"
        )))
        .unwrap();
        let mut base = import_theme_record_json(&base_json, &plugins, &[]).unwrap();
        let validation = validate_theme_record_for_save(&base, &component_plugins);
        assert!(validation.is_ok(), "unexpected incomplete Base Theme values: {validation:?}");

        base.component_themes
            .get_mut("button")
            .unwrap()
            .variant_props
            .get_mut("primary")
            .unwrap()
            .get_mut("normal")
            .unwrap()
            .remove("background");

        let missing = validate_theme_record_for_save(&base, &component_plugins)
            .expect_err("root base should require component values");
        assert!(missing
            .iter()
            .any(|item| item == "component button variant primary/normal property background"));
    }

    #[test]
    fn incomplete_base_draft_round_trips_without_schema_defaults() {
        let plugin: PluginDefinition =
            serde_json::from_str(include_str!("../defaults/components/button.schema.json")).unwrap();
        let spec =
            plugin::BuiltinComponentSpec { key: "button", component: "Button", source_file: "button.slint" };
        let plugins = vec![(spec, plugin)];
        let component_plugins = plugins
            .iter()
            .map(|(spec, plugin)| (spec.key.to_string(), plugin.clone()))
            .collect::<HashMap<_, _>>();
        let draft = json!({
            "id": "base_theme",
            "name": "Base Theme",
            "tokens": {
                "colors": {
                    "light": {
                        "primary": "#123456"
                    }
                }
            },
            "components": {
                "button": {
                    "variantProps": {
                        "primary": {
                            "normal": {
                                "background": "#654321"
                            }
                        }
                    }
                }
            }
        });

        let record = import_theme_record_json(&draft, &plugins, &[]).unwrap();
        let missing = validate_theme_record_for_save(&record, &component_plugins)
            .expect_err("an incomplete Base Theme must keep showing a warning");
        assert!(missing.iter().any(|item| item == "token spacing.md"));
        assert!(missing
            .iter()
            .any(|item| item == "component button variant primary/normal property foreground"));

        let exported = export_theme_record_json(&[record], 0, &component_plugins);
        assert_eq!(
            exported.pointer("/tokens/colors/light/primary"),
            Some(&Value::String("#123456".to_string()))
        );
        assert!(exported.pointer("/tokens/spacing/md").is_none());
        assert_eq!(
            exported.pointer("/components/button/variantProps/primary/normal/background"),
            Some(&Value::String("#654321".to_string()))
        );
        assert!(exported.pointer("/components/button/variantProps/primary/normal/foreground").is_none());
        assert!(exported.pointer("/components/button/sizeProps").is_none());
    }

    #[test]
    fn missing_base_values_are_grouped_for_save_dialog() {
        let missing = vec![
            "component chip variant filled/focused property background".to_string(),
            "component button size common property border-width".to_string(),
        ];

        assert_eq!(
            format_missing_base_values(&missing),
            "The following components/props are missing values:\n\n\
Button\n  Sizes.Border Width\n\n\
Chip\n  Variants.Filled.Focused.Background"
        );
    }

    #[test]
    fn parent_selector_options_include_none_and_select_base_parent() {
        let base = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &[]);
        let child =
            build_theme_record("App Theme", false, Some(BASE_THEME_NAME), TokenTheme::base_theme(), &[]);

        let (options, selected) = parent_theme_options(&[base.clone(), child], 1);
        assert_eq!(options, vec!["None".to_string(), BASE_THEME_NAME.to_string()]);
        assert_eq!(selected, 1);

        let (options, selected) = parent_theme_options(&[base], 0);
        assert_eq!(options, vec!["None".to_string()]);
        assert_eq!(selected, 0);
    }

    #[test]
    fn variant_reset_icon_model_uses_base_only_for_root_and_local_chain_for_rest() {
        let plugin: PluginDefinition =
            serde_json::from_str(include_str!("../defaults/components/button.schema.json")).unwrap();
        let spec =
            plugin::BuiltinComponentSpec { key: "button", component: "Button", source_file: "button.slint" };
        let plugins = vec![(spec, plugin)];
        let base = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &plugins);
        let mut child = base.clone();
        child.meta = ThemeMeta {
            name: "App Theme".to_string(),
            is_builtin: false,
            parent_name: Some(BASE_THEME_NAME.to_string()),
        };
        clear_component_override_ownership(&mut child);

        let base_data = base.component_themes.get("button").unwrap();
        let child_data = child.component_themes.get("button").unwrap();
        let primary_normal_background = &child_data.variant_props["primary"]["normal"]["background"];
        assert!(variant_prop_is_overridden_for_ui(
            base_data,
            None,
            None,
            "primary",
            "normal",
            "background",
            &base_data.variant_props["primary"]["normal"]["background"],
        ));
        assert!(!variant_prop_is_overridden_for_ui(
            child_data,
            Some(base_data),
            None,
            "primary",
            "normal",
            "background",
            primary_normal_background,
        ));
        assert!(!variant_prop_is_overridden_for_ui(
            child_data,
            Some(base_data),
            None,
            "secondary",
            "normal",
            "background",
            &child_data.variant_props["secondary"]["normal"]["background"],
        ));
        assert!(!variant_prop_is_overridden_for_ui(
            child_data,
            Some(base_data),
            None,
            "primary",
            "focused",
            "background",
            &child_data.variant_props["primary"]["focused"]["background"],
        ));

        let child_data = child.component_themes.get_mut("button").unwrap();
        child_data.set_variant_override(
            "primary",
            "normal",
            "background",
            PropertyValue::Token("color.danger".to_string()),
        );
        assert!(variant_prop_is_overridden_for_ui(
            child_data,
            Some(base_data),
            None,
            "primary",
            "normal",
            "background",
            &child_data.variant_props["primary"]["normal"]["background"],
        ));
        assert!(!variant_prop_is_overridden_for_ui(
            child_data,
            Some(base_data),
            None,
            "secondary",
            "normal",
            "background",
            &child_data.variant_props["secondary"]["normal"]["background"],
        ));

        child_data.set_variant_override(
            "secondary",
            "normal",
            "background",
            PropertyValue::Token("color.secondary".to_string()),
        );
        assert!(variant_prop_is_overridden_for_ui(
            child_data,
            Some(base_data),
            None,
            "secondary",
            "normal",
            "background",
            &child_data.variant_props["secondary"]["normal"]["background"],
        ));
    }

    #[test]
    fn size_reset_icon_model_uses_common_as_concrete_size_baseline() {
        let plugin: PluginDefinition = serde_json::from_str(
            r##"{
                "component": "Test",
                "variants": ["default"],
                "states": ["normal"],
                "sizes": ["sm", "md"],
                "sizeProps": [
                    { "name": "height", "type": "float",
                      "defaults": { "default": 8.0, "sm": 6.0, "md": 8.0 } }
                ]
            }"##,
        )
        .unwrap();
        let spec = plugin::BuiltinComponentSpec { key: "test", component: "Test", source_file: "test.slint" };
        let plugins = vec![(spec, plugin)];
        let mut base = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &plugins);
        let token_store = token_store_from_theme(&base.tokens);
        {
            let base_data = base.component_themes.get("test").unwrap();
            assert!(size_prop_is_overridden_for_ui(
                base_data,
                None,
                None,
                crate::plugin::DEFAULT_SIZE_KEY,
                "height",
                &base_data.size_props[crate::plugin::DEFAULT_SIZE_KEY]["height"],
            ));
        }
        {
            let base_data = base.component_themes.get_mut("test").unwrap();
            base_data.set_size_default("height", PropertyValue::Float(10.0));
            assert_eq!(
                base_data.size_props[crate::plugin::DEFAULT_SIZE_KEY]["height"].as_float(),
                Some(10.0)
            );
            base_data.clear_size_default("height", &token_store);
            assert_eq!(base_data.size_props[crate::plugin::DEFAULT_SIZE_KEY]["height"].as_float(), Some(8.0));
            assert!(
                size_prop_is_overridden_for_ui(
                    base_data,
                    None,
                    None,
                    crate::plugin::DEFAULT_SIZE_KEY,
                    "height",
                    &base_data.size_props[crate::plugin::DEFAULT_SIZE_KEY]["height"],
                ),
                "Base/None Common size props remain overrides of the empty base"
            );
        }
        let mut child = base.clone();
        child.meta = ThemeMeta {
            name: "App Theme".to_string(),
            is_builtin: false,
            parent_name: Some(BASE_THEME_NAME.to_string()),
        };
        clear_component_override_ownership(&mut child);

        let base_data = base.component_themes.get("test").unwrap();
        let child_data = child.component_themes.get("test").unwrap();
        assert!(!size_prop_is_overridden_for_ui(
            child_data,
            Some(base_data),
            None,
            crate::plugin::DEFAULT_SIZE_KEY,
            "height",
            &child_data.size_props[crate::plugin::DEFAULT_SIZE_KEY]["height"],
        ));

        let child_data = child.component_themes.get_mut("test").unwrap();
        child_data.set_size_default("height", PropertyValue::Float(10.0));
        assert!(size_prop_is_overridden_for_ui(
            child_data,
            Some(base_data),
            None,
            crate::plugin::DEFAULT_SIZE_KEY,
            "height",
            &child_data.size_props[crate::plugin::DEFAULT_SIZE_KEY]["height"],
        ));
        assert!(!size_prop_is_overridden_for_ui(
            child_data,
            Some(base_data),
            None,
            "sm",
            "height",
            &child_data.size_props["sm"]["height"],
        ));

        child_data.set_size_override("sm", "height", PropertyValue::Float(6.0));
        assert!(size_prop_is_overridden_for_ui(
            child_data,
            Some(base_data),
            None,
            "sm",
            "height",
            &child_data.size_props["sm"]["height"],
        ));
    }

    #[test]
    fn component_child_reset_icons_distinguish_inherited_and_child_only_props() {
        let plugins = crate::plugin::load_all_plugins_from_repo().unwrap();
        let component_plugins: HashMap<String, PluginDefinition> =
            plugins.iter().map(|(spec, plugin)| (spec.key.to_string(), plugin.clone())).collect();
        let mut base = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &plugins);

        assert_eq!(
            &base.component_themes["input"].variant_props["default"]["normal"]["background"],
            &PropertyValue::Token("color.surface".to_string()),
            "sanity check: this test uses the real Input schema value"
        );
        let search_data = base.component_themes.get("search").unwrap();
        let search_parent = component_parent_data(&component_plugins, &base.component_themes, search_data);
        assert!(search_parent.is_some());

        assert!(
            !variant_prop_is_overridden_for_ui(
                search_data,
                None,
                search_parent,
                "default",
                "normal",
                "background",
                &search_data.variant_props["default"]["normal"]["background"],
            ),
            "Search inherits Input background, so Base/None should not show reset until Search changes it"
        );
        assert!(
            !variant_prop_is_overridden_for_ui(
                search_data,
                None,
                search_parent,
                "default",
                "focused",
                "border-color",
                &search_data.variant_props["default"]["focused"]["border-color"],
            ),
            "Search Focused Border Color matches Input Focused, so it should not show as a child override"
        );

        let base_json = export_theme_record_json(std::slice::from_ref(&base), 0, &component_plugins);
        assert!(
            base_json
                .get("components")
                .and_then(|components| components.get("search"))
                .and_then(|search| search.get("variantProps"))
                .and_then(|variants| variants.get("default"))
                .and_then(|default| default.get("focused"))
                .and_then(|focused| focused.get("border-color"))
                .is_none(),
            "Search should not serialize inherited Input focused border-color"
        );

        base.component_themes.get_mut("search").unwrap().set_variant_default(
            "normal",
            "background",
            PropertyValue::Token("color.danger".to_string()),
        );
        let search_data = base.component_themes.get("search").unwrap();
        let search_parent = component_parent_data(&component_plugins, &base.component_themes, search_data);
        assert!(variant_prop_is_overridden_for_ui(
            search_data,
            None,
            search_parent,
            "default",
            "normal",
            "background",
            &search_data.variant_props["default"]["normal"]["background"],
        ));

        base.component_themes.get_mut("search").unwrap().set_variant_default(
            "normal",
            "border-color",
            PropertyValue::Token("color.danger".to_string()),
        );
        let search_data = base.component_themes.get("search").unwrap();
        let search_parent = component_parent_data(&component_plugins, &base.component_themes, search_data);
        assert_eq!(
            search_data.variant_props["default"]["focused"]["border-color"],
            PropertyValue::Token("color.danger".to_string()),
            "Search Focused should follow Search Normal once Normal is locally overridden"
        );
        assert!(
            !variant_prop_is_overridden_for_ui(
                search_data,
                None,
                search_parent,
                "default",
                "focused",
                "border-color",
                &search_data.variant_props["default"]["focused"]["border-color"],
            ),
            "Focused should not show a reset icon when it is inheriting Search Normal"
        );

        let dropdown_data = base.component_themes.get("dropdown").unwrap();
        let dropdown_parent =
            component_parent_data(&component_plugins, &base.component_themes, dropdown_data);
        assert!(dropdown_parent.is_some());
        assert!(
            !size_prop_is_overridden_for_ui(
                dropdown_data,
                None,
                dropdown_parent,
                crate::plugin::DEFAULT_SIZE_KEY,
                "border-width",
                &dropdown_data.size_props[crate::plugin::DEFAULT_SIZE_KEY]["border-width"],
            ),
            "Dropdown border-width is inherited from Input"
        );
        assert!(
            size_prop_is_overridden_for_ui(
                dropdown_data,
                None,
                dropdown_parent,
                crate::plugin::DEFAULT_SIZE_KEY,
                "control-height",
                &dropdown_data.size_props[crate::plugin::DEFAULT_SIZE_KEY]["control-height"],
            ),
            "Dropdown control-height is child-only, so Base/None should show reset"
        );

        base.component_themes
            .get_mut("dropdown")
            .unwrap()
            .set_size_default("border-width", PropertyValue::Float(4.0));
        base.component_themes.get_mut("dropdown").unwrap().mark_parent_override("border-width");
        let dropdown_data = base.component_themes.get("dropdown").unwrap();
        let dropdown_parent =
            component_parent_data(&component_plugins, &base.component_themes, dropdown_data);
        assert!(size_prop_is_overridden_for_ui(
            dropdown_data,
            None,
            dropdown_parent,
            crate::plugin::DEFAULT_SIZE_KEY,
            "border-width",
            &dropdown_data.size_props[crate::plugin::DEFAULT_SIZE_KEY]["border-width"],
        ));

        let mut app_theme = base.clone();
        app_theme.meta = ThemeMeta {
            name: "App Theme".to_string(),
            is_builtin: false,
            parent_name: Some(BASE_THEME_NAME.to_string()),
        };
        clear_component_override_ownership(&mut app_theme);
        let base_dropdown = base.component_themes.get("dropdown").unwrap();
        let app_dropdown = app_theme.component_themes.get("dropdown").unwrap();
        let app_dropdown_parent =
            component_parent_data(&component_plugins, &app_theme.component_themes, app_dropdown);
        assert!(
            !size_prop_is_overridden_for_ui(
                app_dropdown,
                Some(base_dropdown),
                app_dropdown_parent,
                crate::plugin::DEFAULT_SIZE_KEY,
                "control-height",
                &app_dropdown.size_props[crate::plugin::DEFAULT_SIZE_KEY]["control-height"],
            ),
            "Child-only props in an app theme still compare to the Base theme"
        );
    }

    #[test]
    fn component_child_import_and_export_elide_inherited_common_size_values() {
        let plugins = crate::plugin::load_all_plugins_from_repo().unwrap();
        let component_plugins = component_plugin_map(&plugins);

        let imported = import_theme_record_json(
            &json!({
                "id": "base_theme",
                "name": BASE_THEME_NAME,
                "components": {
                    "search": {
                        "variantProps": {
                            "default": {
                                "focused": {
                                    "border-color": "color.primary"
                                }
                            }
                        },
                        "sizeProps": {
                            "common": {
                                "border-width": 1.0
                            }
                        }
                    },
                    "dropdown": {
                        "sizeProps": {
                            "common": {
                                "border-width": 1.0,
                                "control-height": "controlSize.md"
                            }
                        }
                    }
                }
            }),
            &plugins,
            &[],
        )
        .expect("base theme import should succeed");

        let search = imported.component_themes.get("search").unwrap();
        assert!(
            !search.parent_prop_is_overridden("border-width"),
            "Search border-width matches Input and should stay inherited"
        );
        assert!(
            !search.variant_prop_is_overridden("default", "focused", "border-color"),
            "Search focused border-color matches Input and should stay inherited"
        );
        let dropdown = imported.component_themes.get("dropdown").unwrap();
        assert!(
            !dropdown.parent_prop_is_overridden("border-width"),
            "Dropdown border-width matches Input and should stay inherited"
        );

        let exported = export_theme_record_json(std::slice::from_ref(&imported), 0, &component_plugins);
        assert!(
            exported
                .get("components")
                .and_then(|components| components.get("search"))
                .and_then(|search| search.get("sizeProps"))
                .and_then(|sizes| sizes.get("common"))
                .and_then(|common| common.get("border-width"))
                .is_none(),
            "Search should not serialize inherited Input common fields"
        );
        assert!(
            exported
                .get("components")
                .and_then(|components| components.get("search"))
                .and_then(|search| search.get("variantProps"))
                .and_then(|variants| variants.get("default"))
                .and_then(|default| default.get("focused"))
                .and_then(|focused| focused.get("border-color"))
                .is_none(),
            "Search should not serialize inherited Input focused fields"
        );
        assert!(
            exported
                .get("components")
                .and_then(|components| components.get("dropdown"))
                .and_then(|dropdown| dropdown.get("sizeProps"))
                .and_then(|sizes| sizes.get("common"))
                .and_then(|common| common.get("border-width"))
                .is_none(),
            "Dropdown should not serialize inherited Input common fields"
        );
        assert_eq!(
            exported
                .get("components")
                .and_then(|components| components.get("dropdown"))
                .and_then(|dropdown| dropdown.get("sizeProps"))
                .and_then(|sizes| sizes.get("common"))
                .and_then(|common| common.get("control-height"))
                .and_then(Value::as_str),
            Some("controlSize.md"),
            "Dropdown-only Common fields should still be serialized"
        );
    }

    #[test]
    fn base_export_includes_common_component_values_but_child_export_elides_inherited_values() {
        let plugin: PluginDefinition =
            serde_json::from_str(include_str!("../defaults/components/button.schema.json")).unwrap();
        let spec =
            plugin::BuiltinComponentSpec { key: "button", component: "Button", source_file: "button.slint" };
        let plugins = vec![(spec, plugin)];
        let component_plugins = component_plugin_map(&plugins);
        let base_json: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../sdk/crates/foundation-themes/themes/base_theme.json"
        )))
        .unwrap();
        let mut base = import_theme_record_json(&base_json, &plugins, &[]).unwrap();
        base.meta.is_builtin = true;

        let base_json = export_theme_record_json(std::slice::from_ref(&base), 0, &component_plugins);
        assert_eq!(base_json.get("id").and_then(Value::as_str), Some("base_theme"));
        assert_eq!(base_json.get("name").and_then(Value::as_str), Some(BASE_THEME_NAME));
        assert!(base_json.get("parent").is_none());
        assert_eq!(
            base_json.pointer("/tokens/colors/light/transparent").and_then(Value::as_str),
            Some("#00000000")
        );
        assert_eq!(base_json.pointer("/tokens/fontWeight/medium").and_then(Value::as_i64), Some(500));
        assert_eq!(
            base_json
                .get("components")
                .and_then(|components| components.get("button"))
                .and_then(|button| button.get("variantProps"))
                .and_then(|variants| variants.get("primary"))
                .and_then(|primary| primary.get("normal"))
                .and_then(|normal| normal.get("background"))
                .and_then(Value::as_str),
            Some("color.primary")
        );
        assert!(base_json
            .get("components")
            .and_then(|components| components.get("button"))
            .and_then(|button| button.get("variantProps"))
            .and_then(|variants| variants.get("primary"))
            .and_then(|primary| primary.get("focused"))
            .and_then(|focused| focused.get("background"))
            .is_none());
        assert_eq!(
            base_json
                .get("components")
                .and_then(|components| components.get("button"))
                .and_then(|button| button.get("variantProps"))
                .and_then(|variants| variants.get("primary"))
                .and_then(|primary| primary.get("focused"))
                .and_then(|focused| focused.get("borderColor"))
                .and_then(Value::as_str),
            Some("color.primary.light")
        );
        assert_eq!(
            base_json
                .get("components")
                .and_then(|components| components.get("button"))
                .and_then(|button| button.get("variantProps"))
                .and_then(|variants| variants.get("secondary"))
                .and_then(|secondary| secondary.get("normal"))
                .and_then(|normal| normal.get("background"))
                .and_then(Value::as_str),
            Some("color.secondary")
        );
        assert!(base_json
            .get("components")
            .and_then(|components| components.get("button"))
            .and_then(|button| button.get("variantProps"))
            .and_then(|variants| variants.get("secondary"))
            .and_then(|secondary| secondary.get("focused"))
            .and_then(|focused| focused.get("background"))
            .is_none());
        assert!(base_json
            .get("components")
            .and_then(|components| components.get("button"))
            .and_then(|button| button.get("sizeProps"))
            .and_then(|sizes| sizes.get("common"))
            .is_some());

        let mut child = base.clone();
        child.meta = ThemeMeta {
            name: "App Theme".to_string(),
            is_builtin: false,
            parent_name: Some(BASE_THEME_NAME.to_string()),
        };
        clear_component_override_ownership(&mut child);
        let inherited_json = export_theme_record_json(&[base.clone(), child.clone()], 1, &component_plugins);
        assert_eq!(inherited_json.get("parent").and_then(Value::as_str), Some("base_theme"));
        assert!(
            inherited_json.get("components").is_none(),
            "unchanged app theme should not emit inherited component values"
        );

        child.component_themes.get_mut("button").unwrap().set_variant_override(
            "primary",
            "normal",
            "background",
            PropertyValue::Token("color.danger".to_string()),
        );
        let changed_json = export_theme_record_json(&[base, child], 1, &component_plugins);
        assert_eq!(
            changed_json
                .get("components")
                .and_then(|components| components.get("button"))
                .and_then(|button| button.get("variantProps"))
                .and_then(|variants| variants.get("primary"))
                .and_then(|primary| primary.get("normal"))
                .and_then(|normal| normal.get("background"))
                .and_then(Value::as_str),
            Some("color.danger")
        );
        assert!(changed_json
            .get("components")
            .and_then(|components| components.get("button"))
            .and_then(|button| button.get("variantProps"))
            .and_then(|variants| variants.get("primary"))
            .and_then(|primary| primary.get("focused"))
            .and_then(|focused| focused.get("background"))
            .is_none());
        assert!(changed_json
            .get("components")
            .and_then(|components| components.get("button"))
            .and_then(|button| button.get("variantProps"))
            .and_then(|variants| variants.get("secondary"))
            .and_then(|secondary| secondary.get("normal"))
            .and_then(|normal| normal.get("background"))
            .is_none());
    }

    #[test]
    fn child_size_export_compares_concrete_sizes_to_common_not_parent_size() {
        let plugin: PluginDefinition = serde_json::from_str(
            r##"{
                "component": "Test",
                "variants": ["default"],
                "states": ["normal"],
                "sizes": ["sm", "md"],
                "sizeProps": [
                    { "name": "height", "type": "float",
                      "defaults": { "default": 8.0, "sm": 6.0, "md": 8.0 } }
                ]
            }"##,
        )
        .unwrap();
        let spec = plugin::BuiltinComponentSpec { key: "test", component: "Test", source_file: "test.slint" };
        let plugins = vec![(spec, plugin)];
        let component_plugins = component_plugin_map(&plugins);
        let base = build_theme_record(BASE_THEME_NAME, true, None, TokenTheme::base_theme(), &plugins);
        let mut child = base.clone();
        child.meta = ThemeMeta {
            name: "App Theme".to_string(),
            is_builtin: false,
            parent_name: Some(BASE_THEME_NAME.to_string()),
        };
        clear_component_override_ownership(&mut child);

        let data = child.component_themes.get_mut("test").unwrap();
        data.set_size_default("height", PropertyValue::Float(10.0));
        data.set_size_override("sm", "height", PropertyValue::Float(6.0));

        let json = export_theme_record_json(&[base, child], 1, &component_plugins);
        assert_eq!(
            json.get("components")
                .and_then(|components| components.get("test"))
                .and_then(|test| test.get("sizeProps"))
                .and_then(|sizes| sizes.get("common"))
                .and_then(|common| common.get("height"))
                .and_then(Value::as_f64),
            Some(10.0)
        );
        assert_eq!(
            json.get("components")
                .and_then(|components| components.get("test"))
                .and_then(|test| test.get("sizeProps"))
                .and_then(|sizes| sizes.get("sm"))
                .and_then(|sm| sm.get("height"))
                .and_then(Value::as_f64),
            Some(6.0)
        );
    }
}
