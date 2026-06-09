// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed resolution layer over the string-keyed `Theme::resolve_style_for_*`
//! API. Components implement [`ThemedComponent`] to declare their valid
//! variant / size / state enums; callers then resolve styles by value instead
//! of by stringly-typed key — typos become compile errors rather than empty
//! `StyleProps` at runtime.
//!
//! ```ignore
//! use components::theme::{create_default_theme, ColorScheme, ComponentState};
//! use components::{ButtonComponent, ButtonSize, ButtonVariant};
//!
//! let theme = create_default_theme();
//! let style = theme.resolve::<ButtonComponent>(
//!     ButtonVariant::Primary,
//!     Some(ButtonSize::Md),
//!     ComponentState::Focused,
//!     ColorScheme::Light,
//! );
//! ```
//!
//! The string-keyed API is still public — runtime-defined components (loaded
//! from JSON, registered by external plugins, etc.) keep working through it.

use super::{ColorScheme, ComponentState, StyleProps, Theme};

/// A value that names a variant/size/state within a component theme.
/// Implemented by the typed enums each component exposes.
pub trait ComponentKey: Copy + 'static {
    /// Stable string identifier used as the HashMap key inside `Theme`.
    /// Must match the key the component author used when registering styles.
    fn key(self) -> &'static str;
}

/// A typed view onto a UI component's theme entries. Implementors associate
/// the component's HashMap key (in `Theme::components`) with the enums that
/// list its valid variants, sizes, and states.
pub trait ThemedComponent {
    /// The string under which this component's styles are stored in
    /// `Theme::components`.
    const NAME: &'static str;

    type Variant: ComponentKey;
    type Size: ComponentKey;
    type State: ComponentKey;
}

impl ComponentKey for ComponentState {
    fn key(self) -> &'static str { ComponentState::key(self) }
}

impl Theme {
    /// Typed style resolution. Equivalent to
    /// `resolve_style_for_state_key_with_scheme` but with enum keys.
    pub fn resolve<C: ThemedComponent>(
        &self,
        variant: C::Variant,
        size: Option<C::Size>,
        state: C::State,
        scheme: ColorScheme,
    ) -> StyleProps {
        self.resolve_style_for_state_key_with_scheme(
            C::NAME,
            variant.key(),
            size.map(|s| s.key()),
            state.key(),
            scheme,
        )
    }

    /// Like `resolve`, but resolves against the default (light) color scheme.
    pub fn resolve_light<C: ThemedComponent>(
        &self,
        variant: C::Variant,
        size: Option<C::Size>,
        state: C::State,
    ) -> StyleProps {
        self.resolve::<C>(variant, size, state, ColorScheme::Light)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::create_default_theme;

    // Inline test component to keep this module's tests self-contained without
    // depending on the layout of ButtonComponent (which lives in `button.rs`).

    #[derive(Copy, Clone)]
    enum TestVariant {
        Primary,
    }
    impl ComponentKey for TestVariant {
        fn key(self) -> &'static str {
            match self {
                TestVariant::Primary => "primary",
            }
        }
    }

    #[derive(Copy, Clone)]
    enum TestSize {
        Md,
    }
    impl ComponentKey for TestSize {
        fn key(self) -> &'static str {
            match self {
                TestSize::Md => "md",
            }
        }
    }

    struct ButtonTestComponent;
    impl ThemedComponent for ButtonTestComponent {
        const NAME: &'static str = "button";
        type Variant = TestVariant;
        type Size = TestSize;
        type State = ComponentState;
    }

    #[test]
    fn typed_resolve_returns_same_style_as_string_resolve() {
        let theme = create_default_theme();
        let typed = theme.resolve::<ButtonTestComponent>(
            TestVariant::Primary,
            Some(TestSize::Md),
            ComponentState::Default,
            ColorScheme::Light,
        );
        let stringly = theme.resolve_style_for_state_key_with_scheme(
            "button",
            "primary",
            Some("md"),
            "default",
            ColorScheme::Light,
        );
        assert_eq!(typed, stringly);
    }
}
