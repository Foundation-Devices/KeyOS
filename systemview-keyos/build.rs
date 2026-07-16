// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{env, path::PathBuf};

fn main() {
    // If target OS is not xous, abort
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target_os != "xous" {
        return;
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Create SystemView bindings
    println!("cargo:rerun-if-changed=src/wrapper.h");

    let bindings = bindgen::Builder::default()
        // prefix `cty` instead of `std` for `no_std`
        .disable_header_comment()
        .ctypes_prefix("cty")
        .use_core()
        .header("src/wrapper.h")
        .clang_arg("-Ilib/Config")
        .blocklist_item("__gnuc.*")
        .blocklist_item("__GNUC.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Unable to generate bindings");

    bindings.write_to_file(out_dir.join("bindings.rs")).expect("Couldn't write bindings!");

    // Compile SystemView
    cc::Build::new()
        .compiler("arm-none-eabi-gcc")
        .file("lib/SEGGER/SEGGER_SYSVIEW.c")
        .file("lib/SEGGER/SEGGER_RTT.c")
        .include("lib/SEGGER")
        .include("lib/Config")
        .define("CALLBACKS_OS_TIME", "")
        .define("__ARM7A__", "1")
        .define("SEGGER_RTT_SECTION", "\".rtt_cb_section\"")
        .pic(true)
        .flag("-mno-unaligned-access")
        .compile("systemview");
}
