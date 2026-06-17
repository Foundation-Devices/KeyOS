// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering},
    Mutex,
};

mod consts;
pub(crate) mod draw;
mod virtbuttons;
pub mod window;

use gui_server_api::consts::FPS;
use server::MessageId as _;

use crate::{
    handlers::OnVsyncMessage,
    layers::{Layer, LayerStack},
    Gui,
};

pub const MAX_LAYERS: usize = 4;

static LAYER_STACK: Mutex<LayerStack> = Mutex::new(LayerStack { layers: [None, None, None, None] });

static LCD_BACKLIGHT_LEVEL: AtomicU8 = AtomicU8::new(0xff);

static SCALE_FACTOR: AtomicUsize = AtomicUsize::new(0x100);

/// Packed RGB LED color (LE bytes [r, g, b, 0]) as last set by the hosted RGB server.
/// `u32::MAX` is the sentinel meaning "not yet set" — valid colors always have byte 3 == 0.
static CURRENT_RGB_LED_COLOR: AtomicU32 = AtomicU32::new(u32::MAX);

static VIRTUAL_VSYNC_EVENTS: Mutex<Vec<Box<dyn FnMut() + Send>>> = Mutex::new(Vec::new());

static DISPLAY_DIRTY: AtomicBool = AtomicBool::new(true);

static VSYNC_HAPPENED: AtomicBool = AtomicBool::new(false);

pub(crate) struct PlatformDisplay {
    lcd_on: bool,
}

impl PlatformDisplay {
    pub(crate) fn init(initial_base: Layer) -> Self {
        LAYER_STACK.lock().unwrap().push(initial_base);

        // Virtual V-sync thread
        std::thread::spawn(move || loop {
            for handler in VIRTUAL_VSYNC_EVENTS.lock().unwrap().iter_mut() {
                handler()
            }
            std::thread::sleep(std::time::Duration::from_secs_f64(1.0 / FPS as f64));
        });

        Self { lcd_on: true }
    }

    pub(crate) fn subscribe_to_vsync(&self, context: &mut server::ServerContext<Gui>) {
        let cid = xous::connect(context.sid()).expect("Could not connect to self");

        VIRTUAL_VSYNC_EVENTS.lock().unwrap().push(Box::new(move || {
            if LCD_BACKLIGHT_LEVEL.load(Ordering::SeqCst) != 0 {
                VSYNC_HAPPENED.store(true, std::sync::atomic::Ordering::Relaxed);
                if let Err(e) = xous::try_send_message(
                    cid,
                    xous::Message::Scalar(xous::ScalarMessage {
                        id: OnVsyncMessage::ID,
                        ..Default::default()
                    }),
                ) {
                    log::error!("Could not send OnVSyncMessage: {e:?}");
                }
            }
        }));
    }

    pub(crate) fn setup_layers(&mut self, layers: LayerStack) {
        let mut current = LAYER_STACK.lock().unwrap();
        if *current != layers {
            *current = layers;
            DISPLAY_DIRTY.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn turn_lcd_on(&mut self) { self.lcd_on = true; }

    pub(crate) fn turn_lcd_off(&mut self) { self.lcd_on = false; }

    pub(crate) fn is_lcd_on(&self) -> bool { self.lcd_on }

    pub(crate) fn is_dimmed(&self) -> bool { false }

    pub(crate) fn with_layer_stack<F, R>(mut f: F) -> R
    where
        F: FnMut(&LayerStack) -> R,
    {
        f(&LAYER_STACK.lock().unwrap())
    }

    pub(crate) fn backlight_level() -> u8 { LCD_BACKLIGHT_LEVEL.load(Ordering::SeqCst) }

    pub(crate) fn set_scale_factor(scale_factor: usize) {
        if SCALE_FACTOR.swap(scale_factor, Ordering::Relaxed) != scale_factor {
            DISPLAY_DIRTY.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn scale_factor() -> f64 { SCALE_FACTOR.load(Ordering::Relaxed) as f64 / 256.0 }

    pub(crate) fn set_rgb_led_color(packed: u32) {
        if CURRENT_RGB_LED_COLOR.swap(packed, Ordering::Relaxed) != packed {
            DISPLAY_DIRTY.store(true, Ordering::Relaxed);
        }
    }

    /// Returns the current RGB LED color, or `None` if not yet set by the hosted RGB server.
    pub(crate) fn rgb_led_color() -> Option<u32> {
        let v = CURRENT_RGB_LED_COLOR.load(Ordering::Relaxed);
        if v == u32::MAX {
            None
        } else {
            Some(v)
        }
    }

    pub(crate) fn set_backlight_level_pct(&mut self, percent: u8) {
        let level = (percent.clamp(0, 100) as u32 * 0xFF / 100) as u8;
        if LCD_BACKLIGHT_LEVEL.swap(level, Ordering::SeqCst) != level {
            DISPLAY_DIRTY.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn take_display_dirty() -> bool { DISPLAY_DIRTY.swap(false, Ordering::Relaxed) }

    pub(crate) fn mark_display_dirty() { DISPLAY_DIRTY.store(true, Ordering::Relaxed); }

    pub fn vsync_happened() -> bool { VSYNC_HAPPENED.swap(false, std::sync::atomic::Ordering::Relaxed) }
}
