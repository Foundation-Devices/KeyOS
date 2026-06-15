// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Handlers for CaptureScreen, InjectTouch, InjectKey, and InjectPowerButton.
//! Used by the passport-drive debug bridge.

use gui_server_api::msg::{CaptureScreen, InjectKey, InjectPowerButton, InjectTouch};
use server::{LendMutHandler, ScalarHandler, ServerContext};
use xous::PID;

use crate::Gui;

impl LendMutHandler<CaptureScreen> for Gui {
    fn handle(
        &mut self,
        CaptureScreen(mut mem): CaptureScreen,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        let out = mem.as_slice_mut();
        self.capture_screen_into(out);
    }
}

impl ScalarHandler<InjectTouch> for Gui {
    fn handle(&mut self, InjectTouch(touch): InjectTouch, _sender: PID, _context: &mut ServerContext<Self>) {
        self.touch_dispatch(touch, true);
    }
}

impl ScalarHandler<InjectKey> for Gui {
    fn handle(
        &mut self,
        InjectKey { is_pressed, key }: InjectKey,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.dispatch_key_event(is_pressed, key);
    }
}

impl ScalarHandler<InjectPowerButton> for Gui {
    fn handle(
        &mut self,
        InjectPowerButton(is_pressed): InjectPowerButton,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.handle_power_button(is_pressed);
    }
}
