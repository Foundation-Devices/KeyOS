// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Instant;

use gui_server_api::consts::{MODAL_DRAG_BAR_MARGIN_PX, MODAL_HEIGHT, SCREEN_HEIGHT, SCREEN_WIDTH};
use gui_server_api::error::NavigationError;
use gui_server_api::msg::{NavigationResult, ShowModal};
use gui_server_api::InputMessage;
use gui_server_api::{
    touch::{Touch, TouchKind},
    ModalStyle,
};
use log::debug;
use server::ArchiveRequest;
use xous::PID;

#[cfg(not(feature = "recovery-os"))]
use crate::permissions::PendingPermissionPrompt;
use crate::touch::TouchGestureOrigin;
use crate::{AppCloseState, Gui, GuiState};

const BACKGROUND_DARKEN_ALPHA_MAX: u8 = 255 - 24; // The higher, the darker

const MODAL_SLIDE_STEP_PX: usize = 64;

/// What the modal answers when it finishes: an external `ShowModal` IPC request from another
/// process, or an internally originated permission prompt whose outcome is reported back to
/// the kernel instead of a caller.
#[derive(Debug)]
pub(crate) enum ModalRequest {
    External(ArchiveRequest<ShowModal>),
    #[cfg(not(feature = "recovery-os"))]
    Permission(PendingPermissionPrompt),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModalStateInner {
    /// Started, waiting for it to connect to us
    Waiting,
    /// Popping up, actively being animated
    Expanding,
    /// Fully expanded and being displayed
    Normal,
    /// Being actively dragged
    Dragged,
    /// Popping out
    Collapsing,
}

#[derive(Debug)]
pub(crate) struct ModalState {
    modal_style: ModalStyle,
    modal_height: usize,
    state: ModalStateInner,
    top: PID,
    bottom: PID,
    navigation_request: Option<ModalRequest>,
}

impl ModalState {
    fn is_draggable(&self) -> bool { matches!(self.modal_style, ModalStyle::SlideUpDraggablePopup) }

    fn final_height(&self) -> usize {
        if self.is_fullscreen() {
            SCREEN_HEIGHT
        } else {
            MODAL_HEIGHT
        }
    }

    pub fn modal_pid(&self) -> PID { self.top }

    pub fn background_pid(&self) -> PID { self.bottom }

    // Returns true if collapsed
    pub fn animation_tick(&mut self) -> bool {
        // Instant style has no animation
        if matches!(self.modal_style, ModalStyle::Instant) {
            return matches!(self.state, ModalStateInner::Collapsing);
        }

        match self.state {
            ModalStateInner::Normal | ModalStateInner::Waiting | ModalStateInner::Dragged => false,
            ModalStateInner::Expanding => {
                if self.modal_height < self.final_height().saturating_sub(MODAL_SLIDE_STEP_PX) {
                    self.modal_height += MODAL_SLIDE_STEP_PX;
                } else {
                    self.modal_height = self.final_height();
                    self.state = ModalStateInner::Normal;
                }
                false
            }
            ModalStateInner::Collapsing => {
                if self.modal_height > MODAL_SLIDE_STEP_PX {
                    self.modal_height -= MODAL_SLIDE_STEP_PX;
                    false
                } else {
                    true
                }
            }
        }
    }

    pub fn respond(&mut self, result: NavigationResult) {
        match self.navigation_request.take() {
            Some(ModalRequest::External(r)) => {
                let _ = r.response.respond(result);
            }
            #[cfg(not(feature = "recovery-os"))]
            Some(ModalRequest::Permission(prompt)) => prompt.finish(result),
            None => {}
        }
        self.state = ModalStateInner::Collapsing;
    }

    pub fn get_navigation_request(&self) -> Option<&[u8]> {
        self.navigation_request.as_ref().map(|request| match request {
            ModalRequest::External(r) => r.message.args.as_slice(),
            #[cfg(not(feature = "recovery-os"))]
            ModalRequest::Permission(prompt) => prompt.args(),
        })
    }

    pub fn is_waiting(&self) -> bool { self.state == ModalStateInner::Waiting }

    pub fn expand(&mut self) { self.state = ModalStateInner::Expanding }

    pub fn y(&self) -> usize { SCREEN_HEIGHT - self.modal_height }

    pub fn is_fullscreen(&self) -> bool {
        matches!(self.modal_style, ModalStyle::SlideUpFullscreen | ModalStyle::Instant)
    }

    pub fn dark_overlay_alpha(&self) -> u8 {
        let height_ratio = self.modal_height as f32 / self.final_height() as f32;
        (BACKGROUND_DARKEN_ALPHA_MAX as f32 * height_ratio) as u8
    }
}

impl Gui {
    pub(crate) fn modal_activate(&mut self, modal_pid: PID, mut request: ArchiveRequest<ShowModal>) {
        debug!("Entered modal state, top PID: {modal_pid}");
        request.response.set_response(|| Err(NavigationError::CanceledBySystem));
        let background_pid = request.response.pid();
        let modal_style = request.message.modal_style;
        self.modal_activate_inner(modal_pid, background_pid, modal_style, ModalRequest::External(request));
    }

    /// Bring up a modal for `modal_pid` over `background_pid`, answering `request` when it
    /// finishes. The caller chooses the background window; internal callers (the permission
    /// prompt) pass the current foreground pid they already hold.
    pub(crate) fn modal_activate_inner(
        &mut self,
        modal_pid: PID,
        background_pid: PID,
        modal_style: ModalStyle,
        request: ModalRequest,
    ) {
        let state = match self.windows.get_mut(&modal_pid) {
            None => ModalStateInner::Waiting,
            Some(window) => {
                if window.close_state != AppCloseState::Running {
                    ModalStateInner::Waiting
                } else {
                    if window.buffers.most_recent_buffer().is_none() {
                        ModalStateInner::Waiting
                    } else {
                        window.last_active = Instant::now();
                        self.notify_switcher_app_activated(modal_pid);
                        ModalStateInner::Expanding
                    }
                }
            }
        };
        let initial_height = match modal_style {
            ModalStyle::Instant => SCREEN_HEIGHT,
            _ => 0,
        };
        let initial_state = match modal_style {
            ModalStyle::Instant => ModalStateInner::Normal,
            _ => state,
        };
        self.change_state(GuiState::Modal(ModalState {
            modal_style,
            modal_height: initial_height,
            state: initial_state,
            top: modal_pid,
            bottom: background_pid,
            navigation_request: Some(request),
        }))
    }

    pub(crate) fn modal_process_touch(&mut self, touch: Touch) {
        let GuiState::Modal(modal_state) = &mut self.state else {
            log::warn!("Modal touch while not in modal state");
            return;
        };
        let modal_y = SCREEN_HEIGHT - modal_state.modal_height;
        match modal_state.state {
            ModalStateInner::Waiting | ModalStateInner::Expanding | ModalStateInner::Collapsing => {}
            ModalStateInner::Dragged => {
                match touch.kind {
                    TouchKind::Drag => {
                        let new_h = SCREEN_HEIGHT.saturating_sub(touch.y) + MODAL_DRAG_BAR_MARGIN_PX / 2;
                        // Clamp the min and max height of the window
                        modal_state.modal_height = new_h.min(MODAL_HEIGHT);
                        self.update_layers();
                    }
                    TouchKind::Release => {
                        if modal_state.modal_height < MODAL_HEIGHT / 2 {
                            debug!("Collapsing");
                            modal_state.state = ModalStateInner::Collapsing;
                        } else {
                            debug!("Expanding");
                            modal_state.state = ModalStateInner::Expanding;
                        }
                    }
                    TouchKind::Press => (),
                }
            }
            ModalStateInner::Normal => {
                if touch.is_press() {
                    if modal_state.is_draggable()
                        && touch.is_within_area(0, modal_y, SCREEN_WIDTH, MODAL_DRAG_BAR_MARGIN_PX)
                    {
                        modal_state.state = ModalStateInner::Dragged;
                        return;
                    }
                    if !touch.is_within_area(0, modal_y, SCREEN_WIDTH, SCREEN_HEIGHT - modal_y) {
                        debug!("Touch consumed, collapsing modal");
                        modal_state.state = ModalStateInner::Collapsing;
                        self.touch_state.origin = TouchGestureOrigin::None;
                        return;
                    }
                }

                let touch = touch.translate_pos(0, modal_y);

                if let Some(modal_window) = self.windows.get(&modal_state.top) {
                    xous::try_send_message(
                        modal_window.input_cid,
                        touch.as_input_message(InputMessage::Touch as usize),
                    )
                    .ok();
                }
            }
        }
    }

    pub(crate) fn is_modal_active(&self) -> bool { matches!(self.state, GuiState::Modal(_)) }

    pub(crate) fn modal_background_pid(&self) -> Option<PID> {
        if let GuiState::Modal(modal_state) = &self.state {
            Some(modal_state.background_pid())
        } else {
            None
        }
    }

    /// The modal background window, when the modal leaves part of it on screen.
    ///
    /// A fullscreen modal covers the background once settled, and slides over it too
    /// briefly on the way in to be worth a full-screen blur.
    pub(crate) fn blurred_modal_background_pid(&self) -> Option<PID> {
        match &self.state {
            GuiState::Modal(modal_state) if !modal_state.is_fullscreen() => {
                Some(modal_state.background_pid())
            }
            _ => None,
        }
    }
}
