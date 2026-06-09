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
    patch_sdk_common, prepare_ledger_app, prepare_ledger_sdk, replace_in_file, run_make_libapp,
    strip_libapp_objects, ArmToolchain, LedgerAppOptions, LedgerGlyphOptions, LedgerSdkOptions,
    BASE_HOSTED_SKIP_FILES, BASE_STRIP_OBJS,
};

const APP_NAME: &str = "app-zcash";
const APP_ICON: &str = "icons/flex_app_zcash.png";
const APP_GIT_TAG: &str = "flex_1.5.1_2.5.0_sdk_v25.11.1";

const SDK_GIT_TAG: &str = "v25.11.1";

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

    if !content.contains("keyos_trace(1503)") {
        if let Some(pos) = content.find("ux_process_ticker_event(") {
            if let Some(brace_pos) = content[pos..].find('{').map(|i| pos + i) {
                content.insert_str(brace_pos + 1, "\n    keyos_trace(1503);");
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
    // SDK v25.11.1 uses a different guard pattern
    content = content.replace(
        "#if (defined(HAVE_BOLOS) && !defined(BOLOS_OS_UPGRADER_APP))",
        "#if 0 // patched by KeyOS build.rs (was HAVE_BOLOS && !BOLOS_OS_UPGRADER_APP)",
    );

    // Remove the section attribute on font arrays — the custom section
    // ._nbgl_fonts_ gets discarded by the linker's --gc-sections.
    // Using the default .rodata section keeps the data reachable.
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

/// Apply Zcash-specific SDK source modifications.
fn patch_sdk(sdk_path: &Path) {
    patch_sdk_common(sdk_path);

    let nbgl_screen_c = sdk_path.join("lib_nbgl/src/nbgl_screen.c");
    if nbgl_screen_c.exists() {
        let mut content = fs::read_to_string(&nbgl_screen_c).unwrap();
        if !content.contains("keyos_trace(4000)") {
            ensure_keyos_trace_extern(&mut content);
            // Trace nbgl_screenPush entry
            if let Some(pos) = content.find("int nbgl_screenPush(") {
                if let Some(brace) = content[pos..].find('{').map(|i| pos + i) {
                    content.insert_str(brace + 1, "\n    keyos_trace(4000);");
                }
            }
            // Trace nbgl_screenSet entry
            if let Some(pos) = content.find("int nbgl_screenSet(") {
                if let Some(brace) = content[pos..].find('{').map(|i| pos + i) {
                    content.insert_str(brace + 1, "\n    keyos_trace(4010);");
                }
            }
            // Trace nbgl_screenRedraw
            if let Some(pos) = content.find("void nbgl_screenRedraw(") {
                if let Some(brace) = content[pos..].find('{').map(|i| pos + i) {
                    content.insert_str(brace + 1, "\n    keyos_trace(4020);");
                }
            }
            fs::write(&nbgl_screen_c, content).unwrap();
        }
    }

    // 6. Debug traces in nbgl_layout.c — nbgl_layoutGet
    let nbgl_layout_c = sdk_path.join("lib_nbgl/src/nbgl_layout.c");
    if nbgl_layout_c.exists() {
        let mut content = fs::read_to_string(&nbgl_layout_c).unwrap();
        if !content.contains("keyos_trace(4100)") {
            if let Some(pos) = content.find("nbgl_layout_t *nbgl_layoutGet(") {
                if let Some(brace) = content[pos..].find('{').map(|i| pos + i) {
                    content.insert_str(brace + 1, "\n    keyos_trace(4100);");
                }
            }
            if let Some(pos) = content.find("int nbgl_layoutDraw(") {
                if let Some(brace) = content[pos..].find('{').map(|i| pos + i) {
                    content.insert_str(brace + 1, "\n    keyos_trace(4110);");
                }
            }
            if let Some(pos) = content.find("int nbgl_layoutAddCenteredInfo(") {
                if let Some(brace) = content[pos..].find('{').map(|i| pos + i) {
                    content.insert_str(brace + 1, "\n    keyos_trace(4120);");
                }
            }
            // Traces inside addContentCenter helper
            if let Some(pos) = content.find("static nbgl_container_t *addContentCenter(") {
                if let Some(brace) = content[pos..].find('{').map(|i| pos + i) {
                    content.insert_str(brace + 1, "\n    keyos_trace(4130);");
                }
            }
            // Trace before nbgl_objPoolGet(CONTAINER) in addContentCenter
            content = content.replace(
                "container = (nbgl_container_t *) nbgl_objPoolGet(CONTAINER, layoutInt->layer);",
                "keyos_trace(4131);\n    container = (nbgl_container_t *) nbgl_objPoolGet(CONTAINER, layoutInt->layer);\n    keyos_trace(4132);",
            );
            // Trace before nbgl_containerPoolGet(6)
            content = content.replace(
                "container->children = nbgl_containerPoolGet(6, layoutInt->layer);",
                "keyos_trace(4133);\n    container->children = nbgl_containerPoolGet(6, layoutInt->layer);\n    keyos_trace(4134);",
            );
            // Trace icon path in addContentCenter
            content = content.replace(
                "if (info->icon != NULL) {",
                "keyos_trace(4135);\n    if (info->icon != NULL) {\n        keyos_trace(4136);",
            );
            content = content.replace(
                "image->buffer               = PIC(info->icon);",
                "keyos_trace(4137);\n        image->buffer               = PIC(info->icon);\n        keyos_trace(4138);",
            );
            content = content.replace(
                "fullHeight += image->buffer->height + info->iconHug;",
                "keyos_trace(4139);\n        fullHeight += image->buffer->height + info->iconHug;\n        keyos_trace(4141);",
            );
            // Trace before/after nbgl_getTextHeightInWidth
            content = content.replace(
                "textArea->obj.area.height = nbgl_getTextHeightInWidth(",
                "keyos_trace(4140);\n        textArea->obj.area.height = nbgl_getTextHeightInWidth(",
            );
            ensure_keyos_trace_extern(&mut content);
            fs::write(&nbgl_layout_c, content).unwrap();
        }
    }

    // 7. Debug traces in nbgl_objInit to find the crash point
    let nbgl_obj_c = sdk_path.join("lib_nbgl/src/nbgl_obj.c");
    if nbgl_obj_c.exists() {
        let mut content = fs::read_to_string(&nbgl_obj_c).unwrap();
        if !content.contains("keyos_trace(3000)") {
            ensure_keyos_trace_extern(&mut content);
            content = content
                .replace("void nbgl_objInit(void)\n{", "void nbgl_objInit(void)\n{\n    keyos_trace(3000);");
            content = content.replace(
                "nbgl_refreshReset();",
                "keyos_trace(3001);\n    nbgl_refreshReset();\n    keyos_trace(3002);",
            );
            content = content.replace(
                "objDrawingDisabled = false;",
                "keyos_trace(3003);\n    objDrawingDisabled = false;\n    keyos_trace(3004);",
            );
            content = content.replace(
                "nbgl_touchInit(false);",
                "keyos_trace(3005);\n    nbgl_touchInit(false);\n    keyos_trace(3006);",
            );
            fs::write(&nbgl_obj_c, content).unwrap();
        }
    }
}

/// Apply all Zcash app source modifications in pure Rust (replaces app-zcash.patch).
fn patch_app(app_path: &Path) {
    // 1. Makefile — disable BLE
    replace_in_file(&app_path.join("Makefile"), "ENABLE_BLUETOOTH = 1", "# ENABLE_BLUETOOTH = 1");

    // 2. src/main.c — rename main→c_main, comment out cpsie, add traces
    let main_c = app_path.join("src/main.c");
    if main_c.exists() {
        let mut c = fs::read_to_string(&main_c).unwrap();
        if !c.contains("c_main") {
            // Add extern declaration for keyos_trace
            c = c.replacen(
                "#include \"os.h\"",
                "#include \"os.h\"\nextern void keyos_trace(unsigned int id);",
                1,
            );
            // Comment out ARM cpsie instruction
            c = c.replace("__asm volatile(\"cpsie i\")", "/* cpsie i - patched out for KeyOS */");
            // Rename main to c_main
            c = c.replace(
                "__attribute__((section(\".boot\"))) int main(int arg0) {",
                "__attribute__((section(\".boot\"))) int c_main(int arg0) {",
            );
            // Add traces in coin_main
            c = c.replace("void coin_main(void) {", "void coin_main(void) {\n    keyos_trace(2000);");
            c = c.replace("UX_INIT();", "keyos_trace(2001);\n        UX_INIT();\n        keyos_trace(2002);");
            c = c.replace(
                "io_seproxyhal_init();",
                "keyos_trace(2003);\n                io_seproxyhal_init();\n                keyos_trace(2004);",
            );
            c = c.replace(
                "btchip_context_init();",
                "keyos_trace(2005);\n                btchip_context_init();\n                keyos_trace(2006);",
            );
            c = c.replace("USB_power(0);", "keyos_trace(2007);\n                USB_power(0);");
            c = c.replace("USB_power(1);", "USB_power(1);\n                keyos_trace(2008);");
            c = c.replace(
                "ui_idle_flow();",
                "keyos_trace(2009);\n                ui_idle_flow();\n                keyos_trace(2010);",
            );
            c = c.replace(
                "app_main();",
                "keyos_trace(2011);\n                app_main();\n                keyos_trace(2012);",
            );
            // Trace the CATCH paths
            c = c.replace(
                "CATCH(EXCEPTION_IO_RESET) {",
                "CATCH(EXCEPTION_IO_RESET) {\n                keyos_trace(2020);",
            );
            c = c.replace("CATCH_ALL {", "CATCH_ALL {\n                keyos_trace(2021);");
            // Trace app_exit
            c = c.replace("void app_exit(void) {", "void app_exit(void) {\n    keyos_trace(2030);");
            fs::write(&main_c, c).unwrap();
        }
    }
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
        LedgerSdkOptions {
            patch_sdk,
            extra_source_fixes: Some(ensure_sdk_trace_fixes),
            ..Default::default()
        },
    )
}

/// Clone and patch the Zcash app. Returns the app path.
fn prepare_app(out_dir: &str, _manifest_dir: &str, hosted: bool) -> PathBuf {
    prepare_ledger_app(
        out_dir,
        APP_NAME,
        APP_GIT_TAG,
        "LEDGER_APP_ZCASH_PATH",
        hosted,
        LedgerAppOptions { patch_app, hosted_inline_asm_paths: &["src/main.c"], ..Default::default() },
    )
}

/// Copy app icon and generate NBGL glyph files via icon2glyph.py.
fn generate_glyphs(out_dir: &str, crate_name: &str, app_path: &Path, sdk_path: &Path) {
    let mut extra_icons = Vec::new();
    for ext in &["png", "gif", "bmp"] {
        let zcash_icon = app_path.join(format!("glyphs/zcash_64px.{ext}"));
        if zcash_icon.exists() {
            extra_icons.push(zcash_icon);
            break;
        }
    }
    generate_ledger_glyphs(
        out_dir,
        crate_name,
        app_path,
        sdk_path,
        LedgerGlyphOptions {
            app_icon: APP_ICON,
            extra_icons: &extra_icons,
            stub_icon_names: &[],
            stub_comment: None,
            chain_icon_stub_dir: None,
        },
    );
}

/// Build the C SDK for the host target (x86_64) using the `cc` crate.
fn build_hosted(out_dir: &str, manifest_dir: &str, crate_name: &str) {
    let sdk_path = prepare_sdk(out_dir, manifest_dir, true);
    let app_path = prepare_app(out_dir, manifest_dir, true);
    generate_glyphs(out_dir, crate_name, &app_path, &sdk_path);

    let mut skip_files = BASE_HOSTED_SKIP_FILES.to_vec();
    skip_files.push("ui_bagl.c"); // BAGL UI code, we use NBGL
    let skip_paths = base_hosted_skip_paths(&sdk_path);

    let source_dirs = base_hosted_source_dirs(&sdk_path, &app_path);
    let mut c_files = collect_c_files(&source_dirs, &skip_files, &skip_paths, &[]);
    append_generated_glyphs_c(&mut c_files, &app_path);

    let mut build = base_hosted_cc_build();
    build.files(&c_files);
    apply_hosted_base_flag_defines(&mut build);
    apply_hosted_flag_defines(&mut build, &["HAVE_LEDGER_PKI", "HAVE_SWAP"]);

    apply_hosted_io_value_defines(&mut build);
    let value_defines: &[(&str, &str)] = &[
        ("APPNAME", "\"Zcash\""),
        ("APP_TICKER", "\"ZEC\""),
        ("BIP44_COIN_TYPE", "133"),
        ("BIP44_COIN_TYPE_2", "133"),
        ("COIN_P2PKH_VERSION", "7352"),
        ("COIN_P2SH_VERSION", "7357"),
        ("COIN_FAMILY", "1"),
        ("COIN_COINID", "\"Zcash\""),
        ("COIN_COINID_HEADER", "\"ZCASH\""),
        ("COIN_COINID_NAME", "\"Zcash\""),
        ("COIN_COINID_SHORT", "\"ZEC\""),
        ("COIN_KIND", "COIN_KIND_ZCASH"),
        ("COIN_COLOR_HDR", "0x3790CA"),
        ("COIN_COLOR_DB", "0x9BC8E5"),
        ("ICONGLYPH", "C_zcash_64px"),
        ("ICONBITMAP", "C_zcash_64px_bitmap"),
        ("ICONHOME", "C_zcash_64px"),
        ("ICON_HOME", "C_zcash_64px"),
        ("APPVERSION_M", "2"),
        ("APPVERSION_N", "5"),
        ("APPVERSION_P", "0"),
        ("APPVERSION", "\"2.5.0\""),
        ("MAJOR_VERSION", "2"),
        ("MINOR_VERSION", "5"),
        ("PATCH_VERSION", "0"),
        ("TCS_LOADER_PATCH_VERSION", "0"),
    ];
    apply_hosted_value_defines(&mut build, value_defines);

    apply_common_hosted_includes(&mut build, &sdk_path, &app_path);
    build.include(app_path.join("include"));

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
    let arm_cflags = base_arm_cflags(&toolchain, &sdk_path, "Zcash").join(" ");
    run_make_libapp(&app_path, &sdk_path, &toolchain, &arm_cflags);

    let libapp = libapp_path(&app_path);
    strip_libapp_objects(&libapp, BASE_STRIP_OBJS);
    let objs = compile_nbgl_arm_objects(&toolchain, &sdk_path, &app_path, "Zcash");
    ar_add(&libapp, &objs);

    emit_app_link_directives(&app_path);
}
