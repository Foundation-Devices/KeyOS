// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::{
    consts::{SCREEN_HEIGHT, SCREEN_WIDTH},
    NextFrameAnimationKind,
};
use xous::PID;

use crate::{
    layers::{Layer, LayerStack},
    Gui, GuiState,
};

const SWITCHER_APP_CARD_Y: usize = 235;
const SWITCHER_APP_CARD_WIDTH: usize = SCREEN_WIDTH / 2;
const SWITCHER_APP_CARD_HEIGHT: usize = SCREEN_HEIGHT / 2;
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum SwitchingAnimation {
    // Zoom "to" in in front of "from", while increasing opacity
    ZoomIn,
    // Zoom "from" out, in front of "to", while decreasing opacity
    ZoomOut,
    // Slide "to" from the bottom
    SlideIn,
    // Slide "from" out to the bottom
    SlideOut,
    // Slide "to" from the top
    SlideInTop,
    // Slide "from" out to the top
    SlideOutTop,

    // "from" is the original app, "to" is the switcher; the app shrinks into its slot in the switcher
    ToSwitcher(ProgressControl),
    // "from" is the switcher, "to" is the app; the app grows to screen size
    FromSwitcher,
    // Fade "to" in in front of "from", increasing opacity
    FadeIn,
    // Fade "from" out, in front of "to", decreasing opacity
    FadeOut,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ProgressControl {
    /// Animation progresses on its own
    Automatic,
    /// Animation is controlled externally (e.g., app switcher responsive transition)
    Manual,
    /// Animation is rolled back from the progress it had made
    Abort,
}

impl SwitchingAnimation {
    pub fn add_layers(&self, layers: &mut LayerStack, from: Layer, to: Layer, progress: usize) {
        let (bg, fg, progress) = match self {
            SwitchingAnimation::ZoomIn
            | SwitchingAnimation::SlideIn
            | SwitchingAnimation::SlideInTop
            | SwitchingAnimation::FromSwitcher
            | SwitchingAnimation::FadeIn => (from, to, progress),
            SwitchingAnimation::ZoomOut
            | SwitchingAnimation::SlideOut
            | SwitchingAnimation::SlideOutTop
            | SwitchingAnimation::ToSwitcher(..)
            | SwitchingAnimation::FadeOut => (to, from, 100 - progress),
        };
        let fg = match self {
            SwitchingAnimation::ZoomIn | SwitchingAnimation::ZoomOut => {
                let dst_width = SCREEN_WIDTH * (100 + progress) / 200;
                let dst_height = SCREEN_HEIGHT * (100 + progress) / 200;
                fg.with_alpha((progress * 255 / 100) as u8)
                    .with_position((SCREEN_WIDTH - dst_width) / 2, (SCREEN_HEIGHT - dst_height) / 2)
                    .with_dst_size(dst_width, dst_height)
            }
            SwitchingAnimation::SlideIn | SwitchingAnimation::SlideOut => {
                fg.with_position(0, SCREEN_HEIGHT * (100 - progress) / 100)
            }
            SwitchingAnimation::SlideOutTop | SwitchingAnimation::SlideInTop => {
                let progress_f32 = progress as f32 / 100.0;
                let progress = (simple_easing::expo_out(progress_f32) * 100.0) as usize;

                let height = SCREEN_HEIGHT * progress / 100;
                let inv_height = SCREEN_HEIGHT * (100 - progress) / 100;

                fg.with_position(0, 0).with_crop(0, inv_height, SCREEN_WIDTH, height)
            }
            SwitchingAnimation::FromSwitcher | SwitchingAnimation::ToSwitcher(..) => {
                let dst_width =
                    SWITCHER_APP_CARD_WIDTH + (SCREEN_WIDTH - SWITCHER_APP_CARD_WIDTH) * progress / 100;
                let dst_height =
                    SWITCHER_APP_CARD_HEIGHT + (SCREEN_HEIGHT - SWITCHER_APP_CARD_HEIGHT) * progress / 100;
                fg.with_position((SCREEN_WIDTH - dst_width) / 2, SWITCHER_APP_CARD_Y * (100 - progress) / 100)
                    .with_dst_size(dst_width, dst_height)
            }
            SwitchingAnimation::FadeIn | SwitchingAnimation::FadeOut => {
                fg.with_alpha((progress * 255 / 100) as u8)
            }
        };
        layers.push(bg);
        layers.push(fg);
    }

    pub fn step_size_ticks(&self) -> usize {
        match self {
            SwitchingAnimation::SlideOutTop => 8, // A bit slower
            _ => 12,
        }
    }
}

#[derive(Debug)]
pub(crate) enum NextFrameAnimationState {
    NotAnimating,
    Waiting { kind: NextFrameAnimationKind },
    Animating { progress: usize, kind: NextFrameAnimationKind },
}

impl Gui {
    pub(crate) fn switching_animation(&self, from: PID, to: PID) -> SwitchingAnimation {
        if Some(from) == self.app_registry.switcher_app_pid() {
            if Some(to) == self.app_registry.launcher_app_pid() {
                SwitchingAnimation::FadeIn
            } else {
                SwitchingAnimation::FromSwitcher
            }
        } else if Some(to) == self.app_registry.switcher_app_pid() {
            if Some(from) == self.app_registry.launcher_app_pid() {
                SwitchingAnimation::FadeOut
            } else {
                SwitchingAnimation::ToSwitcher(ProgressControl::Manual)
            }
        } else if Some(from) == self.app_registry.lock_screen_pid() {
            SwitchingAnimation::SlideOutTop
        } else if Some(to) == self.app_registry.lock_screen_pid() {
            SwitchingAnimation::SlideInTop
        } else if Some(to) == self.app_registry.settings_app_pid() {
            SwitchingAnimation::SlideIn
        } else if Some(from) == self.app_registry.settings_app_pid() {
            SwitchingAnimation::SlideOut
        } else if Some(to) == self.app_registry.launcher_app_pid() {
            SwitchingAnimation::ZoomOut
        } else {
            SwitchingAnimation::ZoomIn
        }
    }

    pub(crate) fn handle_animate_next_frame(&mut self, pid: PID, kind: NextFrameAnimationKind) {
        match &mut self.state {
            GuiState::SingleWindow { pid: current_pid, next_frame_animation, .. } if *current_pid == pid => {
                log::debug!("Animating next frame with kind {kind:?} for app PID={pid}");
                *next_frame_animation = NextFrameAnimationState::Waiting { kind }
            }
            _ => log::debug!("Not animating next frame with kind {kind:?} for app PID={pid}, because it's not in the foreground")
        }
    }

    pub(crate) fn next_frame_animation_layers(
        layers: &mut LayerStack,
        from: Layer,
        to: Layer,
        progress: usize,
        kind: NextFrameAnimationKind,
    ) {
        let offset = SCREEN_WIDTH * progress / 100;
        let inv_offset = SCREEN_WIDTH - offset;
        match kind {
            NextFrameAnimationKind::SlideInLeft => {
                layers.push(from.with_position(offset, 0).with_crop(0, 0, inv_offset, SCREEN_HEIGHT));
                layers.push(to.with_crop(inv_offset, 0, offset, SCREEN_HEIGHT));
            }
            NextFrameAnimationKind::SlideInRight => {
                layers.push(from.with_crop(offset, 0, inv_offset, SCREEN_HEIGHT));
                layers.push(to.with_position(inv_offset, 0).with_crop(0, 0, offset, SCREEN_HEIGHT));
            }
            NextFrameAnimationKind::SlideOutLeft => {
                layers.push(to.with_position(inv_offset, 0).with_crop(0, 0, offset, SCREEN_HEIGHT));
                layers.push(from.with_crop(offset, 0, inv_offset, SCREEN_HEIGHT));
            }
            NextFrameAnimationKind::SlideOutRight => {
                layers.push(to.with_crop(inv_offset, 0, offset, SCREEN_HEIGHT));
                layers.push(from.with_position(offset, 0).with_crop(0, 0, inv_offset, SCREEN_HEIGHT));
            }
        }
    }

    pub(crate) fn set_switching_animation_progress(&mut self, new_progress: Option<usize>) {
        if let GuiState::Switching {
            progress,
            animation: SwitchingAnimation::ToSwitcher(progress_control),
            ..
        } = &mut self.state
        {
            if let Some(new_progress) = new_progress {
                *progress_control = ProgressControl::Manual;
                *progress = new_progress.min(100);
            } else {
                *progress_control = ProgressControl::Automatic;
            }
        }
    }

    pub(crate) fn abort_switching_animation(&mut self) {
        if let GuiState::Switching { animation: SwitchingAnimation::ToSwitcher(progress_control), .. } =
            &mut self.state
        {
            *progress_control = ProgressControl::Abort;
        }
    }
}

const BACKLIGHT_ANIMATION_STEP_SIZE: u8 = 10;

pub(crate) struct BacklightAnimation {
    from_pct: u8,
    to_pct: u8,
    progress_pct: u8,
    complete_action: AnimationCompleteAction,
}

pub(crate) enum AnimationCompleteAction {
    None,
    #[cfg(all(keyos, not(feature = "recovery-os")))]
    LcdDim,
    LcdOff,
}

impl Gui {
    pub(crate) fn backlight_animation_tick(&mut self) {
        let Some(state) = &mut self.backlight_animation else {
            return;
        };

        if state.progress_pct < 100 - BACKLIGHT_ANIMATION_STEP_SIZE {
            state.progress_pct += BACKLIGHT_ANIMATION_STEP_SIZE;

            let pct = if state.from_pct < state.to_pct {
                // Increasing brightness
                (state.from_pct as usize
                    + (state.to_pct - state.from_pct) as usize * state.progress_pct as usize / 100usize)
                    as u8
            } else {
                // Decreasing brightness
                (state.from_pct as usize
                    - (state.from_pct - state.to_pct) as usize * state.progress_pct as usize / 100)
                    as u8
            };

            log::trace!(
                "Backlight animation: {}% (from: {}% to %{}, {} step)",
                pct,
                state.from_pct,
                state.to_pct,
                BACKLIGHT_ANIMATION_STEP_SIZE
            );

            self.display.set_backlight_level_pct(pct.min(100));
        } else {
            // Animation is done
            match state.complete_action {
                AnimationCompleteAction::None => {}
                #[cfg(all(keyos, not(feature = "recovery-os")))]
                AnimationCompleteAction::LcdDim => self.display.dim(),
                AnimationCompleteAction::LcdOff => self.display.turn_lcd_off(),
            }
            self.backlight_animation = None;
        }
    }

    pub(crate) fn animate_backlight_to(&mut self, to_pct: u8, complete_action: AnimationCompleteAction) {
        let screen_is_dimmed = self.display.is_dimmed();
        let screen_is_off = !self.display.is_lcd_on();
        let from_pct = if screen_is_dimmed {
            self.screen_brightness_setting_dimmed()
        } else if screen_is_off {
            0
        } else {
            self.screen_brightness_setting()
        };
        log::debug!(
            "Animating backlight from {from_pct}% to {to_pct}% ({BACKLIGHT_ANIMATION_STEP_SIZE} step)"
        );

        self.backlight_animation =
            Some(BacklightAnimation { from_pct, to_pct, progress_pct: 0, complete_action })
    }
}
