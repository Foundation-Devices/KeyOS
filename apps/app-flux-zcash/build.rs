// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{env, path::Path};

use app_flux_build_support::{
    ar_add, base_arm_cflags, compile_nbgl_arm_objects, emit_app_link_directives, generate_flux_app_module,
    generate_ledger_glyphs, libapp_path, prepare_ledger_app, prepare_ledger_sdk, replace_in_file,
    run_make_libapp, strip_libapp_objects, ArmToolchain, GitPin, LedgerAppOptions, LedgerAppSource,
    LedgerGlyphOptions, LedgerSdkOptions, BASE_STRIP_OBJS,
};

const APP_ICON: &str = "icons/flex_app_zcash.png";
// The C app, archived upstream once Zcash moved to the Rust SDK in `app-zcash`.
const APP_SOURCE: LedgerAppSource = LedgerAppSource {
    name: "app-zcash-legacy",
    pin: GitPin { tag: "flex_1.5.1_2.5.0_sdk_v25.11.1", commit: "e7c488e96863a0483ad6f4454a9d12a6e8499ac3" },
};

const SDK_PIN: GitPin = GitPin { tag: "v25.11.1", commit: "8bc802b2df824e4db6d0acd064cba4142989631d" };

/// Apply all Zcash app source modifications in pure Rust (replaces app-zcash.patch).
fn patch_app(app_path: &Path) {
    // 1. Makefile — disable BLE
    replace_in_file(&app_path.join("Makefile"), "ENABLE_BLUETOOTH = 1", "# ENABLE_BLUETOOTH = 1");

    // 1b. Drop `const` on N_btchip_real so the app's NVM storage lands in writable .bss
    // instead of .rodata. On hardware it lives in flash-backed NVM; under the Flux runtime
    // it is plain RAM backed by the emulator's per-app store, so the first nvm_write() in
    // btchip_context_init() data-aborts if the region stays read-only.
    replace_in_file(
        &app_path.join("src/btchip_nvram.c"),
        "btchip_storage_t const N_btchip_real;",
        "btchip_storage_t N_btchip_real; /* const stripped by KeyOS */",
    );
    replace_in_file(
        &app_path.join("include/btchip_filesystem.h"),
        "extern btchip_storage_t const N_btchip_real;",
        "extern btchip_storage_t N_btchip_real; /* const stripped by KeyOS */",
    );

    // 2. src/main.c: rename main to c_main
    replace_in_file(
        &app_path.join("src/main.c"),
        "__attribute__((section(\".boot\"))) int main(int arg0) {",
        "__attribute__((section(\".boot\"))) int c_main(int arg0) {",
    );
}

fn main() {
    assert_eq!(env::var("CARGO_CFG_TARGET_OS").as_deref(), Ok("xous"), "Flux apps build for the device only");
    let out_dir = env::var("OUT_DIR").unwrap();
    let crate_name = env::var("CARGO_PKG_NAME").unwrap();

    let sdk_path = prepare_ledger_sdk(
        &out_dir,
        &SDK_PIN,
        false,
        LedgerSdkOptions {
            ensure_nbgl_font_data: true,
            ensure_nbgl_draw_text_override: true,
            ..Default::default()
        },
    );
    let app_path = prepare_ledger_app(
        &out_dir,
        &APP_SOURCE,
        false,
        LedgerAppOptions { patch_app, ..Default::default() },
    );
    let extra_icons: Vec<_> = ["png", "gif", "bmp"]
        .iter()
        .map(|ext| app_path.join(format!("glyphs/zcash_64px.{ext}")))
        .filter(|icon| icon.exists())
        .take(1)
        .collect();
    generate_ledger_glyphs(
        &out_dir,
        &crate_name,
        &app_path,
        &sdk_path,
        LedgerGlyphOptions {
            app_icon: APP_ICON,
            extra_icons: &extra_icons,
            stub_icon_names: &[],
            stub_comment: None,
            chain_icon_stub_dir: None,
        },
    );

    let toolchain = ArmToolchain::detect(&out_dir);
    let arm_cflags = base_arm_cflags(&toolchain, &sdk_path, "Zcash").join(" ");
    run_make_libapp(&app_path, &sdk_path, &toolchain, &arm_cflags);

    let libapp = libapp_path(&app_path);
    strip_libapp_objects(&libapp, BASE_STRIP_OBJS);
    let objs = compile_nbgl_arm_objects(&toolchain, &sdk_path, &app_path, "Zcash");
    ar_add(&libapp, &objs);

    // Expose the app's NVM region (nvm_base/NVM_LEN) to main.rs, measured from the
    // final archive so the mirrored size matches the C struct.
    generate_flux_app_module(&out_dir, &app_path, &libapp, &format!("{}arm-none-eabi-nm", toolchain.gccpath));

    emit_app_link_directives(&app_path);
}
