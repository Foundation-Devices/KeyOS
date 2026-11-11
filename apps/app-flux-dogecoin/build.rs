// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{env, path::Path};

use app_flux_build_support::{
    ar_add, base_arm_cflags, collect_icon_files, compile_nbgl_arm_objects, emit_app_link_directives,
    generate_flux_app_module, generate_ledger_glyphs, keep_app_cflags, libapp_path, prepare_ledger_app,
    prepare_ledger_sdk, replace_in_file, run_make_libapp, strip_libapp_objects, ArmToolchain, GitPin,
    LedgerAppOptions, LedgerAppSource, LedgerAppSubmodules, LedgerGlyphOptions, LedgerSdkOptions,
    BASE_STRIP_OBJS,
};

// Dogecoin is the `COIN=dogecoin` variant of the legacy Bitcoin app and its Bitcoin forks.
// On one line, where scripts/update-flux.sh reads and rewrites the pin.
#[rustfmt::skip]
const APP_SOURCE: LedgerAppSource = LedgerAppSource {
    name: "app-bitcoin-legacy",
    pin: GitPin { tag: "flex_1.6.1_2.4.12_sdk_v26.6.1", commit: "dc4dae0f70055269ae64ebecefe9aa91d8086da4" },
};

const SDK_PIN: GitPin = GitPin { tag: "v26.6.1", commit: "473fe57b98c24ce9488b1dfecd51ad92fe665d19" };

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
        LedgerAppOptions { patch_app, submodules: LedgerAppSubmodules::Init, ..Default::default() },
    );

    let mut extra_icons = Vec::new();
    collect_icon_files(&app_path.join("glyphs"), &mut extra_icons);
    generate_ledger_glyphs(
        &out_dir,
        &crate_name,
        &app_path,
        &sdk_path,
        LedgerGlyphOptions {
            app_icon: "icons/flex_app_dogecoin.png",
            extra_icons: &extra_icons,
            stub_icon_names: &[],
            stub_comment: None,
            chain_icon_stub_dir: None,
        },
    );

    let toolchain = ArmToolchain::detect(&out_dir);
    let arm_cflags = base_arm_cflags(&toolchain, &sdk_path, "Dogecoin").join(" ");
    run_make_libapp(&app_path, &sdk_path, &toolchain, &arm_cflags);

    let libapp = libapp_path(&app_path);
    // mem_alloc.o (SDK v26 lib_alloc) redefines mem_alloc/mem_free/mem_init, which
    // the Flux runtime already provides; strip it so the runtime's allocator wins.
    strip_libapp_objects(&libapp, &[BASE_STRIP_OBJS, &["mem_alloc.o"]].concat());
    ar_add(&libapp, &compile_nbgl_arm_objects(&toolchain, &sdk_path, &app_path, "Dogecoin"));

    generate_flux_app_module(&out_dir, &app_path, &libapp, &format!("{}arm-none-eabi-nm", toolchain.gccpath));
    emit_app_link_directives(&app_path);
}

fn patch_app(app_path: &Path) {
    replace_in_file(&app_path.join("Makefile"), "COIN=bitcoin_testnet_legacy", "COIN=dogecoin");

    let lib = app_path.join("lib-app-bitcoin");
    let makefile = lib.join("Makefile");
    keep_app_cflags(&makefile);
    replace_in_file(&makefile, "ENABLE_BLUETOOTH = 1", "# ENABLE_BLUETOOTH = 1");
    // Swap runs the app as a library of the exchange app, which Legacy Mode has no such thing as.
    replace_in_file(&makefile, "ENABLE_SWAP = 1", "# ENABLE_SWAP = 1");

    // The storage lives in NVM that nvm_write() mutates; on KeyOS that is RAM, and `const`
    // would put it in .rodata, where the first write faults.
    replace_in_file(
        &lib.join("context.c"),
        "storage_t const N_real;",
        "storage_t N_real /* const stripped by KeyOS */;",
    );
    replace_in_file(
        &lib.join("filesystem.h"),
        "extern storage_t const N_real;",
        "extern storage_t N_real /* const stripped by KeyOS */;",
    );
}
