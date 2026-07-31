// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::xous::PID;
use {
    slint_keyos_platform::{
        gui_server_api::{
            consts::{SCREEN_HEIGHT, SCREEN_WIDTH},
            touch::{Touch, TouchKind},
        },
        slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel},
    },
    std::sync::Arc,
    std::time::{Duration, Instant},
};

use crate::{AppItem, AppWindow, Constants, Global, GuiApi};

const APP_CLOSE_THRESHOLD_DISTANCE: f32 = 20.0;
const APP_CARD_DRAG_THRESHOLD_DISTANCE: f32 = 30.0;
const APP_CLOSE_TOLERANCE_FROM_CENTER: f32 = 50.0;
const APP_CLOSE_THRESHOLD_VELOCITY: f32 = 300.0;

#[derive(Default)]
pub(crate) struct AppState {
    apps: Vec<AppItem>,
    first_x_offset: f32,
    enforce_timer: Timer,
    allow_enforce: bool,
    app_drag_started_at: Option<Instant>,
    app_first_touch_pos: Option<Touch>,
    app_touch_index: Option<usize>,
    last_touch_pos: Option<Touch>,
    shown_once: bool,
}

/// Preferred direction (bias) when choosing a card to center.
/// To prevent momentary oscillation when centering around a card, prefer to center around the card
/// closer to a position biased in direction of the flick.
#[derive(Debug, Copy, Clone)]
enum PreferredDirection {
    Left,
    Right,
    None,
}

impl AppState {
    pub fn handle_app_started(&mut self, ui: &AppWindow, pid: PID, name: &str) {
        self.apps.insert(0, AppItem { img: Image::default(), name: name.into(), pid: pid.get() as i32 });
        log::debug!("App started: {pid}, {name}");
        self.update_apps_list(ui);
    }

    pub(crate) fn is_app_list_empty(&self) -> bool { self.apps.is_empty() }

    pub(crate) fn take_first_show(&mut self) -> bool { !std::mem::replace(&mut self.shown_once, true) }

    pub fn handle_app_activated(&mut self, ui: &AppWindow, pid: PID) {
        let Some(app_idx) = self.apps.iter().position(|app| app.pid == pid.get() as i32) else {
            log::warn!("Update app fb message got for unknown PID: {pid}");
            return;
        };
        let app = self.apps.remove(app_idx);
        log::debug!("App activated: {pid} {}", app.name);
        self.apps.insert(0, app);

        self.update_apps_list(ui);
    }

    pub(crate) fn handle_update_app_fb(&mut self, ui: &AppWindow, pid: PID, slice: &[u8]) {
        let Some(app) = self.apps.iter_mut().find(|app| app.pid == pid.get() as i32) else {
            log::warn!("Update app fb message got for unknown PID: {pid}");
            return;
        };
        log::debug!("App fb: {pid} {}", app.name);

        let width = (SCREEN_WIDTH / 2) as u32;
        let height = (SCREEN_HEIGHT / 2) as u32;
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(slice, width, height);

        // Round the corners with 8px radius
        round_corners(buffer.make_mut_bytes(), width, height, 8);

        app.img = Image::from_rgba8_premultiplied(buffer);
        self.update_apps_list(ui);
    }

    pub fn handle_app_closed(&mut self, ui: &AppWindow, gui_api: &Arc<GuiApi>, pid: PID) {
        self.apps.retain(|app| app.pid != pid.get() as i32);
        log::debug!("App closed: {pid}");
        self.update_apps_list(ui);
        ui.global::<Global>().set_is_app_hidden(false);
        ui.set_fake_card_y(ui.get_flickable_y());

        if self.apps.is_empty() {
            gui_api.switch_to_launcher().ok();
        } else {
            Self::center_the_current_card(self.apps.len(), PreferredDirection::None, &ui);
        }
    }

    pub(crate) fn handle_touch(&mut self, ui: &AppWindow, gui_api: &Arc<GuiApi>, touch: Touch) {
        self.allow_enforce = matches!(touch.kind, TouchKind::Release);

        match touch.kind {
            TouchKind::Press => {
                self.first_x_offset = ui.get_scroll_offset();
                self.last_touch_pos = Some(touch);

                if self.app_first_touch_pos.is_none() {
                    self.app_first_touch_pos = Some(touch);
                }
            }

            TouchKind::Release => {
                if let Some(drag_started_at) = self.app_drag_started_at {
                    if let Some(app_first_touch) = self.app_first_touch_pos {
                        let elapsed = drag_started_at.elapsed();
                        let (_, y_diff) = touch.diff(&app_first_touch);
                        let distance_y = y_diff.abs() as f32;
                        let is_up = y_diff < 0;
                        let velocity = distance_y / elapsed.as_secs_f32();
                        let fake_card_dragged_distance = ui.get_flickable_y() - ui.get_fake_card_y();
                        let with_close = is_up
                            && velocity >= APP_CLOSE_THRESHOLD_VELOCITY
                            && fake_card_dragged_distance >= APP_CARD_DRAG_THRESHOLD_DISTANCE;

                        self.reset_app_dragged(ui, gui_api, with_close);
                    }
                }

                // The Flickable stops producing events when it reaches either end of the viewport.
                // Simulate the flick on release when the end is reached.
                self.on_flicked(ui, ui.get_scroll_offset());
            }

            TouchKind::Drag => {
                if self.app_first_touch_pos.is_none() {
                    self.app_first_touch_pos = Some(touch);
                }

                if let Some(app_first_touch) = self.app_first_touch_pos {
                    let (_, y_diff) = touch.diff(&app_first_touch);
                    let distance_y = y_diff.abs() as f32;

                    if self.app_drag_started_at.is_none() {
                        let is_up = y_diff < 0;
                        let dragging = is_up && distance_y >= APP_CLOSE_THRESHOLD_DISTANCE;

                        if dragging {
                            let app_touch_index =
                                Self::card_index_from_touch_x(ui, app_first_touch.x as f32, self.apps.len());

                            if let Some((card, Some(app))) =
                                app_touch_index.map(|idx| (idx, self.apps.get(idx)))
                            {
                                if !Self::is_card_centered(ui, card, APP_CLOSE_TOLERANCE_FROM_CENTER) {
                                    return;
                                }

                                Self::center_card(ui, card);

                                self.app_drag_started_at = Some(Instant::now());

                                let fake_card_image =
                                    self.apps.get(card).map(|app| app.img.clone()).unwrap_or_default();
                                ui.global::<Global>().set_fake_app_image(fake_card_image);
                                ui.global::<Global>().set_fake_app_name(app.name.clone().into());
                                ui.global::<Global>().set_hidden_app_index(card as i32);
                                ui.global::<Global>().set_is_app_hidden(true);
                            }
                        }
                    } else if y_diff <= 0 {
                        let y = ui.get_flickable_y() + y_diff as f32;
                        ui.global::<Global>().set_fake_app_anim_duration(0);
                        ui.set_fake_card_y(y);
                    }
                }
            }
        }
    }

    pub(crate) fn on_flicked(&mut self, ui: &AppWindow, offset_x: f32) {
        let bias =
            if offset_x > self.first_x_offset { PreferredDirection::Left } else { PreferredDirection::Right };

        if self.allow_enforce {
            let num_apps = self.apps.len();
            self.enforce_timer.start(
                TimerMode::SingleShot,
                Duration::from_millis(ui.global::<Constants>().get_scroll_anim_duration() as u64),
                {
                    let ui = ui.as_weak();
                    move || {
                        Self::center_the_current_card(num_apps, bias, &ui.unwrap());
                    }
                },
            );
        }
    }

    fn reset_app_dragged(&mut self, ui: &AppWindow, gui_api: &Arc<GuiApi>, with_close: bool) {
        self.app_drag_started_at = None;
        self.last_touch_pos = None;
        self.app_first_touch_pos = None;

        let anim_duration = ui.global::<Constants>().get_app_close_anim_duration();

        // Animate the dragged app card back to the original position
        ui.global::<Global>().set_fake_app_anim_duration(anim_duration);
        ui.set_fake_card_y(ui.get_flickable_y());

        // Finalize the card position after animation is done
        if !with_close {
            Timer::single_shot(Duration::from_millis(anim_duration as u64), {
                let ui = ui.as_weak();
                move || {
                    let ui = ui.unwrap();

                    ui.global::<Global>().set_is_app_hidden(false);
                    ui.set_fake_card_y(ui.get_flickable_y());
                }
            });
        } else {
            self.close_selected_app(ui, gui_api);
        }
    }

    // Returns both expected card x position and card's position on the screen
    fn card_index_to_x_pos(ui: &AppWindow, index: usize) -> (f32, f32) {
        let scroll_offset = ui.get_scroll_offset();
        let card_width = ui.global::<Constants>().get_app_card_width();
        let card_spacer_width = ui.global::<Constants>().get_app_card_spacer_width();
        let card_gap_width = ui.global::<Constants>().get_app_card_gap_width();

        let expected_card_x =
            card_spacer_width + card_gap_width + (card_width + card_gap_width) * index as f32;
        let card_screen_x = scroll_offset + expected_card_x;

        (expected_card_x, card_screen_x)
    }

    fn card_index_from_touch_x(ui: &AppWindow, touch_screen_x: f32, max_apps: usize) -> Option<usize> {
        let scroll_offset = ui.get_scroll_offset();
        let constants = ui.global::<Constants>();

        let card_width = constants.get_app_card_width();
        let gap_width = constants.get_app_card_gap_width();
        let spacer_width = constants.get_app_card_spacer_width();

        let stride = card_width + gap_width;
        let first_card_x = spacer_width + gap_width;

        let touch_content_x = touch_screen_x - scroll_offset;
        if touch_content_x < first_card_x {
            return None;
        }

        let local_x = touch_content_x - first_card_x;

        let index = (local_x / stride).floor() as usize;
        let offset_in_stride = local_x % stride;

        if offset_in_stride >= card_width || index >= max_apps {
            return None;
        }

        Some(index)
    }

    /// Shifts the (would be) screen center in the direction of the flick (if any).
    fn bias_screen_center(bias: PreferredDirection) -> f32 {
        SCREEN_WIDTH as f32 / 2.0f32
            + match bias {
                PreferredDirection::Left => -(SCREEN_WIDTH as f32 / 5.0),
                PreferredDirection::Right => SCREEN_WIDTH as f32 / 5.0,
                PreferredDirection::None => 0.0,
            }
    }

    // Finds the index of a card closest to the given X position
    fn pos_to_card_index(num_cards: usize, ui: &AppWindow, screen_x: f32) -> Option<usize> {
        let card_width = ui.global::<Constants>().get_app_card_width();

        (0..num_cards)
            .map(|i| {
                let (_, card_screen_x) = Self::card_index_to_x_pos(ui, i);
                let card_center_x = card_screen_x + card_width / 2.0f32;
                let distance = (screen_x - card_center_x).abs();
                (i, distance)
            })
            .min_by_key(|(_, distance)| *distance as usize)
            .map(|i| i.0)
    }

    fn update_apps_list(&self, ui: &AppWindow) {
        // This wouldn't be needed if the state stored the ModelRc,
        // VecModel's api is not as friendly, and casting from ModelRc
        // all the time is also inconvenient.
        ui.global::<Global>().set_apps(ModelRc::new(VecModel::from(self.apps.clone())));
    }

    pub(crate) fn process_app_touch(&mut self, ui: &AppWindow, is_down: bool, index: usize) -> bool {
        if !is_down && !ui.global::<Global>().get_is_app_hidden() {
            log::debug!("Regular touch");
            return Self::center_card(ui, index);
        } else {
            self.app_first_touch_pos = self.last_touch_pos;
            self.app_touch_index = Some(index);
        }

        false
    }

    pub(crate) fn close_selected_app(&mut self, ui: &AppWindow, gui_api: &Arc<GuiApi>) {
        let screen_center = Self::bias_screen_center(PreferredDirection::None);
        let Some(card) = Self::pos_to_card_index(self.apps.len(), ui, screen_center) else { return };

        let app = self.apps.remove(card);
        gui_api.close_app(server::xous::PID::new(app.pid as u8).unwrap()).ok();

        let anim_duration = ui.global::<Constants>().get_app_close_anim_duration();
        ui.global::<Global>().set_fake_app_anim_duration(anim_duration);
        ui.global::<Global>().set_fake_app_image(app.img);
        ui.global::<Global>().set_fake_app_name(app.name);
        ui.global::<Global>().set_hidden_app_index(card as i32);
        ui.global::<Global>().set_is_app_hidden(true);

        let card_height = ui.global::<Constants>().get_app_card_height();
        ui.set_fake_card_y(-card_height);
    }

    pub(crate) fn close_all(&mut self, ui: &AppWindow, gui_api: &Arc<GuiApi>) {
        for app in self.apps.iter() {
            gui_api.close_app(server::xous::PID::new(app.pid as u8).unwrap()).ok();
        }

        self.apps.clear();
        self.update_apps_list(ui);

        gui_api.switch_to_launcher().ok();
    }

    fn is_card_centered(ui: &AppWindow, index: usize, tolerance: f32) -> bool {
        let card_width = ui.global::<Constants>().get_app_card_width();

        let (_, card_screen_x) = Self::card_index_to_x_pos(ui, index);
        let card_center_x = card_screen_x + (card_width / 2.0f32);
        let screen_center_x = SCREEN_WIDTH as f32 / 2.0f32;
        let offset_from_center = (card_center_x - screen_center_x).abs();
        offset_from_center <= tolerance
    }

    pub fn center_card(ui: &AppWindow, index: usize) -> bool {
        let card_width = ui.global::<Constants>().get_app_card_width();

        let (expected_card_x, card_screen_x) = Self::card_index_to_x_pos(ui, index);
        let card_center_x = card_screen_x + (card_width / 2.0f32);
        let screen_center_x = SCREEN_WIDTH as f32 / 2.0f32;
        let offset_from_center = (card_center_x - screen_center_x).abs();
        let is_card_centered = offset_from_center <= f32::EPSILON;
        let required_offset_to_center = SCREEN_WIDTH as f32 / 2.0f32 - expected_card_x - card_width / 2.0f32;

        // Bring the card to the center of the screen
        if !is_card_centered {
            ui.set_scroll_offset(required_offset_to_center);
        }

        is_card_centered
    }

    fn center_the_current_card(num_cards: usize, bias: PreferredDirection, ui: &AppWindow) {
        let screen_center = Self::bias_screen_center(bias);
        if let Some(card) = Self::pos_to_card_index(num_cards, ui, screen_center) {
            Self::center_card(ui, card);
        }
    }
}

/// Rounds the corners of an RGBA image buffer by setting corner pixels to transparent.
/// Only iterates over the corner regions, making this very efficient.
fn round_corners(buffer: &mut [u8], width: u32, height: u32, radius: u32) {
    let r = radius as i32;
    let w = width as i32;
    let h = height as i32;

    // Check if a pixel is outside the rounded corner
    let is_outside = |x: i32, y: i32, cx: i32, cy: i32| -> bool {
        let dx = x - cx;
        let dy = y - cy;
        dx * dx + dy * dy > r * r
    };

    // Process only the four corner regions
    for y in 0..r {
        for x in 0..r {
            // Top-left corner
            if is_outside(x, y, r, r) {
                let idx = ((y * w + x) * 4) as usize;
                buffer[idx..idx + 4].fill(0);
            }
            // Top-right corner
            if is_outside(w - 1 - x, y, w - r - 1, r) {
                let idx = ((y * w + (w - 1 - x)) * 4) as usize;
                buffer[idx..idx + 4].fill(0);
            }
            // Bottom-left corner
            if is_outside(x, h - 1 - y, r, h - r - 1) {
                let idx = (((h - 1 - y) * w + x) * 4) as usize;
                buffer[idx..idx + 4].fill(0);
            }
            // Bottom-right corner
            if is_outside(w - 1 - x, h - 1 - y, w - r - 1, h - r - 1) {
                let idx = (((h - 1 - y) * w + (w - 1 - x)) * 4) as usize;
                buffer[idx..idx + 4].fill(0);
            }
        }
    }
}
