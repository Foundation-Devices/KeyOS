// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{env, path::Path};

use app_flux_build_support::{
    ar_add, base_arm_cflags, compile_nbgl_arm_objects, emit_app_link_directives, generate_flux_app_module,
    generate_ledger_glyphs, libapp_path, prepare_ledger_app, prepare_ledger_sdk, replace_in_file,
    run_make_libapp, strip_libapp_objects, ArmToolchain, GitPin, LedgerAppOptions, LedgerAppSource,
    LedgerGlyphOptions, LedgerSdkOptions, BASE_STRIP_OBJS,
};

const APP_ICON: &str = "icons/app_monero_40px.gif";
const APP_SOURCE: LedgerAppSource = LedgerAppSource {
    name: "app-monero",
    pin: GitPin { tag: "flex_1.6.1_2.2.1_sdk_v26.2.0", commit: "28d01ddb0f39654dad18f10f37ac34537e043aef" },
};

const SDK_PIN: GitPin = GitPin { tag: "v26.2.0", commit: "5e4446770d4d7d1ab13c77f8b7c7920cd59943ec" };

fn main() {
    assert_eq!(env::var("CARGO_CFG_TARGET_OS").as_deref(), Ok("xous"), "Flux apps build for the device only");
    let out_dir = env::var("OUT_DIR").unwrap();
    let crate_name = env::var("CARGO_PKG_NAME").unwrap();

    let sdk_path = prepare_ledger_sdk(&out_dir, &SDK_PIN, false, LedgerSdkOptions::default());
    let app_path = prepare_ledger_app(
        &out_dir,
        &APP_SOURCE,
        false,
        LedgerAppOptions { patch_app, ..Default::default() },
    );
    let extra_icons = [app_path.join("glyphs/Monero_64px.gif")];
    generate_ledger_glyphs(
        &out_dir,
        &crate_name,
        &app_path,
        &sdk_path,
        LedgerGlyphOptions {
            app_icon: APP_ICON,
            extra_icons: &extra_icons,
            stub_icon_names: &["C_Monero_64px", "C_monero_64px", "C_Monero_48px"],
            stub_comment: Some("// Stub icons for Monero app (if not generated above)"),
            chain_icon_stub_dir: None,
        },
    );

    let toolchain = ArmToolchain::detect(&out_dir);
    let arm_cflags = base_arm_cflags(&toolchain, &sdk_path, "Monero").join(" ");
    run_make_libapp(&app_path, &sdk_path, &toolchain, &arm_cflags);

    let libapp = libapp_path(&app_path);
    let strip_objs = [BASE_STRIP_OBJS, &["os_io.o", "usbd_ioreq.o", "cx_stubs.o"]].concat();
    strip_libapp_objects(&libapp, &strip_objs);

    let objs = compile_nbgl_arm_objects(&toolchain, &sdk_path, &app_path, "Monero");
    ar_add(&libapp, &objs);

    // Expose the app's NVM region (nvm_base/NVM_LEN) to main.rs, measured from the
    // final archive so the mirrored size matches the C struct.
    generate_flux_app_module(&out_dir, &app_path, &libapp, &format!("{}arm-none-eabi-nm", toolchain.gccpath));

    emit_app_link_directives(&app_path);
}

/// Apply all Monero app source modifications in pure Rust (replaces app-monero.patch).
fn patch_app(app_path: &Path) {
    // Disable BLE (not supported in KeyOS)
    replace_in_file(&app_path.join("Makefile"), "ENABLE_BLUETOOTH = 1", "# ENABLE_BLUETOOTH = 1");

    // Remove `const` from NVM state so it lands in writable .bss instead of .rodata.
    // On the original SDK OS, nvm_write() goes through an OS driver that can write to flash.
    // On KeyOS we just memcpy, so the variable must be in writable memory.
    replace_in_file(
        &app_path.join("src/monero_vars.h"),
        "extern const monero_nv_state_t N_state_pic;",
        "extern monero_nv_state_t N_state_pic; // const removed for KeyOS",
    );
    replace_in_file(
        &app_path.join("src/monero_nvram.c"),
        "const monero_nv_state_t N_state_pic;",
        "monero_nv_state_t N_state_pic; // const removed for KeyOS (nvm_write = memcpy)",
    );
}
