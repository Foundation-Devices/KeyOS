// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

mod theme;

use slint_keyos_platform::app_minimal;

app_minimal!("{{friendly_app_name}}");

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    theme::init(&ui);
    slint_keyos_platform::_internal_init_images_with_theme!(Images, Theme, ui, cx);

    ui.run().expect("UI running");
}
