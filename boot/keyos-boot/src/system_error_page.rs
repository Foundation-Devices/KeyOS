// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        boot_screen::{set_continue_boot, set_curr_page, BootScreenPage},
        gui::{Page, QrCode},
        securam::set_os_arguments,
        select_recovery_image,
        splash::hide_image_overlay,
        system_errors::{get_system_error, CtaAction, SystemErrorCode},
        verify::get_bootloader_version_and_date,
    },
    arrayvec::ArrayVec,
    boot_common::{
        colors::DarkPalette,
        display::backlight_fade_out,
        gui::{
            Bounds, Button, ButtonType, Component, CustomComponent, Line, Text, TextMessage, BUTTON_HEIGHT,
        },
        shutdown,
        tamper::clear_tamper_detection,
        theme::UISize,
        HEIGHT, WIDTH,
    },
    securam_manager::OsArguments,
};

const QR_SIZE_PX: i32 = 380;

pub struct SystemErrorPage {
    page: Page,
}

impl SystemErrorPage {
    pub fn new(show_qr: bool) -> Self {
        let mut page = Page::new(WIDTH as i32, HEIGHT as i32);

        // Initial positions for components
        let curr_x = UISize::SZ8;
        let mut curr_y = UISize::SZ10;
        let main_width = page.bounds().width - 2 * UISize::SZ8;
        let (code, title, message, qr_code, cta_label, cta_action) = get_system_error();

        if let Some(_code) = code {
            // Add the title
            page.add_component(Text::new(
                curr_x,
                curr_y,
                main_width,
                BUTTON_HEIGHT,
                if show_qr { "Support" } else { title },
                true,
                DarkPalette::WHITE,
            ));
            curr_y += BUTTON_HEIGHT;

            // Divider line
            page.add_component(Line::new(
                curr_x,
                curr_y,
                curr_x + main_width,
                curr_y,
                DarkPalette::BORDER_TERTIARY,
                1,
            ));
            curr_y += UISize::SZ7;

            if show_qr && qr_code.is_some() {
                let qr_code = qr_code.unwrap();

                // Draw help messages
                page.add_component(TextMessage::new(
                    curr_x,
                    curr_y,
                    main_width,
                    ArrayVec::try_from(&["Visit by scanning with", "your phone camera."] as &[_]).unwrap(),
                    true,
                ));
                curr_y += UISize::SZ3 * 8;

                if show_qr {
                    // Position QR code in center of screen
                    let qr_x = (WIDTH as i32 - QR_SIZE_PX) / 2;
                    let qr_y = curr_y;
                    page.add_component(CustomComponent(QrCode::new(
                        qr_code, qr_x, qr_y, QR_SIZE_PX, QR_SIZE_PX,
                    )));
                }
                curr_y += QR_SIZE_PX + UISize::SZ1;

                // Draw the URL text
                page.add_component(Text::new(
                    curr_x,
                    curr_y,
                    main_width,
                    BUTTON_HEIGHT,
                    qr_code.qr_url,
                    true,
                    DarkPalette::WHITE,
                ));
            } else {
                // Calculate the actual button area height by counting buttons that will be added
                let mut button_count = 1; // Shut Down button is always present
                if qr_code.is_some() {
                    button_count += 1; // Support button
                }
                if cta_label.is_some() {
                    button_count += 1; // CTA button (Start KeyOS, etc.)
                }

                // Calculate total button area height
                let button_area_height = if show_qr {
                    // Only back button for QR mode
                    BUTTON_HEIGHT + UISize::SZ4 * 2
                } else {
                    // Multiple buttons for error mode
                    // For 3 buttons: 3 * BUTTON_HEIGHT + 2 * UISize::SZ4 (spacing) + UISize::SZ4 *
                    // 2 (padding)
                    button_count as i32 * BUTTON_HEIGHT
                        + (button_count as i32 - 1) * UISize::SZ4
                        + UISize::SZ4 * 2
                };

                let available_text_height = page.bounds().height - curr_y - button_area_height - UISize::SZ10;

                let mut text_message = TextMessage::new(curr_x, curr_y, main_width, message, true);
                text_message.mut_bounds().height = available_text_height;
                page.add_component(text_message);
            }
        }

        // Add the buttons
        if show_qr {
            // Back button
            curr_y = page.bounds().height - UISize::SZ10 - BUTTON_HEIGHT;
            page.add_component(Button::new(
                curr_x,
                curr_y,
                main_width,
                Some("<"),
                Some("Back"),
                ButtonType::Primary,
                || {
                    // Disable QR code layer before switching back
                    hide_image_overlay();
                    set_curr_page(BootScreenPage::SystemError);
                },
            ));
        } else {
            // Show the buttons - laid out from the bottom up
            let button_spacing = BUTTON_HEIGHT + UISize::SZ4;
            curr_y = page.bounds().height - UISize::SZ10 - BUTTON_HEIGHT;

            page.add_component(Button::new(
                curr_x,
                curr_y,
                main_width,
                Some("p"),
                Some("Shut Down"),
                ButtonType::Secondary,
                || {
                    backlight_fade_out();
                    shutdown();
                },
            ));

            // If there is an associated QR code, then show a button to display it
            if qr_code.is_some() {
                curr_y -= button_spacing;
                page.add_component(Button::new(
                    curr_x,
                    curr_y,
                    main_width,
                    Some("L"),
                    Some("Support"),
                    // Make Support the primary if there is no separate CTA button
                    if cta_action.is_none() { ButtonType::Primary } else { ButtonType::Secondary },
                    || {
                        set_curr_page(BootScreenPage::SystemQRCode);
                    },
                ));
            }

            if cta_label.is_some() {
                curr_y -= button_spacing;

                let cta_handler = match cta_action.unwrap() {
                    CtaAction::StartKeyOS => {
                        if code == Some(SystemErrorCode::Tamper) {
                            || {
                                clear_tamper_detection();
                                set_continue_boot(true);
                            }
                        } else {
                            || {
                                set_continue_boot(true);
                            }
                        }
                    }
                    CtaAction::StartRecoveryOS => || {
                        let (bootloader_version, bootloader_build_date) = get_bootloader_version_and_date();
                        set_os_arguments(&OsArguments::RecoveryMode {
                            bootloader_version,
                            bootloader_build_date,
                        })
                        .ok();
                        select_recovery_image();
                        set_continue_boot(true);
                    },
                };

                page.add_component(Button::new(
                    curr_x,
                    curr_y,
                    main_width,
                    Some("P"),
                    cta_label,
                    ButtonType::Primary,
                    cta_handler,
                ));
            }
        }

        // Return the constructed SystemErrorPage
        Self { page }
    }
}

impl Component for SystemErrorPage {
    fn render(&self) { self.page.render(); }

    fn bounds(&self) -> &Bounds { self.page.bounds() }

    fn mut_bounds(&mut self) -> &mut Bounds { self.page.mut_bounds() }

    fn on_press(&mut self, x: i32, y: i32) { self.page.on_press(x, y); }

    fn on_release(&mut self, x: i32, y: i32) { self.page.on_release(x, y); }

    fn on_drag(&mut self, x: i32, y: i32) { self.page.on_drag(x, y); }
}
