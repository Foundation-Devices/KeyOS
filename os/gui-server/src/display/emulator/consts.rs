// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Common UISize.
use gui_server_api::consts::{DEVICE_WIDTH, LCD_X, LCD_Y, SCREEN_HEIGHT, SCREEN_WIDTH};

pub(super) const VIRT_PWR_BUTTON_WIDTH: usize = 128;
pub(super) const VIRT_PWR_BUTTON_HEIGHT: usize = 48;
pub(super) const VIRT_PWR_BUTTON_X: usize = DEVICE_WIDTH as usize / 2 - VIRT_PWR_BUTTON_WIDTH / 2;
pub(super) const VIRT_PWR_BUTTON_Y: usize = 0;

pub(super) const TOUCH_AREA_X: usize = LCD_X as usize;
pub(super) const TOUCH_AREA_Y: usize = LCD_Y as usize;
pub(super) const TOUCH_AREA_W: usize = SCREEN_WIDTH;
pub(super) const TOUCH_AREA_H: usize = SCREEN_HEIGHT;

// On-screen coordinates of the virtual touch button area for hosted mode
pub(super) const VIRT_HOME_BUTTON_X: usize = 220;
pub(super) const VIRT_HOME_BUTTON_Y: usize = 1000;
pub(super) const VIRT_HOME_BUTTON_TOUCH_Y: usize = 880; // On-screen and touch coordinates are different
pub(super) const VIRT_HOME_BUTTON_WIDTH: usize = 150;
pub(super) const VIRT_HOME_BUTTON_HEIGHT: usize = 38; // It's less tall in hardware (10 px)
