// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Symbolicate KeyOS backtraces using `addr2line`
//!
//! Backtrace format: `Backtrace:\n  addr1 addr2 addr3\n  addr4 ...`

use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use clap::Args;
use regex::Regex;

const DEFAULT_TARGET_DIR: &str = "target/armv7a-unknown-xous-elf/release";
const KERNEL_BINARY_NAME: &str = "keyos-kernel";

#[derive(Args, Clone)]
pub struct SymbolicateArgs {
    /// Path to the ELF binary to use for symbolication
    /// If not provided, auto-detects from process name in panic log
    pub binary: Option<PathBuf>,

    /// File containing the backtrace. If not provided, reads from `stdin`
    pub backtrace: Option<PathBuf>,

    /// Path to target directory (default: target/armv7a-unknown-xous-elf/release)
    #[arg(long)]
    pub target_dir: Option<PathBuf>,

    /// Path to addr2line tool (default: arm-none-eabi-addr2line)
    #[arg(long, default_value = "arm-none-eabi-addr2line")]
    pub addr2line: String,
}

/// Extract process name from panic log text.
/// Looks for patterns like `PID=34 (`gui-server`)` or `Process 34 ... [gui-server]`
/// Returns kernel binary name for PID 1
fn extract_process_name(text: &str) -> Option<String> {
    // Check for kernel panic (PID 1)
    let pid1_re = Regex::new(r"PANIC \(PID 1\)").unwrap();
    if pid1_re.is_match(text) {
        return Some(KERNEL_BINARY_NAME.to_string());
    }

    // Pattern 1: System process PID=X (`process-name`)
    let backtick_re = Regex::new(r"PID[=\s]\d+\s*\(`([^`]+)`\)").unwrap();
    if let Some(caps) = backtick_re.captures(text) {
        return Some(caps[1].to_string());
    }

    // Pattern 2: Process X ... [process-name]
    let bracket_re = Regex::new(r"Process\s+\d+\s+[^\[]*\[([^\]]+)\]").unwrap();
    if let Some(caps) = bracket_re.captures(text) {
        return Some(caps[1].to_string());
    }

    None
}

/// Parse backtrace text and extract addresses.
fn parse_backtrace(text: &str) -> Vec<u32> {
    let mut addrs = Vec::new();
    let hex_re = Regex::new(r"\b([0-9a-fA-F]{5,8})\b").unwrap();

    for line in text.lines() {
        if !line.contains("Backtrace") && !line.contains(':') {
            continue;
        }

        for caps in hex_re.captures_iter(line) {
            if let Ok(addr) = u32::from_str_radix(&caps[1], 16) {
                // User code: 0x1000..=0x0fff_ffff, Kernel: 0xffd0_0000+
                let valid = (0x1000..=0x0fff_ffff).contains(&addr) || addr >= 0xffd0_0000;
                if valid {
                    addrs.push(addr);
                }
            }
        }
    }

    addrs
}

/// Run addr2line to symbolicate addresses.
fn symbolicate(addr2line_cmd: &str, binary: &PathBuf, addrs: &[u32]) -> Result<()> {
    if addrs.is_empty() {
        println!("No valid addresses to symbolicate");
        return Ok(());
    }

    let addr_strs: Vec<String> = addrs.iter().map(|a| format!("0x{:x}", a)).collect();

    let output = Command::new(addr2line_cmd)
        .args(["-fCe"]) // -f: function names, -C: demangle, -e: executable
        .arg(binary)
        .args(&addr_strs)
        .output()
        .with_context(|| format!("Failed to run {}", addr2line_cmd))?;

    if !output.status.success() {
        anyhow::bail!("addr2line failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    for (i, addr) in addrs.iter().enumerate() {
        let func = lines.get(i * 2).unwrap_or(&"??");
        let loc = lines.get(i * 2 + 1).unwrap_or(&"??:0");
        println!("#{:2}: 0x{:08x} -> {}", i, addr, func);
        if !loc.starts_with("??:") {
            println!("      {}", loc);
        }
    }

    Ok(())
}

/// Main entry point for symbolicate subcommand
pub fn run_symbolicate(args: SymbolicateArgs) -> Result<()> {
    let text = if let Some(ref path) = args.backtrace {
        fs::read_to_string(path)
            .with_context(|| format!("Failed to read backtrace file: {}", path.display()))?
    } else {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).context("Failed to read from stdin")?;
        buf
    };

    let addrs = parse_backtrace(&text);
    if addrs.is_empty() {
        anyhow::bail!("No addresses found in backtrace");
    }

    // Determine the binary path
    let binary = if let Some(binary) = args.binary {
        binary
    } else {
        let process_name = extract_process_name(&text)
            .context("Could not extract process name from panic log. Please specify --binary manually.")?;

        let target_dir = args.target_dir.unwrap_or_else(|| PathBuf::from(DEFAULT_TARGET_DIR));

        let binary_path = target_dir.join(&process_name);
        if !binary_path.exists() {
            anyhow::bail!(
                "Binary not found: {}. Build the project or specify --binary manually.",
                binary_path.display()
            );
        }

        println!("Auto-detected binary: {}", binary_path.display());
        binary_path
    };

    println!("Found {} frames", addrs.len());
    symbolicate(&args.addr2line, &binary, &addrs)?;

    Ok(())
}
