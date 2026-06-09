// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

#[doc(hidden)]
#[macro_export]
macro_rules! __define_theme_props {
    ($style:expr,) => {};
    ($style:expr, $prop:literal = $value:expr; $($rest:tt)*) => {{
        $crate::theme::set_style_prop($style, $prop, $value);
        $crate::__define_theme_props!($style, $($rest)*);
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __define_theme_variant {
    ($variant:expr,) => {};
    ($variant:expr, default { $($props:tt)* } $($rest:tt)*) => {{
        $crate::__define_theme_props!(&mut $variant.default, $($props)*);
        $crate::__define_theme_variant!($variant, $($rest)*);
    }};
    ($variant:expr, size $size_name:literal { $($props:tt)* } $($rest:tt)*) => {{
        {
            let __style = $crate::theme::size_style_mut($variant, $size_name);
            $crate::__define_theme_props!(__style, $($props)*);
        }
        $crate::__define_theme_variant!($variant, $($rest)*);
    }};
    ($variant:expr, state $state_name:literal { $($props:tt)* } $($rest:tt)*) => {{
        {
            let __style = $crate::theme::state_style_mut($variant, $state_name);
            $crate::__define_theme_props!(__style, $($props)*);
        }
        $crate::__define_theme_variant!($variant, $($rest)*);
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __define_theme_component {
    ($theme:expr, $component_name:literal,) => {};
    ($theme:expr, $component_name:literal, base { $($props:tt)* } $($rest:tt)*) => {{
        {
            let __component = $crate::theme::component_mut(&mut $theme, $component_name);
            $crate::__define_theme_props!(&mut __component.base, $($props)*);
        }
        $crate::__define_theme_component!($theme, $component_name, $($rest)*);
    }};
    ($theme:expr, $component_name:literal, variant $variant_name:literal { $($variant_body:tt)* } $($rest:tt)*) => {{
        {
            let __component = $crate::theme::component_mut(&mut $theme, $component_name);
            let __variant = $crate::theme::variant_mut(__component, $variant_name);
            $crate::__define_theme_variant!(__variant, $($variant_body)*);
        }
        $crate::__define_theme_component!($theme, $component_name, $($rest)*);
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __define_theme_body {
    ($theme:ident,) => {};
    ($theme:ident, extends $parent:expr; $($rest:tt)*) => {{
        let __parent_theme = $parent;
        $theme.inherit_from(&__parent_theme);
        $crate::__define_theme_body!($theme, $($rest)*);
    }};
    ($theme:ident, token light $path:literal = $value:expr; $($rest:tt)*) => {{
        $crate::theme::set_token(&mut $theme, $crate::theme::ColorScheme::Light, $path, $value);
        $crate::__define_theme_body!($theme, $($rest)*);
    }};
    ($theme:ident, token dark $path:literal = $value:expr; $($rest:tt)*) => {{
        $crate::theme::set_token(&mut $theme, $crate::theme::ColorScheme::Dark, $path, $value);
        $crate::__define_theme_body!($theme, $($rest)*);
    }};
    ($theme:ident, tokens { $($token_body:tt)* } $($rest:tt)*) => {{
        $crate::__define_theme_tokens!($theme, $($token_body)*);
        $crate::__define_theme_body!($theme, $($rest)*);
    }};
    ($theme:ident, component $component_name:literal { $($component_body:tt)* } $($rest:tt)*) => {{
        $crate::__define_theme_component!($theme, $component_name, $($component_body)*);
        $crate::__define_theme_body!($theme, $($rest)*);
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __define_theme_tokens {
    ($theme:ident,) => {};
    ($theme:ident, light { $($entries:tt)* } $($rest:tt)*) => {{
        $crate::__define_theme_token_entries!(
            $theme,
            $crate::theme::ColorScheme::Light,
            $($entries)*
        );
        $crate::__define_theme_tokens!($theme, $($rest)*);
    }};
    ($theme:ident, dark { $($entries:tt)* } $($rest:tt)*) => {{
        $crate::__define_theme_token_entries!(
            $theme,
            $crate::theme::ColorScheme::Dark,
            $($entries)*
        );
        $crate::__define_theme_tokens!($theme, $($rest)*);
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __define_theme_token_entries {
    ($theme:ident, $scheme:expr,) => {};
    ($theme:ident, $scheme:expr, $path:literal = $value:expr; $($rest:tt)*) => {{
        $crate::theme::set_token(&mut $theme, $scheme, $path, $value);
        $crate::__define_theme_token_entries!($theme, $scheme, $($rest)*);
    }};
}

#[macro_export]
macro_rules! define_theme {
    ($vis:vis fn $name:ident () -> $ret:ty { $($body:tt)* }) => {
        $vis fn $name() -> $ret {
            let mut theme = <$ret>::new();
            $crate::__define_theme_body!(theme, $($body)*);
            theme
        }
    };
}
