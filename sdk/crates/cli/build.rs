// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let src_dir = Path::new(&manifest_dir).join("tests").join("support").join("fakes");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let dest_dir = Path::new(&out_dir).join("fake-bin");

    fs::create_dir_all(&dest_dir).expect("create fake-bin dir");
    println!("cargo:rerun-if-changed={}", src_dir.display());

    for entry in fs::read_dir(&src_dir).expect("read fakes dir") {
        let path = entry.expect("dir entry").path();
        if !path.is_file() {
            continue;
        }
        let dest = dest_dir.join(path.file_name().unwrap());
        fs::copy(&path, &dest).expect("copy fake");
        println!("cargo:rerun-if-changed={}", path.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).expect("chmod fake");
        }
    }

    println!("cargo:rustc-env=FOUNDATION_FAKE_BIN={}", dest_dir.display());
}
