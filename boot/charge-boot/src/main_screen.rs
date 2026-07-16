// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        batt::{
            batt_color, get_battery_soc, is_charging, start_charging, stop_charging, THRESHOLD_CHARGE_STOP,
            THRESHOLD_GREEN,
        },
        flash_erase::erase_flash_blocks,
        gui::{BatteryIcon, Page},
        rgb::rgb_set_multiple,
    },
    atsama5d27::{
        pmc::{PeripheralId, Pmc},
        sfc::Sfc,
        twi::Twi,
    },
    boot_common::{
        colors::DarkPalette,
        display::{backlight_fade_out, backlight_set, lcd_sleep, lcd_wake, swap_buffers, DISPLAY},
        get_pit,
        gui::{Button, ButtonType, CustomComponent, Text, BUTTON_HEIGHT},
        pins::PWR_BTN,
        shutdown,
        theme::UISize,
        touch::get_last_touch,
        HEIGHT, WIDTH,
    },
    embedded_graphics::{draw_target::DrawTarget, pixelcolor::Rgb888, prelude::RgbColor},
    ft3269::{Ft3269, TouchKind},
    fuse::{get_board_revision, get_colorway, BoardRevision, Colorway},
    keyos::MASTER_CLOCK_SPEED,
};

pub const DEFAULT_BACKLIGHT_LEVEL: u8 = 0x73; // 55% backlight
                                              // const LOW_BATTERY_DELAY_BEFORE_SHUTDOWN_MS: u32 = 1000 * 3;

const IDLE_TIMEOUT_MS: u32 = 1000 * 5;

pub(crate) fn show_main_screen() -> ! {
    let i2c = Twi::twi0();
    let mut ctp = Ft3269::new(i2c);

    let mut pit = get_pit();

    let mut menu_page = MenuPage::new();

    let mut touch_active = false;
    let mut needs_redraw = true;

    let mut idle_timer = 0u32;
    let mut lcd_is_sleeping = false;
    let mut finish_added = false;
    let mut charge_check_counter = 0;
    let mut rgb_counter = 0;
    let mut rgb_wave_step = 0;
    let mut prev_soc = 0;
    let mut prev_charging = false;
    let mut charge_enabled = true;
    let mut pwr_btn_hold_ms = 0u32;

    const PWR_BTN_HOLD_THRESHOLD_MS: u32 = 3000;

    backlight_set(DEFAULT_BACKLIGHT_LEVEL);

    loop {
        pit.busy_wait_ms(MASTER_CLOCK_SPEED, 10);

        if !lcd_is_sleeping {
            if needs_redraw {
                display_clear();
                menu_page.render();
                needs_redraw = false;
                swap_buffers();
            }

            // Check for touches
            if let Some((x, y, kind)) = get_last_touch(&mut ctp) {
                match kind {
                    TouchKind::Press => {
                        menu_page.on_press(x as i32, y as i32);
                        needs_redraw = true;
                        touch_active = true;
                    }
                    TouchKind::Release => {
                        if touch_active {
                            menu_page.on_release(x as i32, y as i32);
                            needs_redraw = true;
                            touch_active = false;
                        }
                    }
                    TouchKind::Drag => {
                        if !touch_active {
                            menu_page.on_press(x as i32, y as i32);
                            needs_redraw = true;
                            touch_active = true;
                        }
                    }
                    TouchKind::Reserved => {}
                }
            }

            if idle_timer < IDLE_TIMEOUT_MS {
                idle_timer = idle_timer.saturating_add(10);
            } else {
                lcd_sleep();
                backlight_set(0xff);
                lcd_is_sleeping = true;
            }
        }

        charge_check_counter += 1;
        if charge_check_counter > 50 {
            let soc = get_battery_soc() as u8;
            if soc != prev_soc {
                prev_soc = soc;
                needs_redraw = true;
                if soc >= THRESHOLD_GREEN {
                    if !finish_added {
                        menu_page.add_finished_button();
                        finish_added = true;
                        needs_redraw = true;
                    }
                    if soc >= THRESHOLD_CHARGE_STOP && charge_enabled {
                        stop_charging();
                        charge_enabled = false;
                    }
                } else if !charge_enabled {
                    start_charging();
                    charge_enabled = true;
                }
            }
            let charging = is_charging();
            if charging != prev_charging {
                prev_charging = charging;
                needs_redraw = true;
            }
            charge_check_counter = 0;
        }

        rgb_counter += 1;
        if rgb_counter > 10 {
            rgb_counter = 0;
            rgb_wave_step += 1;
            let led_color = batt_color(prev_soc as _);
            if prev_charging {
                for i in 0..4 {
                    rgb_set_multiple(i..i + 1, rgb_wave(rgb_wave_step + i * 4, led_color));
                }
            } else {
                rgb_set_multiple(0..4, led_color);
            }
        }

        // The power button pulls to ground on press
        if !PWR_BTN.get() {
            // Wake up if we were sleeping
            if lcd_is_sleeping {
                lcd_wake();
                backlight_set(DEFAULT_BACKLIGHT_LEVEL);
                lcd_is_sleeping = false;
            }
            idle_timer = 0;
            // Require holding for 3 seconds before shutting down
            pwr_btn_hold_ms = pwr_btn_hold_ms.saturating_add(10);
            if pwr_btn_hold_ms >= PWR_BTN_HOLD_THRESHOLD_MS {
                switch_to_samba_and_shut_down();
            }
        } else {
            pwr_btn_hold_ms = 0;
        }
    }
}

fn rgb_wave(step: usize, color: Rgb888) -> Rgb888 {
    const WAVE: [u16; 16] = [0, 1, 2, 4, 8, 8, 8, 8, 4, 2, 1, 0, 0, 0, 0, 0];
    let wave = WAVE[step % WAVE.len()];
    Rgb888::new(
        ((color.r() as u16) * wave / 8) as u8,
        ((color.g() as u16) * wave / 8) as u8,
        ((color.b() as u16) * wave / 8) as u8,
    )
}

pub(crate) fn display_clear() {
    if let Some(display) = unsafe { (*core::ptr::addr_of_mut!(DISPLAY)).as_mut() } {
        display.clear(DarkPalette::BLACK).ok();
    }
}

pub struct MenuPage {
    page: Page,
}

impl MenuPage {
    pub fn new() -> Self {
        let mut page = Page::new(WIDTH as i32, HEIGHT as i32);

        // Initial positions for components
        let curr_x = UISize::SZ10;
        let main_width = page.bounds().width - 2 * UISize::SZ10;

        // Add the title
        page.add_component(Text::new(
            curr_x,
            100,
            main_width,
            BUTTON_HEIGHT,
            "BATTERY STATUS",
            true,
            DarkPalette::WHITE,
        ));

        // Add colorway and board revision info
        let mut pmc = Pmc::new();
        pmc.enable_peripheral_clock(PeripheralId::Sfc);
        let sfc = Sfc::new();
        let colorway = get_colorway(&sfc).unwrap_or(Colorway::Dark);
        let board_rev = get_board_revision(&sfc);
        let info_text: &'static str = match (colorway, board_rev) {
            (Colorway::Light, BoardRevision::RevD1) => "LIGHT / D1",
            (Colorway::Light, BoardRevision::RevD6) => "LIGHT / D6",
            (Colorway::Dark, BoardRevision::RevD1) => "DARK / D1",
            (Colorway::Dark, BoardRevision::RevD6) => "DARK / D6",
        };
        page.add_component(Text::new(
            curr_x,
            160,
            main_width,
            BUTTON_HEIGHT,
            info_text,
            true,
            DarkPalette::WHITE,
        ));

        page.add_component(CustomComponent(BatteryIcon::new(100, 300, page.bounds().width - 200, 100)));

        // Return the MenuPage instance
        Self { page }
    }

    pub fn add_finished_button(&mut self) {
        let curr_x = UISize::SZ10;
        let main_width = self.page.bounds().width - 2 * UISize::SZ10;
        self.page.add_component(Button::new(
            curr_x,
            600,
            main_width,
            Some("P"),
            Some("Ready for Sealing"),
            ButtonType::Primary,
            switch_to_samba_and_shut_down,
        ));
    }

    pub fn render(&self) { self.page.render(); }

    pub fn on_press(&mut self, x: i32, y: i32) { self.page.on_press(x, y); }

    pub fn on_release(&mut self, x: i32, y: i32) { self.page.on_release(x, y); }
}

fn switch_to_samba_and_shut_down() {
    erase_flash_blocks();
    backlight_fade_out();
    shutdown();
}
