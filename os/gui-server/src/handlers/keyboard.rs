// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::msg::{HideKeyboard, KeyPressed, KeyReleased, UpdateKeyboard};
use log::warn;
use server::{ArchiveHandler, Owned, ScalarHandler, ServerContext};
use xous::PID;

use crate::Gui;

impl ArchiveHandler<UpdateKeyboard> for Gui {
    fn handle(&mut self, msg: Owned<UpdateKeyboard>, sender: PID, _context: &mut ServerContext<Self>) {
        let args = msg.as_slice();
        self.show_keyboard_for_an_app(sender, args);
        if self.active_app_pid() == Some(sender) {
            if let Some(keyboard_window) = &mut self.keyboard_window {
                keyboard_window.forward_update(args);
            }
        }
    }
}

impl ScalarHandler<HideKeyboard> for Gui {
    fn handle(&mut self, _msg: HideKeyboard, sender: PID, _context: &mut ServerContext<Self>) {
        self.hide_keyboard_for_an_app(sender);
    }
}

impl ScalarHandler<KeyPressed> for Gui {
    fn handle(&mut self, KeyPressed(key): KeyPressed, sender: PID, _context: &mut ServerContext<Self>) {
        if !self.keyboard_window.as_ref().map(|w| w.pid == sender).unwrap_or(false) {
            warn!("Ignored key press event message from a non-keyboard app");
            return;
        }

        if let Some(key) = key {
            self.dispatch_key_event(true, key);
        }
    }
}

impl ScalarHandler<KeyReleased> for Gui {
    fn handle(&mut self, KeyReleased(key): KeyReleased, sender: PID, _context: &mut ServerContext<Self>) {
        if !self.keyboard_window.as_ref().map(|w| w.pid == sender).unwrap_or(false) {
            warn!("Ignored key release event message from a non-keyboard app");
            return;
        }

        if let Some(key) = key {
            self.dispatch_key_event(false, key);
        }
    }
}
