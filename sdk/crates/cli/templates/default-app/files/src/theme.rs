// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

// App theming.
//
// The app theme is authored as JSON (the source of truth) at the path named by
// `theme` in app-config.toml and compiled to Rust by the Foundation CLI. Use
// `foundation theme` to edit it visually.
//
// You normally don't edit anything here except the app-specific overrides at
// the bottom of customize_theme().

slint_keyos_platform::settings::use_api!(
    slint_keyos_platform::settings,
    slint_keyos_platform::server
);

use foundation_themes::ColorScheme;
use slint_keyos_platform::slint::ComponentHandle;

foundation_themes::include_theme!(app_theme);

pub fn init(ui: &crate::AppWindow) {
    customize_theme(ui, SettingsApi::default().get_system_theme());

    let ui_weak = ui.as_weak();
    let mut updates = slint_keyos_platform::subscribe_scalar::<settings_permissions::SettingsPermissions, _>(
        settings_permissions::settings::messages::SubscribeSystemTheme,
    );
    slint_keyos_platform::spawn_local(async move {
        while let Some(system_theme) = updates.next().await {
            let Some(ui) = ui_weak.upgrade() else {
                break;
            };
            customize_theme(&ui, system_theme);
        }
    })
    .detach();
}

fn customize_theme(
    ui: &crate::AppWindow,
    system_theme: settings_permissions::settings::global::SystemTheme,
) {
    let scheme = match system_theme {
        settings_permissions::settings::global::SystemTheme::Dark => ColorScheme::Dark,
        settings_permissions::settings::global::SystemTheme::Light => ColorScheme::Light,
    };

    // Apply the app theme to every component binding (palette, button styles,
    // sizes, named text-role font sizes).
    foundation_themes::apply_theme!(ui, app_theme::theme(), scheme);

    // App-specific overrides go below (usually empty). For example, to make
    // titles a bit larger than the base theme:
    //
    //   ui.global::<crate::Theme>().set_font_size_title(28.0);
}
