// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{env, path::Path};

use app_flux_build_support::{
    ar_add, base_arm_cflags, collect_icon_files, compile_nbgl_arm_objects, emit_app_link_directives,
    generate_flux_app_module, generate_ledger_glyphs, libapp_path, prepare_ledger_app, prepare_ledger_sdk,
    replace_in_file, run_make_libapp, strip_libapp_objects, ArmToolchain, GitPin, LedgerAppOptions,
    LedgerAppSource, LedgerGlyphOptions, LedgerSdkOptions, BASE_STRIP_OBJS,
};

const APP_NAME: &str = "Tezos Wallet";
// Its one submodule is the Tezos monorepo, which only the tests use, so it is not cloned.
const APP_SOURCE: LedgerAppSource = LedgerAppSource {
    name: "app-tezos",
    pin: GitPin { tag: "flex_1.7.0_3.2.5_sdk_v27.1.2", commit: "ba52cc744955104576e437ac798240a1f2738256" },
};

const SDK_PIN: GitPin = GitPin { tag: "v27.1.2", commit: "6709cd2bdfebb224ee070f7c1211b54254262579" };

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
    let repo_path = prepare_ledger_app(
        &out_dir,
        &APP_SOURCE,
        false,
        LedgerAppOptions { patch_app, ..Default::default() },
    );
    // The top-level Makefile only drives the Docker builds; the app's own is under app/.
    let app_path = repo_path.join("app");

    let mut extra_icons = Vec::new();
    collect_icon_files(&app_path.join("glyphs"), &mut extra_icons);
    generate_ledger_glyphs(
        &out_dir,
        &crate_name,
        &app_path,
        &sdk_path,
        LedgerGlyphOptions {
            app_icon: "icons/flex_tezos.gif",
            extra_icons: &extra_icons,
            stub_icon_names: &[],
            stub_comment: None,
            chain_icon_stub_dir: None,
        },
    );

    let toolchain = ArmToolchain::detect(&out_dir);
    let arm_cflags = base_arm_cflags(&toolchain, &sdk_path, APP_NAME).join(" ");
    run_make_libapp(&app_path, &sdk_path, &toolchain, &arm_cflags);

    let libapp = libapp_path(&app_path);
    // mem_alloc.o (SDK v26 lib_alloc) redefines mem_alloc/mem_free/mem_init, which
    // the Flux runtime already provides; strip it so the runtime's allocator wins.
    strip_libapp_objects(&libapp, &[BASE_STRIP_OBJS, &["mem_alloc.o"]].concat());
    ar_add(&libapp, &compile_nbgl_arm_objects(&toolchain, &sdk_path, &app_path, APP_NAME));

    generate_flux_app_module(&out_dir, &app_path, &libapp, &format!("{}arm-none-eabi-nm", toolchain.gccpath));
    emit_app_link_directives(&app_path);
}

fn patch_app(repo_path: &Path) {
    let makefile = repo_path.join("app/Makefile");
    replace_in_file(&makefile, "ENABLE_BLUETOOTH = 1", "# ENABLE_BLUETOOTH = 1");
    // Swap runs the app as a library of the exchange app, which Legacy Mode has no such thing as.
    replace_in_file(&makefile, "ENABLE_SWAP = 1", "# ENABLE_SWAP = 1");

    // The settings live in NVM that nvm_write() mutates; on KeyOS that is RAM, and `const`
    // would put them in .rodata, where the first write faults.
    replace_in_file(
        &repo_path.join("app/src/globals.c"),
        "const settings_t N_settings_real;",
        "settings_t N_settings_real /* const stripped by KeyOS */;",
    );
    replace_in_file(
        &repo_path.join("app/src/globals.h"),
        "extern const settings_t N_settings_real;",
        "extern settings_t N_settings_real /* const stripped by KeyOS */;",
    );
}
