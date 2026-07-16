// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use ft3269::{Ft3269, PowerMode, Touch as FtTouch, TouchKind as FtTouchKind};
use gpio::{GpioApi, GpioPin};
use gui_server_api::{
    consts::SCREEN_HEIGHT,
    touch::{Touch, TouchKind},
};
use i2c::Peripheral;

use crate::{gpio::GpioPermissions, Gui};

i2c::use_api!();

// The release event is sent wrong by the touch controller, it is off by this much if the release is within
// the virutal button area.
const VIRT_BUTTON_RELEASE_OFFSET: u16 = 36;

pub(crate) struct HwTouchState {
    ft3269: ft3269::Ft3269<I2cPeripheral>,
    gpio_api: GpioApi<GpioPermissions>,
    enabled: bool,
}

impl Default for HwTouchState {
    fn default() -> Self {
        log::debug!("Claiming touch controller I2C peripheral");
        let i2c_api = I2cApi::default();
        let i2c_periph =
            i2c_api.claim_peripheral(Peripheral::TouchController).expect("Could not claim touch peripheral");
        let ft3269 = Ft3269::new(i2c_periph);

        let gpio_api = GpioApi::<GpioPermissions>::default();
        gpio_api
            .claim_pin(GpioPin::CtpRstB, gpio::PinSettings::OutputHigh, false)
            .expect("Could not claim the touch reset pin");

        HwTouchState {
            ft3269,
            gpio_api,
            // The bootloader resets and enables the controller for us
            enabled: true,
        }
    }
}
impl HwTouchState {
    pub(crate) fn enable(&mut self) {
        if self.enabled {
            return;
        }
        // We can only wake up from hibernate state with a hard reset to the controller.
        self.gpio_api.set_pin(GpioPin::CtpRstB, false).ok();
        std::thread::sleep(Duration::from_millis(5));
        self.gpio_api.set_pin(GpioPin::CtpRstB, true).ok();
        self.enabled = true;
    }

    pub(crate) fn disable(&mut self) {
        if !self.enabled {
            return;
        }
        if let Err(e) = self.ft3269.set_power_mode(PowerMode::Hibernate) {
            log::error!("Error setting touch module state to Hibernate: {e:?}");
        }
        self.enabled = false;
    }
}

impl Gui {
    pub(crate) fn handle_touch_irq(&mut self) {
        if !self.touch_state.hw_state.enabled {
            log::debug!("Spurious touch IRQ");
            return;
        }

        let mut touch_buf: [FtTouch; 5] = [FtTouch::default(); 5];

        if self.touch_state.hw_state.ft3269.touches(&mut touch_buf).is_ok() {
            for touch in &touch_buf {
                // Convert from hardware representation to ours, should it be any different
                let kind = match touch.kind {
                    FtTouchKind::Press => TouchKind::Press,
                    FtTouchKind::Release => TouchKind::Release,
                    FtTouchKind::Drag => TouchKind::Drag,
                    FtTouchKind::Reserved => {
                        // All others will be "Reserved" after this one.
                        break;
                    }
                };

                let y = if kind == TouchKind::Release && touch.y > SCREEN_HEIGHT as u16 {
                    touch.y - VIRT_BUTTON_RELEASE_OFFSET
                } else {
                    touch.y
                };

                let touch = Touch { kind, id: touch.id as usize, x: touch.x as usize, y: y as usize };

                self.touch_dispatch(touch, false);
            }
        }
    }
}
