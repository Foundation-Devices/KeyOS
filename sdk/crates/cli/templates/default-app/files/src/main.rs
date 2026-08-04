// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

mod theme;

use slint_keyos_platform::app_ui2;

app_ui2!("{{friendly_app_name}}");

fn app_main(_cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    theme::init(&ui);

    // Setup button callback
    ui.global::<Callbacks>().on_button_clicked(move || {
        log::info!("Get started clicked");
    });

    ui.run().expect("UI running");
}
