// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::{consts::DEFAULT_KEYBOARD_HEIGHT, InputMessage, Key};
use server::AsScalar;
use {
    crate::Gui,
    log::{debug, error, warn},
    xous::{CID, PID},
};

use crate::{BlurBufferState, BufferChain};

const KEYBOARD_ANIMATION_STEP_PX: usize = 30;

pub(crate) struct KeyboardWindow {
    pub(crate) input_cid: CID,
    pub(crate) pid: PID,
    pub(crate) buffers: BufferChain,
    pub(crate) blur_state: BlurBufferState,

    /// Cached archived bytes of the last `UpdateKeyboard` forwarded to the
    /// keyboard app.
    pub(crate) last_update_args: Vec<u8>,

    /// Snapshot of `last_update_args` taken when the keyboard most
    /// recently delivered a frame.
    pub(crate) last_drawn_args: Vec<u8>,

    /// True if the last notification sent to the window was "show", false if last notification was "hidden"
    pub(crate) notified_shown: bool,
}

impl KeyboardWindow {
    /// Forward already-archived `UpdateKeyboard` args to the keyboard
    /// process and remember them as the latest known state.
    pub(crate) fn forward_update(&mut self, args: &[u8]) {
        self.last_update_args = args.into();
        let buf = xous_ipc::Buffer::from_bytes(args);
        if let Err(e) = buf.send(self.input_cid, InputMessage::Custom1 as u32) {
            error!("Failed to send UpdateKeyboard to keyboard app: {e:?}");
        }
    }

    pub(crate) fn notify_keyboard(&self, msg: InputMessage) {
        log::trace!("Notifying keyboard : {msg:?}");
        let msg = xous::Message::new_scalar(msg as usize, 0, 0, 0, 0);
        if let Err(e) = xous::send_message(self.input_cid, msg) {
            error!("Failed to notify keyboard: {e:?}");
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct KeyboardState {
    /// Cached archived `UpdateKeyboard` bytes from this app, replayed to
    /// the keyboard process whenever this app becomes active or its
    /// requested layout needs to be re-applied.
    pub(crate) update_args: Vec<u8>,
    pub state: KeyboardCurrentState,
}

#[derive(Debug, Copy, Clone, Default)]
pub(crate) enum KeyboardCurrentState {
    #[default]
    Hidden,
    SlidingIn {
        height: usize,
    },
    Showing,
    SlidingOut {
        height: usize,
    },
}

impl KeyboardState {
    pub fn height(&self) -> Option<usize> {
        match self.state {
            KeyboardCurrentState::Hidden => None,
            KeyboardCurrentState::SlidingIn { height } | KeyboardCurrentState::SlidingOut { height } => {
                Some(height)
            }
            KeyboardCurrentState::Showing => Some(DEFAULT_KEYBOARD_HEIGHT),
        }
    }
}

impl Gui {
    pub(crate) fn show_keyboard_for_an_app(&mut self, pid: PID, args: &[u8]) {
        debug!("Requested to show keyboard for PID {pid}");
        let Some(app) = self.windows.get_mut(&pid) else {
            warn!("Requested to show the keyboard for an app (PID {pid}) that is not registered");
            return;
        };

        app.keyboard_state.update_args = args.into();
        match &app.keyboard_state.state {
            KeyboardCurrentState::Showing | KeyboardCurrentState::SlidingIn { .. } => {}
            KeyboardCurrentState::Hidden => {
                app.keyboard_state.state = KeyboardCurrentState::SlidingIn { height: 0 }
            }
            KeyboardCurrentState::SlidingOut { height } => {
                app.keyboard_state.state = KeyboardCurrentState::SlidingIn { height: *height }
            }
        }

        if self.active_app_pid() == Some(pid) {
            self.update_keyboard_window();
            self.update_layers();
        }
    }

    pub(crate) fn hide_keyboard_for_an_app(&mut self, pid: PID) {
        debug!("Requested to hide the keyboard");
        let active_pid = self.active_app_pid();
        let Some(app) = self.windows.get_mut(&pid) else {
            warn!("Requested to hide the keyboard for an app (PID {pid}) that is not registered");
            return;
        };

        match &app.keyboard_state.state {
            KeyboardCurrentState::Hidden | KeyboardCurrentState::SlidingOut { .. } => {}
            KeyboardCurrentState::Showing => {
                app.keyboard_state.state =
                    KeyboardCurrentState::SlidingOut { height: DEFAULT_KEYBOARD_HEIGHT }
            }
            KeyboardCurrentState::SlidingIn { height } => {
                app.keyboard_state.state = KeyboardCurrentState::SlidingOut { height: *height }
            }
        }
        if active_pid == Some(pid) {
            self.update_keyboard_window();
            self.update_layers();
        } else {
            app.keyboard_state.state = KeyboardCurrentState::Hidden;
        }
    }

    pub(crate) fn update_keyboard_window(&mut self) {
        let Some(pid) = self.active_app_pid() else { return };
        let Some(window) = self.windows.get_mut(&pid) else { return };
        let Some(keyboard_window) = &mut self.keyboard_window else { return };

        let visible = !matches!(window.keyboard_state.state, KeyboardCurrentState::Hidden);
        if visible && !keyboard_window.notified_shown {
            keyboard_window.buffers.show();
            keyboard_window.notify_keyboard(InputMessage::Visible);
            keyboard_window.notified_shown = true;
        }
        if !visible && keyboard_window.notified_shown {
            keyboard_window.buffers.hide();
            keyboard_window.notify_keyboard(InputMessage::Hidden);
            keyboard_window.notified_shown = false;
        }

        if !matches!(window.keyboard_state.state, KeyboardCurrentState::Hidden)
            && window.keyboard_state.update_args != keyboard_window.last_update_args
        {
            keyboard_window.forward_update(&window.keyboard_state.update_args);
        }
    }

    pub(crate) fn keyboard_animation_tick(&mut self) {
        let Some(pid) = self.active_app_pid() else { return };
        let Some(window) = self.windows.get_mut(&pid) else { return };
        let Some(keyboard_window) = &mut self.keyboard_window else { return };

        match &mut window.keyboard_state.state {
            KeyboardCurrentState::SlidingOut { height } => {
                if *height > KEYBOARD_ANIMATION_STEP_PX {
                    *height -= KEYBOARD_ANIMATION_STEP_PX;
                } else {
                    window.keyboard_state.state = KeyboardCurrentState::Hidden;
                }
            }
            KeyboardCurrentState::SlidingIn { height } => {
                // If the keyboard hasn't acked the latest payload yet, wait.
                let right_args = window.keyboard_state.update_args == keyboard_window.last_drawn_args;
                // If we don't even have a display buffer, also wait.
                let has_buffers = window.buffers.most_recent_buffer().is_some();
                if right_args && has_buffers {
                    if *height < DEFAULT_KEYBOARD_HEIGHT - KEYBOARD_ANIMATION_STEP_PX {
                        *height += KEYBOARD_ANIMATION_STEP_PX;
                    } else {
                        window.keyboard_state.state = KeyboardCurrentState::Showing;
                    }
                }
            }
            _ => {}
        }
        self.update_keyboard_window();
    }

    /// Sends the key press/release event to the currently active app.
    pub(crate) fn dispatch_key_event(&mut self, is_pressed: bool, key: Key) {
        let [arg1, arg2] = key.as_scalar();

        let input_msg_kind = if is_pressed { InputMessage::KeyPress } else { InputMessage::KeyRelease };

        debug!("Sending key {}: {:?}", if is_pressed { "press" } else { "release" }, key);

        self.with_active_app_mut(|app| {
            let msg = xous::Message::new_scalar(input_msg_kind as usize, arg1 as usize, arg2 as usize, 0, 0);
            if let Err(e) = xous::send_message(app.input_cid, msg) {
                error!("Failed to send the input event to the app: {e:?}");
            }
        });
    }

    #[cfg(not(keyos))]
    pub(crate) fn dispatch_scroll_event(&mut self, x: u32, y: u32, delta_x: f32, delta_y: f32) {
        debug!("Sending scroll: pos=({x},{y}) delta=({delta_x},{delta_y})");
        self.with_active_app_mut(|app| {
            let msg = xous::Message::new_scalar(
                InputMessage::Scroll as usize,
                x as usize,
                y as usize,
                delta_x.to_bits() as usize,
                delta_y.to_bits() as usize,
            );
            if let Err(e) = xous::send_message(app.input_cid, msg) {
                error!("Failed to send scroll event to the app: {e:?}");
            }
        });
    }
}
