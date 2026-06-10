// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use app_flux_build_support::{
    append_generated_glyphs_c, apply_common_hosted_includes, apply_hosted_base_flag_defines,
    apply_hosted_io_value_defines, apply_hosted_value_defines, ar_add, arm_include_flags, base_arm_cflags,
    base_hosted_cc_build, base_hosted_skip_paths, base_hosted_source_dirs, collect_c_files,
    compile_nbgl_arm_objects, emit_app_link_directives, generate_ledger_glyphs, libapp_path,
    prepare_ledger_app, prepare_ledger_sdk, replace_in_file, run_make_libapp, strip_libapp_objects,
    ArmToolchain, LedgerAppOptions, LedgerGlyphOptions, LedgerSdkOptions, BASE_HOSTED_SKIP_FILES,
    BASE_STRIP_OBJS,
};

const APP_NAME: &str = "app-monero";
const APP_ICON: &str = "icons/app_monero_40px.gif";
const APP_GIT_TAG: &str = "flex_1.5.1_2.1.3_sdk_bb50f3b1338bc8fb5d5f195088d0e4175789ec44";

const SDK_GIT_TAG: &str = "bb50f3b1338bc8fb5d5f195088d0e4175789ec44";

fn ensure_nbgl_home_traces(sdk_path: &Path) -> Result<(), String> {
    let nbgl_use_case = sdk_path.join("lib_nbgl/src/nbgl_use_case.c");
    if !nbgl_use_case.exists() {
        return Ok(());
    }

    let mut content = fs::read_to_string(&nbgl_use_case)
        .map_err(|e| format!("Failed to read {}: {e}", nbgl_use_case.display()))?;

    if !content.contains("keyos_trace(1302)") {
        content =
            content.replace("bundleNavStartHome();", "keyos_trace(1302);\n        bundleNavStartHome();");
    }

    if !content.contains("keyos_trace(1310)") {
        if let Some(pos) = content.find("static void bundleNavStartHome(void)") {
            if let Some(brace_pos) = content[pos..].find('{').map(|i| pos + i) {
                content.insert_str(brace_pos + 1, "\n    keyos_trace(1310);");
            }
        }
    }

    if !content.contains("keyos_trace(1311)") {
        content = content.replace(
            "bundleNavStartSettings,\n                   context->quitCallback);\n",
            "bundleNavStartSettings,\n                   context->quitCallback);\n    keyos_trace(1311);\n",
        );
    }

    if !content.contains("keyos_trace(1320)") {
        content = content.replace("reset_callbacks();", "reset_callbacks();\n    keyos_trace(1320);");
        content = content.replace(
            ".tuneId        = TUNE_TAP_CASUAL};",
            ".tuneId        = TUNE_TAP_CASUAL};\n    keyos_trace(1321);",
        );
        content = content
            .replace("    if (tagline == NULL) {", "    keyos_trace(1322);\n    if (tagline == NULL) {");
        content = content.replace(
            "    onContinue = topRightCallback;",
            "    keyos_trace(1323);\n    onContinue = topRightCallback;",
        );
        content = content.replace(
            "    pageContext = nbgl_pageDrawInfo(",
            "    keyos_trace(1324);\n    pageContext = nbgl_pageDrawInfo(",
        );
        content = content.replace(
            "    nbgl_refreshSpecial(FULL_COLOR_CLEAN_REFRESH);",
            "    keyos_trace(1325);\n    nbgl_refreshSpecial(FULL_COLOR_CLEAN_REFRESH);",
        );
    }
    if !content.contains("keyos_trace(1330)") {
        content = content.replace(
            "    if (tagline == NULL) {",
            "    if (tagline == NULL) {\n        keyos_trace(1330);\n        if (appName == NULL) { keyos_trace(1331); }\n        keyos_trace(1332);",
        );
        content = content.replace(
            "        if (strlen(appName) > MAX_APP_NAME_FOR_SDK_TAGLINE) {",
            "        keyos_trace(1333);\n        if (strlen(appName) > MAX_APP_NAME_FOR_SDK_TAGLINE) {\n            keyos_trace(1334);",
        );
        content = content.replace(
            "            snprintf(tmpString,",
            "            keyos_trace(1335);\n            snprintf(tmpString,",
        );
        content = content.replace(
            "            snprintf(tmpString,",
            "            keyos_trace(1336);\n            snprintf(tmpString,",
        );
        content = content.replace(
            "        if (nbgl_getTextNbLinesInWidth(SMALL_REGULAR_FONT, tmpString, AVAILABLE_WIDTH, false) > 3) {",
            "        keyos_trace(1337);\n        if (nbgl_getTextNbLinesInWidth(SMALL_REGULAR_FONT, tmpString, AVAILABLE_WIDTH, false) > 3) {\n            keyos_trace(1338);",
        );
        content = content.replace(
            "            snprintf(tmpString,",
            "            keyos_trace(1339);\n            snprintf(tmpString,",
        );
        content = content.replace(
            "        info.centeredInfo.text2 = tmpString;",
            "        keyos_trace(1340);\n        info.centeredInfo.text2 = tmpString;",
        );
    }

    // Add extern declaration for keyos_trace if traces were injected
    ensure_keyos_trace_extern(&mut content);

    fs::write(&nbgl_use_case, content)
        .map_err(|e| format!("Failed to write {}: {e}", nbgl_use_case.display()))?;
    Ok(())
}

/// Remove the C `nbgl_drawText` function from nbgl_draw.c so that the Rust
/// override in main.rs takes precedence at link time.
fn ensure_nbgl_draw_char_trace(sdk_path: &Path) -> Result<(), String> {
    let nbgl_draw = sdk_path.join("lib_nbgl/src/nbgl_draw.c");
    if !nbgl_draw.exists() {
        return Ok(());
    }

    let mut content =
        fs::read_to_string(&nbgl_draw).map_err(|e| format!("Failed to read {}: {e}", nbgl_draw.display()))?;

    if content.contains("// patched by KeyOS: nbgl_drawText removed") {
        return Ok(());
    }

    if let Some(start) = content.find("\nnbgl_font_id_e nbgl_drawText(") {
        let func_start = start + 1;
        if let Some(open_brace) = content[func_start..].find('{') {
            let mut depth = 0;
            let mut end = func_start + open_brace;
            for (i, ch) in content[func_start + open_brace..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = func_start + open_brace + i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let replacement = "// patched by KeyOS: nbgl_drawText removed (provided by Rust override)\n";
            content.replace_range(func_start..end, replacement);
        }
    }

    fs::write(&nbgl_draw, content).map_err(|e| format!("Failed to write {}: {e}", nbgl_draw.display()))?;
    Ok(())
}

/// Ensure `extern void keyos_trace(unsigned int id);` is declared before its first use.
fn ensure_keyos_trace_extern(content: &mut String) {
    if let Some(first_use) = content.find("keyos_trace(") {
        let decl = "extern void keyos_trace(unsigned int id);";
        if !content[..first_use].contains(decl) {
            if let Some(pos) = content.find("#include") {
                if let Some(line_end) = content[pos..].find('\n').map(|i| pos + i) {
                    content.insert_str(line_end + 1, &format!("\n{decl}\n"));
                } else {
                    content.insert_str(0, &format!("{decl}\n"));
                }
            } else {
                content.insert_str(0, &format!("{decl}\n"));
            }
        }
    }
}

/// Add traces to ux_process_finger_event and ux_process_ticker_event in lib_ux_nbgl/ux.c.
fn ensure_ux_touch_traces(sdk_path: &Path) -> Result<(), String> {
    let ux_c = sdk_path.join("lib_ux_nbgl/ux.c");
    if !ux_c.exists() {
        return Ok(());
    }

    let mut content =
        fs::read_to_string(&ux_c).map_err(|e| format!("Failed to read {}: {e}", ux_c.display()))?;

    if !content.contains("keyos_trace(1500)") {
        if let Some(pos) = content.find("ux_process_finger_event(") {
            if let Some(brace_pos) = content[pos..].find('{').map(|i| pos + i) {
                content.insert_str(brace_pos + 1, "\n    keyos_trace(1500);");
            }
        }
    }

    if !content.contains("keyos_trace(1501)") {
        for pat in ["ux_forward_event(true)) {", "ux_forward_event(TRUE)) {"] {
            if content.contains(pat) {
                content = content.replace(pat, &format!("{pat}\n        keyos_trace(1501);"));
                break;
            }
        }
    }

    if !content.contains("keyos_trace(1502)") {
        if let Some(handler_pos) = content.find("ux_process_finger_event(") {
            if let Some(pos) = content[handler_pos..].find("nbgl_touchHandler(").map(|i| handler_pos + i) {
                content.insert_str(pos, "keyos_trace(1502);\n        ");
            }
        }
    }

    if !content.contains("keyos_trace(1504)") {
        if let Some(pos) = content.find("touch_get_last_info(") {
            content.insert_str(pos, "keyos_trace(1504);\n    ");
        }
    }

    if !content.contains("keyos_trace(1505)") {
        if let Some(ticker_pos) = content.find("ux_process_ticker_event(") {
            if let Some(pos) = content[ticker_pos..].find("nbgl_touchHandler(").map(|i| ticker_pos + i) {
                content.insert_str(pos, "keyos_trace(1505);\n    ");
            }
        }
    }

    ensure_keyos_trace_extern(&mut content);

    fs::write(&ux_c, content).map_err(|e| format!("Failed to write {}: {e}", ux_c.display()))?;
    Ok(())
}

/// Add trace to io_event FINGER_EVENT case in lib_standard_app/io.c.
fn ensure_io_event_traces(sdk_path: &Path) -> Result<(), String> {
    let io_c = sdk_path.join("lib_standard_app/io.c");
    if !io_c.exists() {
        return Ok(());
    }

    let mut content =
        fs::read_to_string(&io_c).map_err(|e| format!("Failed to read {}: {e}", io_c.display()))?;

    if !content.contains("keyos_trace(1506)") {
        if content.contains("UX_FINGER_EVENT(G_io_seproxyhal_spi_buffer)") {
            content = content.replace(
                "UX_FINGER_EVENT(G_io_seproxyhal_spi_buffer)",
                "keyos_trace(1506);\n        UX_FINGER_EVENT(G_io_seproxyhal_spi_buffer)",
            );
        }
    }

    ensure_keyos_trace_extern(&mut content);

    fs::write(&io_c, content).map_err(|e| format!("Failed to write {}: {e}", io_c.display()))?;
    Ok(())
}

/// Add traces to nbgl_touch.c for debugging touch event handling.
fn ensure_nbgl_touch_handler_traces(sdk_path: &Path) -> Result<(), String> {
    let touch_c = sdk_path.join("lib_nbgl/src/nbgl_touch.c");
    if !touch_c.exists() {
        return Ok(());
    }

    let mut content =
        fs::read_to_string(&touch_c).map_err(|e| format!("Failed to read {}: {e}", touch_c.display()))?;

    if !content.contains("keyos_trace(1510)") {
        if let Some(pos) = content.find("void nbgl_touchHandler(") {
            if let Some(brace_pos) = content[pos..].find('{').map(|i| pos + i) {
                content.insert_str(brace_pos + 1, "\n    keyos_trace(1510);");
            }
        }
    }

    if !content.contains("keyos_trace(1514)") {
        if let Some(handler_pos) = content.find("void nbgl_touchHandler(") {
            let search = "foundObj = getTouchedObject(nbgl_screenGetTop(), touchStatePosition);";
            if let Some(pos) = content[handler_pos..].find(search).map(|i| handler_pos + i) {
                let after = pos + search.len();
                content.insert_str(
                    after,
                    "\n    if (foundObj != NULL) { keyos_trace(1514); } else { keyos_trace(1515); }",
                );
            }
        }
    }

    if !content.contains("keyos_trace(1520)") {
        let apply_fn = "static void applytouchStatePosition(nbgl_obj_t *obj, nbgl_touchType_t eventType)";
        if let Some(pos) = content.find(apply_fn) {
            if let Some(brace_pos) = content[pos..].find('{').map(|i| pos + i) {
                content.insert_str(brace_pos + 1, "\n    keyos_trace(1520);");
            }
        }
    }

    ensure_keyos_trace_extern(&mut content);

    fs::write(&touch_c, content).map_err(|e| format!("Failed to write {}: {e}", touch_c.display()))?;
    Ok(())
}

fn ensure_nbgl_font_traces(sdk_path: &Path) -> Result<(), String> {
    let nbgl_fonts = sdk_path.join("lib_nbgl/src/nbgl_fonts.c");
    if !nbgl_fonts.exists() {
        return Ok(());
    }

    let mut content = fs::read_to_string(&nbgl_fonts)
        .map_err(|e| format!("Failed to read {}: {e}", nbgl_fonts.display()))?;

    content = content.replace(
        "#if defined(BOLOS_OS_UPGRADER_APP)",
        "#if 1 // patched by KeyOS build.rs (was BOLOS_OS_UPGRADER_APP)",
    );

    // SDK v25.11.1+ uses a different guard pattern
    content = content.replace(
        "#if (defined(HAVE_BOLOS) && !defined(BOLOS_OS_UPGRADER_APP))",
        "#if 0 // patched by KeyOS build.rs (was HAVE_BOLOS && !BOLOS_OS_UPGRADER_APP)",
    );

    // Remove the section attribute on font arrays — the custom section
    // ._nbgl_fonts_ gets discarded by the linker's --gc-sections.
    content =
        content.replace("__attribute__((section(\"._nbgl_fonts_\")))", "/* section removed for KeyOS */");

    if !content.contains("keyos_trace(1400)") {
        if let Some(fn_pos) = content.find("uint16_t nbgl_getTextNbLinesInWidth(") {
            if let Some(brace_pos) = content[fn_pos..].find('{').map(|i| fn_pos + i) {
                content.insert_str(brace_pos + 1, "\n    keyos_trace(1400);");
            }
        }
        let font_line = "const nbgl_font_t *font  = nbgl_getFont(fontId);";
        if let Some(line_pos) = content.find(font_line) {
            let insert_pos = line_pos + font_line.len();
            content.insert_str(
                insert_pos,
                "\n    keyos_trace(1401);\n    if (font == NULL) { keyos_trace(1402); return 1; }",
            );
        }
    }

    if let Some(first_use) = content.find("keyos_trace(") {
        let decl = "extern void keyos_trace(unsigned int id);";
        let has_decl_before_use = content[..first_use].contains(decl);
        if !has_decl_before_use {
            if let Some(pos) = content.find("#include") {
                if let Some(line_end) = content[pos..].find('\n').map(|i| pos + i) {
                    content.insert_str(line_end + 1, "\nextern void keyos_trace(unsigned int id);\n");
                } else {
                    content.insert_str(0, "extern void keyos_trace(unsigned int id);\n");
                }
            } else {
                content.insert_str(0, "extern void keyos_trace(unsigned int id);\n");
            }
        }
    }

    fs::write(&nbgl_fonts, content).map_err(|e| format!("Failed to write {}: {e}", nbgl_fonts.display()))?;
    Ok(())
}

fn ensure_sdk_trace_fixes(sdk_path: &Path) {
    let _ = ensure_nbgl_home_traces(sdk_path);
    let _ = ensure_nbgl_font_traces(sdk_path);
    let _ = ensure_nbgl_draw_char_trace(sdk_path);
    let _ = ensure_ux_touch_traces(sdk_path);
    let _ = ensure_io_event_traces(sdk_path);
    let _ = ensure_nbgl_touch_handler_traces(sdk_path);
}

/// Clone and patch the SDK. Returns the SDK path.
fn prepare_sdk(out_dir: &str, _manifest_dir: &str, hosted: bool) -> PathBuf {
    prepare_ledger_sdk(
        out_dir,
        SDK_GIT_TAG,
        hosted,
        LedgerSdkOptions { extra_source_fixes: Some(ensure_sdk_trace_fixes), ..Default::default() },
    )
}

/// Clone and patch the Monero app. Returns the app path.
fn prepare_app(out_dir: &str, _manifest_dir: &str, hosted: bool) -> PathBuf {
    prepare_ledger_app(
        out_dir,
        APP_NAME,
        APP_GIT_TAG,
        "LEDGER_APP_MONERO_PATH",
        hosted,
        LedgerAppOptions { patch_app, hosted_inline_asm_paths: &["src/main.c"], ..Default::default() },
    )
}

/// Copy app icon and generate NBGL glyph files via icon2glyph.py.
fn generate_glyphs(out_dir: &str, crate_name: &str, app_path: &Path, sdk_path: &Path) {
    let extra_icons = [app_path.join("glyphs/Monero_64px.gif")];
    generate_ledger_glyphs(
        out_dir,
        crate_name,
        app_path,
        sdk_path,
        LedgerGlyphOptions {
            app_icon: APP_ICON,
            extra_icons: &extra_icons,
            stub_icon_names: &["C_Monero_64px", "C_monero_64px", "C_Monero_48px"],
            stub_comment: Some("// Stub icons for Monero app (if not generated above)"),
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
    let skip_paths = base_hosted_skip_paths(&sdk_path);

    let source_dirs = base_hosted_source_dirs(&sdk_path, &app_path);
    let mut c_files = collect_c_files(&source_dirs, &skip_files, &skip_paths, &[]);
    append_generated_glyphs_c(&mut c_files, &app_path);

    let mut build = base_hosted_cc_build();
    build.files(&c_files);
    apply_hosted_base_flag_defines(&mut build);

    apply_hosted_io_value_defines(&mut build);
    let value_defines: &[(&str, &str)] = &[
        ("APPNAME", "\"Monero\""),
        ("CUSTOM_IO_APDU_BUFFER_SIZE", "(255+5+64)"),
        ("MONERO_VERSION_MAJOR", "2"),
        ("MONERO_VERSION_MINOR", "1"),
        ("MONERO_VERSION_MICRO", "3"),
        ("APPVERSION", "\"2.1.3\""),
        ("MAJOR_VERSION", "2"),
        ("MINOR_VERSION", "1"),
        ("PATCH_VERSION", "3"),
    ];
    apply_hosted_value_defines(&mut build, value_defines);

    apply_common_hosted_includes(&mut build, &sdk_path, &app_path);

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
    let arm_cflags = base_arm_cflags(&toolchain, &sdk_path, "Monero").join(" ");
    run_make_libapp(&app_path, &sdk_path, &toolchain, &arm_cflags);

    let libapp = libapp_path(&app_path);
    let strip_objs = [BASE_STRIP_OBJS, &["os_io.o", "usbd_ioreq.o", "cx_stubs.o"]].concat();
    strip_libapp_objects(&libapp, &strip_objs);

    let objs = compile_nbgl_arm_objects(&toolchain, &sdk_path, &app_path, "Monero");
    ar_add(&libapp, &objs);

    let nbgl_obj_dir = app_path.join("build/flex/bin/nbgl_objs");

    // Generate cx_bn_wrappers.c — SVC_cx_call wrappers for cx_bn_* big-number
    // functions. High-level functions (cx_ecfp_*, cx_edwards_*, cx_aes_*) are
    // marked __attribute__((weak)) so Rust overrides in crypto.rs take precedence.
    let cx_bn_wrappers_src = nbgl_obj_dir.join("cx_bn_wrappers.c");
    let cx_bn_wrappers_content = r#"
#include <stdint.h>
#include <stddef.h>
typedef uint32_t cx_err_t;
typedef uint32_t cx_bn_t;
unsigned int SVC_cx_call(unsigned int syscall_id, unsigned int *parameters);

#define W(name, id, nparams, ...) \
    cx_err_t name(__VA_ARGS__); \
    cx_err_t name(__VA_ARGS__)

cx_err_t cx_bn_lock(size_t word_nbytes, uint32_t flags) {
    unsigned int p[2] = {(unsigned int)word_nbytes, (unsigned int)flags};
    return SVC_cx_call(0x02000112, p);
}
uint32_t cx_bn_unlock(void) {
    unsigned int p[2] = {0, 0};
    return (uint32_t)SVC_cx_call(0x000000b6, p);
}
uint32_t cx_bn_is_locked(void) {
    unsigned int p[2] = {0, 0};
    return (uint32_t)SVC_cx_call(0x000000b7, p);
}
cx_err_t cx_bn_alloc(cx_bn_t *x, size_t nbytes) {
    unsigned int p[2] = {(unsigned int)x, (unsigned int)nbytes};
    return SVC_cx_call(0x02000113, p);
}
cx_err_t cx_bn_alloc_init(cx_bn_t *x, size_t nbytes, const uint8_t *value, size_t value_nbytes) {
    unsigned int p[4] = {(unsigned int)x, (unsigned int)nbytes, (unsigned int)value, (unsigned int)value_nbytes};
    return SVC_cx_call(0x04000114, p);
}
cx_err_t cx_bn_destroy(cx_bn_t *x) {
    unsigned int p[2] = {(unsigned int)x, 0};
    return SVC_cx_call(0x010000bc, p);
}
cx_err_t cx_bn_nbytes(const cx_bn_t x, size_t *nbytes) {
    unsigned int p[2] = {(unsigned int)x, (unsigned int)nbytes};
    return SVC_cx_call(0x0200010d, p);
}
cx_err_t cx_bn_init(cx_bn_t x, const uint8_t *value, size_t value_nbytes) {
    unsigned int p[3] = {(unsigned int)x, (unsigned int)value, (unsigned int)value_nbytes};
    return SVC_cx_call(0x03000115, p);
}
cx_err_t cx_bn_set_u32(cx_bn_t x, uint32_t n) {
    unsigned int p[2] = {(unsigned int)x, (unsigned int)n};
    return SVC_cx_call(0x020000c1, p);
}
cx_err_t cx_bn_export(const cx_bn_t x, uint8_t *bytes, size_t nbytes) {
    unsigned int p[3] = {(unsigned int)x, (unsigned int)bytes, (unsigned int)nbytes};
    return SVC_cx_call(0x030000c3, p);
}
cx_err_t cx_bn_cmp(const cx_bn_t a, const cx_bn_t b, int *diff) {
    unsigned int p[3] = {(unsigned int)a, (unsigned int)b, (unsigned int)diff};
    return SVC_cx_call(0x030000c4, p);
}
cx_err_t cx_bn_add(cx_bn_t r, const cx_bn_t a, const cx_bn_t b) {
    unsigned int p[3] = {(unsigned int)r, (unsigned int)a, (unsigned int)b};
    return SVC_cx_call(0x03000119, p);
}
cx_err_t cx_bn_sub(cx_bn_t r, const cx_bn_t a, const cx_bn_t b) {
    unsigned int p[3] = {(unsigned int)r, (unsigned int)a, (unsigned int)b};
    return SVC_cx_call(0x0300011a, p);
}
cx_err_t cx_bn_mul(cx_bn_t r, const cx_bn_t a, const cx_bn_t b) {
    unsigned int p[3] = {(unsigned int)r, (unsigned int)a, (unsigned int)b};
    return SVC_cx_call(0x030000d2, p);
}
cx_err_t cx_bn_mod_add(cx_bn_t r, const cx_bn_t a, const cx_bn_t b, const cx_bn_t n) {
    unsigned int p[4] = {(unsigned int)r, (unsigned int)a, (unsigned int)b, (unsigned int)n};
    return SVC_cx_call(0x040000d3, p);
}
cx_err_t cx_bn_mod_sub(cx_bn_t r, const cx_bn_t a, const cx_bn_t b, const cx_bn_t n) {
    unsigned int p[4] = {(unsigned int)r, (unsigned int)a, (unsigned int)b, (unsigned int)n};
    return SVC_cx_call(0x040000d4, p);
}
cx_err_t cx_bn_mod_mul(cx_bn_t r, const cx_bn_t a, const cx_bn_t b, const cx_bn_t n) {
    unsigned int p[4] = {(unsigned int)r, (unsigned int)a, (unsigned int)b, (unsigned int)n};
    return SVC_cx_call(0x040000d5, p);
}
cx_err_t cx_bn_reduce(cx_bn_t r, const cx_bn_t d, const cx_bn_t n) {
    unsigned int p[3] = {(unsigned int)r, (unsigned int)d, (unsigned int)n};
    return SVC_cx_call(0x030000d6, p);
}
cx_err_t cx_bn_mod_pow2(cx_bn_t r, const cx_bn_t a, const uint8_t *e, uint32_t e_len, const cx_bn_t n) {
    unsigned int p[5] = {(unsigned int)r, (unsigned int)a, (unsigned int)e, (unsigned int)e_len, (unsigned int)n};
    return SVC_cx_call(0x050000ee, p);
}
cx_err_t cx_bn_mod_invert_nprime(cx_bn_t r, const cx_bn_t a, const cx_bn_t n) {
    unsigned int p[3] = {(unsigned int)r, (unsigned int)a, (unsigned int)n};
    return SVC_cx_call(0x030000da, p);
}
cx_err_t cx_bn_mod_u32_invert(cx_bn_t r, uint32_t a, cx_bn_t n) {
    unsigned int p[3] = {(unsigned int)r, (unsigned int)a, (unsigned int)n};
    return SVC_cx_call(0x03000116, p);
}
cx_err_t cx_bn_is_prime(const cx_bn_t n, _Bool *prime) {
    unsigned int p[2] = {(unsigned int)n, (unsigned int)prime};
    return SVC_cx_call(0x020000ef, p);
}
cx_err_t cx_bn_next_prime(cx_bn_t n) {
    unsigned int p[2] = {(unsigned int)n, 0};
    return SVC_cx_call(0x010000f0, p);
}

/* EC domain + point operations (used by cx_ecfp.c) */
typedef uint32_t cx_curve_t;
typedef struct { uint32_t _[4]; } cx_ecpoint_t;

/* cx_ecdomain_parameters_length + cx_ecdomain_parameter — provided by libapp.a */
cx_err_t cx_ecdomain_parameters_length(cx_curve_t cv, size_t *length);
cx_err_t cx_ecpoint_alloc(cx_ecpoint_t *P, cx_curve_t cv) {
    unsigned int p[2] = {(unsigned int)P, (unsigned int)cv};
    return SVC_cx_call(0x020000f1, p);
}
cx_err_t cx_ecpoint_destroy(cx_ecpoint_t *P) {
    unsigned int p[2] = {(unsigned int)P, 0};
    return SVC_cx_call(0x010000f2, p);
}
cx_err_t cx_ecpoint_init(cx_ecpoint_t *P, const uint8_t *x, size_t x_len, const uint8_t *y, size_t y_len) {
    unsigned int p[5] = {(unsigned int)P, (unsigned int)x, (unsigned int)x_len, (unsigned int)y, (unsigned int)y_len};
    return SVC_cx_call(0x050000f3, p);
}
cx_err_t cx_ecpoint_export(const cx_ecpoint_t *P, uint8_t *x, size_t x_len, uint8_t *y, size_t y_len) {
    unsigned int p[5] = {(unsigned int)P, (unsigned int)x, (unsigned int)x_len, (unsigned int)y, (unsigned int)y_len};
    return SVC_cx_call(0x050000f5, p);
}
cx_err_t cx_ecpoint_compress(const cx_ecpoint_t *P, uint8_t *xy, size_t xy_len, uint32_t *sign) {
    unsigned int p[4] = {(unsigned int)P, (unsigned int)xy, (unsigned int)xy_len, (unsigned int)sign};
    return SVC_cx_call(0x0400012c, p);
}
cx_err_t cx_ecpoint_decompress(cx_ecpoint_t *P, const uint8_t *xy, size_t xy_len, uint32_t sign) {
    unsigned int p[4] = {(unsigned int)P, (unsigned int)xy, (unsigned int)xy_len, (unsigned int)sign};
    return SVC_cx_call(0x0400012d, p);
}
cx_err_t cx_ecpoint_add(cx_ecpoint_t *R, const cx_ecpoint_t *P, const cx_ecpoint_t *Q) {
    unsigned int p[3] = {(unsigned int)R, (unsigned int)P, (unsigned int)Q};
    return SVC_cx_call(0x0300010e, p);
}
cx_err_t cx_ecpoint_rnd_scalarmul(cx_ecpoint_t *P, const uint8_t *k, size_t k_len) {
    unsigned int p[3] = {(unsigned int)P, (unsigned int)k, (unsigned int)k_len};
    return SVC_cx_call(0x03000127, p);
}
/* cx_ecdomain_parameter — provided by libapp.a (ecdomain.o) */

/* AES stubs — Monero uses cx_aes for key protection (monero_aes_derive).
 * We provide minimal implementations so init succeeds. */
typedef struct { uint32_t size; uint8_t keys[32]; } cx_aes_key_t;

__attribute__((weak)) cx_err_t cx_aes_init_key_no_throw(const uint8_t *raw_key, size_t key_len, cx_aes_key_t *key) {
    if (!key) return 1;
    for (size_t i = 0; i < sizeof(cx_aes_key_t); i++) ((uint8_t*)key)[i] = 0;
    if (key_len != 16 && key_len != 24 && key_len != 32) return 1;
    key->size = key_len;
    for (size_t i = 0; i < key_len; i++) key->keys[i] = raw_key[i];
    return 0;
}

__attribute__((weak)) cx_err_t cx_aes_no_throw(const cx_aes_key_t *key, uint32_t mode, const uint8_t *in, size_t in_len,
                         uint8_t *out, size_t *out_len) {
    /* Minimal stub: copy input to output (no real encryption).
     * This is sufficient for monero_aes_derive to succeed; the derived
     * "protection key" won't actually protect anything in KeyOS but the
     * init flow continues and the UI renders. */
    (void)key; (void)mode;
    if (out && in && in_len > 0) {
        for (size_t i = 0; i < in_len; i++) out[i] = in[i];
    }
    if (out_len) *out_len = in_len;
    return 0;
}

/* High-level EC operations: implemented using cx_ecpoint_* wrappers above */
#define CX_OK 0
#define CX_CHECK(call) do { error = (call); if (error != CX_OK) goto end; } while(0)

__attribute__((weak)) cx_err_t cx_ecfp_add_point_no_throw(cx_curve_t curve, uint8_t *R, const uint8_t *P, const uint8_t *Q) {
    size_t size;
    cx_ecpoint_t ecR, ecP, ecQ;
    cx_err_t error;
    CX_CHECK(cx_ecdomain_parameters_length(curve, &size));
    CX_CHECK(cx_bn_lock(size, 0));
    CX_CHECK(cx_ecpoint_alloc(&ecP, curve));
    CX_CHECK(cx_ecpoint_alloc(&ecQ, curve));
    CX_CHECK(cx_ecpoint_alloc(&ecR, curve));
    CX_CHECK(cx_ecpoint_init(&ecP, P + 1, size, P + 1 + size, size));
    CX_CHECK(cx_ecpoint_init(&ecQ, Q + 1, size, Q + 1 + size, size));
    CX_CHECK(cx_ecpoint_add(&ecR, &ecP, &ecQ));
    R[0] = 0x04;
    CX_CHECK(cx_ecpoint_export(&ecR, &R[1], size, &R[1 + size], size));
end:
    cx_bn_unlock();
    return error;
}

__attribute__((weak)) cx_err_t cx_ecfp_scalar_mult_no_throw(cx_curve_t curve, uint8_t *P, const uint8_t *k, size_t k_len) {
    size_t size;
    cx_ecpoint_t ecP;
    cx_err_t error;
    CX_CHECK(cx_ecdomain_parameters_length(curve, &size));
    CX_CHECK(cx_bn_lock(size, 0));
    CX_CHECK(cx_ecpoint_alloc(&ecP, curve));
    CX_CHECK(cx_ecpoint_init(&ecP, P + 1, size, P + 1 + size, size));
    CX_CHECK(cx_ecpoint_rnd_scalarmul(&ecP, k, k_len));
    P[0] = 0x04;
    CX_CHECK(cx_ecpoint_export(&ecP, &P[1], size, &P[1 + size], size));
end:
    cx_bn_unlock();
    return error;
}

__attribute__((weak)) cx_err_t cx_edwards_compress_point_no_throw(cx_curve_t curve, uint8_t *P, size_t P_len) {
    cx_ecpoint_t P_ec;
    cx_err_t error;
    uint32_t sign;
    size_t size;
    (void)P_len;
    CX_CHECK(cx_ecdomain_parameters_length(curve, &size));
    CX_CHECK(cx_bn_lock(size, 0));
    CX_CHECK(cx_ecpoint_alloc(&P_ec, curve));
    CX_CHECK(cx_ecpoint_init(&P_ec, P + 1, size, P + 1 + size, size));
    CX_CHECK(cx_ecpoint_compress(&P_ec, P + 1, size, &sign));
    /* Encode sign into the top bit of the last byte (Ed25519 convention) */
    if (sign) P[1 + size - 1] |= 0x80;
    /* Move compressed coordinate after prefix */
    for (size_t i = 0; i < size; i++) P[1 + size + i] = P[1 + i];
    P[0] = 0x02;
end:
    cx_bn_unlock();
    return error;
}

__attribute__((weak)) cx_err_t cx_edwards_decompress_point_no_throw(cx_curve_t curve, uint8_t *P, size_t P_len) {
    cx_ecpoint_t P_ec;
    cx_err_t error;
    uint32_t sign;
    size_t size;
    (void)P_len;
    CX_CHECK(cx_ecdomain_parameters_length(curve, &size));
    sign = P[1 + size - 1] >> 7;
    P[1 + size - 1] &= 0x7F;
    CX_CHECK(cx_bn_lock(size, 0));
    CX_CHECK(cx_ecpoint_alloc(&P_ec, curve));
    CX_CHECK(cx_ecpoint_decompress(&P_ec, P + 1, size, sign));
    P[0] = 0x04;
    CX_CHECK(cx_ecpoint_export(&P_ec, &P[1], size, &P[1 + size], size));
end:
    cx_bn_unlock();
    return error;
}
"#;
    fs::write(&cx_bn_wrappers_src, cx_bn_wrappers_content)
        .unwrap_or_else(|e| panic!("Failed to write cx_bn_wrappers.c: {e}"));

    let cx_bn_wrappers_obj = nbgl_obj_dir.join("cx_bn_wrappers.o");
    let out = Command::new(&toolchain.cc)
        .arg("--target=arm-none-eabi")
        .arg("-march=armv7-a")
        .arg("-mthumb")
        .arg("-mfloat-abi=soft")
        .arg("-fPIC")
        .arg("-fshort-enums")
        .arg("-Os")
        .arg("-c")
        .args(arm_include_flags(&toolchain))
        .arg("-o")
        .arg(&cx_bn_wrappers_obj)
        .arg(&cx_bn_wrappers_src)
        .output()
        .unwrap();
    if !out.status.success() {
        panic!("Failed to compile cx_bn_wrappers.c: {}", String::from_utf8_lossy(&out.stderr));
    }
    Command::new("ar").arg("r").arg(&libapp).arg(&cx_bn_wrappers_obj).output().ok();

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
