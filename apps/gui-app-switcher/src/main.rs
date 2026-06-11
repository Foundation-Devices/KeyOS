// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::num::NonZeroU8;

use server::xous::PID;
use slint_keyos_platform::{
    app,
    gui_server_api::{
        consts::{SCREEN_HEIGHT, SCREEN_WIDTH},
        touch::Touch,
        InputMessage,
    },
    slint::ComponentHandle,
    StoredValue,
};

mod state;
use state::AppState;

app!("Switcher", role = ClaimSwitcherRole);
fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let state = { StoredValue::new(AppState::default()) };

    ui.global::<Callbacks>().on_switch_to_launcher({
        let gui_api = cx.gui.clone();
        move || {
            gui_api.switch_to_launcher().ok();
        }
    });

    ui.global::<Callbacks>().on_app_touched({
        let gui_api = cx.gui.clone();
        let ui = ui.clone_strong();

        move |is_down, index, pid| {
            if state.borrow_mut().process_app_touch(&ui, is_down, index as usize) {
                let x = SCREEN_WIDTH / 2;
                let y = SCREEN_HEIGHT / 2;
                if let Some(pid) = NonZeroU8::new(pid as u8) {
                    gui_api.switch_to(pid, x, y).ok();
                }
            }
        }
    });

    ui.global::<Callbacks>().on_flicked({
        let ui = ui.clone_strong();
        move |ox| {
            state.borrow_mut().on_flicked(&ui, ox);
        }
    });

    ui.global::<Callbacks>().on_close_all({
        let gui_api = cx.gui.clone();
        let ui = ui.clone_strong();
        move || {
            state.borrow_mut().close_all(&ui, &gui_api);
        }
    });

    cx.set_input_handler({
        let ui = ui.clone_strong();
        let gui_api = cx.gui.clone();

        move |input| match input.msg {
            InputMessage::Visible => {
                AppState::center_card(&ui, 0);
                if state.borrow().is_app_list_empty() {
                    gui_api.switch_to_launcher().ok();
                }
            }

            InputMessage::Hidden => {
                AppState::center_card(&ui, 0);

                // XXX: Since scroll is an animated attribute, just setting it only starts an animation,
                //      which won't be completed while we are hidden, and the next time we are shown,
                //      the first frame will be wrong.
                //      With this timer and update we can force a render in the background after the
                //      animation finished.
                let ui = ui.clone_strong();
                slint_keyos_platform::spawn_local(async move {
                    slint_keyos_platform::sleep(std::time::Duration::from_millis(150)).await;
                    AppState::center_card(&ui, 0);
                })
                .detach();
            }

            InputMessage::Touch => {
                if let Some(touch) = Touch::try_from_input_message(&input.envelope.body) {
                    state.borrow_mut().handle_touch(&ui, &gui_api, touch);
                }
            }

            // App started
            InputMessage::Custom1 => {
                let msg = &input.envelope.body.memory_message().expect("Custom1 was not a scalar message");
                let pid = PID::try_from(msg.offset.unwrap()).unwrap();
                let name_range = msg
                    .buf
                    .subrange(0, msg.valid.expect("zero length name").get())
                    .expect("invalid name length");
                let name = core::str::from_utf8(name_range.as_slice()).expect("Name was not utf-8");
                state.borrow_mut().handle_app_started(&ui, pid, name);
            }

            // App activated
            InputMessage::Custom2 => {
                let msg = &input.envelope.body.scalar_message().expect("Custom2 was not a scalar message");
                state.borrow_mut().handle_app_activated(&ui, PID::new(msg.arg1 as u8).unwrap());
            }
            // App framebuffer
            InputMessage::Custom3 => {
                let msg = &input.envelope.body.memory_message().expect("Custom3 was not a memory message");
                let pid = PID::try_from(msg.offset.unwrap()).unwrap();
                state.borrow_mut().handle_update_app_fb(&ui, pid, msg.buf.as_slice());
            }
            // App closed
            InputMessage::Custom4 => {
                let msg = &input.envelope.body.scalar_message().expect("Custom4 was not a scalar message");
                state.borrow_mut().handle_app_closed(&ui, &gui_api, PID::new(msg.arg1 as u8).unwrap());
            }

            _ => {}
        }
    });

    ui.run().expect("UI running");
}
