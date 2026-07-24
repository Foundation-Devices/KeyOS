// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Foundation theme system.
//!
//! Themes are authored as JSON (the editor / source-of-truth format) and
//! compiled to Rust on demand. The flow is:
//!
//! ```text
//! ~/.foundation/themes/json/<id>.json   (source of truth — editor writes these)
//!         │  `foundation themes build`
//!         ▼
//! ~/.foundation/themes/rust/<id>.rs     (generated; each exposes `fn theme()`)
//!         │  include_theme!(<id>)  in the app
//!         ▼
//! app picks a base theme and calls apply_theme!(ui, <id>::theme(), scheme)
//! ```
//!
//! - [`build`] holds the JSON→Rust codegen (used by the CLI and, historically, app build scripts).
//! - [`runtime`] holds the data-extraction helpers used by [`apply_theme!`].
//! - The SDK ships base themes as JSON under this crate's `themes/` directory; they're seeded into
//!   `~/.foundation/themes/json/` for the editor and as inheritance roots for user themes.

use std::path::PathBuf;

pub use components::{
    color, define_theme, get_token, token_ref, Color as ThemeColor, ColorScheme, ComponentState,
    Theme as ExportTheme, TokenValue,
};

pub mod build;
pub mod runtime;

/// Root of the user-scoped theme cache: `~/.foundation/themes`.
///
/// Honors `FOUNDATION_THEMES_DIR` for tests / sandboxed builds, otherwise
/// `~/.foundation/themes`, falling back to `./.foundation/themes` if the home
/// directory can't be determined.
pub fn themes_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FOUNDATION_THEMES_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join(".foundation").join("themes")
}

/// Editor source-of-truth JSON directory: `<themes_dir>/json`.
pub fn themes_json_dir() -> PathBuf { themes_dir().join("json") }

/// Generated Rust directory: `<themes_dir>/rust`.
pub fn themes_rust_dir() -> PathBuf { themes_dir().join("rust") }

/// Include a generated theme module by id, producing `mod <id> { … }` with a
/// `pub fn theme() -> ExportTheme`.
///
/// Resolves the generated file from `FOUNDATION_THEMES_RUST_DIR` at compile
/// time — `foundation build` / `foundation sim` set this to
/// `~/.foundation/themes/rust`. Building outside the Foundation CLI requires
/// setting that env var (run `foundation themes build` first).
///
/// ```ignore
/// foundation_themes::include_theme!(base_theme);
/// let t = base_theme::theme();
/// ```
#[macro_export]
macro_rules! include_theme {
    ($name:ident) => {
        pub mod $name {
            include!(concat!(env!("FOUNDATION_THEMES_RUST_DIR"), "/", stringify!($name), ".rs"));
        }
    };
}

/// Push a resolved [`ExportTheme`] into an app's Slint `Theme` global.
///
/// Expands at the call site so it can reference the app's generated
/// `crate::Theme` type. Sets the palette, the full `color-*` surface, the
/// shared design tokens, and the named text-role font sizes. Button
/// variant/size styling is not pushed here — it lives in the generated
/// `ButtonTheme` global (button_theme.slint), which cascades from those
/// `color-*` + token values automatically.
///
/// ```ignore
/// foundation_themes::include_theme!(base_theme);
///
/// pub fn customize_theme(ui: &crate::AppWindow) {
///     let scheme = foundation_themes::ColorScheme::Light;
///     foundation_themes::apply_theme!(ui, base_theme::theme(), scheme);
///     // App-specific overrides (usually empty):
///     // ui.global::<crate::Theme>().set_font_size_title(28.0);
/// }
/// ```
#[macro_export]
macro_rules! apply_theme {
    ($ui:expr, $theme:expr, $scheme:expr) => {{
        let __theme = $theme;
        let __scheme = $scheme;
        let __g = $ui.global::<crate::Theme>();

        __g.set_is_dark(matches!(__scheme, $crate::ColorScheme::Dark));

        // ----- semantic palette -----
        let __color = |category: &str, key: &str, fr: u8, fg: u8, fb: u8| {
            $crate::runtime::token_color(
                &__theme,
                __scheme,
                category,
                key,
                $crate::ThemeColor::rgb(fr, fg, fb),
            )
            .to_slint()
        };
        __g.set_palette_primary(__color("color", "primary", 0, 157, 185));
        __g.set_palette_primary_pressed(__color("color", "primary.dark", 0, 111, 131));
        __g.set_palette_secondary(__color("color", "secondary", 213, 212, 213));
        __g.set_palette_secondary_pressed(__color("color", "secondary.dark", 227, 226, 226));
        __g.set_palette_danger(__color("color", "danger", 255, 51, 51));
        __g.set_palette_surface(__color("color", "surface", 255, 255, 255));
        __g.set_palette_card(__color("color", "surface", 255, 255, 255));
        __g.set_palette_background(__color("color", "background", 255, 255, 255));
        __g.set_palette_foreground(__color("color", "foreground", 35, 31, 32));
        __g.set_palette_muted(__color("color", "muted", 149, 147, 148));
        __g.set_palette_border(__color("color", "border", 213, 212, 213));

        // ----- full color tokens (for per-component variant styling, e.g. button) -----
        __g.set_color_primary(__color("color", "primary", 0, 157, 185));
        __g.set_color_primary_light(__color("color", "primary.light", 51, 177, 199));
        __g.set_color_primary_dark(__color("color", "primary.dark", 0, 111, 131));
        __g.set_color_secondary(__color("color", "secondary", 213, 212, 213));
        __g.set_color_secondary_dark(__color("color", "secondary.dark", 227, 226, 226));
        __g.set_color_foreground(__color("color", "foreground", 35, 31, 32));
        __g.set_color_foreground_light(__color("color", "foreground.light", 35, 31, 32));
        __g.set_color_muted(__color("color", "muted", 149, 147, 148));
        __g.set_color_border(__color("color", "border", 213, 212, 213));
        __g.set_color_surface(__color("color", "surface", 255, 255, 255));
        __g.set_color_background(__color("color", "background", 255, 255, 255));
        __g.set_color_danger(__color("color", "danger", 255, 51, 51));
        __g.set_color_danger_light(__color("color", "danger.light", 255, 92, 92));
        __g.set_color_danger_dark(__color("color", "danger.dark", 181, 36, 36));
        __g.set_color_success(__color("color", "success", 22, 163, 74));
        __g.set_color_warning(__color("color", "warning", 217, 119, 6));
        __g.set_color_info(__color("color", "info", 37, 99, 235));
        __g.set_color_white(__color("color", "white", 255, 255, 255));
        __g.set_color_transparent(
            $crate::runtime::token_color(
                &__theme,
                __scheme,
                "color",
                "transparent",
                $crate::ThemeColor::rgba(0, 0, 0, 0),
            )
            .to_slint(),
        );

        // ----- button styling -----
        // Button variant/state styles + sizes are no longer pushed from here.
        // They live in the generated ButtonTheme global (button_theme.slint),
        // whose entries are bindings over the color-* + token surface set above,
        // so they cascade to the active scheme automatically.

        // ----- named text-role font sizes -----
        let __len = |key: &str, fallback: f32| {
            $crate::runtime::token_length(&__theme, __scheme, "typography", key, fallback)
        };
        __g.set_font_size_title(__len("font-size-title", 24.0));
        __g.set_font_size_body(__len("font-size-body", 18.0));
        __g.set_font_size_subtitle(__len("font-size-subtitle", 16.0));
        __g.set_font_size_label(__len("font-size-label", 14.0));

        // ----- shared design tokens (font.*, fontSize.*, fontWeight.*, controlSize.*,
        // iconSize.*, radius.*, controlRadius.*, controlPaddingInline.*, spacing.*) -----
        // Mirrors the editor's init_theme_global_tokens so real apps get the same
        // tokens the components read. Missing tokens fall back to the theme.slint defaults.
        let __tok = |category: &str, key: &str, fallback: f32| {
            $crate::runtime::token_length(&__theme, __scheme, category, key, fallback)
        };
        let __font = |key: &str, fallback: &str| {
            $crate::runtime::token_string(&__theme, __scheme, "font", key, fallback)
        };
        __g.set_font_primary(__font("primary", "Montserrat").into());
        __g.set_font_secondary(__font("secondary", "Montserrat").into());
        __g.set_font_tertiary(__font("tertiary", "Montserrat").into());
        __g.set_font_size_xs(__tok("fontSize", "xs", 12.0));
        __g.set_font_size_caption(__tok("fontSize", "caption", 13.0));
        __g.set_font_size_helper(__tok("fontSize", "helper", __len("font-size-helper", 14.0)));
        __g.set_font_size_sm(__tok("fontSize", "sm", 20.0));
        __g.set_font_size_md(__tok("fontSize", "md", 22.0));
        __g.set_font_size_lg(__tok("fontSize", "lg", 24.0));
        __g.set_font_weight_normal(__tok("fontWeight", "normal", 400.0) as i32);
        __g.set_font_weight_medium(__tok("fontWeight", "medium", 500.0) as i32);
        __g.set_font_weight_semibold(__tok("fontWeight", "semibold", 600.0) as i32);
        __g.set_font_weight_bold(__tok("fontWeight", "bold", 700.0) as i32);
        __g.set_border_width_none(__tok("borderWidth", "none", 0.0));
        __g.set_border_width_sm(__tok("borderWidth", "sm", 1.0));
        __g.set_border_width_focus(__tok("borderWidth", "focus", 2.0));
        __g.set_control_size_sm(__tok("controlSize", "sm", 48.0));
        __g.set_control_size_md(__tok("controlSize", "md", 56.0));
        __g.set_control_size_lg(__tok("controlSize", "lg", 64.0));
        __g.set_choice_control_size_sm(__tok("choiceControlSize", "sm", 24.0));
        __g.set_choice_control_size_md(__tok("choiceControlSize", "md", 28.0));
        __g.set_choice_control_size_lg(__tok("choiceControlSize", "lg", 32.0));
        __g.set_switch_size_sm(__tok("switchSize", "sm", 20.0));
        __g.set_switch_size_md(__tok("switchSize", "md", 24.0));
        __g.set_switch_size_lg(__tok("switchSize", "lg", 28.0));
        __g.set_icon_size_sm(__tok("iconSize", "sm", 20.0));
        __g.set_icon_size_md(__tok("iconSize", "md", 24.0));
        __g.set_icon_size_lg(__tok("iconSize", "lg", 28.0));
        __g.set_radius_sm(__tok("radius", "sm", 8.0));
        __g.set_radius_md(__tok("radius", "md", 16.0));
        __g.set_radius_lg(__tok("radius", "lg", 24.0));
        __g.set_radius_default(__tok("radius", "default", 24.0));
        __g.set_radius_full(__tok("radius", "full", 9999.0));
        __g.set_control_radius_sm(__tok("controlRadius", "sm", 20.0));
        __g.set_control_radius_md(__tok("controlRadius", "md", 24.0));
        __g.set_control_radius_lg(__tok("controlRadius", "lg", 28.0));
        __g.set_control_padding_inline_sm(__tok("controlPaddingInline", "sm", 14.0));
        __g.set_control_padding_inline_md(__tok("controlPaddingInline", "md", 16.0));
        __g.set_control_padding_inline_lg(__tok("controlPaddingInline", "lg", 20.0));
        __g.set_spacing_xs(__tok("spacing", "xs", 4.0));
        __g.set_spacing_sm(__tok("spacing", "sm", 8.0));
        __g.set_spacing_md(__tok("spacing", "md", 12.0));
        __g.set_spacing_lg(__tok("spacing", "lg", 16.0));
        __g.set_spacing_xl(__tok("spacing", "xl", 24.0));
    }};
}
