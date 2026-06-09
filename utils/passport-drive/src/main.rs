// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! passport-drive — Rust CLI to interact with Passport Prime over USB.
//!
//! Uses raw USB bulk endpoints via `rusb` for all device interaction
//! (screenshot, tap, logs, kernel debug commands).

mod hid;
mod mcp;
pub(crate) mod screenshot;

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use usb_debug_protocol::{Command, LaunchAppResult, LaunchAppStatus, TouchKind, UsbDebugClient};

// Screen / framebuffer constants (pub(crate) so mcp module can use them)
pub(crate) const SCREEN_WIDTH: u32 = 480;
pub(crate) const SCREEN_HEIGHT: u32 = 800;
pub(crate) const FB_SIZE: usize = (SCREEN_WIDTH * SCREEN_HEIGHT * 4) as usize;

// Log protocol constant
const LOG_TERMINATOR: u8 = 0x1E;

#[derive(Parser)]
#[command(name = "passport-drive", about = "Drive Passport Prime over USB")]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Take a screenshot and save as PNG
    Screenshot {
        /// Output PNG path
        #[arg(short, long, default_value = "/tmp/passport-screen.png")]
        output: PathBuf,
    },
    /// Tap at (x, y) coordinates
    Tap { x: u16, y: u16 },
    /// Swipe from (sx,sy) to (ex,ey)
    Swipe {
        sx: u16,
        sy: u16,
        ex: u16,
        ey: u16,
        /// Number of drag steps
        #[arg(short, long, default_value = "15")]
        steps: u16,
    },
    /// Press power button (press + release)
    Power,
    /// Tap, wait, then screenshot
    TapScreenshot {
        x: u16,
        y: u16,
        /// Output PNG path
        #[arg(short, long, default_value = "/tmp/passport-screen.png")]
        output: PathBuf,
        /// Wait time in ms after tap before screenshot
        #[arg(short, long, default_value = "800")]
        wait: u64,
    },
    /// Swipe, wait, then screenshot
    SwipeScreenshot {
        sx: u16,
        sy: u16,
        ex: u16,
        ey: u16,
        /// Output PNG path
        #[arg(short, long, default_value = "/tmp/passport-screen.png")]
        output: PathBuf,
        /// Wait time in ms after swipe before screenshot
        #[arg(short, long, default_value = "1000")]
        wait: u64,
    },
    /// Run a sequence of actions from a JSON file
    Run {
        /// Path to JSON actions file
        file: PathBuf,
    },
    /// Stream device logs to stdout (uses USB vendor interface)
    Logs {
        /// Maximum number of lines to print (0 = unlimited)
        #[arg(short = 'n', long, default_value = "0")]
        max_lines: usize,
        /// Filter: only print lines containing this substring
        #[arg(short, long)]
        filter: Option<String>,
        /// Include stale/boot log data (don't drain on open)
        #[arg(long)]
        include_stale: bool,
    },
    /// Type text into the focused input field on the device
    InputText {
        /// Text to type
        text: String,
    },
    /// Launch a Flux app by its 16-byte hex app ID
    LaunchApp {
        /// 32-character hex app ID (with optional 0x prefix)
        app_id: String,
    },
    /// Close/kill an app by PID (uses gui-server graceful close)
    CloseApp {
        /// Process ID to close
        pid: u16,
    },
    /// Reboot device into SAM-BA mode
    RebootSamba,
    /// Print the KeyOS version string (same as Settings → About → KeyOS)
    GetVersion,
    /// Start MCP server mode (JSON-RPC over stdio for AI integration)
    Mcp,
    /// SAM-BA bootloader commands (device must be in SAM-BA mode)
    #[command(subcommand)]
    Samba(SambaCommand),
}

#[derive(Subcommand)]
enum SambaCommand {
    /// Show SAM-BA monitor version
    Version,
    /// Read a 32-bit word from a memory address
    ReadU32 {
        #[arg(value_parser = parse_hex_u32)]
        address: u32,
    },
    /// Write a 32-bit word to a memory address
    WriteU32 {
        #[arg(value_parser = parse_hex_u32)]
        address: u32,
        #[arg(value_parser = parse_hex_u32)]
        value: u32,
    },
    /// Flash a boot image to the device
    Flash {
        image: PathBuf,
        #[arg(short, long)]
        boot: bool,
        #[arg(short, long)]
        system: bool,
        #[arg(long)]
        no_verify: bool,
    },
    /// Dump flash contents to a file
    DumpFlash {
        #[arg(short, long, default_value = "flash_dump.bin")]
        output: PathBuf,
        #[arg(short = 'n', long, default_value = "8")]
        megabytes: usize,
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// Reboot device from SAM-BA mode into normal mode
    Reboot,
}

fn parse_hex_u32(s: &str) -> std::result::Result<u32, String> {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u32::from_str_radix(s, 16).map_err(|e| format!("Invalid hex value: {e}"))
}

// USB transport helpers

fn open_usb() -> Result<UsbDebugClient> { UsbDebugClient::open().context("Failed to open USB device") }

fn tap(client: &UsbDebugClient, x: u16, y: u16, kind: TouchKind) -> Result<()> {
    client.send(Command::Tap { x, y, kind }, Duration::from_secs(5))?;
    Ok(())
}

fn save_screenshot_png(payload: &[u8], output: &PathBuf) -> Result<()> {
    let png_data = screenshot::bgra_to_png(payload).map_err(|e| anyhow::anyhow!(e))?;
    std::fs::write(output, &png_data).with_context(|| format!("Cannot create {}", output.display()))?;
    eprintln!("Saved {}", output.display());
    Ok(())
}

// Higher-level actions (USB transport)

fn do_screenshot(client: &UsbDebugClient, output: &PathBuf) -> Result<()> {
    eprintln!("Taking screenshot...");
    let payload = client.send(Command::Screenshot, Duration::from_secs(20))?;
    save_screenshot_png(&payload, output)
}

fn do_tap(client: &UsbDebugClient, x: u16, y: u16) -> Result<()> {
    eprintln!("Tap ({x}, {y}) press...");
    tap(client, x, y, TouchKind::Press)?;
    std::thread::sleep(Duration::from_millis(50));
    eprintln!("Tap ({x}, {y}) release...");
    tap(client, x, y, TouchKind::Release)?;
    eprintln!("Tap OK");
    Ok(())
}

fn do_swipe(client: &UsbDebugClient, sx: u16, sy: u16, ex: u16, ey: u16, steps: u16) -> Result<()> {
    eprintln!("Swipe ({sx},{sy}) -> ({ex},{ey})");
    tap(client, sx, sy, TouchKind::Press)?;
    for i in 1..=steps {
        let x = sx as f32 + (ex as f32 - sx as f32) * i as f32 / steps as f32;
        let y = sy as f32 + (ey as f32 - sy as f32) * i as f32 / steps as f32;
        std::thread::sleep(Duration::from_millis(20));
        tap(client, x as u16, y as u16, TouchKind::Drag)?;
    }
    tap(client, ex, ey, TouchKind::Release)?;
    eprintln!("Swipe OK");
    Ok(())
}

fn do_input_text(client: &UsbDebugClient, text: &str) -> Result<()> {
    eprintln!("Typing {} chars...", text.chars().count());
    client.send(Command::InputText(text.to_string()), Duration::from_secs(10))?;
    eprintln!("Input text OK");
    Ok(())
}

fn do_get_version(client: &UsbDebugClient) -> Result<()> {
    let payload = client.send(Command::GetVersion, Duration::from_secs(5))?;
    let version = String::from_utf8_lossy(&payload);
    println!("{version}");
    Ok(())
}

fn do_power(client: &UsbDebugClient) -> Result<()> {
    eprintln!("Power button press...");
    client.send(Command::PowerButton { pressed: true }, Duration::from_secs(5))?;
    std::thread::sleep(Duration::from_millis(200));
    eprintln!("Power button release...");
    client.send(Command::PowerButton { pressed: false }, Duration::from_secs(5))?;
    eprintln!("Power OK");
    Ok(())
}

// Log streaming (USB vendor interface)

fn do_logs_usb(client: &UsbDebugClient, max_lines: usize, filter: Option<&str>) -> Result<()> {
    let mut printed: usize = 0;
    let mut line_buf: Vec<u8> = Vec::with_capacity(4096);

    loop {
        let data = match client.read_logs(Duration::from_secs(5)) {
            Ok(d) => d,
            Err(_) => continue,
        };

        for &b in &data {
            if b == LOG_TERMINATOR {
                let text = String::from_utf8_lossy(&line_buf);
                let text = text.trim_end();
                if !text.is_empty() {
                    let show = match filter {
                        Some(pat) => text.contains(pat),
                        None => true,
                    };
                    if show {
                        println!("{text}");
                        printed += 1;
                        if max_lines > 0 && printed >= max_lines {
                            return Ok(());
                        }
                    }
                }
                line_buf.clear();
            } else {
                line_buf.push(b);
                if line_buf.len() > 16384 {
                    line_buf.drain(..line_buf.len() - 4096);
                }
            }
        }
    }
}

// JSON action format for `run`

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Tap([u16; 2]),
    Swipe([u16; 4]),
    Screenshot(String),
    Wait(u64),
    Power(bool),
    InputText(String),
}

fn run_actions(client: &UsbDebugClient, actions: &[Action]) -> Result<()> {
    for (i, action) in actions.iter().enumerate() {
        match action {
            Action::Tap([x, y]) => {
                eprintln!("[{i}] tap ({x}, {y})");
                do_tap(client, *x, *y)?;
            }
            Action::Swipe([sx, sy, ex, ey]) => {
                eprintln!("[{i}] swipe ({sx},{sy}) -> ({ex},{ey})");
                do_swipe(client, *sx, *sy, *ex, *ey, 15)?;
            }
            Action::Screenshot(path) => {
                eprintln!("[{i}] screenshot -> {path}");
                do_screenshot(client, &PathBuf::from(path))?;
            }
            Action::Wait(ms) => {
                eprintln!("[{i}] wait {ms}ms");
                std::thread::sleep(Duration::from_millis(*ms));
            }
            Action::Power(pressed) => {
                eprintln!("[{i}] power pressed={pressed}");
                client.send(Command::PowerButton { pressed: *pressed }, Duration::from_secs(5))?;
            }
            Action::InputText(text) => {
                eprintln!("[{i}] input_text ({} chars)", text.chars().count());
                do_input_text(client, text)?;
            }
        }
    }
    Ok(())
}

// SAM-BA helpers

fn wait_for_samba() -> Result<sambuca::Sambuca> {
    eprint!("Waiting for SAM-BA USB device...");
    let s = sambuca::flash::wait_for_device(Duration::from_secs(30))?;
    eprintln!(" connected.");
    Ok(s)
}

fn do_samba_flash(image: &PathBuf, boot_only: bool, system_only: bool, no_verify: bool) -> Result<()> {
    let raw = std::fs::read(image).with_context(|| format!("Cannot read {}", image.display()))?;

    // Pad to sector alignment.
    let mut img = raw;
    let target_len = img.len().next_multiple_of(sambuca::flash::SECTOR_SIZE);
    img.resize(target_len, 0);

    // Compute data slice and offset based on partition flags.
    /// System partition start sector for Passport Prime.
    const SYSTEM_PARTITION_START_SECTOR: usize = 2048;
    let system_start = sambuca::flash::SECTOR_SIZE * SYSTEM_PARTITION_START_SECTOR;
    let (data, offset) = if boot_only ^ system_only {
        if boot_only {
            let boot_end = img[..system_start]
                .iter()
                .rposition(|b| *b != 0)
                .unwrap_or(0)
                .saturating_add(1)
                .next_multiple_of(sambuca::flash::SECTOR_SIZE);
            (img[..boot_end].to_vec(), 0u64)
        } else {
            (img[system_start..].to_vec(), system_start as u64)
        }
    } else {
        (img, 0u64)
    };

    let mut sambuca = wait_for_samba()?;
    eprintln!("SAM-BA version: {}", sambuca.version().context("reading SAM-BA version")?);

    let start = Instant::now();
    sambuca.flash_image(&data, offset, !no_verify, |p| match p {
        sambuca::flash::FlashProgress::Writing { percent } => eprint!("\rFlashing: {percent}%"),
        sambuca::flash::FlashProgress::Verifying { percent } => eprint!("\rVerifying: {percent}%"),
        sambuca::flash::FlashProgress::Patched { chunks, attempts } => {
            eprintln!("\nWarning: patched {chunks} chunk(s) during verification ({attempts} attempts)");
        }
    })?;
    eprintln!();

    eprintln!("Done in {:.1}s", start.elapsed().as_secs_f32());
    eprintln!("Rebooting into normal mode...");
    sambuca::flash::reboot_to_normal(&mut sambuca)?;

    Ok(())
}

fn do_samba_dump(output: &PathBuf, megabytes: usize, offset: usize) -> Result<()> {
    if megabytes == 0 {
        bail!("megabytes must be > 0");
    }
    if offset % sambuca::flash::SECTOR_SIZE != 0 {
        bail!("offset must be 512-byte aligned");
    }
    let total = megabytes * 1024 * 1024;
    eprintln!("Dumping {} MB from flash offset {} to {}", megabytes, offset, output.display());

    let mut sambuca = wait_for_samba()?;
    eprintln!("SAM-BA version: {}", sambuca.version().context("reading SAM-BA version")?);

    let file =
        std::fs::File::create(output).with_context(|| format!("Cannot create {}", output.display()))?;
    let mut writer = io::BufWriter::new(file);

    let start = Instant::now();
    let mut last_pct = 0;
    sambuca.dump_flash(offset as u64, total, &mut writer, |read| {
        let pct = read * 100 / total;
        if pct != last_pct {
            eprint!("\rReading: {pct}%");
            last_pct = pct;
        }
    })?;
    writer.flush().context("flushing output")?;
    eprintln!();
    eprintln!(
        "Done in {:.1}s — {} bytes written to {}",
        start.elapsed().as_secs_f32(),
        total,
        output.display()
    );
    Ok(())
}

// Main

fn main() -> Result<()> {
    let cli = Cli::parse();

    // SAM-BA commands don't use USB debug mode
    match &cli.command {
        CliCommand::Samba(samba_cmd) => {
            return match samba_cmd {
                SambaCommand::Version => {
                    let mut sambuca = wait_for_samba()?;
                    let ver = sambuca.version().context("reading SAM-BA version")?;
                    println!("{ver}");
                    Ok(())
                }
                SambaCommand::ReadU32 { address } => {
                    let mut sambuca = wait_for_samba()?;
                    let val =
                        sambuca.read_u32(*address).with_context(|| format!("reading 0x{address:08x}"))?;
                    println!("0x{val:08x}");
                    Ok(())
                }
                SambaCommand::WriteU32 { address, value } => {
                    let mut sambuca = wait_for_samba()?;
                    sambuca
                        .write_u32(*address, *value)
                        .with_context(|| format!("writing 0x{value:08x} to 0x{address:08x}"))?;
                    eprintln!("OK");
                    Ok(())
                }
                SambaCommand::Flash { image, boot, system, no_verify } => {
                    do_samba_flash(image, *boot, *system, *no_verify)
                }
                SambaCommand::DumpFlash { output, megabytes, offset } => {
                    do_samba_dump(output, *megabytes, *offset)
                }
                SambaCommand::Reboot => {
                    let mut sambuca = wait_for_samba()?;
                    eprintln!("Rebooting into normal mode...");
                    sambuca::flash::reboot_to_normal(&mut sambuca)?;
                    Ok(())
                }
            };
        }
        CliCommand::Mcp => {
            return mcp::run();
        }
        _ => {}
    }

    // All other commands use USB debug mode
    let client = open_usb()?;

    match cli.command {
        CliCommand::Screenshot { output } => do_screenshot(&client, &output)?,
        CliCommand::Tap { x, y } => do_tap(&client, x, y)?,
        CliCommand::Swipe { sx, sy, ex, ey, steps } => do_swipe(&client, sx, sy, ex, ey, steps)?,
        CliCommand::Power => do_power(&client)?,
        CliCommand::InputText { text } => do_input_text(&client, &text)?,
        CliCommand::TapScreenshot { x, y, output, wait } => {
            do_tap(&client, x, y)?;
            std::thread::sleep(Duration::from_millis(wait));
            do_screenshot(&client, &output)?;
        }
        CliCommand::SwipeScreenshot { sx, sy, ex, ey, output, wait } => {
            do_swipe(&client, sx, sy, ex, ey, 15)?;
            std::thread::sleep(Duration::from_millis(wait));
            do_screenshot(&client, &output)?;
        }
        CliCommand::Run { file } => {
            let content =
                std::fs::read_to_string(&file).with_context(|| format!("Cannot read {}", file.display()))?;
            let actions: Vec<Action> = serde_json::from_str(&content).context("Invalid JSON actions file")?;
            run_actions(&client, &actions)?;
        }
        CliCommand::LaunchApp { app_id } => {
            let hex = app_id.strip_prefix("0x").unwrap_or(&app_id);
            let bytes = hex::decode(hex).context("Invalid hex app ID")?;
            anyhow::ensure!(bytes.len() == 16, "App ID must be exactly 16 bytes (32 hex chars)");
            let app_id: [u8; 16] = bytes.try_into().unwrap();
            eprintln!("Launching app...");
            let payload = client.send(Command::LaunchApp { app_id }, Duration::from_secs(10))?;
            let result = LaunchAppResult::decode(&payload)?;
            match result.status {
                LaunchAppStatus::Launched => {
                    eprintln!("App launched with PID {}.", result.pid);
                }
                LaunchAppStatus::AlreadyRunning => {
                    eprintln!(
                        "App is already running with PID {}. Newly copied code will not run until the app is closed and launched again.",
                        result.pid
                    );
                }
            }
        }
        CliCommand::CloseApp { pid } => {
            eprintln!("Closing app with PID {pid}...");
            client.send(Command::CloseApp { pid }, Duration::from_secs(5))?;
            eprintln!("Close app request sent.");
        }
        CliCommand::RebootSamba => {
            eprintln!("Sending reboot-to-SAM-BA command...");
            let _ = client.send(Command::RebootSamba, Duration::from_secs(5));
            eprintln!("Device rebooting into SAM-BA mode.");
        }
        CliCommand::GetVersion => do_get_version(&client)?,
        CliCommand::Logs { max_lines, filter, include_stale } => {
            if !include_stale {
                std::thread::sleep(Duration::from_millis(500));
                while client.read_logs(Duration::ZERO).is_ok() {}
            }
            eprintln!("Streaming logs (Ctrl+C to stop)...");
            do_logs_usb(&client, max_lines, filter.as_deref())?;
        }
        CliCommand::Mcp | CliCommand::Samba(_) => unreachable!(),
    }

    Ok(())
}
