// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{env, path::Path};

use app_flux_build_support::{
    ar_add, base_arm_cflags, collect_icon_files, compile_nbgl_arm_objects, emit_app_link_directives,
    generate_flux_app_module, generate_ledger_glyphs, keep_app_cflags, libapp_path, prepare_ledger_app,
    prepare_ledger_sdk, replace_in_file, run_make_libapp, strip_libapp_objects, ArmToolchain, GitPin,
    LedgerAppOptions, LedgerAppSource, LedgerGlyphOptions, LedgerSdkOptions, BASE_STRIP_OBJS,
};

const APP_SOURCE: LedgerAppSource = LedgerAppSource {
    name: "app-xrp",
    pin: GitPin { tag: "flex_1.7.0_2.8.2_sdk_v27.1.1", commit: "ea1a3af8117a61d9ab9ea8b233e4ac0398c85320" },
};

const SDK_PIN: GitPin = GitPin { tag: "v27.1.1", commit: "66b3776fa288d1baf4427b346ba86a78c80ee9a1" };

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

    let mut extra_icons = Vec::new();
    collect_icon_files(&app_path.join("glyphs"), &mut extra_icons);
    generate_ledger_glyphs(
        &out_dir,
        &crate_name,
        &app_path,
        &sdk_path,
        LedgerGlyphOptions {
            app_icon: "img/flex_app_xrp.gif",
            extra_icons: &extra_icons,
            stub_icon_names: &[],
            stub_comment: None,
            chain_icon_stub_dir: None,
        },
    );

    let toolchain = ArmToolchain::detect(&out_dir);
    let arm_cflags = base_arm_cflags(&toolchain, &sdk_path, "XRP").join(" ");
    run_make_libapp(&app_path, &sdk_path, &toolchain, &arm_cflags);

    let libapp = libapp_path(&app_path);
    // mem_alloc.o (SDK v26 lib_alloc) redefines mem_alloc/mem_free/mem_init, which
    // the Flux runtime already provides; strip it so the runtime's allocator wins.
    strip_libapp_objects(&libapp, &[BASE_STRIP_OBJS, &["mem_alloc.o"]].concat());
    ar_add(&libapp, &compile_nbgl_arm_objects(&toolchain, &sdk_path, &app_path, "XRP"));

    generate_flux_app_module(&out_dir, &app_path, &libapp, &format!("{}arm-none-eabi-nm", toolchain.gccpath));
    emit_app_link_directives(&app_path);
}

fn patch_app(app_path: &Path) {
    let makefile = app_path.join("Makefile");
    keep_app_cflags(&makefile);
    replace_in_file(&makefile, "ENABLE_BLUETOOTH = 1", "# ENABLE_BLUETOOTH = 1");
    // Swap runs the app as a library of the exchange app, which Legacy Mode has no such thing as.
    replace_in_file(&makefile, "ENABLE_SWAP = 1", "# ENABLE_SWAP = 1");

    // The settings live in NVM that nvm_write() mutates; on KeyOS that is RAM, and `const`
    // would put them in .rodata, where the first write faults.
    replace_in_file(
        &app_path.join("src/app_main.c"),
        "const internal_storage_t N_storage_real;",
        "internal_storage_t N_storage_real /* const stripped by KeyOS */;",
    );
    replace_in_file(
        &app_path.join("src/globals.h"),
        "extern const internal_storage_t N_storage_real;",
        "extern internal_storage_t N_storage_real /* const stripped by KeyOS */;",
    );
}
