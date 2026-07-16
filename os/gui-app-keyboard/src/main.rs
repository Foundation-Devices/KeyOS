// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod cache;
#[macro_use]
pub mod colors;
mod drawing;
mod font;
mod key_slot;
mod keyboard;
mod keys;
mod layout;
mod overlay;
mod sliding;

use std::{rc::Rc, sync::atomic::AtomicBool};

use gui_server_api::{
    consts::DEFAULT_KEYBOARD_HEIGHT,
    msg::UpdateKeyboard,
    touch::{Touch, TouchKind},
    InputMessage, KeyboardKind,
};
use worker::WorkerHandle;
use xous::MessageEnvelope;
use xous_api_ticktimer::TicktimerCallback;

use crate::{cache::refresh_cache, keyboard::KeyboardState};

gui_server_api::use_api!();
haptics::use_api!();
#[cfg(not(feature = "recovery-os"))]
settings::use_api!();

static IS_DARK: AtomicBool = AtomicBool::new(true);

fn main() -> ! {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let gui = GuiApi::register(gui_server_api::AppKind::Keyboard, "keyboard", DEFAULT_KEYBOARD_HEIGHT)
        .expect("can't register app UI");

    let haptics_api = Rc::new(HapticsApi::default());
    let long_press_callback = TicktimerCallback::new(gui.sid()).unwrap();
    let mut state =
        keyboard::KeyboardState::new(KeyboardKind::Numbers, haptics_api.clone(), long_press_callback);

    let background_worker = WorkerHandle::default();
    background_worker
        .spawn(async { xous::set_thread_priority(xous::ThreadPriority::AppBackground1).unwrap() })
        .detach();
    #[cfg(not(feature = "recovery-os"))]
    subscribe_theme_change(&background_worker);

    #[cfg(feature = "recovery-os")]
    background_worker.spawn(async { refresh_cache() }).detach();

    loop {
        let (event, msg) = gui.receive_input().unwrap();
        process_input(event, msg, &mut state, &gui);
    }
}

#[cfg(not(feature = "recovery-os"))]
fn subscribe_theme_change(background_worker: &WorkerHandle) {
    let mut theme_updates = background_worker
        .subscribe_scalar::<settings_permissions::SettingsPermissions, _>(
            settings::messages::SubscribeSystemTheme,
        );
    background_worker
        .spawn(async move {
            while let Some(theme) = theme_updates.next().await {
                IS_DARK
                    .store(theme == settings::global::SystemTheme::Dark, std::sync::atomic::Ordering::SeqCst);
                refresh_cache();
            }
        })
        .detach();
}

fn process_input(event: InputMessage, msg: MessageEnvelope, state: &mut KeyboardState, gui: &GuiApi) {
    match event {
        InputMessage::Touch => {
            if let Some(touch) = Touch::try_from_input_message(&msg.body) {
                match touch.kind {
                    TouchKind::Press => {
                        state.on_pressed(touch.x as f32, touch.y as f32);
                    }
                    TouchKind::Drag => {
                        if let Some(key) = state.on_moved(touch.x as f32, touch.y as f32) {
                            gui.key_pressed(key).expect("gui-server api unavailable");
                            gui.key_released(key).expect("gui-server api unavailable");
                        }
                    }
                    TouchKind::Release => {
                        if let Some(key) = state.on_released(touch.x as f32, touch.y as f32) {
                            gui.key_pressed(key).expect("gui-server api unavailable");
                            gui.key_released(key).expect("gui-server api unavailable");
                        }
                    }
                };
                state.request_redraw(gui);
            }
        }
        InputMessage::Hidden => {
            state.shift_state_request(false);
            state.request_redraw(gui);
        }
        InputMessage::Custom1 => {
            let owned = match server::Owned::<UpdateKeyboard>::new(msg) {
                Ok(o) => o,
                Err(e) => {
                    log::warn!("Custom1 was not a valid UpdateKeyboard archive: {e:?}");
                    return;
                }
            };
            log::debug!("Got keyboard update: {owned:?}");
            let kind = KeyboardKind::from(&owned.kind);
            if state.kind() != kind {
                state.set_kind(kind);
            }
            state.shift_state_request(owned.request_caps);
            state.set_accept_button_text(owned.accept_button_text.as_str());
            state.set_accept_button_enabled(owned.accept_button_enabled);
            state.set_delete_button_enabled(owned.delete_button_enabled);
            state.request_redraw(gui);
        }
        InputMessage::Custom4 => {
            state.on_long_press();
            state.request_redraw(gui);
        }
        InputMessage::FrameBuffer => {
            let msg = msg.take_message();
            if let Some(mem) = msg.memory_message() {
                let vsync_time = mem.offset.map(|o| o.get()).unwrap_or(0);
                let is_new = mem.valid.is_some();
                state.draw(vsync_time, is_new, mem.buf);
                #[cfg(keyos)]
                xous::syscall::flush_cache(mem.buf, xous::CacheOperation::Clean).expect("clean cache");
                gui.submit_frame(mem.buf).unwrap();
            }
        }
        _ => (),
    }
}
