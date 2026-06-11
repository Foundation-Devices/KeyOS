// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Virtual button handling routines.

use gui_server_api::{
    touch::{Touch, TouchKind},
    InputMessage,
};

use crate::registry::AppRole;
use crate::Gui;

impl Gui {
    pub(crate) fn virtbutton_process_touch(&mut self, touch: Touch) {
        if self.is_onboarding_running() {
            match touch.kind {
                TouchKind::Press => {
                    self.haptics_click();
                    self.rgb_led.virt_button_press_animation();
                }
                TouchKind::Drag => {}
                TouchKind::Release => {
                    self.rgb_led.virt_button_release_animation();
                    self.notify_onboarding_home_button_pressed();
                }
            }
            return;
        }

        match touch.kind {
            TouchKind::Press => {
                if !self.home_button_enabled() {
                    // Give discouraging feedback when pressing a disabled home button
                    self.haptics_triple_click();
                    self.rgb_led.disabled_virt_button_press_animation();
                } else {
                    self.haptics_click();
                    self.rgb_led.virt_button_press_animation();
                    self.touch_state.switcher_gesture_state.virt_button_initial_pos.replace(touch);
                }
            }

            TouchKind::Drag => {
                if self.home_button_enabled() {
                    // Sometimes a gesture starts with a drag with no Press touch event
                    if self.touch_state.switcher_gesture_state.virt_button_initial_pos.is_none() {
                        self.touch_state.switcher_gesture_state.virt_button_initial_pos.replace(touch);
                    }

                    crate::switcher::process_touch(self, touch);
                }
            }

            TouchKind::Release => {
                if self.home_button_enabled() {
                    self.rgb_led.virt_button_release_animation();
                    if self.touch_state.switcher_gesture_state.virt_button_initial_pos.is_none()
                        || !self.touch_state.switcher_gesture_state.started
                    {
                        self.switch_to_launcher();
                        self.touch_state.switcher_gesture_state.virt_button_initial_pos = None;
                    }
                } else {
                    self.rgb_led.disabled_virt_button_release_animation();
                }
            }
        }
    }

    fn notify_onboarding_home_button_pressed(&self) {
        let Some(onboarding_pid) = self.app_registry.pid(AppRole::Onboarding) else {
            log::error!("No onboarding app PID found");
            return;
        };
        let Some(onboarding_window) = self.windows.get(&onboarding_pid) else {
            log::error!("No onboarding app window found");
            return;
        };

        if let Err(e) = xous::send_message(
            onboarding_window.input_cid,
            xous::Message::new_scalar(InputMessage::Custom1 as usize, 0, 0, 0, 0),
        ) {
            log::error!("Failed to notify onboarding about home button press: {e:?}");
        }
    }
}
