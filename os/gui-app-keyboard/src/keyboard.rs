// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use assets::{BG_IMAGE_HEIGHT, BG_IMAGE_WIDTH};
use gui_server_api::{
    consts::{DEFAULT_KEYBOARD_HEIGHT, FPS, KEYBOARD_TOP_BAR_MARGIN, SCREEN_HEIGHT, SCREEN_WIDTH},
    InputMessage, Key, KeyboardKind,
};
use tiny_skia::{IntRect, Pixmap, PixmapMut, Rect, Transform};
use xous::MemoryRange;
use xous_api_ticktimer::{Ticktimer, TicktimerCallback};

use crate::{
    cache::with_cached_pixmap,
    drawing::blit,
    keys::{KeyAction, KeyDef},
    layout::{
        alpha::{
            LAYOUT_ALPHA_LOWER, LAYOUT_ALPHA_NUMERIC, LAYOUT_ALPHA_PUNCTUATION, LAYOUT_ALPHA_UPPER,
            LAYOUT_ALPHA_UPPER_CAPS,
        },
        decimal::LAYOUT_DECIMAL,
        numeric::LAYOUT_NUMERIC,
        Layout, LayoutType,
    },
    overlay::OverlayCache,
    sliding::{CursorSliding, SlideEvent},
    GuiApi, HapticsApi,
};

pub const KEY_DEFAULT_WIDTH: f32 = 40.0;
pub const KEY_HEIGHT: f32 = 58.0;
pub const KEY_BORDER_RADIUS: f32 = 12.0;
pub const KEY_V_MARGIN: f32 = (ROW_HEIGHT - KEY_HEIGHT) / 2.0;

pub const ROW_HEIGHT: f32 = 74.0;
pub const KEYS_TOP_MARGIN: f32 = 4_f32;

pub const KEY_FONT_SCALE: f32 = 38.0;

const LONG_OVERLAY_DELAY: usize = 500;

pub struct KeyboardState {
    kind: KeyboardKind,
    shift_state: ShiftState,
    layout_type: LayoutType,
    layout: &'static Layout,

    /// Custom label for the Return ("Done") key, supplied via Slint's
    /// `accept-button-text` property. Empty means use the default "Done".
    accept_button_text: String,
    /// Whether the Return ("Done") key is enabled. When false, render it
    /// disabled and ignore presses.
    accept_button_enabled: bool,
    /// Whether the Backspace ("Delete") key is enabled. When false, render
    /// it disabled and ignore presses.
    delete_button_enabled: bool,

    overlay_cache: OverlayCache,

    show_overlay_long: bool,

    key_pressed: Option<PressedKey>,
    overlay_x: Option<i32>,
    overlay_char: Option<char>,
    key_pressed_pixmap: Option<Pixmap>,

    haptics_api: Rc<HapticsApi>,
    long_press_callback: TicktimerCallback,

    sliding: CursorSliding,

    redraw_requested: bool,

    ticktimer: Ticktimer,

    previous_buffers: [FrameBufferState; 2],
}

#[derive(Clone)]
struct FrameBufferState {
    layout: &'static Layout,
    dirty: Option<IntRect>,
    accept_button_text: String,
    accept_button_enabled: bool,
    delete_button_enabled: bool,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftState {
    #[default]
    Lowercase,
    Uppercase,
    CapsLock,
}

impl ShiftState {
    pub fn next(&self) -> Self {
        match self {
            ShiftState::Lowercase => ShiftState::Uppercase,
            ShiftState::Uppercase => ShiftState::CapsLock,
            ShiftState::CapsLock => ShiftState::Lowercase,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PressedKey {
    id: u64,

    x: f32,
    y: f32,

    at: Instant,
}

impl KeyboardState {
    pub fn new(
        kind: KeyboardKind,
        haptics_api: Rc<HapticsApi>,
        long_press_callback: TicktimerCallback,
    ) -> Self {
        const _: () = assert!(SCREEN_WIDTH == BG_IMAGE_WIDTH);
        const _: () = assert!(DEFAULT_KEYBOARD_HEIGHT == BG_IMAGE_HEIGHT + KEYBOARD_TOP_BAR_MARGIN);
        // Create initial state
        let mut result = Self {
            kind,
            shift_state: Default::default(),
            layout_type: Default::default(),
            layout: &LAYOUT_ALPHA_LOWER,

            accept_button_text: String::new(),
            accept_button_enabled: true,
            delete_button_enabled: true,

            key_pressed: None,
            key_pressed_pixmap: None,

            overlay_cache: Default::default(),

            overlay_x: None,
            overlay_char: None,

            show_overlay_long: false,

            haptics_api,
            long_press_callback,

            sliding: CursorSliding::new(),

            // GUI server sends us a buffer at start
            redraw_requested: true,

            ticktimer: Ticktimer::default(),

            previous_buffers: Default::default(),
        };
        result.refresh_layout();
        result
    }

    pub fn request_redraw(&mut self, gui: &GuiApi) {
        if !self.redraw_requested {
            self.redraw_requested = true;
            gui.request_redraw().ok();
        }
    }

    pub fn kind(&self) -> KeyboardKind { self.kind }

    pub fn set_kind(&mut self, kind: KeyboardKind) {
        self.kind = kind;
        self.cleanup();
        self.shift_state = Default::default();
        self.layout_type = Default::default();
        self.refresh_layout();
    }

    pub fn set_accept_button_text(&mut self, text: &str) {
        if self.accept_button_text != text {
            self.accept_button_text = text.into();
        }
    }

    pub fn set_accept_button_enabled(&mut self, enabled: bool) { self.accept_button_enabled = enabled; }

    pub fn set_delete_button_enabled(&mut self, enabled: bool) { self.delete_button_enabled = enabled; }

    pub fn get_key(&self, click_x: f32, click_y: f32) -> Option<(&KeyDef, f32, f32)> {
        let (y, row) = space_filling_find(
            self.layout.rows_with_coords(),
            KEYBOARD_TOP_BAR_MARGIN as f32..SCREEN_HEIGHT as f32,
            click_y,
        )?;
        let (x, key) = space_filling_find(row.keys_with_coords(), 0.0..SCREEN_WIDTH as f32, click_x)?;
        Some((key.key, x, y))
    }

    pub fn get_key_def(&self, key_id: u64) -> Option<&KeyDef> {
        let slot = self.layout.get_key(key_id)?;
        Some(slot.key)
    }

    pub fn refresh_layout(&mut self) {
        self.layout = match self.kind {
            KeyboardKind::Alphanumeric | KeyboardKind::Password | KeyboardKind::Email => {
                match self.layout_type {
                    LayoutType::Alphabetic => match self.shift_state {
                        ShiftState::Lowercase => &LAYOUT_ALPHA_LOWER,
                        ShiftState::Uppercase => &LAYOUT_ALPHA_UPPER,
                        ShiftState::CapsLock => &LAYOUT_ALPHA_UPPER_CAPS,
                    },
                    LayoutType::Numeric => &LAYOUT_ALPHA_NUMERIC,
                    LayoutType::Punctuation => &LAYOUT_ALPHA_PUNCTUATION,
                }
            }
            KeyboardKind::Numbers => &LAYOUT_NUMERIC,
            KeyboardKind::Decimal => &LAYOUT_DECIMAL,
        };
    }

    pub fn shift_state_request(&mut self, shift_on: bool) {
        if shift_on {
            if self.shift_state == ShiftState::Lowercase {
                self.shift_state = ShiftState::Uppercase;
                self.refresh_layout();
            }
        } else {
            if self.shift_state == ShiftState::Uppercase {
                self.shift_state = ShiftState::Lowercase;
                self.refresh_layout();
            }
        }
    }

    pub fn on_pressed(&mut self, x: f32, y: f32) {
        let (touch_x, touch_y) = (x, y);
        let Some((key, x, y)) = self.get_key(x, y) else { return };
        match key.on_released {
            KeyAction::Return if !self.accept_button_enabled => return,
            KeyAction::Backspace if !self.delete_button_enabled => return,
            _ => {}
        }
        let key = key.clone();
        let id = key.id();

        self.key_pressed = Some(PressedKey { id, x, y, at: Instant::now() });
        self.overlay_x = Some(x as i32);
        if key.overlay.len() > 1 {
            self.long_press_callback.request(LONG_OVERLAY_DELAY, InputMessage::Custom4 as usize, 0);
        }

        if key.on_released == KeyAction::Space {
            self.sliding.start(touch_x, touch_y);
        }

        self.haptics_api.vibrate(haptics::HapticPattern::SharpClick100);
    }

    fn cleanup(&mut self) -> (Option<char>, Option<PressedKey>) {
        self.show_overlay_long = false;
        self.overlay_x = None;

        let was_sliding = self.sliding.reset();

        let last_key = if was_sliding { None } else { self.key_pressed };
        self.key_pressed = None;
        self.key_pressed_pixmap = None;
        let overlay_char = if was_sliding { None } else { self.overlay_char };
        self.overlay_char = None;

        self.long_press_callback.cancel(InputMessage::Custom4 as usize);

        (overlay_char, last_key)
    }

    pub fn on_released(&mut self, x: f32, y: f32) -> Option<Key> {
        log::trace!("released {x}, {y}");

        let (overlay_char, last_key) = self.cleanup();

        if let Some(ch) = overlay_char {
            self.shift_state_request(false);
            return Some(Key::Char(ch as usize));
        }

        let key = last_key.and_then(|key| self.get_key_def(key.id));

        let Some(key) = key else {
            return None;
        };

        match key.on_released {
            KeyAction::Insert => {
                let result = key.label.chars().next().map(|c| Key::Char(c as usize));
                self.shift_state_request(false);
                result
            }
            KeyAction::None => None,
            KeyAction::Return => Some(Key::Char('\n' as usize)),
            KeyAction::Backspace => Some(Key::Backspace),
            KeyAction::Space => Some(Key::Char(' ' as usize)),
            KeyAction::Shift => {
                self.shift_state = self.shift_state.next();
                self.refresh_layout();
                None
            }
            KeyAction::ChangeLayer(layer_type) => {
                self.layout_type = layer_type;
                self.refresh_layout();
                None
            }
        }
    }

    pub fn on_moved(&mut self, x: f32, y: f32) -> Option<Key> {
        if let Some(event) = self.sliding.on_moved(x, y) {
            return match event {
                SlideEvent::Activated => {
                    self.long_press_callback.cancel(InputMessage::Custom4 as usize);
                    self.show_overlay_long = false;
                    self.overlay_x = None;
                    self.overlay_char = None;
                    self.key_pressed_pixmap = None;
                    self.haptics_api.click();
                    None
                }
                SlideEvent::EmitKey(key) => Some(key),
                SlideEvent::Idle => None,
            };
        }

        // --- Normal (non-sliding) move handling ---
        const HALF_HEIGHT: f32 = ROW_HEIGHT * 0.5;

        let Some(key_pressed) = &self.key_pressed else { return None };

        // check if moved away from pressed char
        if (key_pressed.y + HALF_HEIGHT - y).abs() > HALF_HEIGHT * 3.0 {
            // forget pressed key;
            let _ = self.cleanup();
        } else {
            // overlay shown, we need to select char from overlay
            self.overlay_x = Some(x as i32);
        }

        None
    }

    pub fn on_long_press(&mut self) {
        self.haptics_api.vibrate(haptics::HapticPattern::SharpClick100);
        self.show_overlay_long = true;
    }

    fn full_rect() -> IntRect {
        IntRect::from_xywh(0, 0, SCREEN_WIDTH as u32, DEFAULT_KEYBOARD_HEIGHT as u32).unwrap()
    }

    pub fn draw(&mut self, vsync_time: usize, full_redraw: bool, mut framebuffer: MemoryRange) {
        self.redraw_requested = false;
        // If we got a "hot" buffer that's still used by the LCDC, wait until we
        // can be sure it's not used anymore.
        let lcdc_estimated_render_tick = vsync_time + 1000 / FPS;
        let current_tick = self.ticktimer.elapsed_ms() as usize;
        if lcdc_estimated_render_tick > current_tick {
            let diff = lcdc_estimated_render_tick - current_tick;
            // If the difference is too large, we probably wrapped on 32 bit,
            // or there was some other issue with the calculation.
            // In this case just let it glitch visually. It will resolve in 1-2 frames.
            if diff < 100 {
                log::trace!("Sleeping {diff}");
                std::thread::sleep(Duration::from_millis(diff as u64));
            }
        }

        let mut target = PixmapMut::from_bytes(
            framebuffer.as_slice_mut(),
            SCREEN_WIDTH as u32,
            DEFAULT_KEYBOARD_HEIGHT as u32,
        )
        .unwrap();

        let prev = self.previous_buffers[1].clone();
        with_cached_pixmap(&self.layout, |layout_image| {
            if full_redraw || !core::ptr::eq(self.layout, prev.layout) {
                blit(&mut target, layout_image, Self::full_rect());
            } else {
                if let Some(d) = prev.dirty {
                    blit(&mut target, layout_image, d);
                }
                if prev.accept_button_text != self.accept_button_text
                    || prev.accept_button_enabled != self.accept_button_enabled
                {
                    if let Some(region) = self.key_region(crate::keys::KeyAction::Return) {
                        blit(&mut target, layout_image, region);
                    }
                }
                if prev.delete_button_enabled != self.delete_button_enabled {
                    if let Some(region) = self.key_region(crate::keys::KeyAction::Backspace) {
                        blit(&mut target, layout_image, region);
                    }
                }
            }
        });

        self.draw_button_overrides(&mut target);
        let new_dirty = self.draw_overlay(&mut target);
        let new_state = FrameBufferState {
            layout: self.layout,
            dirty: new_dirty,
            accept_button_text: self.accept_button_text.clone(),
            accept_button_enabled: self.accept_button_enabled,
            delete_button_enabled: self.delete_button_enabled,
        };
        self.previous_buffers = [new_state, self.previous_buffers[0].clone()];
    }

    /// Bounding rect of the (at most one) key in the current layout whose
    /// action matches `action`, inflated to cover the drop shadow drawn
    /// around it.
    fn key_region(&self, action: crate::keys::KeyAction) -> Option<IntRect> {
        use crate::colors::{SHADOW_OFFSET_Y, SHADOW_RADIUS};
        let pad = SHADOW_RADIUS as i32;
        let pad_bottom = pad + SHADOW_OFFSET_Y as i32;
        for (y_range, row) in self.layout.rows_with_coords() {
            for (x_range, slot) in row.keys_with_coords() {
                if slot.key.on_released != action {
                    continue;
                }
                let l = x_range.start.floor() as i32 - pad;
                let t = y_range.start.floor() as i32 - pad;
                let r = (x_range.start + slot.width).ceil() as i32 + pad;
                let b = (y_range.start + slot.height).ceil() as i32 + pad_bottom;
                return IntRect::from_ltrb(l, t, r, b);
            }
        }
        None
    }

    /// Re-render the Return-action and Backspace-action keys in the current
    /// layout to apply `accept_button_text`, `accept_button_enabled`, and
    /// `delete_button_enabled` if needed
    fn draw_button_overrides(&self, pixmap: &mut PixmapMut) {
        if self.accept_button_text.is_empty() && self.accept_button_enabled && self.delete_button_enabled {
            return;
        }
        let accept_label = (!self.accept_button_text.is_empty()).then(|| self.accept_button_text.as_str());
        let accept_style = (!self.accept_button_enabled).then_some(crate::colors::KeyStyle::Disabled);
        let delete_style = (!self.delete_button_enabled).then_some(crate::colors::KeyStyle::Disabled);
        for (y_range, row) in self.layout.rows_with_coords() {
            for (x_range, slot) in row.keys_with_coords() {
                let (label_override, style_override) = match slot.key.on_released {
                    crate::keys::KeyAction::Return => (accept_label, accept_style),
                    crate::keys::KeyAction::Backspace => (None, delete_style),
                    _ => (None, None),
                };
                if label_override.is_none() && style_override.is_none() {
                    continue;
                }
                slot.draw(x_range.start, y_range.start, pixmap, false, label_override, style_override);
            }
        }
    }

    fn draw_overlay(&mut self, pixmap: &mut PixmapMut) -> Option<IntRect> {
        // Don't draw any key overlay while cursor sliding is active.
        if self.sliding.is_active() {
            return None;
        }

        let Some(key_pressed) = self.key_pressed else {
            return None;
        };
        // render overlay if necessary
        let Some(slot) = self.layout.get_key(key_pressed.id) else {
            return None;
        };

        let x = key_pressed.x;
        let y = key_pressed.y;
        let rect = &Rect::from_xywh(x, y, slot.width, KEY_HEIGHT).unwrap();

        let Some(mouse_x) = self.overlay_x else {
            return None;
        };

        if slot.key.overlay == "" {
            let label_override = match slot.key.on_released {
                KeyAction::Return if !self.accept_button_text.is_empty() => {
                    Some(self.accept_button_text.as_str())
                }
                _ => None,
            };
            let pressed = self.key_pressed_pixmap.get_or_insert_with(|| {
                let mut pressed = Pixmap::new(slot.width.ceil() as u32 + 2, KEY_HEIGHT as u32 + 2).unwrap();
                slot.draw(
                    x.fract() + 1.0,
                    y.fract() + 1.0,
                    &mut pressed.as_mut(),
                    true,
                    label_override,
                    None,
                );
                pressed
            });
            let x = x as i32 - 1;
            let y = y as i32 - 1;
            pixmap.draw_pixmap(x, y, pressed.as_ref(), &Default::default(), Transform::identity(), None);
            IntRect::from_xywh(x, y, pressed.width(), pressed.height())
        } else {
            let text = if self.show_overlay_long {
                slot.key.overlay
            } else {
                if let Some(c) = slot.key.label.chars().next() {
                    &slot.key.label[..c.len_utf8()]
                } else {
                    "?"
                }
            };
            let (dirty, ch) = crate::overlay::draw(&mut self.overlay_cache, &text, pixmap, rect, mouse_x);
            if let Some(ch) = ch {
                self.overlay_char = Some(ch);
            }
            Some(dirty)
        }
    }
}

impl Default for FrameBufferState {
    fn default() -> Self {
        Self {
            layout: &LAYOUT_ALPHA_LOWER,
            dirty: Some(KeyboardState::full_rect()),
            accept_button_text: String::new(),
            accept_button_enabled: true,
            delete_button_enabled: true,
        }
    }
}

fn space_filling_find<'a, T>(
    iter: impl Iterator<Item = (core::ops::Range<f32>, &'a T)>,
    full_range: core::ops::Range<f32>,
    click: f32,
) -> Option<(f32, &'a T)> {
    let mut start = full_range.start;
    let mut iter = iter.peekable();
    while let Some((range, result)) = iter.next() {
        let end = if let Some((next_range, _)) = iter.peek() {
            (range.end + next_range.start) * 0.5
        } else {
            full_range.end
        };
        if (start..end).contains(&click) {
            return Some((range.start, result));
        }
        start = end;
    }
    None
}
