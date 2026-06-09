// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{boxed_err, Result};

pub fn absolute_path(root: &Path, value: &Path) -> PathBuf {
    if value.is_absolute() {
        value.to_path_buf()
    } else {
        root.join(value)
    }
}

pub fn ensure_clean_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    fs::create_dir_all(path)?;
    Ok(())
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

pub fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)?;
    Ok(())
}

pub fn copy_file_if_exists(source: &Path, destination: &Path) -> Result<()> {
    if source.exists() {
        copy_file(source, destination)?;
    }
    Ok(())
}

pub fn copy_dir_all(source: &Path, destination: &Path) -> Result<()> {
    if !source.exists() {
        return Err(boxed_err(format!("copy source does not exist: {}", source.display())));
    }

    fs::create_dir_all(destination)?;

    let mut entries: Vec<_> = fs::read_dir(source)?.collect::<std::result::Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if path.is_dir() {
            copy_dir_all(&path, &destination_path)?;
        } else {
            copy_file(&path, &destination_path)?;
        }
    }

    Ok(())
}

pub fn copy_dir_contents(source: &Path, destination: &Path) -> Result<()> {
    ensure_dir(destination)?;

    let mut entries: Vec<_> = fs::read_dir(source)?.collect::<std::result::Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if path.is_dir() {
            copy_dir_all(&path, &destination_path)?;
        } else {
            copy_file(&path, &destination_path)?;
        }
    }

    Ok(())
}

pub fn run_command(command: &mut Command, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("running command: {:?}", command);
    }

    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(boxed_err(format!("command failed with status {status}: {:?}", command)))
    }
}

pub fn capture_command(command: &mut Command) -> Result<String> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(boxed_err(format!("command failed with status {}: {:?}", output.status, command)));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn command_exists(name: &str) -> bool {
    Command::new(name).arg("--version").output().map(|output| output.status.success()).unwrap_or(false)
}

pub fn sha256(path: &Path) -> Result<String> {
    if command_exists("shasum") {
        let output = capture_command(Command::new("shasum").arg("-a").arg("256").arg(path))?;
        return Ok(output.split_whitespace().next().unwrap_or_default().to_string());
    }

    if command_exists("sha256sum") {
        let output = capture_command(Command::new("sha256sum").arg(path))?;
        return Ok(output.split_whitespace().next().unwrap_or_default().to_string());
    }

    Err(boxed_err("neither shasum nor sha256sum is available"))
}

pub fn display_name(path: &Path) -> String {
    path.file_name().unwrap_or_else(|| OsStr::new("")).to_string_lossy().to_string()
}
