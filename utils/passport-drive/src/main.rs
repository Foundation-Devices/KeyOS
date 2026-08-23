// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! passport-drive — Rust CLI to interact with Passport Prime over USB.
//!
//! Uses raw USB bulk endpoints via `rusb` for all device interaction
//! (screenshot, tap, logs, kernel debug commands).

mod hid;
mod load_app;
mod mcp;
pub(crate) mod screenshot;

use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use clap::{Parser, Subcommand};
use rusb::UsbContext;
use serde::Deserialize;
use usb_debug_protocol::{Command, LaunchAppResult, LaunchAppStatus, UsbDebugClient};

// Screen / framebuffer constants (pub(crate) so mcp module can use them)
pub(crate) const SCREEN_WIDTH: u32 = 480;
pub(crate) const SCREEN_HEIGHT: u32 = 800;
pub(crate) const FB_SIZE: usize = (SCREEN_WIDTH * SCREEN_HEIGHT * 4) as usize;

// Log protocol constant
const LOG_TERMINATOR: u8 = 0x1E;

/// Directory that file access is confined to, canonical. Unset means no restriction.
static JAIL: OnceLock<PathBuf> = OnceLock::new();

/// Canonicalize the confinement root once, so every later comparison is against a real directory.
pub(crate) fn init_jail(jail: Option<PathBuf>) -> Result<()> {
    let Some(jail) = jail else {
        return Ok(());
    };

    let root = jail
        .canonicalize()
        .with_context(|| format!("Cannot resolve the jail directory {}", jail.display()))?;
    ensure!(root.is_dir(), "The jail path is not a directory: {}", root.display());
    // Relative paths then resolve inside the jail instead of wherever the server happened to be
    // started. This is only about what they resolve against; check_jail is what confines them.
    std::env::set_current_dir(&root)
        .with_context(|| format!("Cannot enter the jail directory {}", root.display()))?;
    JAIL.set(root).expect("the jail is initialized once per process");
    Ok(())
}

/// Refuse a path that resolves outside the confinement root. Symlinks are followed first, so a link
/// pointing out is caught rather than banned. Callers must check every path they open, and the
/// error names the path as it was given so it stays recognisable to whoever passed it.
pub(crate) fn check_jail(path: &Path) -> Result<()> {
    let Some(jail) = JAIL.get() else {
        return Ok(());
    };

    let real = path.canonicalize().with_context(|| format!("Cannot resolve {}", path.display()))?;
    ensure!(
        real.starts_with(jail),
        "Path points outside the allowed directory: {}. Pass a path relative to the base directory.",
        path.display()
    );
    Ok(())
}

pub(crate) fn launch_app_failure_message(status: LaunchAppStatus) -> Option<&'static str> {
    match status {
        LaunchAppStatus::Launched | LaunchAppStatus::AlreadyRunning => None,
        LaunchAppStatus::AppIdNotFound => Some(
            "app ID was not found after scanning installed apps; the uploaded bundle may have been skipped",
        ),
        LaunchAppStatus::SignatureRejected => Some(
            "app signature or bundle hashes were rejected; rebuild and reload the app, and if the signing identity changed, allow the matching publisher certificate first",
        ),
        LaunchAppStatus::NoCertificate => Some(
            "no matching allowed publisher certificate is installed; import the matching certificate in Settings > Apps > Allowed Publishers",
        ),
        LaunchAppStatus::PublisherCertificateExpired => Some(
            "the matching publisher certificate has expired; compare get_system_time with its expiry date in Settings > Apps > Allowed Publishers",
        ),
        LaunchAppStatus::PublisherCertificateNotYetActive => Some(
            "the matching publisher certificate is not valid yet; compare get_system_time with its start date in Settings > Apps > Allowed Publishers",
        ),
        LaunchAppStatus::KeyOsVersionTooOld => {
            Some("the app requires a newer KeyOS version; update KeyOS and try again")
        }
        LaunchAppStatus::NotReady => Some("launcher is not ready yet; unlock the device and try again"),
        LaunchAppStatus::InternalError => Some("internal launch error; check device logs"),
    }
}

pub(crate) fn launch_app_transport_error_message(error: &str) -> String {
    if error.contains("device returned status 0x01") {
        format!(
            "{error}. The upload completed, so Developer Mode is reachable; this is a generic launch rejection from firmware that did not report a detailed reason. For sideloaded apps, the most likely cause is that no matching allowed publisher certificate is installed in Settings > Apps > Allowed Publishers."
        )
    } else {
        error.to_string()
    }
}

const TAP_HOLD_MS: u16 = 50;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_app_failure_message_reports_missing_matching_certificate() {
        assert_eq!(
            launch_app_failure_message(LaunchAppStatus::NoCertificate),
            Some(
                "no matching allowed publisher certificate is installed; import the matching certificate in Settings > Apps > Allowed Publishers"
            )
        );
    }

    #[test]
    fn launch_app_failure_message_reports_signature_reload_guidance() {
        let message = launch_app_failure_message(LaunchAppStatus::SignatureRejected).unwrap();

        assert!(message.contains("rebuild and reload"));
        assert!(message.contains("signing identity changed"));
    }

    #[test]
    fn launch_app_failure_message_reports_keyos_upgrade_guidance() {
        let message = launch_app_failure_message(LaunchAppStatus::KeyOsVersionTooOld).unwrap();

        assert!(message.contains("newer KeyOS version"));
        assert!(message.contains("update KeyOS"));
    }

    #[test]
    fn system_time_is_rendered_as_utc_so_a_local_offset_is_not_read_as_a_fault() {
        assert_eq!(format_utc(1_754_400_600).unwrap(), "2025-08-05T13:30:00Z UTC");
    }

    #[test]
    fn timestamps_round_trip_through_rfc_3339() {
        assert_eq!(parse_timestamp("2025-08-05T13:30:00Z").unwrap(), 1_754_400_600);
        // Same instant, written in another offset.
        assert_eq!(parse_timestamp("2025-08-05T15:30:00+02:00").unwrap(), 1_754_400_600);

        assert!(parse_timestamp("yesterday").is_err());
        assert!(parse_timestamp("1969-01-01T00:00:00Z").is_err());
    }

    #[test]
    fn system_time_payload_must_be_exactly_eight_bytes() {
        assert_eq!(decode_system_time(&1_754_400_600u64.to_le_bytes()).unwrap(), 1_754_400_600);
        assert!(decode_system_time(&[0u8; 7]).is_err());
        assert!(decode_system_time(&[0u8; 9]).is_err());
    }

    #[test]
    fn launch_app_transport_error_explains_legacy_generic_status() {
        let message = launch_app_transport_error_message("device returned status 0x01");

        assert!(message.contains("generic launch rejection"));
        assert!(message.contains("matching allowed publisher certificate"));
    }
}

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
        /// Gesture duration in ms
        #[arg(long, default_value = "300")]
        duration_ms: u16,
        /// Number of drag steps
        #[arg(short, long, default_value = "15")]
        steps: u8,
    },
    /// Press power button
    Power {
        /// Hold long enough to open the shutdown control center instead of short-press locking
        #[arg(long)]
        long: bool,
    },
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
        /// Gesture duration in ms
        #[arg(long, default_value = "300")]
        duration_ms: u16,
        /// Number of drag steps
        #[arg(short, long, default_value = "15")]
        steps: u8,
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
    /// List connected Passport Prime USB devices
    ListPorts,
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
    /// Print the compact kernel process list
    GetProcessList,
    /// Print the device clock in UTC
    #[command(name = "get_time", alias = "get-time")]
    GetTime,
    /// Set the device clock. Requires Developer Mode and an unlocked device.
    #[command(name = "set_time", alias = "set-time")]
    SetTime {
        /// RFC 3339 timestamp (e.g. 2026-08-05T12:00:00Z), or "now" for this computer's clock
        #[arg(default_value = "now")]
        timestamp: String,
    },
    /// Upload an app bundle into keyos/sideloaded-apps/<app-id> over usb-debug
    #[command(name = "load_app", alias = "load-app")]
    LoadApp {
        /// Directory containing app.elf, manifest.json, and optional icon.bin/resources
        app_path: PathBuf,
    },
    /// Start MCP server mode for AI integration (JSON-RPC over stdio, or HTTP with --http)
    Mcp {
        /// Serve Streamable HTTP on this address instead of stdio
        #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "127.0.0.1:8000")]
        http: Option<SocketAddr>,
        /// Confine file access to paths that resolve under this directory
        #[arg(long, value_name = "PATH")]
        jail: Option<PathBuf>,
    },
    /// Send one ISO 7816 APDU over HID and print the RAPDU
    SendApdu {
        /// Hex-encoded APDU bytes, without Legacy HID framing
        apdu_hex: String,
        /// HID read timeout in milliseconds
        #[arg(long, default_value = "10000")]
        timeout_ms: i32,
    },
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

fn parse_hex_bytes(s: &str) -> Result<Vec<u8>> {
    let hex_clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if hex_clean.len() % 2 != 0 {
        bail!("hex input must have an even number of characters");
    }

    let bytes = (0..hex_clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex_clean[i..i + 2], 16))
        .collect::<std::result::Result<Vec<u8>, _>>()
        .context("invalid hex input")?;
    Ok(bytes)
}

// USB transport helpers

fn open_usb() -> Result<UsbDebugClient> { UsbDebugClient::open().context("Failed to open USB device") }

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
    eprintln!("Tap ({x}, {y})...");
    client.send(
        Command::Swipe { start_x: x, start_y: y, end_x: x, end_y: y, duration_ms: TAP_HOLD_MS, steps: 0 },
        Duration::from_millis(u64::from(TAP_HOLD_MS) + 5_000),
    )?;
    eprintln!("Tap OK");
    Ok(())
}

fn do_swipe(
    client: &UsbDebugClient,
    sx: u16,
    sy: u16,
    ex: u16,
    ey: u16,
    duration_ms: u16,
    steps: u8,
) -> Result<()> {
    eprintln!("Swipe ({sx},{sy}) -> ({ex},{ey}) duration={duration_ms}ms steps={steps}");
    client.send(
        Command::Swipe { start_x: sx, start_y: sy, end_x: ex, end_y: ey, duration_ms, steps },
        Duration::from_millis(u64::from(duration_ms) + 5_000),
    )?;
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

fn do_get_process_list(client: &UsbDebugClient) -> Result<()> {
    let payload = client.send(Command::GetProcessList, Duration::from_secs(5))?;
    print!("{}", String::from_utf8_lossy(&payload));
    Ok(())
}

fn do_get_time(client: &UsbDebugClient) -> Result<()> {
    let payload = client.send(Command::GetSystemTime, Duration::from_secs(5))?;
    let unix_seconds = decode_system_time(&payload)?;
    println!("{} (unix {unix_seconds})", format_utc(unix_seconds)?);
    Ok(())
}

fn do_set_time(client: &UsbDebugClient, timestamp: &str) -> Result<()> {
    let unix_seconds = parse_timestamp(timestamp)?;
    client.send(Command::SetSystemTime { unix_seconds }, Duration::from_secs(5))?;
    println!("device clock set to {} (unix {unix_seconds})", format_utc(unix_seconds)?);
    Ok(())
}

fn decode_system_time(payload: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] =
        payload.try_into().map_err(|_| anyhow::anyhow!("expected 8 payload bytes, got {}", payload.len()))?;
    Ok(u64::from_le_bytes(bytes))
}

/// The device clock is UTC; Settings renders the local time zone on top of it, so anything shown
/// to a human says so rather than leaving an offset to be read as a fault.
fn format_utc(unix_seconds: u64) -> Result<String> {
    let seconds = i64::try_from(unix_seconds).context("timestamp does not fit in an i64")?;
    let timestamp = jiff::Timestamp::from_second(seconds)
        .map_err(|e| anyhow::anyhow!("{unix_seconds} is not a representable timestamp: {e}"))?;
    Ok(format!("{timestamp:?} UTC"))
}

fn parse_timestamp(timestamp: &str) -> Result<u64> {
    let parsed = if timestamp == "now" {
        jiff::Timestamp::now()
    } else {
        timestamp
            .parse::<jiff::Timestamp>()
            .map_err(|e| anyhow::anyhow!("{timestamp:?} is not an RFC 3339 timestamp: {e}"))?
    };
    u64::try_from(parsed.as_second()).context("timestamps before the Unix epoch cannot be set")
}

fn do_power(client: &UsbDebugClient, long: bool) -> Result<()> {
    eprintln!("Power button {} press...", if long { "long" } else { "short" });
    client.send(Command::PowerButton { long }, Duration::from_secs(5))?;
    eprintln!("Power OK");
    Ok(())
}

fn do_send_apdu(apdu_hex: &str, timeout_ms: i32) -> Result<()> {
    let apdu = parse_hex_bytes(apdu_hex)?;
    if apdu.len() < 4 {
        bail!("APDU must be at least 4 bytes (CLA INS P1 P2)");
    }

    let (device, mode) = hid::open_hid()?;
    let mode_str = match mode {
        hid::HidMode::Legacy => "Legacy",
        hid::HidMode::Fido => "CTAP/FIDO",
    };
    eprintln!("Opened HID device in {mode_str} mode");

    let rapdu = hid::exchange_apdu(&device, &apdu, timeout_ms)?;
    let hex: String = rapdu.iter().map(|b| format!("{b:02x}")).collect();
    let sw = if rapdu.len() >= 2 {
        format!("{:02x}{:02x}", rapdu[rapdu.len() - 2], rapdu[rapdu.len() - 1])
    } else {
        "(no SW)".to_string()
    };
    println!("RAPDU ({} bytes, SW={}): {}", rapdu.len(), sw, hex);
    Ok(())
}

fn do_list_ports() -> Result<()> {
    let context = rusb::Context::new().context("Failed to initialize USB context")?;
    let devices = context.devices().context("Failed to enumerate USB devices")?;

    let mut found = false;
    for dev in devices.iter() {
        let desc = dev.device_descriptor().context("Failed to read USB device descriptor")?;
        let vid = desc.vendor_id();
        let pid = desc.product_id();
        let label = match (vid, pid) {
            (0x1307, 0x0165) => "Passport Prime",
            (0x2c97, 0x7011) => "Passport Prime (Flux/legacy)",
            (0x03eb, 0x6124) => "SAM-BA bootloader",
            _ => continue,
        };
        found = true;
        println!(
            "Bus {:03} Device {:03} — {label} (VID:{vid:04x} PID:{pid:04x})",
            dev.bus_number(),
            dev.address()
        );
    }

    if !found {
        println!("No Passport Prime USB devices found.");
    }
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
    /// Power button press; true = long, false = short.
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
                do_swipe(client, *sx, *sy, *ex, *ey, 300, 15)?;
            }
            Action::Screenshot(path) => {
                eprintln!("[{i}] screenshot -> {path}");
                do_screenshot(client, &PathBuf::from(path))?;
            }
            Action::Wait(ms) => {
                eprintln!("[{i}] wait {ms}ms");
                std::thread::sleep(Duration::from_millis(*ms));
            }
            Action::Power(long) => {
                eprintln!("[{i}] power {} press", if *long { "long" } else { "short" });
                client.send(Command::PowerButton { long: *long }, Duration::from_secs(5))?;
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
        CliCommand::Mcp { http, jail } => {
            return match http {
                Some(addr) => mcp::run_http(*addr, jail.clone()),
                None => mcp::run(jail.clone()),
            };
        }
        CliCommand::SendApdu { apdu_hex, timeout_ms } => {
            return do_send_apdu(apdu_hex, *timeout_ms);
        }
        CliCommand::ListPorts => {
            return do_list_ports();
        }
        _ => {}
    }

    // All other commands use USB debug mode
    let client = open_usb()?;

    match cli.command {
        CliCommand::Screenshot { output } => do_screenshot(&client, &output)?,
        CliCommand::Tap { x, y } => do_tap(&client, x, y)?,
        CliCommand::Swipe { sx, sy, ex, ey, duration_ms, steps } => {
            do_swipe(&client, sx, sy, ex, ey, duration_ms, steps)?;
        }
        CliCommand::Power { long } => do_power(&client, long)?,
        CliCommand::InputText { text } => do_input_text(&client, &text)?,
        CliCommand::TapScreenshot { x, y, output, wait } => {
            do_tap(&client, x, y)?;
            std::thread::sleep(Duration::from_millis(wait));
            do_screenshot(&client, &output)?;
        }
        CliCommand::SwipeScreenshot { sx, sy, ex, ey, duration_ms, steps, output, wait } => {
            do_swipe(&client, sx, sy, ex, ey, duration_ms, steps)?;
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
            let payload = client
                .send(Command::LaunchApp { app_id }, Duration::from_secs(10))
                .map_err(|e| anyhow::anyhow!("{}", launch_app_transport_error_message(&e.to_string())))?;
            let result = LaunchAppResult::decode(&payload)?;
            match result.status {
                LaunchAppStatus::Launched => {
                    eprintln!("App launched with PID {}.", result.pid);
                }
                LaunchAppStatus::AlreadyRunning => {
                    eprintln!(
                        "App is already running with PID {}. Newly uploaded code will not run until the app is closed and launched again.",
                        result.pid
                    );
                }
                status => {
                    let reason = launch_app_failure_message(status).unwrap_or("unknown launch failure");
                    bail!("App launch failed: {reason}");
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
        CliCommand::GetProcessList => do_get_process_list(&client)?,
        CliCommand::GetTime => do_get_time(&client)?,
        CliCommand::SetTime { timestamp } => do_set_time(&client, &timestamp)?,
        CliCommand::LoadApp { app_path } => {
            eprintln!("Uploading app from {}...", app_path.display());
            let report = load_app::load_app(|cmd, timeout| client.send(cmd, timeout), &app_path)?;
            eprintln!(
                "Loaded {} into {}/{} ({}, resources: {} files / {} bytes).",
                report.app_id,
                load_app::DEVICE_DIR,
                report.app_id,
                report.files_summary(),
                report.resource_files,
                report.resource_bytes
            );
        }
        CliCommand::Logs { max_lines, filter, include_stale } => {
            if !include_stale {
                std::thread::sleep(Duration::from_millis(500));
                while client.read_logs(Duration::ZERO).is_ok() {}
            }
            eprintln!("Streaming logs (Ctrl+C to stop)...");
            do_logs_usb(&client, max_lines, filter.as_deref())?;
        }
        CliCommand::Mcp { .. }
        | CliCommand::SendApdu { .. }
        | CliCommand::ListPorts
        | CliCommand::Samba(_) => {
            unreachable!()
        }
    }

    Ok(())
}
