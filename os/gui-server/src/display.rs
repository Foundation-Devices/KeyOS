// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(keyos))]
pub mod emulator;

#[cfg(keyos)]
mod lcdc;

#[cfg(not(keyos))]
pub(crate) use emulator::*;
#[cfg(keyos)]
pub(crate) use lcdc::*;

pub(crate) const DEFAULT_BACKLIGHT_LEVEL_PERCENT: u8 = 70;
pub(crate) const DIM_LEVEL_DIVIDER: u8 = 5;

impl crate::Gui {
    pub(crate) fn screen_brightness_setting(&self) -> u8 {
        #[cfg(not(feature = "recovery-os"))]
        {
            self.settings.get_screen_brightness().0
        }

        #[cfg(feature = "recovery-os")]
        {
            DEFAULT_BACKLIGHT_LEVEL_PERCENT
        }
    }

    pub(crate) fn screen_brightness_setting_dimmed(&self) -> u8 {
        self.screen_brightness_setting() / DIM_LEVEL_DIVIDER
    }
}
