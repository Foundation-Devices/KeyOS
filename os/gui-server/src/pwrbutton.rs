// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Instant;

use log::{debug, warn};
use server::MessageId as _;
use xous::SID;
use xous_ticktimer::TicktimerCallback;

use crate::{handlers::PowerButtonTimerCallback, Gui};

const POWER_BUTTON_LONG_PRESS_DURATION_MS: u64 = 1000;

#[derive(Default)]
pub(crate) struct PowerButtonState {
    pub(crate) last_pressed: Option<Instant>,
    callback: Option<TicktimerCallback>,
    had_long_press: bool,
}

impl PowerButtonState {
    pub(crate) fn init(&mut self, sid: SID) -> Result<(), xous::Error> {
        self.callback.replace(TicktimerCallback::new(sid)?);
        Ok(())
    }
}

impl Gui {
    pub(crate) fn handle_power_button(&mut self, pressed: bool) {
        if !self.power_button_enabled() {
            debug!("Power button ignored while disabled by kiosk policy");
            return;
        }

        // Ignore power button events when shutting down
        if self.shutting_down.is_some() {
            return;
        }

        // Don't turn off the screen during init
        if matches!(self.state, crate::GuiState::Splash) {
            return;
        }

        debug!("Power button {}", if pressed { "pressed" } else { "released" });

        let Some(callback) = self.power_button_state.callback.as_ref() else {
            warn!("Power button ticktimer callback not initialized");
            return;
        };

        if !self.display.is_lcd_on() {
            if !pressed {
                self.reset_auto_lock();
                self.turn_on_lcd();
            }

            return;
        }

        if pressed {
            self.power_button_state.had_long_press = false;
            self.power_button_state.last_pressed.replace(Instant::now());
            callback.request(POWER_BUTTON_LONG_PRESS_DURATION_MS as usize, PowerButtonTimerCallback::ID, 0);
        } else {
            self.power_button_state.last_pressed.take();
            callback.cancel(PowerButtonTimerCallback::ID);

            if !self.power_button_state.had_long_press {
                #[cfg(not(feature = "recovery-os"))]
                self.lock();
                self.start_turning_off_lcd();
            }
        }
    }

    pub(crate) fn handle_power_button_callback(&mut self) {
        if !self.power_button_enabled() {
            debug!("Power button callback ignored while disabled by kiosk policy");
            return;
        }

        debug!("Callback from power button ticktimer");

        if let Some(pressed_at) = self.power_button_state.last_pressed {
            if pressed_at.elapsed().as_millis() >= POWER_BUTTON_LONG_PRESS_DURATION_MS as u128 {
                debug!("Power button long press detected");
                self.power_button_state.had_long_press = true;

                // Undim the screen upon the long press
                let _ = self.reset_auto_lock();

                if let Some(control_center) = &mut self.control_center_window {
                    control_center.notify_shutdown_mode(true);
                }
                self.control_center_expand();
            }
        }
    }
}
