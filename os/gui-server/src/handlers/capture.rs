// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Handlers for CaptureScreen, InjectTouch, InjectKey, and InjectPowerButton.
//! Used by the passport-drive debug bridge.

use gui_server_api::msg::{CaptureScreen, InjectKey, InjectPowerButton, InjectTouch};
use server::{LendMutHandler, ScalarHandler, ServerContext};
use xous::PID;

use crate::Gui;

impl Gui {
    #[cfg(all(keyos, not(feature = "recovery-os")))]
    fn debug_capture_injection_allowed(&self, operation: &str, sender: PID) -> bool {
        if self.developer_mode_enabled {
            return true;
        }

        log::warn!("Rejecting {operation} from PID={sender}: Developer Mode is disabled");
        false
    }

    #[cfg(all(keyos, feature = "recovery-os"))]
    fn debug_capture_injection_allowed(&self, operation: &str, sender: PID) -> bool {
        log::warn!("Rejecting {operation} from PID={sender}: debug capture and input injection are disabled in Recovery OS");
        false
    }

    #[cfg(not(keyos))]
    fn debug_capture_injection_allowed(&self, _operation: &str, _sender: PID) -> bool { true }
}

impl LendMutHandler<CaptureScreen> for Gui {
    fn handle(
        &mut self,
        CaptureScreen(mut mem): CaptureScreen,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        let out = mem.as_slice_mut();
        if !self.debug_capture_injection_allowed("CaptureScreen", sender) {
            out.fill(0);
            return;
        }

        self.capture_screen_into(out);
    }
}

impl ScalarHandler<InjectTouch> for Gui {
    fn handle(&mut self, InjectTouch(touch): InjectTouch, sender: PID, _context: &mut ServerContext<Self>) {
        if !self.debug_capture_injection_allowed("InjectTouch", sender) {
            return;
        }

        self.touch_dispatch(touch, true);
    }
}

impl ScalarHandler<InjectKey> for Gui {
    fn handle(
        &mut self,
        InjectKey { is_pressed, key }: InjectKey,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        if !self.debug_capture_injection_allowed("InjectKey", sender) {
            return;
        }

        self.dispatch_key_event(is_pressed, key);
    }
}

impl ScalarHandler<InjectPowerButton> for Gui {
    fn handle(
        &mut self,
        InjectPowerButton(is_pressed): InjectPowerButton,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        if !self.debug_capture_injection_allowed("InjectPowerButton", sender) {
            return;
        }

        self.handle_power_button(is_pressed);
    }
}
