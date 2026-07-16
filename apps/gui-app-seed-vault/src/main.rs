// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::state::AppState,
    slint_keyos_platform::{
        app,
        gui_server_api::{
            navigation::qrscanner::{MatchedQrResult, ScanQrResult},
            InputMessage,
        },
        StoredValue,
    },
};

mod callbacks;
pub mod error;
mod seed;
pub mod state;

app!("Vault");

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    cx.config.enable_swipe_back.set(false);

    let mut app_state = AppState::new(cx.gui.clone(), ui.as_weak());

    if app_state.is_empty() {
        ui.global::<Navigate>().invoke_main(NavigateOptions { replace: true, animate: Animate::None });
    }

    app_state.update_accounts();

    let ui_state = ui.global::<Callbacks>();
    ui_state.set_sort_mode(app_state.get_sort_mode() as i32);

    let app_state = StoredValue::new(app_state);

    callbacks::init_callbacks(app_state);

    cx.set_input_handler({
        let gui_api = cx.gui.clone();
        let ui = ui.clone_strong();
        move |input| {
            if input.msg != InputMessage::NavigationFocused {
                return;
            }

            let Ok(Some(nav_bytes)) = gui_api.navigate_pending() else {
                log::error!("Navigation focused but no pending nav request");
                return;
            };

            let Some(MatchedQrResult { scan_result, matched_rules }) =
                MatchedQrResult::from_slice(&nav_bytes)
            else {
                return;
            };

            let ScanQrResult::Qr { data, .. } = scan_result else {
                log::warn!(
                    "Unexpected scan result type for universal QR: matched_rules={:?}",
                    matched_rules.iter().map(|r| r.rule_id.as_str()).collect::<Vec<_>>()
                );
                return;
            };

            ui.global::<Navigate>().invoke_return_home_animate(Animate::None);
            app_state.borrow_mut().handle_qr_input(data);
        }
    });

    ui.run().expect("UI running");
}
