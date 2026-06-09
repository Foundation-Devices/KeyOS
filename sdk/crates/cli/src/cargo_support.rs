// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::path::Path;
use std::process::Command;

use serde_json::Value;

pub fn emit_cargo_messages(project_root: &Path, sdk_root: &Path, stdout: &[u8]) {
    for line in String::from_utf8_lossy(stdout).lines() {
        let Some(rendered) = rendered_compiler_message(line, project_root, sdk_root) else {
            continue;
        };

        eprint!("{rendered}");
        if !rendered.ends_with('\n') {
            eprintln!();
        }
    }
}

pub fn emit_stderr_if_present(project_root: &Path, sdk_root: &Path, stderr: &[u8]) {
    let filtered = filter_cargo_stderr(&String::from_utf8_lossy(stderr), project_root, sdk_root);
    let filtered = filtered.trim();
    if !filtered.is_empty() {
        eprintln!("{filtered}");
    }
}

pub fn configure_host_build_environment(cmd: &mut Command) {
    #[cfg(target_os = "macos")]
    configure_macos_host_build_environment(cmd);
}

pub(crate) fn rendered_compiler_message(line: &str, project_root: &Path, sdk_root: &Path) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("reason")?.as_str()? != "compiler-message" {
        return None;
    }

    let message = value.get("message")?;
    let rendered = message.get("rendered")?.as_str()?.to_string();
    let level = message.get("level")?.as_str()?;
    if level == "warning" && !should_display_warning(message, project_root, sdk_root) {
        return None;
    }

    Some(rendered)
}

fn should_display_warning(message: &Value, project_root: &Path, sdk_root: &Path) -> bool {
    let Some(spans) = message.get("spans").and_then(Value::as_array) else {
        return true;
    };

    let mut saw_file_span = false;
    for span in spans {
        let Some(file_name) = span.get("file_name").and_then(Value::as_str) else {
            continue;
        };
        let span_path = Path::new(file_name);
        if !span_path.is_absolute() {
            continue;
        }
        saw_file_span = true;

        if span_path.starts_with(project_root) {
            return true;
        }

        if span_path.starts_with(sdk_root) {
            continue;
        }
    }

    !saw_file_span
}

pub(crate) fn filter_cargo_stderr(stderr: &str, project_root: &Path, sdk_root: &Path) -> String {
    let mut blocks = Vec::new();
    let mut current = Vec::new();

    for line in stderr.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                let block = current.join("\n");
                if should_keep_stderr_block(&block, project_root, sdk_root) {
                    blocks.push(block);
                }
                current.clear();
            }
            continue;
        }

        current.push(line.to_string());
    }

    if !current.is_empty() {
        let block = current.join("\n");
        if should_keep_stderr_block(&block, project_root, sdk_root) {
            blocks.push(block);
        }
    }

    blocks.join("\n\n")
}

fn should_keep_stderr_block(block: &str, project_root: &Path, sdk_root: &Path) -> bool {
    let trimmed = block.trim();
    if trimmed.is_empty() {
        return false;
    }

    if !trimmed.starts_with("warning:") {
        return true;
    }

    let project_root = project_root.display().to_string();
    if trimmed.contains(&project_root) {
        return true;
    }

    if trimmed.contains(&sdk_root.display().to_string()) {
        return false;
    }

    if trimmed.contains("/.cargo/registry/") || trimmed.contains("/.cargo/git/checkouts/") {
        return false;
    }

    if trimmed.contains(" generated ") && trimmed.contains("warning") {
        return false;
    }

    true
}

#[cfg(target_os = "macos")]
fn configure_macos_host_build_environment(cmd: &mut Command) {
    cmd.env_remove("DEVELOPER_DIR");
    cmd.env_remove("SDKROOT");

    if let Some(developer_dir) = macos_command_output("/usr/bin/xcode-select", &["-p"]) {
        cmd.env("DEVELOPER_DIR", developer_dir);
    }

    if let Some(sdk_root) = macos_command_output("/usr/bin/xcrun", &["--sdk", "macosx", "--show-sdk-path"]) {
        cmd.env("SDKROOT", sdk_root);
    }
}

#[cfg(target_os = "macos")]
fn macos_command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::filter_cargo_stderr;

    #[test]
    fn filters_sdk_warning_blocks() {
        let project_root = Path::new("/tmp/project");
        let sdk_root = Path::new("/tmp/sdk");
        let stderr = br#"warning: unused import: `foo`
 --> /tmp/sdk/lib/slint/example.rs:1:5

warning: dead code
 --> /tmp/project/src/main.rs:1:1
"#;

        let filtered = filter_cargo_stderr(&String::from_utf8_lossy(stderr), project_root, sdk_root);
        assert!(!filtered.contains("/tmp/sdk/lib/slint/example.rs"));
        assert!(filtered.contains("/tmp/project/src/main.rs"));
    }
}
