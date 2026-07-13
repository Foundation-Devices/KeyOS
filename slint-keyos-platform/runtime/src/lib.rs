// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg_attr(keyos, feature(stdarch_arm_neon_intrinsics))]
#![feature(must_not_suspend)]

pub use slint_keyos_platform_common::{UI2_ICON_SET, UI_ICON_SET};
pub use {
    fiat_symbols, file_backed, fs, futures_lite, gui_server_api, i18n, log, log_server, phf,
    server::{self, FromScalar},
    slint,
    slint_keyos_platform_macros::*,
    xous,
};

pub mod qrcode;
pub mod raw_image;
pub mod route;
pub mod router;
#[cfg(not(feature = "recovery-os"))]
pub use settings;
pub mod navigation;
pub mod skia;
pub mod utilites;

mod fonts;
mod platform;
mod runtime;
mod window;

pub use platform::*;
pub use runtime::*;
pub use window::*;

/// Performs the registration named by `app!`'s `role`: a plain app (`app`), the
/// dedicated `keyboard`/`control_center` windows, or an ordinary app that also
/// claims a role (any claim message type). Expands inside `app!` where `GuiApi`
/// is in scope.
#[macro_export]
macro_rules! _internal_register_app {
    (app, $name:expr, $height:expr) => {
        GuiApi::register($name, $height).expect("can't register app UI")
    };
    (keyboard, $name:expr, $height:expr) => {
        GuiApi::register_keyboard($height).expect("can't register app UI")
    };
    (control_center, $name:expr, $height:expr) => {
        GuiApi::register_control_center($height).expect("can't register app UI")
    };
    ($claim:ident, $name:expr, $height:expr) => {
        GuiApi::register_with_role::<$crate::gui_server_api::msg::$claim>($name, $height)
            .expect("can't register app UI")
    };
}

#[macro_export]
macro_rules! app {
    ($name:expr, role = $role:ident, height = $height:expr) => {
        use $crate::slint;
        slint::include_modules!();
        include!(concat!(env!("OUT_DIR"), "/router_init.rs"));
        include!(concat!(env!("OUT_DIR"), "/tr.rs"));

        $crate::fs::use_api!($crate::fs, $crate::server);
        $crate::gui_server_api::use_api!($crate::gui_server_api, $crate::server);
        $crate::_internal_not_recovery!(
            $crate::settings::use_api!($crate::settings, $crate::server);
        );
        type AppContext = $crate::AppContext<gui_permissions::GuiPermissions, fs_permissions::FileSystemPermissions>;

        fn main() {
            const WIDTH: usize = $crate::gui_server_api::consts::SCREEN_WIDTH;
            const HEIGHT: usize = $height;
            const NAME: &str = $name;

            let gui_api = $crate::_internal_register_app!($role, NAME, HEIGHT);
            let gui_api = std::sync::Arc::new(gui_api);

            $crate::Runtime::unsafe_init({
                let gui_api = gui_api.clone();
                move || {
                    gui_api.wake_event_loop()
                }
            });

            let fs = Default::default();
            let app_context = $crate::AppContext::new(gui_api.clone(), fs);
            let platform = $crate::KeyOsPlatform::<WIDTH, HEIGHT, _>::new(NAME, app_context.clone());
            $crate::_internal_not_recovery!(platform.subscribe_to_theme_changes::<settings_permissions::SettingsPermissions>());
            $crate::slint::platform::set_platform(Box::new(platform)).expect("set platform");

            let ui = AppWindow::new().expect("create app window");

            $crate::_internal_init_ui_utils!(Utils, ui);
            $crate::_internal_init_images!(ui, app_context);
            $crate::_internal_not_recovery!(
                $crate::_internal_init_theme!(
                    CurrentTheme,
                    ui,
                    settings_permissions::SettingsPermissions
                );
            );
            create_router!(ui, app_context);
            init_tr!(ui);

            app_main(app_context, ui);
        }
    };

    ($name:expr, role = $role:ident) => {
        $crate::app!($name, role = $role, height = $crate::gui_server_api::consts::SCREEN_HEIGHT);
    };

    ($name:expr) => {
        $crate::app!($name, role = app);
    };
}

/// App bootstrap for SDK apps built on the shared `ui2` surface. Binds the
/// `Images` global to the `ui2` icon set. The app provides its own theme,
/// globals, and translations.
#[macro_export]
macro_rules! app_ui2 {
    ($name:expr,height = $height:expr) => {
        use $crate::slint;
        slint::include_modules!();
        include!(concat!(env!("OUT_DIR"), "/router_init.rs"));

        $crate::fs::use_api!($crate::fs, $crate::server);
        $crate::gui_server_api::use_api!($crate::gui_server_api, $crate::server);
        $crate::_internal_not_recovery!(
            $crate::settings::use_api!($crate::settings, $crate::server);
        );
        type AppContext = $crate::AppContext<gui_permissions::GuiPermissions, fs_permissions::FileSystemPermissions>;

        fn main() {
            const WIDTH: usize = $crate::gui_server_api::consts::SCREEN_WIDTH;
            const HEIGHT: usize = $height;
            const NAME: &str = $name;

            let gui_api = GuiApi::register(NAME, HEIGHT).expect("can't register app UI");
            let gui_api = std::sync::Arc::new(gui_api);

            $crate::Runtime::unsafe_init({
                let gui_api = gui_api.clone();
                move || {
                    gui_api.wake_event_loop()
                }
            });

            let fs = Default::default();
            let app_context = $crate::AppContext::new(gui_api.clone(), fs);
            let platform = $crate::KeyOsPlatform::<WIDTH, HEIGHT, _>::new(NAME, app_context.clone());
            $crate::slint::platform::set_platform(Box::new(platform)).expect("set platform");

            let ui = AppWindow::new().expect("create app window");

            let fs = app_context.fs.clone();
            let image_cache = Default::default();
            ui.global::<Images>().on_image(move |name| {
                $crate::raw_image::load_raw_image(&fs, &image_cache, format!("images/{name}").into(), false, false)
            });

            let fs = app_context.fs.clone();
            let icon_cache = Default::default();
            ui.global::<Images>().on_icon(move |path, size| {
                $crate::raw_image::load_icon(&fs, &icon_cache, path, size, $crate::UI2_ICON_SET)
            });

            create_router!(ui, app_context);

            app_main(app_context, ui);
        }
    };

    ($name:expr) => {
        $crate::app_ui2!($name, height = $crate::gui_server_api::consts::SCREEN_HEIGHT);
    };
}

#[macro_export]
macro_rules! _internal_init_ui_utils {
    ($utils:ty, $app:ident) => {{
        use $crate::slint;

        $app.global::<$utils>().on_qrcode($crate::qrcode::render);
        $app.global::<$utils>().on_qrcode_parts($crate::qrcode::encode_qr_parts);
        $app.global::<$utils>().on_color_from_hsl($crate::utilites::color_from_hsl);
        $app.global::<$utils>().on_color_from_rgb($crate::utilites::color_from_rgb);
        $app.global::<$utils>().on_get_hsv($crate::utilites::get_hsv);
        $app.global::<$utils>().on_get_rgb($crate::utilites::get_rgb);
        $app.global::<$utils>().on_percent_to_string($crate::utilites::percent_to_string);
        $app.global::<$utils>().on_string_to_percent($crate::utilites::string_to_percent);
        $app.global::<$utils>().on_index_of($crate::utilites::index_of);
        $app.global::<$utils>().on_string_length(|s| s.len() as i32);
        $app.global::<$utils>()
            .on_currency_symbol(|code| $crate::fiat_symbols::symbol_for_code(&code).into());
        $app.global::<$utils>().on_join($crate::utilites::join);
        $app.global::<$utils>().on_split($crate::utilites::split);
        $app.global::<$utils>().on_split_by_length($crate::utilites::split_by_length);
        $app.global::<$utils>().on_fill_array($crate::utilites::fill_array);
        $app.global::<$utils>().on_color_to_hex($crate::utilites::color_to_hex);
        $app.global::<$utils>().on_arc($crate::skia::doughnut);
        $app.global::<$utils>().on_loader($crate::skia::loader);
        $app.global::<$utils>().on_circular_progress($crate::skia::circular_progress);
        $app.global::<$utils>().on_round_image($crate::skia::round_corners);
        $app.global::<$utils>().on_round_image_scaling($crate::skia::round_corners_scaling);
        $app.global::<$utils>().on_shorten_string($crate::utilites::shorten_string);
        // color picker functions
        $app.global::<$utils>().on_pick_color($crate::skia::pick_color);
        $app.global::<$utils>().on_hue_slider($crate::skia::hue_slider);
        $app.global::<$utils>().on_color_palette($crate::skia::color_palette);

        $app.global::<$utils>().on_frame(
            |width: f32,
             height: f32,
             border_radius: f32,
             dash: slint::ModelRc<f32>,
             gradient_direction: crate::GradientDirection,
             gradient_stops: slint::ModelRc<crate::GradientStop>,
             stroke_width: f32,
             stroke_color: slint::Color| {
                use slint::Model; // enable iter() for ModelRc
                use $crate::skia::{GradientDirection, SlintGradientStop};

                let dash: Vec<f32> = dash.iter().collect();

                let d: u8 = gradient_direction as u8;
                let gradient_direction = match d {
                    0 => GradientDirection::None,
                    1 => GradientDirection::Vertical,
                    2 => GradientDirection::Horizontal,
                    3 => GradientDirection::DiagonalFolling,
                    4 => GradientDirection::DiagonalRizing,
                    5 => GradientDirection::Radial,
                    _ => panic!("invalid GradientDirection value: {d}"),
                };

                let gradient_stops = gradient_stops
                    .iter()
                    .map(|s: crate::GradientStop| SlintGradientStop::new(s.color, s.stop))
                    .collect();

                $crate::skia::frame(
                    width,
                    height,
                    border_radius,
                    dash,
                    gradient_direction,
                    gradient_stops,
                    stroke_width,
                    stroke_color,
                )
            },
        );

        // length, length, color, [GradientStop], length, length, [GradientStop], length, [GradientStop],
        // string) -> image;
        $app.global::<$utils>().on_line_card(
            |width: f32,
             height: f32,
             background_color: slint::Color,
             background_gradient: slint::ModelRc<crate::GradientStop>,
             border_radius: f32,
             border_width: f32,
             border_gradient: slint::ModelRc<crate::GradientStop>,
             outer_border_width: f32,
             outer_border_gradient: slint::ModelRc<crate::GradientStop>,
             template: slint::SharedString| {
                use slint::Model; // enable iter() for ModelRc
                use $crate::skia::SlintGradientStop;

                let background_gradient = background_gradient
                    .iter()
                    .map(|s: crate::GradientStop| SlintGradientStop::new(s.color, s.stop))
                    .collect();

                let border_gradient = border_gradient
                    .iter()
                    .map(|s: crate::GradientStop| SlintGradientStop::new(s.color, s.stop))
                    .collect();

                let outer_border_gradient = outer_border_gradient
                    .iter()
                    .map(|s: crate::GradientStop| SlintGradientStop::new(s.color, s.stop))
                    .collect();

                $crate::skia::line_card(
                    width,
                    height,
                    background_color,
                    background_gradient,
                    border_radius,
                    border_width,
                    border_gradient,
                    outer_border_width,
                    outer_border_gradient,
                    template.into(),
                )
            },
        );

        // width, height, border-radius, gradient-start, gradient-end, border-width, border-color
        $app.global::<$utils>().on_panel(
            |width, height, border_radius, gradient_start, gradient_end, border_width, border_color| {
                $crate::skia::frame(
                    width,
                    height,
                    border_radius,
                    [].to_vec(),
                    $crate::skia::GradientDirection::Vertical,
                    vec![
                        $crate::skia::SlintGradientStop::new(gradient_start, 0.0),
                        $crate::skia::SlintGradientStop::new(gradient_end, 100.0),
                    ],
                    border_width,
                    border_color,
                )
            },
        );

        $app.global::<$utils>().on_check_bounds($crate::utilites::check_bounds);
        $app.global::<$utils>().on_is_in_bounds($crate::utilites::is_in_bounds);
        $app.global::<$utils>().on_exceeds_maximum($crate::utilites::exceeds_maximum);
        $app.global::<$utils>().on_increment_string($crate::utilites::increment_string);
        $app.global::<$utils>().on_decrement_string($crate::utilites::decrement_string);
        $app.global::<$utils>().on_filter_special_characters($crate::utilites::filter_special_characters);
    }};
}

#[macro_export]
macro_rules! _internal_init_images {
    ($app:ident, $cx:ident) => {{
        // #IF PREVIEW

        // #ELSE
        use $crate::slint;

        let fs = $cx.fs.clone();
        let common_image_cache = Default::default();
        $app.global::<Images>().on_common({
            let ui = $app.as_weak();
            move |path| {
                let is_dark =
                    ui.upgrade().map(|ui| ui.global::<CurrentTheme>().get_is_dark()).unwrap_or(false);
                $crate::raw_image::load_raw_image(&fs, &common_image_cache, path, false, is_dark)
            }
        });

        let fs = $cx.fs.clone();
        let common_image_cache = Default::default();
        $app.global::<Images>().on_nine_slice({
            let ui_nine_slice = $app.as_weak();
            move |path| {
                let is_dark = ui_nine_slice
                    .upgrade()
                    .map(|ui| ui.global::<CurrentTheme>().get_is_dark())
                    .unwrap_or(false);
                $crate::raw_image::load_raw_image(&fs, &common_image_cache, path, true, is_dark)
            }
        });

        let fs = $cx.fs.clone();
        let icon_cache = Default::default();
        $app.global::<Images>().on_icon(move |path, size| {
            $crate::raw_image::load_icon(&fs, &icon_cache, path, size, $crate::UI_ICON_SET)
        });

        // #ENDIF
    }};
}

#[cfg(feature = "recovery-os")]
#[macro_export]
macro_rules! _internal_not_recovery {
    ($($tt:tt)*) => {};
}

#[cfg(not(feature = "recovery-os"))]
#[macro_export]
macro_rules! _internal_not_recovery {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[macro_export]
macro_rules! _internal_init_theme {
    ($current_theme:ty, $app:ident, $permissions:path) => {{
        use $crate::settings::{global::SystemTheme, messages::SubscribeSystemTheme, SettingsApi};
        let api = SettingsApi::<$permissions>::default();

        // blocking call to get the current theme
        // we want initial renders to be done with the correct theme when launching apps
        let palette = match api.get_system_theme() {
            SystemTheme::Light => Palettes::Light,
            SystemTheme::Dark => Palettes::Dark,
        };
        $app.global::<$current_theme>().set_palette(palette);

        let ui = $app.clone_strong();
        let mut updates = $crate::subscribe_scalar::<$permissions, _>(SubscribeSystemTheme);
        $crate::spawn_local(async move {
            while let Some(theme) = updates.next().await {
                let palette = match theme {
                    SystemTheme::Light => Palettes::Light,
                    SystemTheme::Dark => Palettes::Dark,
                };
                ui.global::<$current_theme>().set_palette(palette);
            }
        })
        .detach();
    }};
}
