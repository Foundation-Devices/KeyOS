// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::process::Command;

use crate::builder::{get_package_metadata, Builder};

pub fn reload_service(crate_name: String) {
    println!("Building {} for hosted mode...", crate_name);
    Builder::hosted().build_local_crate(&crate_name);

    println!("Signalling simulator to reload {}...", crate_name);

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let socket_path = "/tmp/keyos-sim-reload.sock";
        match UnixStream::connect(socket_path) {
            Ok(mut stream) => {
                writeln!(stream, "{}", crate_name).expect("failed to write to reload socket");
                println!("Reload request sent for '{}'.", crate_name);
            }
            Err(e) => {
                eprintln!("Could not connect to reload socket at {}: {}", socket_path, e);
                eprintln!("Is the simulator running? (just sim)");
                std::process::exit(1);
            }
        }
    }

    #[cfg(not(unix))]
    {
        eprintln!("Hot-reload is only supported on Unix platforms.");
        std::process::exit(1);
    }
}

pub fn watch_service(crate_name: String) {
    let package = get_package_metadata(&crate_name);
    let crate_dir = package.manifest_path.parent().expect("manifest path has no parent");

    println!("Watching {} for changes (Ctrl-C to stop)...", crate_dir);

    let reload_cmd = format!("cargo xtask reload {}", crate_name);
    let status = Command::new("cargo")
        .args(["watch", "-w", crate_dir.as_str(), "-s", &reload_cmd])
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Failed to run `cargo watch`: {e}");
            eprintln!("Make sure it is installed: cargo install cargo-watch");
            std::process::exit(1);
        });

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}
