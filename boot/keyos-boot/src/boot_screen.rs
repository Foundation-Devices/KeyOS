// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        batt::{check_low_battery, is_charging, BatteryState},
        menu_page::MenuPage,
        progress_bar::{ProgressBarMessage, DISPLAY_PB_OVERLAY},
        splash::init_splash_layers,
        system_error_page::SystemErrorPage,
        PROGRESS_BAR,
    },
    atsama5d27::{
        lcdc::Lcdc,
        pio::{Direction, Func, Pio},
        twi::Twi,
    },
    boot_common::{
        colors::{DarkPalette, UIColors},
        display::{
            backlight_dim, backlight_fade_out, backlight_set, init_display, init_lcdc, swap_buffers,
            ArgbDisplay, DISPLAY,
        },
        get_pit,
        gui::Component,
        pins::PWR_BTN,
        reboot,
        touch::get_last_touch,
        FB_PB_OVERLAY_ADDR, HEIGHT, PB_OVERLAY_HEIGHT, WIDTH,
    },
    core::sync::atomic::{AtomicBool, Ordering},
    embedded_graphics::prelude::DrawTarget,
    ft3269::{Ft3269, TouchKind},
    keyos::MASTER_CLOCK_SPEED,
};

const LOW_BATTERY_BACKLIGHT_LEVEL: u8 = 0xe6; // 10% backlight
const DEFAULT_BACKLIGHT_LEVEL: u8 = 0x73; // 55% backlight
                                          // const LOW_BATTERY_DELAY_BEFORE_SHUTDOWN_MS: u32 = 1000 * 3;

const IDLE_TIMEOUT_MS: u32 = 1000 * 60;
const DIM_TIMEOUT_MS: u32 = IDLE_TIMEOUT_MS - 1000 * 10; // 10s earlier than IDLE_TIMEOUT_MS
const LOW_BATTERY_SCREEN_TIMEOUT_MS: u32 = 1000 * 5; // 5 seconds
const LOW_BATTERY_TIMEOUT_STEP_MS: u32 = 250; // 250ms

pub static mut CURR_PAGE: BootScreenPage = BootScreenPage::Menu;

pub fn set_curr_page(page: BootScreenPage) {
    unsafe {
        CURR_PAGE = page;
    }
}

static CONTINUE_BOOT: AtomicBool = AtomicBool::new(false);

pub fn set_continue_boot(should_continue: bool) { CONTINUE_BOOT.store(should_continue, Ordering::SeqCst); }

pub fn should_continue_boot() -> bool { CONTINUE_BOOT.load(Ordering::SeqCst) }

#[derive(Copy, Clone, PartialEq)]
pub enum BootScreenPage {
    Menu = 0,
    SystemError = 1,
    SystemQRCode = 2,
}

pub(crate) fn show_loading_screen() {
    init_lcdc(init_splash_layers);
    init_display();

    unsafe {
        let fb = core::slice::from_raw_parts_mut(FB_PB_OVERLAY_ADDR as *mut u32, WIDTH * PB_OVERLAY_HEIGHT);
        fb.fill(UIColors::TRANSPARENT_BLACK.into_inner());
        DISPLAY_PB_OVERLAY = Some(ArgbDisplay::new(fb, WIDTH, PB_OVERLAY_HEIGHT));
    }

    let power_btn = Pio::pa25();
    power_btn.set_direction(Direction::Input);
    power_btn.set_func(Func::Gpio);

    let mut num_btn_presses = 0;
    let mut last_btn_state = power_btn.get();

    let lcdc = Lcdc::new(WIDTH as u16, HEIGHT as u16);
    if let Some(pb) = unsafe { (*core::ptr::addr_of_mut!(PROGRESS_BAR)).as_mut() } {
        // In a low battery state, dim the backlight and shutdown the device after a delay
        if let BatteryState::Low = check_low_battery() {
            lcdc.set_pwm_compare_value(LOW_BATTERY_BACKLIGHT_LEVEL);
            pb.set_message(ProgressBarMessage::LowBattery);

            let mut curr_pb_message = ProgressBarMessage::LowBattery;
            let mut timeout = LOW_BATTERY_SCREEN_TIMEOUT_MS;
            while timeout > 0 {
                // Check if the power button is pressed
                let btn_state = power_btn.get();
                if btn_state != last_btn_state {
                    if !btn_state {
                        num_btn_presses += 1;
                    }
                    last_btn_state = btn_state;
                }

                // Continue booting if the button pressed 5 times
                if num_btn_presses >= 5 {
                    break;
                }

                get_pit().busy_wait_ms(MASTER_CLOCK_SPEED, LOW_BATTERY_TIMEOUT_STEP_MS);
                timeout = timeout.saturating_sub(LOW_BATTERY_TIMEOUT_STEP_MS);

                // Reboot if the battery becomes sufficiently charged.
                // Since the splash screen is loaded before the entry point, we reboot to ensure the
                // correct one is loaded.
                if let BatteryState::Ok = check_low_battery() {
                    reboot();
                }

                // Update the message depending on the charging status
                let pb_message = if is_charging() {
                    ProgressBarMessage::LowBatteryCharging
                } else {
                    ProgressBarMessage::LowBattery
                };
                if pb_message != curr_pb_message {
                    pb.set_message(pb_message);
                    curr_pb_message = pb_message;
                }
            }

            // Shutdown if the timeout is reached
            if timeout == 0 {
                boot_common::shutdown();
            }
        } else {
            lcdc.set_pwm_compare_value(DEFAULT_BACKLIGHT_LEVEL);
        }
    }
}

pub(crate) fn show_boot_screen(initial_page: BootScreenPage) {
    let i2c = Twi::twi0();
    let mut ctp = Ft3269::new(i2c);

    let mut pit = get_pit();

    let mut menu_page = MenuPage::new();
    let mut system_error_page = SystemErrorPage::new(false);
    let mut system_qr_code_page = SystemErrorPage::new(true);

    let mut touch_active = false;
    let mut needs_redraw = true;

    set_curr_page(initial_page);

    let mut idle_timer = 0u32;
    let mut is_dim = false;
    let mut prev_touch_kind = TouchKind::Reserved as u8;
    loop {
        if needs_redraw {
            display_clear();

            unsafe {
                match CURR_PAGE {
                    BootScreenPage::Menu => {
                        menu_page.render();
                    }
                    BootScreenPage::SystemError => {
                        system_error_page.render();
                    }
                    BootScreenPage::SystemQRCode => {
                        system_qr_code_page.render();
                    }
                }
            }
            needs_redraw = false;
            swap_buffers();
        }

        // Check for touches
        if let Some((x, y, kind)) = get_last_touch(&mut ctp) {
            if kind as u8 != prev_touch_kind && is_dim {
                prev_touch_kind = kind as u8;
                backlight_set(DEFAULT_BACKLIGHT_LEVEL);
                idle_timer = 0;
                is_dim = false;
            }

            match kind {
                TouchKind::Press => {
                    unsafe {
                        match CURR_PAGE {
                            BootScreenPage::Menu => menu_page.on_press(x as i32, y as i32),
                            BootScreenPage::SystemError => system_error_page.on_press(x as i32, y as i32),
                            BootScreenPage::SystemQRCode => system_qr_code_page.on_press(x as i32, y as i32),
                        }
                    }
                    needs_redraw = true;
                    touch_active = true;
                }
                TouchKind::Release => {
                    if touch_active {
                        unsafe {
                            match CURR_PAGE {
                                BootScreenPage::Menu => menu_page.on_release(x as i32, y as i32),
                                BootScreenPage::SystemError => {
                                    system_error_page.on_release(x as i32, y as i32)
                                }
                                BootScreenPage::SystemQRCode => {
                                    system_qr_code_page.on_release(x as i32, y as i32)
                                }
                            }
                        }
                        needs_redraw = true;
                        touch_active = false;
                    }
                }
                TouchKind::Drag => {
                    if !touch_active {
                        unsafe {
                            match CURR_PAGE {
                                BootScreenPage::Menu => menu_page.on_press(x as i32, y as i32),
                                BootScreenPage::SystemError => system_error_page.on_press(x as i32, y as i32),
                                BootScreenPage::SystemQRCode => {
                                    system_qr_code_page.on_press(x as i32, y as i32)
                                }
                            }
                        }
                        touch_active = true;
                    }
                    unsafe {
                        // Only SystemError page needs custom drag handling for scrolling
                        if CURR_PAGE == BootScreenPage::SystemError {
                            system_error_page.on_drag(x as i32, y as i32);
                        }
                    }
                    needs_redraw = true;
                }
                TouchKind::Reserved => {}
            }
        }

        pit.busy_wait_ms(MASTER_CLOCK_SPEED, 10);
        idle_timer = idle_timer.saturating_add(10);

        if idle_timer >= IDLE_TIMEOUT_MS {
            backlight_fade_out();
            boot_common::shutdown();
        } else if idle_timer >= DIM_TIMEOUT_MS && !is_dim {
            backlight_dim(LOW_BATTERY_BACKLIGHT_LEVEL);
            is_dim = true;
        }

        if should_continue_boot() {
            set_continue_boot(false); // Reset to allow other menu screens to be shown when needed

            boot_common::wdt_reset();
            return;
        }

        boot_common::wdt_reset();
    }
}

pub(crate) fn display_clear() {
    if let Some(display) = unsafe { (*core::ptr::addr_of_mut!(DISPLAY)).as_mut() } {
        display.clear(DarkPalette::BLACK).ok();
    }
}

const TRY_BOOT_MENU_TIMEOUT_MS: u32 = 1000;

pub(crate) fn try_boot_menu() {
    let mut pit = get_pit();

    let power_btn = PWR_BTN;

    let mut prev_state = power_btn.get();
    let mut num_presses = 0;
    let mut timeout = 0u32;
    loop {
        pit.busy_wait_ms(MASTER_CLOCK_SPEED, 10);
        timeout = timeout.saturating_add(10);

        if timeout >= TRY_BOOT_MENU_TIMEOUT_MS {
            break; // Exit the loop after the idle timeout
        }

        // Check if the power button is pressed
        let btn_state = power_btn.get();
        if !prev_state && btn_state {
            num_presses += 1;
            timeout = 0;
        }

        prev_state = btn_state;

        if num_presses >= 2 {
            crate::splash::hide_progress_layer();
            crate::splash::hide_splash_layer();
            show_boot_screen(BootScreenPage::Menu);
            return;
        }
    }
}
