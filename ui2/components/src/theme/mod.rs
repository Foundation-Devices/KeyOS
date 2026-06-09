// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

mod default_tokens;
mod macros;
mod theme;
mod tokens;
mod typed;

pub use default_tokens::create_default_tokens;
pub use theme::*;
pub use tokens::*;
pub use typed::{ComponentKey, ThemedComponent};

/// Color scheme for theming
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorScheme {
    #[default]
    Light,
    Dark,
}

/// Component interaction state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ComponentState {
    #[default]
    Default,
    Focused,
    Loading,
    Pressed,
    Disabled,
    Confirmed,
    Visited,
}

impl ComponentState {
    pub fn key(self) -> &'static str {
        match self {
            ComponentState::Default => "default",
            ComponentState::Focused => "focused",
            ComponentState::Loading => "loading",
            ComponentState::Pressed => "pressed",
            ComponentState::Disabled => "disabled",
            ComponentState::Confirmed => "confirmed",
            ComponentState::Visited => "visited",
        }
    }
}

/// Create the default theme with all component styles
pub fn create_default_theme() -> Theme {
    use std::collections::HashMap;

    let tokens = create_default_tokens();
    let mut theme = Theme { tokens, components: HashMap::new() };

    // Add button component styles
    add_button_theme(&mut theme);

    theme
}

/// Last-resort fallback when `color.primary` isn't defined in the token set.
/// Should never fire for valid themes; kept as a visible, named constant so
/// debugging is easier than chasing a transparent button.
const FALLBACK_PRIMARY: Color = Color::rgb(26, 154, 154);
/// Same idea as `FALLBACK_PRIMARY`, for `color.foreground`.
const FALLBACK_FOREGROUND: Color = Color::rgb(26, 26, 26);

fn add_button_theme(theme: &mut Theme) {
    use std::collections::HashMap;

    let light = &theme.tokens.light;
    let primary = light.get_color("color", "primary").unwrap_or(FALLBACK_PRIMARY);
    let foreground = light.get_color("color", "foreground").unwrap_or(FALLBACK_FOREGROUND);

    let mut button_component = ComponentTheme::new();
    set_style_prop(&mut button_component.base, "font_weight", token_ref("fontWeight.medium"));
    set_style_prop(&mut button_component.base, "border_width", 0.0);
    set_style_prop(&mut button_component.base, "opacity", 1.0);
    set_style_prop(&mut button_component.base, "touch_expansion", 4.0);

    let mut variants = HashMap::new();
    for variant_name in ["primary", "secondary", "danger", "ghost", "tertiary"] {
        let mut variant = VariantTheme::new();

        match variant_name {
            "primary" => {
                set_style_prop(&mut variant.default, "background", token_ref("color.primary"));
                set_style_prop(&mut variant.default, "foreground", token_ref("color.white"));
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Focused.key()),
                    "border_color",
                    token_ref("color.primary.light"),
                );
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Focused.key()),
                    "border_width",
                    2.0,
                );
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Pressed.key()),
                    "background",
                    token_ref("color.primary.dark"),
                );
            }
            "secondary" => {
                set_style_prop(&mut variant.default, "background", token_ref("color.secondary"));
                set_style_prop(&mut variant.default, "foreground", token_ref("color.foreground"));
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Focused.key()),
                    "border_color",
                    token_ref("color.primary"),
                );
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Focused.key()),
                    "border_width",
                    2.0,
                );
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Pressed.key()),
                    "background",
                    token_ref("color.secondary.dark"),
                );
            }
            "danger" => {
                set_style_prop(&mut variant.default, "background", token_ref("color.danger"));
                set_style_prop(&mut variant.default, "foreground", token_ref("color.white"));
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Focused.key()),
                    "border_color",
                    token_ref("color.danger.light"),
                );
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Focused.key()),
                    "border_width",
                    2.0,
                );
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Pressed.key()),
                    "background",
                    token_ref("color.danger.dark"),
                );
            }
            "ghost" => {
                set_style_prop(&mut variant.default, "background", token_ref("color.transparent"));
                set_style_prop(&mut variant.default, "foreground", token_ref("color.foreground"));
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Focused.key()),
                    "border_color",
                    token_ref("color.primary"),
                );
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Focused.key()),
                    "border_width",
                    2.0,
                );
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Pressed.key()),
                    "background",
                    foreground.with_alpha(0.1),
                );
            }
            "tertiary" => {
                set_style_prop(&mut variant.default, "background", token_ref("color.transparent"));
                set_style_prop(&mut variant.default, "foreground", token_ref("color.primary"));
                set_style_prop(&mut variant.default, "border_color", token_ref("color.primary"));
                set_style_prop(&mut variant.default, "border_width", 2.0);
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Focused.key()),
                    "border_color",
                    token_ref("color.primary"),
                );
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Focused.key()),
                    "border_width",
                    2.0,
                );
                set_style_prop(
                    state_style_mut(&mut variant, ComponentState::Pressed.key()),
                    "background",
                    primary.with_alpha(0.2),
                );
            }
            _ => {}
        }

        set_style_prop(
            state_style_mut(&mut variant, ComponentState::Disabled.key()),
            "opacity",
            token_ref("opacity.disabled"),
        );

        for (size_name, font_size_token, padding_h_token, padding_v_token, icon_value, min_height) in [
            (
                "sm",
                "fontSize.sm",
                "spacing.md",
                "spacing.xs",
                ThemeValue::from(token_ref("spacing.lg")),
                52.0,
            ),
            ("md", "fontSize.md", "spacing.lg", "spacing.sm", ThemeValue::from(24.0), 60.0),
            (
                "lg",
                "fontSize.lg",
                "spacing.xl",
                "spacing.md",
                ThemeValue::from(token_ref("spacing.xl")),
                68.0,
            ),
        ] {
            let size_style = size_style_mut(&mut variant, size_name);
            set_style_prop(size_style, "font_family", token_ref("font.primary"));
            set_style_prop(size_style, "font_size", token_ref(font_size_token));
            set_style_prop(size_style, "padding_horizontal", token_ref(padding_h_token));
            set_style_prop(size_style, "padding_vertical", token_ref(padding_v_token));
            set_style_prop(size_style, "icon_size", icon_value.clone());
            set_style_prop(size_style, "min_height", min_height);
            set_style_prop(size_style, "border_radius", token_ref("radius.default"));
        }

        variants.insert(variant_name.to_string(), variant);
    }

    button_component.variants = variants;
    theme.components.insert("button".to_string(), button_component);
}
