// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    env,
    path::{Path, PathBuf},
};

use app_flux_build_support::{
    append_generated_glyphs_c, apply_common_hosted_includes, apply_hosted_base_flag_defines,
    apply_hosted_flag_defines, apply_hosted_io_value_defines, apply_hosted_value_defines, ar_add,
    base_arm_cflags, base_hosted_cc_build, base_hosted_skip_paths, base_hosted_source_dirs, collect_c_dirs,
    collect_c_files, collect_icon_files, compile_nbgl_arm_objects, emit_app_link_directives,
    generate_ledger_glyphs, libapp_path, prepare_ledger_app, prepare_ledger_sdk, replace_in_file,
    run_make_libapp, strip_libapp_objects, ArmToolchain, LedgerAppOptions, LedgerGlyphOptions,
    LedgerSdkOptions, BASE_HOSTED_SKIP_FILES, BASE_STRIP_OBJS,
};

const APP_NAME: &str = "app-solana";
const APP_ICON: &str = "icons/icon_solana_40px.gif";
const APP_GIT_TAG: &str = "flex_1.5.1_1.14.0_sdk_v25.11.5";

const SDK_GIT_TAG: &str = "v25.11.5";

/// Clone and patch the SDK. Returns the SDK path.
fn prepare_sdk(out_dir: &str, _manifest_dir: &str, hosted: bool) -> PathBuf {
    prepare_ledger_sdk(
        out_dir,
        SDK_GIT_TAG,
        hosted,
        LedgerSdkOptions {
            ensure_nbgl_font_data: true,
            ensure_nbgl_draw_text_override: true,
            ..Default::default()
        },
    )
}

/// Clone and patch the Solana app. Returns the app path.
fn prepare_app(out_dir: &str, _manifest_dir: &str, hosted: bool) -> PathBuf {
    prepare_ledger_app(
        out_dir,
        APP_NAME,
        APP_GIT_TAG,
        "LEDGER_APP_SOLANA_PATH",
        hosted,
        LedgerAppOptions { patch_app, hosted_inline_asm_paths: &["src/main.c"], ..Default::default() },
    )
}

/// Copy app icon and generate NBGL glyph files via icon2glyph.py.
fn generate_glyphs(out_dir: &str, crate_name: &str, app_path: &Path, sdk_path: &Path) {
    let mut extra_icons = Vec::new();
    collect_icon_files(&app_path.join("glyphs"), &mut extra_icons);
    generate_ledger_glyphs(
        out_dir,
        crate_name,
        app_path,
        sdk_path,
        LedgerGlyphOptions {
            app_icon: APP_ICON,
            extra_icons: &extra_icons,
            stub_icon_names: &["C_solana_64px", "C_Solana_64px", "C_home_solana_64px"],
            stub_comment: Some("// Stub icons for Solana app (if not generated above)"),
            chain_icon_stub_dir: None,
        },
    );
}

/// Build the C SDK for the host target (x86_64) using the `cc` crate.
fn build_hosted(out_dir: &str, manifest_dir: &str, crate_name: &str) {
    let sdk_path = prepare_sdk(out_dir, manifest_dir, true);
    let app_path = prepare_app(out_dir, manifest_dir, true);
    generate_glyphs(out_dir, crate_name, &app_path, &sdk_path);

    let skip_files = BASE_HOSTED_SKIP_FILES.to_vec();
    let skip_suffixes: &[&str] = &["_test.c"];
    let skip_paths = base_hosted_skip_paths(&sdk_path);

    let mut source_dirs = base_hosted_source_dirs(&sdk_path, &app_path);
    source_dirs.push(app_path.join("src/ui"));
    source_dirs.push(app_path.join("src/swap"));
    for dir in collect_c_dirs(&app_path.join("libsol")) {
        source_dirs.push(dir);
    }

    let mut c_files = collect_c_files(&source_dirs, &skip_files, &skip_paths, skip_suffixes);
    append_generated_glyphs_c(&mut c_files, &app_path);

    let mut build = base_hosted_cc_build();
    build.files(&c_files);
    apply_hosted_base_flag_defines(&mut build);
    apply_hosted_flag_defines(&mut build, &["HAVE_SWAP", "HAVE_LEDGER_PKI"]);

    apply_hosted_io_value_defines(&mut build);
    let value_defines: &[(&str, &str)] = &[
        ("APPNAME", "\"Solana\""),
        ("CUSTOM_IO_APDU_BUFFER_SIZE", "(255+5+64)"),
        ("APPVERSION", "\"1.14.0\""),
        ("APPVERSION_M", "1"),
        ("APPVERSION_N", "14"),
        ("APPVERSION_P", "0"),
        ("MAJOR_VERSION", "1"),
        ("MINOR_VERSION", "14"),
        ("PATCH_VERSION", "0"),
    ];
    apply_hosted_value_defines(&mut build, value_defines);

    apply_common_hosted_includes(&mut build, &sdk_path, &app_path);
    build
        .include(sdk_path.join("lib_tlv"))
        .include(sdk_path.join("lib_tlv/use_cases"))
        .include(sdk_path.join("lib_pki"))
        .include(app_path.join("src/ui"))
        .include(app_path.join("src/swap"))
        .include(app_path.join("libsol"))
        .include(app_path.join("libsol/include"))
        .include(app_path.join("libsol/include/sol"));

    build.compile("app");
}

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let out_dir = env::var("OUT_DIR").unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let crate_name = env::var("CARGO_PKG_NAME").unwrap();

    if target_os != "xous" {
        build_hosted(&out_dir, &manifest_dir, &crate_name);
        return;
    }

    // ARM (xous) build path
    let sdk_path = prepare_sdk(&out_dir, &manifest_dir, false);

    let app_path = prepare_app(&out_dir, &manifest_dir, false);
    generate_glyphs(&out_dir, &crate_name, &app_path, &sdk_path);

    let toolchain = ArmToolchain::detect(&out_dir);
    let mut arm_cflags = base_arm_cflags(&toolchain, &sdk_path, "Solana");
    arm_cflags.push(format!("-I{}", app_path.join("src/ui").display()));
    arm_cflags.push(format!("-I{}", app_path.join("src/swap").display()));
    arm_cflags.push(format!("-I{}", app_path.join("libsol").display()));
    arm_cflags.push(format!("-I{}", app_path.join("libsol/include").display()));
    arm_cflags.push(format!("-I{}", app_path.join("libsol/include/sol").display()));
    let arm_cflags = arm_cflags.join(" ");
    run_make_libapp(&app_path, &sdk_path, &toolchain, &arm_cflags);

    let libapp = libapp_path(&app_path);
    let strip_objs = [BASE_STRIP_OBJS, &["os_io.o", "usbd_ioreq.o"]].concat();
    strip_libapp_objects(&libapp, &strip_objs);

    let objs = compile_nbgl_arm_objects(&toolchain, &sdk_path, &app_path, "Solana");
    ar_add(&libapp, &objs);

    emit_app_link_directives(&app_path);
}

/// Apply all Solana app source modifications in pure Rust (replaces app-solana.patch).
fn patch_app(app_path: &Path) {
    // Disable BLE (not supported in KeyOS)
    replace_in_file(&app_path.join("Makefile"), "ENABLE_BLUETOOTH = 1", "# ENABLE_BLUETOOTH = 1");

    // Drop `const` on N_storage_real so it lands in .bss (writable). The SDK
    // expects it to live in NVM that nvm_write() can mutate; with `const` it
    // ends up in .rodata and the first nvm_write() data-aborts. Other Flux
    // emulator apps (e.g. Ethereum) don't qualify it `const` for the same reason.
    // Use surrounding context so replace_in_file's "already patched" check
    // (substring containment) doesn't false-positive after the first patch.
    replace_in_file(
        &app_path.join("src/main_application.c"),
        "G_command;\n\nconst internalStorage_t N_storage_real;",
        "G_command;\n\ninternalStorage_t N_storage_real; /* const stripped by KeyOS */",
    );
    replace_in_file(
        &app_path.join("src/globals.h"),
        "extern const internalStorage_t N_storage_real;",
        "extern internalStorage_t N_storage_real; /* const stripped by KeyOS */",
    );
}
