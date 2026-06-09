// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

#[cfg(keyos)]
use slint_keyos_platform::gui_server_api;
use slint_keyos_platform::{
    app,
    gui_server_api::{navigation::alerts::InvokeAlert, InputMessage},
    StoredValue,
};

pub struct AppState {
    pub ui: slint::Weak<AppWindow>,
    pub gui: Arc<GuiApi>,
}

app!("Alerts", kind = Alerts);

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let state = StoredValue::new(AppState { ui: ui.as_weak(), gui: cx.gui.clone() });

    set_input_handler(&cx, state);

    #[cfg(keyos)]
    init_state(state);

    ui.run().expect("Failed to run UI");
}

fn set_input_handler(cx: &AppContext, state: StoredValue<AppState>) {
    cx.set_input_handler(move |input| {
        let state = state.borrow();
        let ui = state.ui.unwrap();
        match input.msg {
            InputMessage::NavigationFocused => {
                let Ok(Some(nav_bytes)) = state.gui.navigate_pending() else {
                    log::error!("Navigation focused but no pending nav request");
                    return;
                };
                let Some(options) = InvokeAlert::from_slice(&nav_bytes) else {
                    log::error!("Failed to parse InvokeAlert from a nav request");
                    return;
                };

                log::debug!("Invoking alert: {:?}", options);
                let generic_alert_global = ui.global::<GenericAlertGlobal>();
                generic_alert_global.set_app_title(options.app_title.unwrap_or("".to_string()).into());
                generic_alert_global.set_title(options.title.into());
                generic_alert_global.set_icon(options.icon.into());
                generic_alert_global.set_line1(options.line1.into());
                generic_alert_global.set_line2(options.line2.unwrap_or("".to_string()).into());
                generic_alert_global.set_button1_title(options.button1_title.into());
                generic_alert_global
                    .set_button2_title(options.button2_title.unwrap_or("".to_string()).into());

                ui.global::<Navigate>().invoke_generic_alert(Default::default());
            }
            InputMessage::NavigationCancelled => {}
            InputMessage::Hidden => {}
            _ => {}
        };
    });
}

#[cfg(keyos)]
fn init_state(state: StoredValue<AppState>) {
    let ui = state.borrow().ui.unwrap();

    let callbacks = ui.global::<Callbacks>();
    callbacks.on_button1_clicked(move || {
        log::info!("Button1 pressed");

        state.with(|state| {
            let gui_clone = state.gui.clone();

            let alert_result = gui_server_api::navigation::alerts::AlertResult::Button1Pressed.serialize();
            if let Err(e) = gui_clone.navigate_finish(alert_result) {
                log::error!("Failed to finish navigation: {:?}", e);
            }
        })
    });

    callbacks.on_button2_clicked(move || {
        log::debug!("Button2 pressed");

        state.with(|state| {
            let gui_clone = state.gui.clone();

            let alert_result = gui_server_api::navigation::alerts::AlertResult::Button2Pressed.serialize();
            if let Err(e) = gui_clone.navigate_finish(alert_result) {
                log::error!("Failed to finish navigation: {:?}", e);
            }
        })
    });

    callbacks.on_button3_clicked(move || {
        log::debug!("Button3 pressed");

        state.with(|state| {
            let gui_clone = state.gui.clone();

            let alert_result = gui_server_api::navigation::alerts::AlertResult::Button3Pressed.serialize();
            if let Err(e) = gui_clone.navigate_finish(alert_result) {
                log::error!("Failed to finish navigation: {:?}", e);
            }
        })
    });
}
