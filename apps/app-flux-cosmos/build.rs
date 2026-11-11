// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{env, path::Path};

use app_flux_build_support::{
    ar_add, base_arm_cflags, collect_icon_files, compile_nbgl_arm_objects, emit_app_link_directives,
    generate_flux_app_module, generate_ledger_glyphs, libapp_path, patch_zondax_app, prepare_ledger_app,
    prepare_ledger_sdk, replace_in_file, run_make_libapp, strip_libapp_objects, ArmToolchain, GitPin,
    LedgerAppOptions, LedgerAppSource, LedgerAppSubmodules, LedgerGlyphOptions, LedgerSdkOptions,
    BASE_STRIP_OBJS,
};

const APP_SOURCE: LedgerAppSource = LedgerAppSource {
    name: "app-cosmos",
    pin: GitPin { tag: "flex_1.7.0_2.39.4_sdk_v27.1.6", commit: "6ee23cbf770658e091c194a1e31db45f220d106b" },
};

const SDK_PIN: GitPin = GitPin { tag: "v27.1.6", commit: "bdaa015f7c336b2069c21a51fc53b8397145f283" };

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
        LedgerAppOptions { patch_app, submodules: LedgerAppSubmodules::Init, ..Default::default() },
    );
    // A Zondax app keeps its Makefile, sources and build output under app/.
    let app_path = repo_path.join("app");

    let mut extra_icons = Vec::new();
    collect_icon_files(&app_path.join("glyphs"), &mut extra_icons);
    generate_ledger_glyphs(
        &out_dir,
        &crate_name,
        &app_path,
        &sdk_path,
        LedgerGlyphOptions {
            app_icon: "flex_icon.gif",
            extra_icons: &extra_icons,
            stub_icon_names: &[],
            stub_comment: None,
            chain_icon_stub_dir: None,
        },
    );

    let toolchain = ArmToolchain::detect(&out_dir);
    let arm_cflags = base_arm_cflags(&toolchain, &sdk_path, "Cosmos").join(" ");
    run_make_libapp(&app_path, &sdk_path, &toolchain, &arm_cflags);

    let libapp = libapp_path(&app_path);
    // mem_alloc.o (SDK v26 lib_alloc) redefines mem_alloc/mem_free/mem_init, which
    // the Flux runtime already provides; strip it so the runtime's allocator wins.
    strip_libapp_objects(&libapp, &[BASE_STRIP_OBJS, &["mem_alloc.o"]].concat());
    ar_add(&libapp, &compile_nbgl_arm_objects(&toolchain, &sdk_path, &app_path, "Cosmos"));

    generate_flux_app_module(&out_dir, &app_path, &libapp, &format!("{}arm-none-eabi-nm", toolchain.gccpath));
    emit_app_link_directives(&app_path);
}

fn patch_app(repo_path: &Path) {
    patch_zondax_app(repo_path);

    let makefile = repo_path.join("app/Makefile");
    replace_in_file(&makefile, "ENABLE_BLUETOOTH = 1", "# ENABLE_BLUETOOTH = 1");
    // Swap runs the app as a library of the exchange app, which Legacy Mode has no such thing as.
    replace_in_file(&makefile, "    HAVE_SWAP ?= 1", "    HAVE_SWAP ?= 0 # no swap on KeyOS");

    // main() takes the library-mode argument in r0; the runtime calls a no-argument entry, so
    // give it one that starts the app as the dashboard would.
    let main_c = repo_path.join("app/src/common/main.c");
    replace_in_file(
        &main_c,
        "__attribute__((section(\".boot\"))) int main(int arg0) {",
        "int c_main(int arg0);\nvoid cosmos_main(void) { c_main(0); }\n\n\
         __attribute__((section(\".boot\"))) int c_main(int arg0) {",
    );
}
