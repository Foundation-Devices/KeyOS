// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use app_flux_build_support::{
    append_generated_glyphs_c, apply_common_hosted_includes, apply_hosted_base_flag_defines,
    apply_hosted_flag_defines, apply_hosted_io_value_defines, apply_hosted_value_defines, ar_add,
    base_arm_cflags, base_hosted_cc_build, base_hosted_skip_paths, base_hosted_source_dirs, collect_c_files,
    compile_nbgl_arm_objects, emit_app_link_directives, generate_ledger_glyphs, libapp_path,
    prepare_ledger_app, prepare_ledger_sdk, replace_in_file, run_make_libapp, strip_libapp_objects,
    ArmToolchain, LedgerAppOptions, LedgerAppSubmodules, LedgerGlyphOptions, LedgerSdkOptions,
    BASE_HOSTED_SKIP_FILES, BASE_STRIP_OBJS,
};

const APP_NAME: &str = "app-ethereum";
const APP_ICON: &str = "icons/flex_app_chain_1.gif";
const APP_GIT_TAG: &str = "flex_1.5.1_1.21.3_sdk_a60a56df284c45d136bfc53e0b862b3072aa9006";

const SDK_GIT_TAG: &str = "a60a56df284c45d136bfc53e0b862b3072aa9006";

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

/// Clone and patch the Ethereum app. Returns the app path.
fn prepare_app(out_dir: &str, _manifest_dir: &str, hosted: bool) -> PathBuf {
    prepare_ledger_app(
        out_dir,
        APP_NAME,
        APP_GIT_TAG,
        "LEDGER_APP_ETHEREUM_PATH",
        hosted,
        LedgerAppOptions {
            patch_app,
            hosted_inline_asm_paths: &["src/main.c", "ethereum-plugin-sdk/src/main.c"],
            submodules: LedgerAppSubmodules::Init,
        },
    )
}

/// Copy app icon and generate NBGL glyph files via icon2glyph.py.
fn generate_glyphs(out_dir: &str, crate_name: &str, app_path: &Path, sdk_path: &Path) {
    let extra_icons = [app_path.join("glyphs/chain_1_64px.gif")];
    generate_ledger_glyphs(
        out_dir,
        crate_name,
        app_path,
        sdk_path,
        LedgerGlyphOptions {
            app_icon: APP_ICON,
            extra_icons: &extra_icons,
            stub_icon_names: &["C_multisig_64px"],
            stub_comment: Some("// Stub chain icons (not needed for home screen rendering)"),
            chain_icon_stub_dir: Some("glyphs"),
        },
    );
}

/// Build the C SDK for the host target (x86_64) using the `cc` crate.
fn build_hosted(out_dir: &str, manifest_dir: &str, crate_name: &str) {
    let sdk_path = prepare_sdk(out_dir, manifest_dir, true);
    let app_path = prepare_app(out_dir, manifest_dir, true);
    generate_glyphs(out_dir, crate_name, &app_path, &sdk_path);

    let mut skip_files = BASE_HOSTED_SKIP_FILES.to_vec();
    skip_files.extend_from_slice(&[
        "eth_plugin_handler.c", // references ethPluginSharedRW_t removed by patch
        // eth_swap_utils.c included; HAVE_SWAP defined for type/enum access
        "network_icons.c", // needs generated net_icons.gen.h (dynamic networks not needed)
        "network_icons.h", // header for network_icons.c
    ]);
    let skip_paths = base_hosted_skip_paths(&sdk_path);

    let mut source_dirs = base_hosted_source_dirs(&sdk_path, &app_path);
    source_dirs.push(app_path.join("src/nbgl"));
    source_dirs.push(app_path.join("ethereum-plugin-sdk/src"));
    let features_dir = app_path.join("src/features");
    if let Ok(entries) = fs::read_dir(&features_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                source_dirs.push(entry.path());
            }
        }
    }

    let mut c_files = collect_c_files(&source_dirs, &skip_files, &skip_paths, &[]);
    append_generated_glyphs_c(&mut c_files, &app_path);

    let mut build = base_hosted_cc_build();
    build.files(&c_files);
    apply_hosted_base_flag_defines(&mut build);
    apply_hosted_flag_defines(
        &mut build,
        &["IS_NOT_A_PLUGIN", "HAVE_TRANSACTION_CHECKS", "HAVE_LEDGER_PKI", "HAVE_SWAP"],
    );

    apply_hosted_io_value_defines(&mut build);
    let value_defines: &[(&str, &str)] = &[
        ("APPNAME", "\"Ethereum\""),
        ("APP_TICKER", "\"ETH\""),
        ("APP_CHAIN_ID", "1"),
        ("ICONGLYPH", "C_chain_1_64px"),
        ("ICONBITMAP", "C_chain_1_64px_bitmap"),
        ("ICONHOME", "C_chain_1_64px"),
        ("APPVERSION_M", "1"),
        ("APPVERSION_N", "21"),
        ("APPVERSION_P", "3"),
        ("APPVERSION", "\"1.21.3\""),
        ("MAJOR_VERSION", "1"),
        ("MINOR_VERSION", "21"),
        ("PATCH_VERSION", "3"),
    ];
    apply_hosted_value_defines(&mut build, value_defines);

    apply_common_hosted_includes(&mut build, &sdk_path, &app_path);
    build.include(app_path.join("src/nbgl")).include(app_path.join("ethereum-plugin-sdk/src"));
    if let Ok(entries) = fs::read_dir(&features_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                build.include(entry.path());
            }
        }
    }

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

    // Remove app source files that reference types removed from plugin SDK
    // (ethPluginSharedRW_t / ethPluginSharedRO_t were removed from ethereum-plugin-sdk)
    let app_delete_files = [
        "src/eth_plugin_handler.c",
        // eth_swap_utils.c kept: swap enabled for type/enum access
        "src/plugins/eth2/eth2_plugin.c",
        "src/plugins/erc1155/erc1155_plugin.c",
        "src/plugins/erc721/erc721_plugin.c",
        "src/plugins/erc20/erc20_plugin.c",
    ];
    for f in &app_delete_files {
        let p = app_path.join(f);
        if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }

    let toolchain = ArmToolchain::detect(&out_dir);
    let arm_cflags = base_arm_cflags(&toolchain, &sdk_path, "Ethereum").join(" ");
    run_make_libapp(&app_path, &sdk_path, &toolchain, &arm_cflags);

    let libapp = libapp_path(&app_path);
    strip_libapp_objects(&libapp, BASE_STRIP_OBJS);
    let objs = compile_nbgl_arm_objects(&toolchain, &sdk_path, &app_path, "Ethereum");
    ar_add(&libapp, &objs);

    emit_app_link_directives(&app_path);
}

/// Apply all Ethereum app source modifications in pure Rust (replaces app-ethereum.patch).
/// When `hosted` is true, skip modifications that conflict with the host libc (time.h, etc.).
fn patch_app(app_path: &Path) {
    let hosted = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "xous";
    // 1. Makefile — disable BLE (swap kept enabled for type/enum access)
    replace_in_file(&app_path.join("Makefile"), "ENABLE_BLUETOOTH = 1", "# ENABLE_BLUETOOTH = 1");

    // 2. ethereum.mk — disable stack canary
    replace_in_file(
        &app_path.join("makefile_conf/chain/ethereum.mk"),
        "DEFINES += HAVE_BOLOS_APP_STACK_CANARY", // C define name — do not rename
        "# DEFINES += HAVE_BOLOS_APP_STACK_CANARY", // C define name — do not rename
    );

    // 3. features.mk — disable dynamic allocation
    replace_in_file(
        &app_path.join("makefile_conf/features.mk"),
        "ENABLE_DYNAMIC_ALLOC = 1",
        "ENABLE_DYNAMIC_ALLOC = 0",
    );

    // 4. src/main.c — add eth_main entry point, rename main→c_main
    let main_c = app_path.join("src/main.c");
    if main_c.exists() {
        let mut c = fs::read_to_string(&main_c).unwrap();
        if !c.contains("eth_main") {
            // Add eth_main before ethereum_main
            c = c.replacen(
                "int ethereum_main(eth_libargs_t *args) {",
                "void eth_main(void) {\n\
                     coin_main(NULL);\n\
                 }\n\n\
                 int ethereum_main(eth_libargs_t *args) {",
                1,
            );
            // Rename main to c_main
            c = c.replacen(
                "__attribute__((section(\".boot\"))) int main(int arg0) {",
                "__attribute__((section(\".boot\"))) int c_main(int arg0) {",
                1,
            );
            fs::write(&main_c, c).unwrap();
        }
    }

    // 4b. N_storage_real: remove `const` so the symbol lands in writable .bss
    // instead of .rodata. On the original hardware N_storage_real lives in NVM
    // (flash) and nvm_write uses a special syscall; on KeyOS we place it in
    // RAM so the Rust pre-initialization and nvm_write can write to it.
    //
    // The marker keeps replace_in_file's idempotency check from matching the
    // original declaration as if it were already patched.
    replace_in_file(
        &app_path.join("src/main.c"),
        "const internalStorage_t N_storage_real;",
        "internalStorage_t N_storage_real /* const stripped by KeyOS */;",
    );
    replace_in_file(
        &app_path.join("src/shared_context.h"),
        "extern const internalStorage_t N_storage_real;",
        "extern internalStorage_t N_storage_real;",
    );

    // 5-6. time_format.h/c — custom time_t and gmtime_r (ARM only; hosted uses system libc)
    if !hosted {
        replace_in_file(&app_path.join("src/time_format.h"), "#include <time.h>", "// #include <time.h>");
        replace_in_file(
            &app_path.join("src/time_format.h"),
            "#include <stddef.h>\n\nbool",
            "#include <stddef.h>\n\ntypedef long long time_t;\n\nbool",
        );
        replace_in_file(
            &app_path.join("src/time_format.c"),
            "#include \"time_format.h\"\n\nstatic bool get_time_struct",
            "#include \"time_format.h\"\n\n\
             struct tm {\n  int\ttm_sec;\n  int\ttm_min;\n  int\ttm_hour;\n  int\ttm_mday;\n\
               int\ttm_mon;\n  int\ttm_year;\n  int\ttm_wday;\n  int\ttm_yday;\n  int\ttm_isdst;\n};\n\n\
             static struct tm *gmtime_r(const time_t *timep, struct tm *result) {\n\
                 time_t t = *timep;\n\
                 *result = (struct tm){0};\n\
                 result->tm_sec = t % 60; t /= 60;\n\
                 result->tm_min = t % 60; t /= 60;\n\
                 result->tm_hour = t % 24; t /= 24;\n\
                 result->tm_mday = t % 365 + 1;\n\
                 result->tm_year = 70 + (t / 365);\n\
                 result->tm_wday = (t + 4) % 7;\n\
                 result->tm_yday = t % 365;\n\
                 result->tm_isdst = 0;\n\
                 return result;\n\
             }\n\n\
             static bool get_time_struct",
        );
    }

    // 7-11. Replace ctype.h includes with local stubs
    // signMessage/cmd_signMessage.c
    replace_in_file(
        &app_path.join("src/features/signMessage/cmd_signMessage.c"),
        "#include <ctype.h>",
        "// #include <ctype.h>\n\nstatic int isspace(int c) {\n    return c == ' ' || c == '\\t' || c == '\\n' || c == '\\r' || c == '\\v' || c == '\\f';\n}\n\nstatic int isprint(int c) {\n    return c >= 32 && c <= 126;\n}",
    );

    // provide_trusted_name/trusted_name.c
    replace_in_file(
        &app_path.join("src/features/provide_trusted_name/trusted_name.c"),
        "#include <ctype.h>",
        "// #include <ctype.h>\n\nstatic int isalpha(int c) {\n    return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z');\n}\n\nstatic int islower(int c) {\n    return c >= 'a' && c <= 'z';\n}\n\nstatic int isdigit(int c) {\n    return c >= '0' && c <= '9';\n}",
    );

    // utils.c
    replace_in_file(
        &app_path.join("src/utils.c"),
        "#include <ctype.h>\n",
        "// #include <ctype.h>\n\nstatic int isprint(int c) {\n    return c >= 32 && c <= 126;\n}\n",
    );

    // eth_swap_utils.c
    replace_in_file(
        &app_path.join("src/eth_swap_utils.c"),
        "#include <ctype.h>\n",
        "// #include <ctype.h>\n\nstatic int toupper(int c) {\n    return (c >= 'a' && c <= 'z') ? c - 32 : c;\n}\n",
    );

    // ui_approve_tx.c
    replace_in_file(
        &app_path.join("src/nbgl/ui_approve_tx.c"),
        "#include <ctype.h>\n",
        "// #include <ctype.h>\n\nstatic int tolower(int c) {\n    return (c >= 'A' && c <= 'Z') ? c + 32 : c;\n}\n",
    );
}
