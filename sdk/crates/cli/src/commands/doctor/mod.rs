// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Doctor command - checks development environment setup

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Result;
use foundation_core::{
    AppConfig, SdkRoot, APP_CONFIG_FILE, APP_ICON_SIZE_PX, DISPLAY_APP_NAME_ALLOWED_CHARS,
};

/// Check result with status and optional fix message
struct CheckResult {
    name: String,
    passed: bool,
    status: String,
    fix: Option<String>,
}

/// Execute the doctor command
pub fn execute() -> Result<()> {
    println!("Checking development environment...");
    println!();

    let mut checks = Vec::new();

    // Check Nix installation
    checks.push(check_nix());

    // Check Nix shell is active
    checks.push(check_nix_shell());

    // Check Foundation SDK root
    checks.push(check_sdk_root());

    // Check Rust toolchain
    checks.push(check_cargo());

    // Check KeyOS target
    checks.push(check_keyos_target());

    // Check arm-none-eabi-strip
    checks.push(check_arm_strip());

    // Check cosign2
    checks.push(check_cosign2());

    // Check the raw asset conversion helper
    checks.push(check_asset_tool());

    // Check the Foundation Slint viewer (or the plain alias)
    checks.push(check_slint_viewer());

    // Check git
    checks.push(check_git());

    // Check app-config names when doctor is run from inside an app project
    checks.push(check_app_config_names());

    // Check app icon size when doctor is run from inside an app project
    checks.push(check_app_icon_size());

    // Print results
    let mut all_passed = true;
    for check in &checks {
        let status_indicator = if check.passed {
            "\x1b[32m✓\x1b[0m" // green checkmark
        } else {
            "\x1b[31m✗\x1b[0m" // red X
        };
        println!("  {} {}: {}", status_indicator, check.name, check.status);
        if !check.passed {
            all_passed = false;
            if let Some(fix) = &check.fix {
                println!("      -> {}", fix);
            }
        }
    }

    println!();

    if all_passed {
        println!("All checks passed! Your environment is ready for KeyOS development.");
    } else {
        println!("Some checks failed. Please fix the issues above before proceeding.");
    }

    Ok(())
}

fn check_nix() -> CheckResult {
    let passed = Command::new("nix").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);

    CheckResult {
        name: "Checking Nix installation".to_string(),
        passed,
        status: if passed { "OK".to_string() } else { "MISSING".to_string() },
        fix: if passed {
            None
        } else {
            Some("Install Nix: curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install".to_string())
        },
    }
}

fn check_nix_shell() -> CheckResult {
    let in_nix = std::env::var("IN_NIX_SHELL").is_ok()
        || std::env::var("NIX_BUILD_TOP").is_ok()
        || std::env::var("FOUNDATION_SDK_ROOT").is_ok();

    CheckResult {
        name: "Checking Nix shell environment".to_string(),
        passed: in_nix,
        status: if in_nix { "OK".to_string() } else { "NOT ACTIVE".to_string() },
        fix: if in_nix {
            None
        } else {
            Some("Run 'foundation develop' to enter the Nix development shell".to_string())
        },
    }
}

fn check_sdk_root() -> CheckResult {
    match SdkRoot::discover() {
        Ok(sdk) => CheckResult {
            name: "Checking Foundation SDK root".to_string(),
            passed: true,
            status: sdk.root().display().to_string(),
            fix: None,
        },
        Err(_) => CheckResult {
            name: "Checking Foundation SDK root".to_string(),
            passed: false,
            status: "NOT SET".to_string(),
            fix: Some(
                "Run from a Foundation SDK checkout or unpacked SDK bundle, or set FOUNDATION_SDK_ROOT to that location".to_string(),
            ),
        },
    }
}

fn check_cargo() -> CheckResult {
    let passed = Command::new("cargo").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);

    CheckResult {
        name: "Checking Rust toolchain".to_string(),
        passed,
        status: if passed { "OK".to_string() } else { "MISSING".to_string() },
        fix: if passed {
            None
        } else {
            Some(
                "Rust toolchain should be provided by Nix shell. Try: foundation exit && foundation develop"
                    .to_string(),
            )
        },
    }
}

fn check_keyos_target() -> CheckResult {
    // Probe the actual custom target rather than relying on rustc's built-in target list.
    let output =
        Command::new("rustc").args(["--target", "armv7a-unknown-xous-elf", "--print", "cfg"]).output();

    let passed = output.map(|o| o.status.success()).unwrap_or(false);

    CheckResult {
        name: "Checking KeyOS target".to_string(),
        passed,
        status: if passed { "armv7a-unknown-xous-elf".to_string() } else { "MISSING".to_string() },
        fix: if passed {
            None
        } else {
            Some(
                "KeyOS target should be provided by Nix shell. Try: foundation exit && foundation develop"
                    .to_string(),
            )
        },
    }
}

fn check_arm_strip() -> CheckResult {
    let passed = command_available("arm-none-eabi-strip", &["--version"]);

    CheckResult {
        name: "Checking arm-none-eabi-strip".to_string(),
        passed,
        status: if passed { "OK".to_string() } else { "MISSING".to_string() },
        fix: if passed {
            None
        } else {
            Some("arm-none-eabi-strip should be provided by Nix shell. Try: foundation exit && foundation develop".to_string())
        },
    }
}

fn check_cosign2() -> CheckResult {
    let passed = resolve_tool(&["cosign2"])
        .map(|tool| {
            Command::new(tool).arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
        })
        .unwrap_or_else(|| {
            SdkRoot::discover()
                .ok()
                .map(|sdk| {
                    sdk.keyos_root()
                        .join("imports")
                        .join("cosign2")
                        .join("cosign2-bin")
                        .join("Cargo.toml")
                        .exists()
                })
                .unwrap_or(false)
        });

    CheckResult {
        name: "Checking cosign2".to_string(),
        passed,
        status: if passed { "OK".to_string() } else { "MISSING".to_string() },
        fix: if passed {
            None
        } else {
            Some(
                "cosign2 should be provided by Nix shell. Try: foundation exit && foundation develop"
                    .to_string(),
            )
        },
    }
}

fn check_slint_viewer() -> CheckResult {
    let passed = resolve_tool(&["foundation-slint-viewer", "slint-viewer"])
        .map(|tool| {
            Command::new(tool)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|o| o.success())
                .unwrap_or(false)
        })
        .unwrap_or(false);

    CheckResult {
        name: "Checking foundation-slint-viewer".to_string(),
        passed,
        status: if passed { "OK".to_string() } else { "MISSING".to_string() },
        fix: if passed {
            None
        } else {
            Some("foundation-slint-viewer should be provided by the Foundation development shell. Try: foundation exit && foundation develop".to_string())
        },
    }
}

fn check_asset_tool() -> CheckResult {
    let passed = resolve_tool(&["foundation-asset-tool"])
        .map(|tool| {
            Command::new(tool)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|o| o.success())
                .unwrap_or(false)
        })
        .unwrap_or(false);

    CheckResult {
        name: "Checking foundation-asset-tool".to_string(),
        passed,
        status: if passed { "OK".to_string() } else { "MISSING".to_string() },
        fix: if passed {
            None
        } else {
            Some("foundation-asset-tool should be provided by the Foundation SDK bundle. Reinstall the SDK or run foundation develop.".to_string())
        },
    }
}

fn check_git() -> CheckResult {
    let passed = command_available("git", &["--version"]);

    CheckResult {
        name: "Checking git".to_string(),
        passed,
        status: if passed { "OK".to_string() } else { "MISSING".to_string() },
        fix: if passed {
            None
        } else {
            Some("Install git from your package manager or https://git-scm.com/downloads".to_string())
        },
    }
}

fn check_app_config_names() -> CheckResult {
    match std::env::current_dir() {
        Ok(cwd) => check_app_config_names_from(&cwd),
        Err(error) => CheckResult {
            name: "Checking app config names".to_string(),
            passed: false,
            status: format!("FAILED: {error}"),
            fix: None,
        },
    }
}

fn check_app_config_names_from(start: &Path) -> CheckResult {
    let Some(config_path) = find_app_config(start) else {
        return CheckResult {
            name: "Checking app config names".to_string(),
            passed: true,
            status: "not in app project".to_string(),
            fix: None,
        };
    };

    match AppConfig::load(&config_path).and_then(|config| {
        config.validate_app_names()?;
        Ok(config)
    }) {
        Ok(config) => CheckResult {
            name: "Checking app config names".to_string(),
            passed: true,
            status: format!("OK ({})", config.launcher_name()),
            fix: None,
        },
        Err(error) => CheckResult {
            name: "Checking app config names".to_string(),
            passed: false,
            status: format!("INVALID: {error}"),
            fix: Some(format!(
                "Update {}. app-name must be Cargo-safe; friendly-app-name and launcher-app-name may contain only {}.",
                config_path.display(),
                DISPLAY_APP_NAME_ALLOWED_CHARS
            )),
        },
    }
}

fn check_app_icon_size() -> CheckResult {
    match std::env::current_dir() {
        Ok(cwd) => check_app_icon_size_from(&cwd),
        Err(error) => CheckResult {
            name: "Checking app icon size".to_string(),
            passed: false,
            status: format!("FAILED: {error}"),
            fix: None,
        },
    }
}

fn check_app_icon_size_from(start: &Path) -> CheckResult {
    let Some(config_path) = find_app_config(start) else {
        return CheckResult {
            name: "Checking app icon size".to_string(),
            passed: true,
            status: "not in app project".to_string(),
            fix: None,
        };
    };

    let project_root = config_path.parent().unwrap_or(start);
    match AppConfig::load(&config_path).and_then(|config| config.validate_icon(project_root)) {
        Ok(dimensions) => CheckResult {
            name: "Checking app icon size".to_string(),
            passed: true,
            status: format!("OK ({}x{}px)", dimensions.width, dimensions.height),
            fix: None,
        },
        Err(error) => CheckResult {
            name: "Checking app icon size".to_string(),
            passed: false,
            status: format!("INVALID: {error}"),
            fix: Some(format!(
                "Update the icon referenced by {}. App icons must be {APP_ICON_SIZE_PX}x{APP_ICON_SIZE_PX}px.",
                config_path.display()
            )),
        },
    }
}

fn find_app_config(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let config_path = current.join(APP_CONFIG_FILE);
        if config_path.exists() {
            return Some(config_path);
        }
        current = current.parent()?.to_path_buf();
    }
}

fn resolve_tool(names: &[&str]) -> Option<std::path::PathBuf> {
    SdkRoot::discover()
        .ok()
        .and_then(|sdk| sdk.tool_path(names))
        .or_else(|| names.iter().find_map(|name| which(name)))
}

fn which(command: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(command);
        candidate.is_file().then_some(candidate)
    })
}

fn command_available(command: &str, args: &[&str]) -> bool {
    Command::new(command)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|o| o.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{check_app_config_names_from, check_app_icon_size_from};
    use crate::test_support::make_temp_dir;

    #[test]
    fn app_config_name_check_reports_invalid_display_name() {
        let root_dir = make_temp_dir("doctor-invalid-name");
        let root = root_dir.path();
        write_config(root, "Demo_App");

        let result = check_app_config_names_from(root);

        assert!(!result.passed);
        assert!(result.status.contains("friendly-app-name"));
        assert!(result.status.contains("only A-Z, a-z, 0-9, spaces, and hyphens"));
    }

    #[test]
    fn app_config_name_check_skips_outside_app_project() {
        let root_dir = make_temp_dir("doctor-no-app");
        let root = root_dir.path();

        let result = check_app_config_names_from(root);

        assert!(result.passed);
        assert_eq!(result.status, "not in app project");
    }

    #[test]
    fn app_icon_size_check_reports_invalid_icon_size() {
        let root_dir = make_temp_dir("doctor-invalid-icon");
        let root = root_dir.path();
        fs::create_dir_all(root.join("resources")).unwrap();
        write_config(root, "Demo App");
        fs::write(root.join("resources").join("icon.svg"), r#"<svg width="128" height="96"></svg>"#).unwrap();

        let result = check_app_icon_size_from(root);

        assert!(!result.passed);
        assert!(result.status.contains("Icon must be 96x96px"));
        assert!(result.status.contains("128x96px"));
    }

    #[test]
    fn app_icon_size_check_accepts_96px_icon() {
        let root_dir = make_temp_dir("doctor-valid-icon");
        let root = root_dir.path();
        fs::create_dir_all(root.join("resources")).unwrap();
        write_config(root, "Demo App");
        fs::write(root.join("resources").join("icon.svg"), valid_icon_svg()).unwrap();

        let result = check_app_icon_size_from(root);

        assert!(result.passed);
        assert_eq!(result.status, "OK (96x96px)");
    }

    #[test]
    fn app_icon_size_check_skips_outside_app_project() {
        let root_dir = make_temp_dir("doctor-no-icon-project");
        let root = root_dir.path();

        let result = check_app_icon_size_from(root);

        assert!(result.passed);
        assert_eq!(result.status, "not in app project");
    }

    fn write_config(root: &Path, friendly_name: &str) {
        fs::write(
            root.join("app-config.toml"),
            format!(
                r#"
                app-name = "demo-app"
                friendly-app-name = "{friendly_name}"
                description = "Demo"
                icon = "resources/icon.svg"
                app-id = "0x00112233445566778899aabbccddeeff"
                version = "0.1.0"
                min-keyos-version = "1.0.0"
                "#
            ),
        )
        .unwrap();
    }

    fn valid_icon_svg() -> &'static str { r#"<svg width="96" height="96"></svg>"# }
}
