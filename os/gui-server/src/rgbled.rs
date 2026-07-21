// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use rgb_led::{RgbAnimation, RgbColor};
#[cfg(not(keyos))]
use server::ServerContext;

rgb_led::use_api!();

#[cfg(not(keyos))]
use crate::Gui;

const RGB_BRIGHTNESS_DEFAULT: f32 = crate::display::DEFAULT_BACKLIGHT_LEVEL_PERCENT as f32 / 100.0;

const RGB_CONNECTION_TIMEOUT_MS: u64 = 1000;

const ON_OFF_DURATION_MS: usize = 300;
const CLICK_DURATION_MS: usize = 100;

pub(crate) struct RgbLedState {
    rgb_api: Option<RgbApi>,
    brightness: f32,
    turned_on: bool,
}

impl Default for RgbLedState {
    fn default() -> Self { Self { rgb_api: None, brightness: RGB_BRIGHTNESS_DEFAULT, turned_on: false } }
}

impl RgbLedState {
    #[cfg(not(keyos))]
    pub(crate) fn subscribe_color_updates(&mut self, context: &mut ServerContext<Gui>) {
        self.ensure_connected();
        if let Some(api) = &mut self.rgb_api {
            api.subscribe_color_updates(context);
        }
    }

    fn ensure_connected(&mut self) {
        if self.rgb_api.is_some() {
            return;
        }
        self.rgb_api = RgbApi::try_new_with_timeout(Duration::from_millis(RGB_CONNECTION_TIMEOUT_MS));
    }

    fn animate(&mut self, animation: RgbAnimation) {
        self.ensure_connected();
        if let Some(api) = &mut self.rgb_api {
            api.animate_all(animation)
        }
    }

    pub(crate) fn virt_button_press_animation(&mut self) {
        if self.turned_on {
            // Set to teal color and keep it while pressed
            self.ensure_connected();
            if let Some(api) = &mut self.rgb_api {
                api.set_all_to(RgbColor::TEAL.scale(self.brightness))
            }
        }
    }

    pub(crate) fn virt_button_release_animation(&mut self) {
        if self.turned_on {
            self.animate(RgbAnimation::new(
                RgbColor::TEAL.scale(self.brightness),
                RgbColor::WHITE.scale(self.brightness),
                CLICK_DURATION_MS,
                false,
            ));
        }
    }

    pub(crate) fn disabled_virt_button_press_animation(&mut self) {
        if self.turned_on {
            // Set to red color and keep it while pressed
            self.ensure_connected();
            if let Some(api) = &mut self.rgb_api {
                api.set_all_to(RgbColor::RED.scale(self.brightness))
            }
        }
    }

    pub(crate) fn disabled_virt_button_release_animation(&mut self) {
        if self.turned_on {
            self.animate(RgbAnimation::new(
                RgbColor::RED.scale(self.brightness),
                RgbColor::WHITE.scale(self.brightness),
                CLICK_DURATION_MS,
                false,
            ));
        }
    }

    pub(crate) fn turn_off(&mut self) {
        if self.turned_on {
            self.animate(RgbAnimation::new(
                RgbColor::WHITE.scale(self.brightness),
                RgbColor::BLACK,
                ON_OFF_DURATION_MS,
                false,
            ));
            self.turned_on = false;
        }
    }

    pub(crate) fn turn_on(&mut self) {
        if !self.turned_on {
            self.animate(RgbAnimation::new(
                RgbColor::BLACK,
                RgbColor::WHITE.scale(self.brightness),
                ON_OFF_DURATION_MS,
                false,
            ));
        }
        self.turned_on = true;
    }

    #[cfg(not(feature = "recovery-os"))]
    pub(crate) fn set_brightness_pct(&mut self, percentage: u8) {
        self.brightness = percentage as f32 / 100.0;
        if self.turned_on {
            if let Some(api) = &mut self.rgb_api {
                api.set_all_to(RgbColor::WHITE.scale(self.brightness))
            }
        }
    }
}
