// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! External plugin execution

use std::path::Path;
use std::process;

/// Execute an external plugin, replacing the current process.
///
/// On Unix, refuses to exec a plugin that is world- or group-writable. This is
/// a thin sanity check, not a full sandbox — plugin install (`foundation
/// plugin install`) is still trusted upstream — but it catches the obvious
/// "anyone can drop a binary under ~/.foundation/plugins" case.
pub fn exec_plugin(path: &Path, args: &[String]) -> ! {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::process::CommandExt;

        match std::fs::metadata(path) {
            Ok(meta) => {
                let mode = meta.mode() & 0o777;
                if mode & 0o022 != 0 {
                    eprintln!(
                        "Refusing to exec plugin {}: mode {:#o} is group- or world-writable",
                        path.display(),
                        mode
                    );
                    process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("Could not stat plugin {}: {e}", path.display());
                process::exit(1);
            }
        }

        let err = process::Command::new(path).args(args).exec();
        eprintln!("Failed to exec plugin: {}", err);
        process::exit(1);
    }

    #[cfg(windows)]
    {
        let status = process::Command::new(path).args(args).status().unwrap_or_else(|e| {
            eprintln!("Failed to run plugin: {}", e);
            process::exit(1);
        });
        process::exit(status.code().unwrap_or(1));
    }
}
