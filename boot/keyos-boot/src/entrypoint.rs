// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        batt::init_batt,
        boot_screen::{show_boot_screen, show_loading_screen, try_boot_menu, BootScreenPage},
        hardened_eq,
        memzero::{memzero_async, wait_for_memzero},
        progress_bar::{ProgressBar, ProgressBarMessage},
        securam::{check_reset_cause, init_securam, set_os_arguments},
        select_updated_image, selected_boot_image_kind,
        splash::{
            hide_progress_layer, hide_splash_layer, set_layers_after_boot, set_layers_for_recovery,
            show_progress_layer, show_splash_layer,
        },
        system_errors::{set_system_error, SystemErrorCode},
        verify::{
            get_bootloader_version_and_date, load_and_verify_firmware, load_os_version_info,
            VerificationResult, OS_VERSION,
        },
        BootImageKind, DEFAULT_EXTRA_ENTROPY, DISPLAY_MEMORY_START, EXTRA_ENTROPY, FIRMWARE_JUMP_ADDR,
        PROGRESS_BAR,
    },
    boot_common::{display::backlight_fade_out, i2c::init_i2c, pins::init_pins, random, shutdown},
    keyos::{PLAINTEXT_DRAM_BASE, PLAINTEXT_DRAM_END},
    securam_manager::OsArguments,
};
#[cfg(not(feature = "production"))]
use {
    atsama5d27::rstc::{ResetCause, Rstc},
    boot_common::pins::PWR_BTN,
};

pub fn entrypoint() {
    init_pins();
    init_i2c();

    #[cfg(not(feature = "production"))]
    {
        // Brick prevention for the dev builds in case of a bootloader freeze:
        // When reset by a watchdog and the power button is pressed, reboot in sam-ba mode
        if let ResetCause::Wdt = Rstc::new().reset_cause() {
            if !PWR_BTN.get() {
                boot_common::enter_sam_ba_mode();
            }
        }
    }

    boot_common::wdt_init();

    #[cfg(feature = "tamper")]
    boot_common::tamper::init_tamper_detection();
    init_batt();
    init_securam();

    memzero_async(DISPLAY_MEMORY_START..PLAINTEXT_DRAM_END);

    unsafe {
        PROGRESS_BAR = Some(ProgressBar::default());
    }

    load_os_version_info();
    // If we couldn't load keyos/app.bin (i.e. it doesn't exist), try keyos.update/app.bin
    // If that also fails, we will fail later down the line and show the proper error.
    if unsafe { OS_VERSION == None } {
        select_updated_image();
        load_os_version_info();
    }

    wait_for_memzero();
    memzero_async(PLAINTEXT_DRAM_BASE..DISPLAY_MEMORY_START);

    show_loading_screen();

    random::delay();

    #[cfg(feature = "tamper")]
    crate::securam::check_tamper_detection();

    check_reset_cause();

    // Reload the splash screen because its layer could get clobbered by the error screen
    // support page
    set_layers_for_recovery();

    // If the user is still pressing the power button, show the boot screen menu.
    try_boot_menu();
    random::delay();

    wait_for_memzero();
    loop {
        show_progress_layer();
        show_splash_layer();

        let boot_image_kind = selected_boot_image_kind();

        if let Some(pb) = unsafe { (*core::ptr::addr_of_mut!(PROGRESS_BAR)).as_mut() } {
            match boot_image_kind {
                BootImageKind::Main | BootImageKind::UpdatedMain => {
                    pb.set_message(ProgressBarMessage::VerifyingMain)
                }
                BootImageKind::Recovery => pb.set_message(ProgressBarMessage::VerifyingRecovery),
            }
        }

        let res = load_and_verify_firmware();

        if hardened_eq(res, VerificationResult::Valid) {
            set_layers_after_boot();
            set_normal_os_arguments();
            cleanup_and_jump_firmware();
            // The above function does not return.
        }

        set_system_error(SystemErrorCode::FirmwareVerification, [""]);
        hide_progress_layer();
        hide_splash_layer();

        show_boot_screen(BootScreenPage::SystemError);

        // The error screen has been dismissed, retry loading the recovery firmware
        // if that was the option selected by the user.
        let kind = selected_boot_image_kind();

        if !matches!(kind, BootImageKind::Recovery) {
            break;
        }

        load_os_version_info();
        set_layers_for_recovery();
    }

    backlight_fade_out();
    shutdown();
}

extern "C" {
    static mut _etext: u32;
}

pub(crate) fn cleanup_and_jump_firmware() -> ! {
    unsafe {
        core::arch::asm!(
            // Replace EXTRA_ENTROPY before jumping to potentially 3rd party code, to make it
            // harder to leak.
            "0:",
            "ldr {scratch}, [{entropy_src}], #4",
            "str {scratch}, [{entropy_dst}], #4",
            "subs {entropy_words}, {entropy_words}, #1",
            "bne 0b",
            // Clean the rest of the SRAM to prevent leaking any other potentially unwanted data
            // through the stack or .bss variables. This corrupts the stack of course, but we
            // jump to the firmware right after this.
            "mov {scratch}, #0",
            "1:",
            "str {scratch}, [{scrub_start}], #4",
            "cmp {scrub_start}, {scrub_end}",
            "bne 1b",
            "bx {firmware}",
            scratch = in(reg) 0usize,
            entropy_words = in(reg) 8usize,
            entropy_src = in(reg) &raw const DEFAULT_EXTRA_ENTROPY,
            entropy_dst = in(reg) &raw const EXTRA_ENTROPY,
            scrub_start = in(reg) &raw mut _etext,
            scrub_end = in(reg) 0x0022_0000usize,
            firmware = in(reg) FIRMWARE_JUMP_ADDR,
            options(noreturn),
        )
    }
}

fn set_normal_os_arguments() {
    match selected_boot_image_kind() {
        BootImageKind::Main | BootImageKind::UpdatedMain => {}
        // Recovery and System Information run the recovery image. Its version is not the running
        // main-KeyOS version, and their mode-specific arguments must remain intact.
        BootImageKind::Recovery => return,
    }

    let Some(keyos_version) = (unsafe { OS_VERSION }) else {
        // If we can't load the OS version, we can't set the OS arguments.
        return;
    };

    let (bootloader_version, _bootloader_build_date) = get_bootloader_version_and_date();
    set_os_arguments(&OsArguments::NormalMode { bootloader_version, keyos_version, _padding: [0; 3] }).ok();
}
