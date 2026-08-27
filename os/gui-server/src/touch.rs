// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(keyos)]
mod atsama5d2;

use gui_server_api::consts::{
    KEYBOARD_TOP_BAR_MARGIN, VIRT_BUTTON_PHYS_HEIGHT, VIRT_BUTTON_PHYS_ORIGIN_X, VIRT_BUTTON_PHYS_ORIGIN_Y,
    VIRT_BUTTON_PHYS_WIDTH,
};
use {
    crate::{control_center::ControlCenterWindowState, switcher::SwitcherGestureState, Gui},
    gui_server_api::{
        consts::{SCREEN_HEIGHT, SCREEN_WIDTH},
        touch::{Touch, TouchKind},
        InputMessage,
    },
};

pub(crate) struct TouchState {
    pub(crate) origin: TouchGestureOrigin,
    pub(crate) switcher_gesture_state: SwitcherGestureState,
    pub(crate) offset: i32,

    #[cfg(keyos)]
    pub(crate) hw_state: crate::touch::atsama5d2::HwTouchState,
    last_x: usize,
    last_y: usize,
}

impl TouchState {
    pub fn init() -> Self {
        TouchState {
            origin: TouchGestureOrigin::None,
            // See SFT-5550
            // This initial value is a fallback, it will be controlled by system settings,
            // and updated by the gui-server's subscription to the TouchOffset setting
            offset: -30,
            #[cfg(keyos)]
            hw_state: Default::default(),
            switcher_gesture_state: SwitcherGestureState::default(),
            last_x: 0,
            last_y: 0,
        }
    }
}

/// Allows to track where the touch gesture started and dispatch further touch events
/// there instead of whatever area the touch events happen over after that.
/// E.g. if user touches the Control Center and then drags it down, all touch events shall be
/// processed by the Control Center app.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum TouchGestureOrigin {
    None,
    ControlCenter,
    App,
    Keyboard,
    VirtualButton,
    Modal,
    AppSwitcherGesture,
}

impl Gui {
    /// Sends incoming touch event to the currently active app, Control Center or the
    /// keyboard depending on the touch coordinates and the current state.
    ///
    /// When `injected` is true (debug bridge), the auto-lock dimming is still reset
    /// but the touch is **not** swallowed — so a single tap is enough to interact
    /// with the device even when the screen has dimmed.
    pub fn touch_dispatch(&mut self, touch: Touch, injected: bool) {
        if self.is_control_center_animating() {
            return;
        }

        let is_within_home_button = touch.is_within_area(
            VIRT_BUTTON_PHYS_ORIGIN_X,
            VIRT_BUTTON_PHYS_ORIGIN_Y,
            VIRT_BUTTON_PHYS_WIDTH,
            VIRT_BUTTON_PHYS_HEIGHT,
        );

        // Don't process more than one simultaneous touch, since the code below assumes a certain
        // Press->drag->release order
        if touch.id > 0 {
            return;
        }

        #[cfg(feature = "recovery-os")]
        let is_locked = true;
        #[cfg(not(feature = "recovery-os"))]
        let is_locked = self.is_locked();

        if touch.kind == TouchKind::Press {
            let keyboard_height =
                self.with_active_app_mut(|w| w.keyboard_state.height().unwrap_or(0)).unwrap_or(0)
                    - KEYBOARD_TOP_BAR_MARGIN;
            let is_within_keyboard =
                touch.is_within_area(0, SCREEN_HEIGHT - keyboard_height, SCREEN_WIDTH, keyboard_height);
            self.touch_state.origin = if is_within_home_button {
                if is_locked {
                    // Redirect touches to the lock screen app if the home button is used while locked
                    TouchGestureOrigin::App
                } else {
                    TouchGestureOrigin::VirtualButton
                }
            } else if !touch.is_within_area(0, 0, SCREEN_WIDTH, SCREEN_HEIGHT) {
                TouchGestureOrigin::None
            } else if self.is_touch_within_control_center(touch) {
                TouchGestureOrigin::ControlCenter
            } else if is_within_keyboard {
                TouchGestureOrigin::Keyboard
            } else if self.is_modal_active() {
                TouchGestureOrigin::Modal
            } else if self.touch_state.switcher_gesture_state.started {
                TouchGestureOrigin::AppSwitcherGesture
            } else if self
                .control_center_window
                .as_ref()
                .map(|w| w.state == ControlCenterWindowState::Expanded)
                .unwrap_or(false)
            {
                TouchGestureOrigin::None
            } else {
                TouchGestureOrigin::App
            };
        }

        // If reset_auto_lock returns true, it means we were dimmed. In this case swallow the gesture, because
        // 'undim' was the action. If the user actually wants to click something, they will press again.
        // For injected (debug bridge) touches, still reset the auto-lock but don't swallow.
        if self.reset_auto_lock() && !injected {
            self.touch_state.origin = TouchGestureOrigin::None;
            return;
        }

        if !self.is_control_center_collapsed()
            && !self.is_control_center_animating()
            && self.touch_state.origin != TouchGestureOrigin::ControlCenter
            && matches!(touch.kind, TouchKind::Drag | TouchKind::Press)
        {
            let is_expanded = self
                .control_center_window
                .as_ref()
                .map(|w| w.state == ControlCenterWindowState::Expanded)
                .unwrap_or(false);

            let should_collapse = if is_expanded {
                touch.kind == TouchKind::Press || touch.y < self.touch_state.last_y
            } else {
                false
            };

            if should_collapse {
                self.control_center_collapse();
                // Swallow the gesture, like above except in case of the virtual button,
                // which is a more deliberate action.
                if self.touch_state.origin != TouchGestureOrigin::VirtualButton {
                    self.touch_state.origin = TouchGestureOrigin::None;
                    return;
                }
            }
        }

        // Filter out redundant drag events that are continuously sent by the chip
        if touch.kind == TouchKind::Drag
            && self.touch_state.last_x == touch.x
            && self.touch_state.last_y == touch.y
        {
            return;
        }

        // Allow the swipe up gesture to be uninterrupted by the Release that's generated when
        // crossing a gap between the home button and the screen
        if is_locked && touch.kind == TouchKind::Release && is_within_home_button {
            return;
        }

        self.touch_state.last_x = touch.x;
        self.touch_state.last_y = touch.y;

        // Apply touch offset once for all touch origins.
        // Skip for injected (debug bridge) touches — their coordinates are already
        // in screen-visual space and don't need hardware calibration.
        // Simulator is touch-perfect and has a locked default offset of 0.
        let offset = if injected { 0 } else { self.touch_state.offset };
        let touch = touch.with_offset(0, offset);

        match self.touch_state.origin {
            TouchGestureOrigin::ControlCenter => {
                self.control_center_process_touch(touch);
            }
            TouchGestureOrigin::Keyboard => {
                if let Some(kw) = &self.keyboard_window {
                    let h = self.with_active_app(|w| w.keyboard_state.height().unwrap_or(0)).unwrap_or(0);
                    let touch = touch.translate_pos(0, SCREEN_HEIGHT - h);
                    log::trace!("Touching keyboard: {touch:?}");
                    xous::try_send_message(
                        kw.input_cid,
                        touch.as_input_message(InputMessage::Touch as usize),
                    )
                    .ok();
                }
            }
            TouchGestureOrigin::App => {
                let mut touch = touch;
                // Shift touch to the lockscreen app if it comes from the home button (unlock swipe)
                if is_locked && is_within_home_button {
                    touch = touch.translate_pos(
                        0,
                        VIRT_BUTTON_PHYS_ORIGIN_Y + VIRT_BUTTON_PHYS_HEIGHT - SCREEN_HEIGHT,
                    );
                }

                if let Some(active_pid) = self.active_app_pid() {
                    if let Some(active_app_window) = self.windows.get(&active_pid) {
                        xous::try_send_message(
                            active_app_window.input_cid,
                            touch.as_input_message(InputMessage::Touch as usize),
                        )
                        .ok();
                    }
                }
            }

            TouchGestureOrigin::VirtualButton => {
                self.virtbutton_process_touch(touch);
            }

            TouchGestureOrigin::Modal => {
                self.modal_process_touch(touch);
            }

            TouchGestureOrigin::AppSwitcherGesture => {
                crate::switcher::process_touch(self, touch);
            }

            TouchGestureOrigin::None => {}
        }

        if touch.kind == TouchKind::Release {
            if let Some(w) = &mut self.control_center_window {
                if matches!(w.state, crate::control_center::ControlCenterWindowState::Dragged) {
                    self.control_center_expand();
                }
            }

            self.touch_state.origin = TouchGestureOrigin::None;
        }
    }

    pub(crate) fn touch_on(&mut self) {
        #[cfg(keyos)]
        self.touch_state.hw_state.enable();
    }

    pub(crate) fn touch_off(&mut self) {
        #[cfg(keyos)]
        self.touch_state.hw_state.disable();
    }
}
