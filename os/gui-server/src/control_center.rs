// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{BufferChain, Gui, GuiState},
    gui_server_api::{
        consts::{
            CONTROL_CENTER_DRAG_MARGIN_PX, CONTROL_CENTER_HEIGHT_COLLAPSED_PX,
            CONTROL_CENTER_HEIGHT_EXPANDED_PX, CONTROL_CENTER_MIN_HEIGHT_PX, SCREEN_WIDTH,
        },
        touch::{Touch, TouchKind},
        ControlCenterColor, InputMessage,
    },
    log::{debug, error},
    xous::{CID, PID},
};

/// Width of the corner dead zones on each side of the collapsed Control Center bar.
/// Touches in these areas fall through to the app so that NavHeader buttons (left/right)
/// have priority over accidentally dragging out the Control Center.
///
/// Sized to cover the NavHeader button visual area:
///   side-padding (Size.sz8 = 32 px) + button size (Size.sz10 = 40 px) = 72 px.
const CONTROL_CENTER_CORNER_SIZE_PX: usize = 72;

/// The dark overlay won't become darker than this value
/// TODO: put into the "fine tuning" settings
const DARK_OVERLAY_ALPHA_MAX: u8 = 255 - 24;

// The position of the drag handle measured from the bottom of the framebuffer.
const DRAG_HANDLE_POS_BOTTOM_PX: usize = 28;
/// The blur amount applied to the background under the dark overlay when the Control Center is fully
/// expanded.
// TODO: Reimplement blurring
#[allow(dead_code)]
const BG_BLUR_RADIUS: usize = 16;

const ANIMATION_STEP: usize = 32;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ControlCenterWindowState {
    Collapsed,
    Dragged,
    Expanded,
    Expanding,
    Collapsing,
}

pub(crate) struct ControlCenterWindow {
    pub(crate) input_cid: CID,
    pub(crate) pid: PID,
    pub(crate) buffers: BufferChain,
    pub(crate) state: ControlCenterWindowState,
    pub(crate) curr_height: usize,
    in_shutdown_mode: bool,
    current_color: Option<ControlCenterColor>,
}

impl ControlCenterWindow {
    pub fn new(input_cid: CID, pid: PID) -> Result<Self, xous::Error> {
        let mut buffers =
            BufferChain::new(input_cid, u16::try_from(CONTROL_CENTER_HEIGHT_EXPANDED_PX).unwrap(), pid);
        // Control center starts out as visible
        buffers.show();
        Gui::send_visible_event(pid, input_cid);
        Ok(ControlCenterWindow {
            input_cid,
            pid,
            buffers,
            state: ControlCenterWindowState::Collapsed,
            curr_height: CONTROL_CENTER_HEIGHT_COLLAPSED_PX,
            in_shutdown_mode: false,
            current_color: None,
        })
    }

    pub(crate) fn notify_expanded(&self, expanded: bool) {
        log::debug!("Setting control center expanded={expanded:?}");
        let msg = xous::Message::new_scalar(InputMessage::Custom3 as usize, expanded as usize, 0, 0, 0);
        xous::send_message(self.input_cid, msg)
            .map_err(|e| error!("Failed to notify control center of being expanded={expanded:?}: {e:?}"))
            .ok();
    }

    pub(crate) fn notify_shutdown_mode(&mut self, shutdown_mode: bool) {
        log::debug!("Setting control center shutdopwn mode={shutdown_mode:?}");
        let msg = xous::Message::new_scalar(InputMessage::Custom4 as usize, shutdown_mode as usize, 0, 0, 0);
        self.in_shutdown_mode = shutdown_mode;
        xous::send_message(self.input_cid, msg)
            .map_err(|e| error!("Failed to notify control center of shutdown mode={shutdown_mode:?}: {e:?}"))
            .ok();
    }

    pub(crate) fn notify_color(&mut self, color: Option<ControlCenterColor>) {
        if color == self.current_color {
            return;
        }
        let (has_custom_color, red, green, blue) = color
            .map_or((0, 0, 0, 0), |color| (1, color.red as usize, color.green as usize, color.blue as usize));
        let msg =
            xous::Message::new_scalar(InputMessage::Custom1 as usize, has_custom_color, red, green, blue);
        match xous::send_message(self.input_cid, msg) {
            Ok(_) => self.current_color = color,
            Err(e) => error!("Failed to notify Control Center of color: {e:?}"),
        }
    }
}

impl ControlCenterWindow {
    /// Converts the current Control Center height into the dark overlay alpha value.
    /// The overlay is intended to become less transparent as the Control Center expands, hiding the content
    /// behind it.
    pub(crate) fn dark_overlay_alpha(&self) -> u8 {
        let height_ratio = self.curr_height as f32 / CONTROL_CENTER_HEIGHT_EXPANDED_PX as f32;
        (DARK_OVERLAY_ALPHA_MAX as f32 * height_ratio) as u8
    }
}

impl Gui {
    pub(crate) fn notify_control_center_color(&mut self) {
        let color = self
            .active_app_pid()
            .and_then(|pid| self.windows.get(&pid))
            .and_then(|window| window.control_center_color);
        if let Some(control_center) = &mut self.control_center_window {
            control_center.notify_color(color);
        }
    }

    pub(crate) fn is_touch_within_control_center(&self, touch: Touch) -> bool {
        self.is_control_center_visible()
            && self
                .control_center_window
                .as_ref()
                .map(|window| {
                    // When collapsed, corner areas fall through to the app so NavHeader buttons
                    // (left/right) take priority over accidentally dragging out the Control Center.
                    let in_corner = window.state == ControlCenterWindowState::Collapsed
                        && (touch.x < CONTROL_CENTER_CORNER_SIZE_PX
                            || touch.x >= SCREEN_WIDTH - CONTROL_CENTER_CORNER_SIZE_PX);
                    touch.is_within_area(0, 0, SCREEN_WIDTH, window.curr_height) && !in_corner
                })
                .unwrap_or(false)
    }

    pub(crate) fn is_control_center_visible(&self) -> bool {
        !matches!(self.state, GuiState::Splash | GuiState::SplashFade { .. })
            && (self.control_center_window.as_ref().map(|cw| cw.in_shutdown_mode).unwrap_or(false)
                || self.control_center_enabled())
    }

    pub(crate) fn control_center_process_touch(&mut self, touch: Touch) {
        if !self.is_control_center_visible() {
            return;
        }

        let Some(window) = &self.control_center_window else {
            return;
        };

        match window.state {
            ControlCenterWindowState::Collapsed => {
                if touch.is_drag() {
                    self.set_control_center_window_state(ControlCenterWindowState::Dragged);
                }
            }
            ControlCenterWindowState::Dragged => {
                self.drag_control_center(touch);
            }
            ControlCenterWindowState::Expanded => {
                if touch.kind == TouchKind::Press
                    && touch.y > CONTROL_CENTER_HEIGHT_EXPANDED_PX - CONTROL_CENTER_DRAG_MARGIN_PX
                {
                    self.drag_control_center(touch);
                } else {
                    // Allow touches to pass to the Control Center app
                    xous::try_send_message(
                        window.input_cid,
                        touch.as_input_message(InputMessage::Touch as usize),
                    )
                    .ok();
                }
            }
            ControlCenterWindowState::Collapsing | ControlCenterWindowState::Expanding => {}
        }
    }

    fn set_control_center_window_state(&mut self, new_state: ControlCenterWindowState) {
        let Some(window) = &mut self.control_center_window else {
            return;
        };
        window.state = new_state;
        window.notify_expanded(new_state != ControlCenterWindowState::Collapsed);
        if new_state == ControlCenterWindowState::Collapsed {
            window.notify_shutdown_mode(false);
            window.notify_expanded(false);
        } else {
            window.notify_expanded(true);
        }
    }

    fn drag_control_center(&mut self, touch: Touch) {
        if let Some(window) = &mut self.control_center_window {
            let needs_layer_update = match touch.kind {
                TouchKind::Press => true,
                TouchKind::Drag => {
                    let new_h = (touch.y + DRAG_HANDLE_POS_BOTTOM_PX)
                        .clamp(CONTROL_CENTER_MIN_HEIGHT_PX, CONTROL_CENTER_HEIGHT_EXPANDED_PX);
                    window.curr_height = new_h;
                    window.state = ControlCenterWindowState::Dragged;
                    true
                }
                TouchKind::Release => {
                    debug!("Expanding control center");
                    self.control_center_expand();
                    false
                }
            };

            if needs_layer_update {
                self.update_layers();
            }
        }
    }

    pub(crate) fn control_center_collapse(&mut self) {
        let Some(window) = &mut self.control_center_window else {
            return;
        };

        if window.state == ControlCenterWindowState::Collapsed {
            return;
        }
        self.set_control_center_window_state(ControlCenterWindowState::Collapsing);
    }

    pub(crate) fn control_center_expand(&mut self) {
        let Some(window) = &mut self.control_center_window else {
            return;
        };

        if window.state == ControlCenterWindowState::Expanded {
            return;
        }
        self.set_control_center_window_state(ControlCenterWindowState::Expanding);
    }

    pub(crate) fn is_control_center_collapsed(&self) -> bool {
        self.control_center_window
            .as_ref()
            .map(|w| w.state == ControlCenterWindowState::Collapsed)
            .unwrap_or(false)
    }

    pub(crate) fn is_control_center_animating(&self) -> bool {
        self.control_center_window
            .as_ref()
            .map(|w| {
                matches!(w.state, ControlCenterWindowState::Expanding | ControlCenterWindowState::Collapsing)
            })
            .unwrap_or(false)
    }

    pub(crate) fn is_control_center_blur_active(&self) -> bool {
        self.control_center_window
            .as_ref()
            .map(|w| w.curr_height > CONTROL_CENTER_HEIGHT_EXPANDED_PX / 3)
            .unwrap_or(false)
    }

    pub(crate) fn control_center_animation_tick(&mut self) {
        let Some(control_center) = &mut self.control_center_window else {
            return;
        };
        match control_center.state {
            ControlCenterWindowState::Collapsed
            | ControlCenterWindowState::Dragged
            | ControlCenterWindowState::Expanded => {}
            ControlCenterWindowState::Expanding => {
                if control_center.curr_height < CONTROL_CENTER_HEIGHT_EXPANDED_PX - ANIMATION_STEP {
                    control_center.curr_height += ANIMATION_STEP;
                } else {
                    control_center.curr_height = CONTROL_CENTER_HEIGHT_EXPANDED_PX;
                    self.set_control_center_window_state(ControlCenterWindowState::Expanded);
                }
            }
            ControlCenterWindowState::Collapsing => {
                if control_center.curr_height > CONTROL_CENTER_HEIGHT_COLLAPSED_PX + ANIMATION_STEP {
                    control_center.curr_height -= ANIMATION_STEP;
                } else {
                    control_center.curr_height = CONTROL_CENTER_HEIGHT_COLLAPSED_PX;
                    self.set_control_center_window_state(ControlCenterWindowState::Collapsed);
                }
            }
        }
    }
}
