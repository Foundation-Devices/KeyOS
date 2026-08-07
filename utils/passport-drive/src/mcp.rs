// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! MCP (Model Context Protocol) server mode for passport-drive.
//!
//! Speaks MCP over stdio or Streamable HTTP via the rmcp SDK.
//! Provides tools for AI integration (Claude Code).

use std::collections::VecDeque;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use const_oid::ObjectIdentifier;
use hidapi::HidDevice;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use rusb::UsbContext as _;
use serde::Deserialize;
use tokio::net::TcpListener;
use usb_debug_protocol::{
    Command, LaunchAppResult, LaunchAppStatus, OpenError, TransportError, UsbDebugClient,
    INSTALL_CERTIFICATE_BYTES_MAX,
};
use x509_cert::{
    der::{Decode, DecodePem},
    Certificate,
};

use crate::{
    decode_system_time, format_utc, launch_app_failure_message, launch_app_transport_error_message,
    parse_timestamp, LOG_TERMINATOR,
};

const MAX_LOG_LINES: usize = 2000;
const TAP_HOLD_MS: u16 = 50;
const APDU_TIMEOUT_MS: i32 = 10_000;
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);
const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const SECP256K1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.10");

const INSTRUCTIONS: &str = "\
Drives a Passport Prime over USB. Commands open the connection themselves; call connect only to
start buffering logs before triggering whatever produces them.

The server may be configured to confine file access to a base directory, usually the workspace root.
Where it is, a path that resolves outside that directory is rejected, symlinks followed. Where it is
not, any path the server process can read works. Relative paths resolve against that base directory
where one is set, so a path relative to it is always safe to pass.

Publisher certificates use a two-step decision flow: call install_certificate without
expected_fingerprint to receive the unverified-identity warning and actual fingerprint, then call
again with that exact full fingerprint only after the user explicitly chooses to allow it.";

/// One process drives one physical device, so the state is process-wide.
static STATE: LazyLock<Mutex<McpState>> = LazyLock::new(|| Mutex::new(McpState::new()));

/// Recovers a poisoned lock, or else one panicking tool call bricks every later one.
fn state() -> MutexGuard<'static, McpState> { STATE.lock().unwrap_or_else(|e| e.into_inner()) }

// MCP state

struct McpState {
    device: Option<UsbDebugClient>,
    log_buffer: VecDeque<String>,
    record_buf: Vec<u8>,
    sambuca: Option<sambuca::Sambuca>,
    flash_params: Option<FlashParams>,
    hid_device: Option<HidDevice>,
}

#[derive(Clone)]
struct FlashParams {
    instance: u32,
    ioset: u32,
    partition: u32,
    bus_width: u32,
    voltage: u32,
}

impl McpState {
    fn new() -> Self {
        Self {
            device: None,
            log_buffer: VecDeque::new(),
            record_buf: Vec::new(),
            sambuca: None,
            flash_params: None,
            hid_device: None,
        }
    }

    /// The open device, connecting first if nothing is open yet.
    fn require_device(&mut self) -> Result<&UsbDebugClient, OpenError> {
        if self.device.is_none() {
            self.device = Some(UsbDebugClient::open()?);
        }

        Ok(self.device.as_ref().expect("opened above"))
    }

    /// Send `cmd` to the device. A command that loses frame sync takes the connection down with it,
    /// so the next one opens a fresh transport instead of reusing one whose responses have slipped
    /// a step behind.
    fn send(&mut self, cmd: Command, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        let result = self.require_device()?.send(cmd, timeout);
        if matches!(result, Err(TransportError::OutOfSync(_))) {
            // The log receiver goes with the transport, so drain it first or the frames leading up
            // to the desync are dropped, exactly the ones worth reading.
            self.drain_logs();
            self.device = None;
        }

        result
    }

    fn require_sambuca(&mut self) -> Result<&mut sambuca::Sambuca, String> {
        self.sambuca.as_mut().ok_or_else(|| "SAM-BA not connected. Call samba_connect first.".to_string())
    }

    fn require_flash_params(&self) -> Result<FlashParams, String> {
        self.flash_params
            .clone()
            .ok_or_else(|| "Flash not initialized. Call samba_init_flash first.".to_string())
    }

    /// Drain pending log frames from the USB transport and parse 0x1E-terminated records into the
    /// log buffer.
    fn drain_logs(&mut self) {
        let Some(transport) = self.device.as_ref() else {
            return;
        };

        while let Ok(data) = transport.read_logs(Duration::ZERO) {
            for &b in &data {
                if b == LOG_TERMINATOR {
                    let text = String::from_utf8_lossy(&self.record_buf).trim_end().to_string();
                    self.record_buf.clear();
                    if text.is_empty() {
                        continue;
                    }
                    self.log_buffer.push_back(text);
                    while self.log_buffer.len() > MAX_LOG_LINES {
                        self.log_buffer.pop_front();
                    }
                } else {
                    self.record_buf.push(b);
                    if self.record_buf.len() > 16384 {
                        self.record_buf.drain(..self.record_buf.len() - 4096);
                    }
                }
            }
        }
    }
}

// Result helpers

fn text_result(s: &str) -> CallToolResult { CallToolResult::success(vec![ContentBlock::text(s)]) }

// Tool parameters

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetLogsParams {
    /// Max lines to return (default 100)
    max_lines: Option<u64>,
    /// Optional substring filter
    filter: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TapParams {
    /// X coordinate (0-479)
    x: u16,
    /// Y coordinate (0-799)
    y: u16,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SwipeParams {
    /// Start X coordinate
    start_x: u16,
    /// Start Y coordinate
    start_y: u16,
    /// End X coordinate
    end_x: u16,
    /// End Y coordinate
    end_y: u16,
    /// Gesture duration in milliseconds (default 300)
    duration_ms: Option<u16>,
    /// Number of drag events (default 15)
    steps: Option<u8>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PowerButtonParams {
    /// true = long press, false = short press
    long: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct KernelCommandParams {
    /// Single character kernel debug command (h/i/m/p/t/s/c/a/o/k)
    command: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InputTextParams {
    /// Text to type into the focused input field
    text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LaunchAppParams {
    /// 32-character hex app ID (with optional 0x prefix)
    app_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CloseAppParams {
    /// Process ID to close
    pid: u16,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetSystemTimeParams {
    /// RFC 3339 timestamp (e.g. 2026-08-05T12:00:00Z), or "now" for this computer's clock
    timestamp: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LoadAppParams {
    /// Local app bundle directory containing app.elf, manifest.json, and optional
    /// icon.bin/resources. Must resolve inside the server's base directory when access is confined.
    app_path: String,
    /// Upload as a Flux child app (Legacy mode), launched by the Flux emulator instead of the
    /// system launcher
    flux: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InstallCertificateParams {
    /// Local PEM certificate file to install as an allowed publisher. Must resolve inside the
    /// server's base directory when access is confined.
    certificate_path: String,
    /// Full lowercase publisher fingerprint the user reviewed and explicitly chose to allow.
    /// Omit it to preview the actual full/short fingerprint and identity warning without
    /// installing anything.
    expected_fingerprint: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SambaReadU32Params {
    /// Memory address
    address: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SambaWriteU32Params {
    /// Memory address
    address: u32,
    /// Value to write (32-bit)
    value: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SambaInitFlashParams {
    /// SDMMC instance (default: 0)
    instance: Option<u32>,
    /// Partition number (default: 0)
    partition: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SambaReadFlashParams {
    /// Byte offset in flash (page-aligned)
    offset: u64,
    /// Number of bytes to read (page-aligned)
    length: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SambaWriteFlashParams {
    /// Byte offset in flash (page-aligned)
    offset: u64,
    /// Data to write (base64-encoded)
    data_base64: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SambaVerifyFlashParams {
    /// Byte offset in flash
    offset: u64,
    /// Expected data (base64-encoded)
    data_base64: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendApduParams {
    /// Hex-encoded APDU bytes, e.g. '0003000000' for U2F_VERSION
    apdu_hex: String,
}

// Tools

#[derive(Clone)]
pub struct PassportServer;

#[tool_router]
impl PassportServer {
    /// List connected Passport Prime USB devices (normal, Flux/legacy, and SAM-BA modes).
    #[tool]
    fn list_ports(&self) -> Result<CallToolResult, String> {
        let context = rusb::Context::new().map_err(|e| format!("Failed to initialize USB context: {e}"))?;
        let devices = context.devices().map_err(|e| format!("Failed to enumerate USB devices: {e}"))?;

        let mut lines = Vec::new();
        for dev in devices.iter() {
            let Ok(desc) = dev.device_descriptor() else {
                continue;
            };
            let (vid, pid) = (desc.vendor_id(), desc.product_id());
            let label = match (vid, pid) {
                (0x1307, 0x0165) => "Passport Prime",
                (0x2c97, 0x7011) => "Passport Prime (Flux/legacy)",
                (0x03eb, 0x6124) => "SAM-BA bootloader",
                _ => continue,
            };
            lines.push(format!(
                "Bus {:03} Device {:03} — {label} (VID:{vid:04x} PID:{pid:04x})",
                dev.bus_number(),
                dev.address()
            ));
        }

        if lines.is_empty() {
            return Ok(text_result("No Passport Prime USB devices found."));
        }
        Ok(text_result(&lines.join("\n")))
    }

    /// Open the USB connection to the Passport Prime now, auto-detecting the device. Commands
    /// connect on their own, so this is only worth calling to start capturing logs before
    /// triggering whatever produces them.
    #[tool]
    fn connect(&self) -> Result<CallToolResult, String> {
        state().require_device().map_err(|e| format!("Failed to connect: {e}"))?;
        Ok(text_result("Connected via USB vendor interface. Logs and debug commands on single interface."))
    }

    /// Release the USB interface and drop buffered logs. Commands reconnect on their own, so this
    /// is for handing the device to another process or for starting a fresh log capture.
    #[tool]
    fn disconnect(&self) -> CallToolResult {
        let mut state = state();
        state.device.take();
        state.log_buffer.clear();
        state.record_buf.clear();
        text_result("Disconnected.")
    }

    /// Get recent log lines streamed from the device. Only lines received since the connection
    /// opened are held, so connect before triggering whatever you want the logs from.
    #[tool]
    fn get_logs(
        &self,
        Parameters(GetLogsParams { max_lines, filter }): Parameters<GetLogsParams>,
    ) -> CallToolResult {
        let mut state = state();
        state.drain_logs();

        let max_lines = max_lines.unwrap_or(100) as usize;
        let matching = state.log_buffer.iter().filter(|l| filter.as_ref().is_none_or(|f| l.contains(f)));
        let logs: Vec<&String> = if max_lines > 0 {
            let mut tail: Vec<&String> = matching.rev().take(max_lines).collect();
            tail.reverse();
            tail
        } else {
            matching.collect()
        };

        if logs.is_empty() {
            return text_result("(no logs)");
        }
        text_result(&logs.iter().map(|l| l.as_str()).collect::<Vec<_>>().join("\n"))
    }

    /// Clear the in-memory log buffer.
    #[tool]
    fn clear_logs(&self) -> CallToolResult {
        let mut state = state();
        // Drain pending logs first, then clear everything.
        if let Some(transport) = &state.device {
            while transport.read_logs(Duration::ZERO).is_ok() {}
        }
        state.log_buffer.clear();
        state.record_buf.clear();
        text_result("Log buffer cleared.")
    }

    /// Capture a screenshot from the device screen (480x800 px). Returns a base64-encoded PNG.
    #[tool]
    fn screenshot(&self) -> Result<CallToolResult, String> {
        let payload = state()
            .send(Command::Screenshot, Duration::from_secs(20))
            .map_err(|e| format!("Screenshot failed: {e}"))?;

        let png = crate::screenshot::bgra_to_png(&payload)?;
        let base64 = base64::engine::general_purpose::STANDARD.encode(&png);
        Ok(CallToolResult::success(vec![ContentBlock::image(base64, "image/png")]))
    }

    /// Tap (press + release) at the given screen coordinates. Screen is 480x800 px, origin at top-left.
    #[tool]
    fn tap(&self, Parameters(TapParams { x, y }): Parameters<TapParams>) -> Result<CallToolResult, String> {
        state()
            .send(
                Command::Swipe {
                    start_x: x,
                    start_y: y,
                    end_x: x,
                    end_y: y,
                    duration_ms: TAP_HOLD_MS,
                    steps: 0,
                },
                Duration::from_millis(u64::from(TAP_HOLD_MS) + 5_000),
            )
            .map_err(|e| format!("Tap failed: {e}"))?;

        Ok(text_result(&format!("Tapped at ({x}, {y}).")))
    }

    /// Send a timed swipe gesture. Coordinates are physical touch coordinates; the LCD is y=0..799 and the
    /// virtual button strip extends below it. Equal start and end coordinates hold the press in
    /// place for duration_ms, which is how to long-press.
    #[tool]
    fn swipe(
        &self,
        Parameters(SwipeParams { start_x, start_y, end_x, end_y, duration_ms, steps }): Parameters<
            SwipeParams,
        >,
    ) -> Result<CallToolResult, String> {
        let duration_ms = duration_ms.unwrap_or(300);
        let steps = steps.unwrap_or(15);

        state()
            .send(
                Command::Swipe { start_x, start_y, end_x, end_y, duration_ms, steps },
                Duration::from_millis(u64::from(duration_ms) + 5_000),
            )
            .map_err(|e| format!("Swipe failed: {e}"))?;

        Ok(text_result(&format!(
            "Swipe ({start_x}, {start_y}) -> ({end_x}, {end_y}) over {duration_ms}ms with {steps} steps."
        )))
    }

    /// Simulate a short or long power button press on the device.
    #[tool]
    fn power_button(
        &self,
        Parameters(PowerButtonParams { long }): Parameters<PowerButtonParams>,
    ) -> Result<CallToolResult, String> {
        state()
            .send(Command::PowerButton { long }, Duration::from_secs(5))
            .map_err(|e| format!("Power button failed: {e}"))?;

        Ok(text_result(&format!("Power button {} press.", if long { "long" } else { "short" })))
    }

    /// Send a single-byte kernel debug command via USB and return the output. Commands: h=help, i=IRQ stats,
    /// m=MMU state, p=process list (verbose), t=process list (compact), s=server list, c=cache stats, a=app
    /// IDs (maps app IDs to PIDs), o=memory ownership, k=consistency check. Note: 't' output is also
    /// available via get_process_list.
    #[tool]
    fn send_kernel_command(
        &self,
        Parameters(KernelCommandParams { command }): Parameters<KernelCommandParams>,
    ) -> Result<CallToolResult, String> {
        let [cmd_byte] = command.as_bytes() else {
            return Err("command must be a single character (h/i/m/p/t/s/c/a/o/k)".to_string());
        };

        let payload = state()
            .send(Command::KernelCmd { cmd_byte: *cmd_byte }, Duration::from_secs(5))
            .map_err(|e| format!("Kernel command failed: {e}"))?;
        Ok(text_result(&String::from_utf8_lossy(&payload)))
    }

    /// Reboot the device into SAM-BA bootloader mode. Device will disconnect and reappear as SAM-BA device
    /// (VID:PID 03eb:6124).
    #[tool]
    fn reboot_to_samba(&self) -> Result<CallToolResult, String> {
        let mut state = state();
        state
            .send(Command::RebootSamba, Duration::from_secs(5))
            .map_err(|e| format!("Reboot to SAM-BA failed: {e}"))?;
        // The log receiver goes with the transport, so drain it first or the frames leading up to
        // the reboot are dropped.
        state.drain_logs();
        state.device.take();

        Ok(text_result("Device rebooting to SAM-BA mode. Use samba_connect to connect to it."))
    }

    /// Type text into the focused input field on the device. Injects key press/release events for each
    /// character, bypassing the on-screen keyboard.
    #[tool]
    fn input_text(
        &self,
        Parameters(InputTextParams { text }): Parameters<InputTextParams>,
    ) -> Result<CallToolResult, String> {
        state()
            .send(Command::InputText(text.clone()), Duration::from_secs(10))
            .map_err(|e| format!("Input text failed: {e}"))?;

        Ok(text_result(&format!("Typed {} character(s).", text.chars().count())))
    }

    /// Launch a Flux app by its 16-byte hex app ID. Returns whether it was launched or already running, plus
    /// the PID.
    #[tool]
    fn launch_app(
        &self,
        Parameters(LaunchAppParams { app_id }): Parameters<LaunchAppParams>,
    ) -> Result<CallToolResult, String> {
        let trimmed = app_id.trim();
        let bytes = hex::decode(trimmed.strip_prefix("0x").unwrap_or(trimmed))
            .map_err(|e| format!("Invalid hex app_id: {e}"))?;
        let app_id: [u8; 16] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| format!("app_id must be 16 bytes (32 hex chars), got {} bytes", bytes.len()))?;

        let payload = state().send(Command::LaunchApp { app_id }, Duration::from_secs(10)).map_err(|e| {
            format!("Failed to launch app: {}", launch_app_transport_error_message(&e.to_string()))
        })?;

        let result =
            LaunchAppResult::decode(&payload).map_err(|e| format!("Invalid launch_app response: {e}"))?;
        match result.status {
            LaunchAppStatus::Launched => {
                Ok(text_result(&format!("App launched successfully with PID {}", result.pid)))
            }
            LaunchAppStatus::AlreadyRunning => Ok(text_result(&format!(
                "App is already running with PID {}. Newly uploaded code will not run until the app is closed and launched again.",
                result.pid
            ))),
            status => Err(format!(
                "Launch failed: {}",
                launch_app_failure_message(status).unwrap_or("unknown launch failure")
            )),
        }
    }

    /// Close/kill an app by PID. Uses gui-server's graceful close mechanism. Only works for app processes
    /// (not system services).
    #[tool]
    fn close_app(
        &self,
        Parameters(CloseAppParams { pid }): Parameters<CloseAppParams>,
    ) -> Result<CallToolResult, String> {
        if pid == 0 {
            return Err("pid must be a positive integer (1-65535)".to_string());
        }

        state()
            .send(Command::CloseApp { pid }, Duration::from_secs(5))
            .map_err(|e| format!("Failed to close app: {e}"))?;

        Ok(text_result(&format!("Process {pid} close requested successfully")))
    }

    /// Upload an arbitrary app directory into keyos/sideloaded-apps/<app-id> on the device over
    /// usb-debug (or keyos/apps/gui-app-emu-flux/sideloaded-apps/<app-id> with flux=true, for apps
    /// run by the Flux emulator). The directory must contain app.elf and manifest.json; icon.bin
    /// and resources/ are uploaded when present. Replaces those files if the app already exists.
    #[tool]
    fn load_app(
        &self,
        Parameters(LoadAppParams { app_path, flux }): Parameters<LoadAppParams>,
    ) -> Result<CallToolResult, String> {
        let kind = match flux.unwrap_or(false) {
            true => crate::load_app::SideloadKind::Flux,
            false => crate::load_app::SideloadKind::Standard,
        };
        let mut state = state();
        let report = crate::load_app::load_app(
            |cmd, timeout| state.send(cmd, timeout),
            &PathBuf::from(app_path),
            kind,
        )
        .map_err(|e| format!("load_app failed: {e:#}"))?;

        Ok(text_result(&format!(
            "Loaded {} into {}/{} ({}, resources: {} files / {} bytes).",
            report.app_id,
            kind.device_dir(),
            report.app_id,
            report.files_summary(),
            report.resource_files,
            report.resource_bytes
        )))
    }

    /// Send a local publisher certificate over usb-debug and install it as an allowed publisher.
    #[tool]
    fn install_certificate(
        &self,
        Parameters(InstallCertificateParams { certificate_path, expected_fingerprint }): Parameters<
            InstallCertificateParams,
        >,
    ) -> Result<CallToolResult, String> {
        let path = PathBuf::from(certificate_path);
        crate::check_jail(&path).map_err(|e| format!("{e:#}"))?;
        let metadata =
            fs::metadata(&path).map_err(|e| format!("Could not read certificate {}: {e}", path.display()))?;
        if metadata.len() == 0 {
            return Err(format!("certificate is empty: {}", path.display()));
        }
        if metadata.len() > INSTALL_CERTIFICATE_BYTES_MAX as u64 {
            return Err(format!(
                "certificate is too large: {} bytes (maximum {INSTALL_CERTIFICATE_BYTES_MAX} bytes)",
                metadata.len()
            ));
        }
        let certificate_pem =
            fs::read(&path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        let fingerprint = publisher_certificate_fingerprint(&certificate_pem)?;

        if expected_fingerprint.as_deref() != Some(fingerprint.full.as_str()) {
            return Err(format!(
                "Foundation has NOT verified this publisher's identity.\n\
                 Publisher fingerprint:\n\
                 Full:  {}\n\
                 Short: {}\n\
                 Apps signed by this publisher will be allowed on your Passport.\n\
                 Verify this fingerprint against the publisher's official website or GitHub.\n\
                 After the user explicitly chooses to allow it, retry with expected_fingerprint=\"{}\".",
                fingerprint.full, fingerprint.short, fingerprint.full
            ));
        }

        let expected_fingerprint: [u8; usb_debug_protocol::PUBLISHER_FINGERPRINT_HEX_LEN] =
            fingerprint.full.as_bytes().try_into().map_err(|_| {
                "internal error: canonical publisher fingerprint has the wrong length".to_string()
            })?;
        state()
            .send(
                Command::InstallCertificate { expected_fingerprint, certificate_pem },
                Duration::from_secs(10),
            )
            .map_err(|e| format!("install_certificate failed: {e}"))?;

        Ok(text_result(&format!(
            "Installed {} as an allowed publisher certificate with fingerprint {}.",
            path.display(),
            fingerprint.full
        )))
    }

    /// List SAM-BA bootloader devices (SAMA5D2, VID:PID 03eb:6124). Device must be in SAM-BA mode.
    #[tool]
    fn samba_list_devices(&self) -> Result<CallToolResult, String> {
        let context = rusb::Context::new().map_err(|e| format!("Failed to initialize USB context: {e}"))?;
        let devices = context.devices().map_err(|e| format!("Failed to enumerate USB devices: {e}"))?;

        let samba: Vec<String> = devices
            .iter()
            .filter_map(|dev| {
                let desc = dev.device_descriptor().ok()?;
                (desc.vendor_id() == 0x03eb && desc.product_id() == 0x6124).then(|| {
                    format!(
                        "Bus {:03} Device {:03} (VID:{:04x} PID:{:04x})",
                        dev.bus_number(),
                        dev.address(),
                        desc.vendor_id(),
                        desc.product_id()
                    )
                })
            })
            .collect();

        if samba.is_empty() {
            return Ok(text_result("No SAM-BA devices found. Is the device in bootloader mode?"));
        }
        Ok(text_result(&samba.join("\n")))
    }

    /// Connect to a SAM-BA bootloader device. Auto-detects the port.
    #[tool]
    fn samba_connect(&self) -> Result<CallToolResult, String> {
        let mut state = state();
        if state.sambuca.is_some() {
            return Err("Already connected to SAM-BA. Call samba_disconnect first.".to_string());
        }

        state.sambuca =
            Some(sambuca::Sambuca::new().map_err(|e| format!("Failed to connect to SAM-BA device: {e}"))?);
        Ok(text_result("Connected to SAM-BA device."))
    }

    /// Disconnect from the SAM-BA device.
    #[tool]
    fn samba_disconnect(&self) -> CallToolResult {
        let mut state = state();
        state.sambuca = None;
        state.flash_params = None;
        text_result("Disconnected from SAM-BA device.")
    }

    /// Get SAM-BA bootloader version string.
    #[tool]
    fn samba_version(&self) -> Result<CallToolResult, String> {
        let version =
            state().require_sambuca()?.version().map_err(|e| format!("Failed to read version: {e}"))?;
        Ok(text_result(&format!("SAM-BA version: {version}")))
    }

    /// Read a 32-bit value from a memory address.
    #[tool]
    fn samba_read_u32(
        &self,
        Parameters(SambaReadU32Params { address }): Parameters<SambaReadU32Params>,
    ) -> Result<CallToolResult, String> {
        let val = state().require_sambuca()?.read_u32(address).map_err(|e| format!("Read failed: {e}"))?;
        Ok(text_result(&format!("0x{address:08x}: 0x{val:08x} ({val})")))
    }

    /// Write a 32-bit value to a memory address.
    #[tool]
    fn samba_write_u32(
        &self,
        Parameters(SambaWriteU32Params { address, value }): Parameters<SambaWriteU32Params>,
    ) -> Result<CallToolResult, String> {
        state().require_sambuca()?.write_u32(address, value).map_err(|e| format!("Write failed: {e}"))?;
        Ok(text_result(&format!("Wrote 0x{value:08x} to 0x{address:08x}.")))
    }

    /// Initialize the SDMMC flash applet. Must be called before flash read/write/verify.
    #[tool]
    fn samba_init_flash(
        &self,
        Parameters(SambaInitFlashParams { instance, partition }): Parameters<SambaInitFlashParams>,
    ) -> Result<CallToolResult, String> {
        let params = FlashParams {
            instance: instance.unwrap_or(0),
            ioset: 1,
            partition: partition.unwrap_or(0),
            bus_width: 8,
            voltage: 3,
        };

        let mut state = state();
        state
            .require_sambuca()?
            .initialize_flash_applet(
                params.instance,
                params.ioset,
                params.partition,
                params.bus_width,
                params.voltage,
            )
            .map_err(|e| format!("Flash init failed: {e}"))?;

        let msg = format!(
            "Flash applet initialized.\n  Instance: {}\n  IO set: {}\n  Partition: {}\n  Bus width: {}\n  Voltage: {}",
            params.instance, params.ioset, params.partition, params.bus_width, params.voltage,
        );
        state.flash_params = Some(params);
        Ok(text_result(&msg))
    }

    /// Get flash applet information (buffer address, buffer size, page size).
    #[tool]
    fn samba_flash_info(&self) -> CallToolResult {
        let Some(params) = &state().flash_params else {
            return text_result("Flash applet not initialized. Call samba_init_flash first.");
        };
        text_result(&format!(
            "Flash applet initialized:\n  Instance: {}\n  IO set: {}\n  Partition: {}\n  Bus width: {}\n  Voltage: {}",
            params.instance, params.ioset, params.partition, params.bus_width, params.voltage,
        ))
    }

    /// Read data from flash. Requires samba_init_flash first. Returns base64-encoded data.
    #[tool]
    fn samba_read_flash(
        &self,
        Parameters(SambaReadFlashParams { offset, length }): Parameters<SambaReadFlashParams>,
    ) -> Result<CallToolResult, String> {
        let mut state = state();
        let params = state.require_flash_params()?;
        let mut applet = open_flash_applet(&mut state, &params)?;

        let mut buf = Vec::new();
        applet.read_flash(offset, length, &mut buf, |_| {}).map_err(|e| format!("Flash read failed: {e}"))?;

        let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
        Ok(text_result(&format!("Read {} bytes from offset 0x{offset:x}.\nData (base64): {b64}", buf.len())))
    }

    /// Write data to flash. Requires samba_init_flash first. Data must be base64-encoded.
    #[tool]
    fn samba_write_flash(
        &self,
        Parameters(SambaWriteFlashParams { offset, data_base64 }): Parameters<SambaWriteFlashParams>,
    ) -> Result<CallToolResult, String> {
        let data = base64::engine::general_purpose::STANDARD
            .decode(&data_base64)
            .map_err(|e| format!("Invalid base64: {e}"))?;

        let mut state = state();
        let params = state.require_flash_params()?;
        let mut applet = open_flash_applet(&mut state, &params)?;

        applet.write_flash(offset, &data, |_| {}).map_err(|e| format!("Flash write failed: {e}"))?;
        Ok(text_result(&format!("Wrote {} bytes to offset 0x{offset:x}.", data.len())))
    }

    /// Verify flash contents against provided data. Returns true if match.
    #[tool]
    fn samba_verify_flash(
        &self,
        Parameters(SambaVerifyFlashParams { offset, data_base64 }): Parameters<SambaVerifyFlashParams>,
    ) -> Result<CallToolResult, String> {
        let data = base64::engine::general_purpose::STANDARD
            .decode(&data_base64)
            .map_err(|e| format!("Invalid base64: {e}"))?;

        let mut state = state();
        let params = state.require_flash_params()?;
        let mut applet = open_flash_applet(&mut state, &params)?;

        let stats =
            applet.verify_flash(offset, &data, |_| {}, true).map_err(|e| format!("Verify failed: {e}"))?;

        if stats.num_chunks_patched == 0 {
            return Ok(text_result("Verification PASSED: flash matches data."));
        }
        Ok(text_result(&format!(
            "Verification FAILED: patched {} chunk(s) ({} attempts).",
            stats.num_chunks_patched, stats.num_attempts
        )))
    }

    /// Reboot the device to normal mode (exit SAM-BA mode).
    #[tool]
    fn samba_reboot(&self) -> Result<CallToolResult, String> {
        let mut state = state();
        let sambuca = state.require_sambuca()?;

        sambuca.write_u32(0xF804_8054, 0x6683_0000).map_err(|e| format!("Failed to reset boot bits: {e}"))?;
        sambuca
            .write_u32(0xF804_8000, 0xA500_0001)
            .map_err(|e| format!("Failed to kick reset controller: {e}"))?;

        state.sambuca = None;
        state.flash_params = None;
        Ok(text_result("Device rebooting to normal mode."))
    }

    /// Send an ISO 7816 APDU over USB HID. Auto-detects CTAP/FIDO mode (VID=0x1307, CTAPHID_MSG framing) or
    /// Legacy mode (VID=0x2c97, Legacy HID framing). Returns hex-encoded RAPDU.
    #[tool]
    fn send_apdu(
        &self,
        Parameters(SendApduParams { apdu_hex }): Parameters<SendApduParams>,
    ) -> Result<CallToolResult, String> {
        let hex_clean: String = apdu_hex.chars().filter(|c| !c.is_whitespace()).collect();
        let apdu = hex::decode(&hex_clean).map_err(|e| format!("Invalid hex in apdu_hex: {e}"))?;
        if apdu.len() < 4 {
            return Err("APDU must be at least 4 bytes (CLA INS P1 P2)".to_string());
        }

        let mut state = state();
        if state.hid_device.is_none() {
            let (dev, mode) =
                crate::hid::open_hid().map_err(|e| format!("Failed to open HID device: {e}"))?;
            let mode_str = match mode {
                crate::hid::HidMode::Legacy => "Legacy",
                crate::hid::HidMode::Fido => "CTAP/FIDO",
            };
            eprintln!("[mcp] HID device opened in {mode_str} mode");
            state.hid_device = Some(dev);
        }

        let device = state.hid_device.as_ref().expect("opened above");
        if let Ok(rapdu) = crate::hid::exchange_apdu(device, &apdu, APDU_TIMEOUT_MS) {
            return Ok(text_result(&format_rapdu(&rapdu)));
        }

        // The handle goes stale whenever the device re-enumerates, so retry once on a fresh one.
        let (dev, _) = crate::hid::open_hid().map_err(|e| format!("Failed to reopen HID device: {e}"))?;
        state.hid_device = Some(dev);
        let device = state.hid_device.as_ref().expect("opened above");
        let rapdu = crate::hid::exchange_apdu(device, &apdu, APDU_TIMEOUT_MS)
            .map_err(|e| format!("APDU exchange failed: {e}"))?;
        Ok(text_result(&format_rapdu(&rapdu)))
    }

    /// Get the KeyOS version string running on the device (same value shown on Settings -> About -> KeyOS,
    /// e.g. "1.3.0"). Useful for SDK compatibility checks.
    #[tool]
    fn get_version(&self) -> Result<CallToolResult, String> {
        let payload = state()
            .send(Command::GetVersion, Duration::from_secs(5))
            .map_err(|e| format!("get_version request failed: {e}"))?;
        Ok(text_result(&String::from_utf8_lossy(&payload)))
    }

    /// Get the list of running processes on the device with PID, name, CPU%, RAM usage, and thread states.
    /// Returns the compact process list.
    #[tool]
    fn get_process_list(&self) -> Result<CallToolResult, String> {
        let payload = state()
            .send(Command::GetProcessList, Duration::from_secs(5))
            .map_err(|e| format!("Process list request failed: {e}"))?;
        Ok(text_result(&String::from_utf8_lossy(&payload)))
    }

    /// Probe the Developer Mode gated usb-debug interface. Returns 'enabled' if reachable; otherwise the
    /// request fails.
    #[tool]
    fn get_developer_mode(&self) -> Result<CallToolResult, String> {
        let payload = state()
            .send(Command::GetDeveloperMode, Duration::from_secs(5))
            .map_err(|e| format!("get_developer_mode request failed: {e}"))?;

        // Wire format: single-byte payload, 0x00 = off, 0x01 = on.
        match payload.first() {
            Some(0) => Ok(text_result("disabled")),
            Some(1) => Ok(text_result("enabled")),
            Some(other) => Err(format!("get_developer_mode: unexpected payload byte 0x{other:02x}")),
            None => Err("get_developer_mode: empty payload".to_string()),
        }
    }

    /// Read the device clock, in UTC. Check this FIRST whenever a publisher certificate is refused
    /// or a sideloaded app will not launch: certificate validity is judged against this clock, and
    /// a Passport that lost backup power reads 2024-01-01, which rejects every valid certificate.
    /// Settings shows the local time zone on top of this UTC value.
    #[tool]
    fn get_system_time(&self) -> Result<CallToolResult, String> {
        let payload = state()
            .send(Command::GetSystemTime, Duration::from_secs(5))
            .map_err(|e| format!("get_system_time request failed: {e}"))?;
        let unix_seconds = decode_system_time(&payload).map_err(|e| format!("get_system_time: {e:#}"))?;
        let formatted = format_utc(unix_seconds).map_err(|e| format!("get_system_time: {e:#}"))?;
        Ok(text_result(&format!("{formatted} (unix {unix_seconds})")))
    }

    /// Set the device clock. Requires Developer Mode and an unlocked device, and is unavailable on
    /// production firmware. Pass an RFC 3339 timestamp, or "now" for this computer's clock.
    ///
    /// The device applies the new time on its next RTC tick, so allow a second before reading it
    /// back with get_system_time; an immediate read still returns the old value.
    ///
    /// A paired Envoy re-syncs the clock on its next message, accepting any forward jump and any
    /// change over 10 minutes, so a clock deliberately set backwards for testing may not stay put.
    #[tool]
    fn set_system_time(
        &self,
        Parameters(params): Parameters<SetSystemTimeParams>,
    ) -> Result<CallToolResult, String> {
        let unix_seconds =
            parse_timestamp(&params.timestamp).map_err(|e| format!("set_system_time: {e:#}"))?;
        state()
            .send(Command::SetSystemTime { unix_seconds }, Duration::from_secs(5))
            .map_err(|e| format!("set_system_time request failed: {e}"))?;
        let formatted = format_utc(unix_seconds).map_err(|e| format!("set_system_time: {e:#}"))?;
        Ok(text_result(&format!("device clock set to {formatted} (unix {unix_seconds})")))
    }

    /// Return the number of currently allowed publisher certificates installed on the device.
    #[tool]
    fn get_allowed_publisher_count(&self) -> Result<CallToolResult, String> {
        let payload = state()
            .send(Command::GetAllowedPublisherCount, Duration::from_secs(5))
            .map_err(|e| format!("get_allowed_publisher_count request failed: {e}"))?;
        let bytes: [u8; 2] = payload.as_slice().try_into().map_err(|_| {
            format!("get_allowed_publisher_count: expected 2 payload bytes, got {}", payload.len())
        })?;
        Ok(text_result(&u16::from_le_bytes(bytes).to_string()))
    }
}

#[tool_handler]
impl ServerHandler for PassportServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// The applet borrows the connection, so it has to be rebuilt for every flash operation.
fn open_flash_applet<'a>(
    state: &'a mut McpState,
    params: &FlashParams,
) -> Result<sambuca::FlashApplet<'a>, String> {
    state
        .require_sambuca()?
        .initialize_flash_applet(
            params.instance,
            params.ioset,
            params.partition,
            params.bus_width,
            params.voltage,
        )
        .map_err(|e| format!("Flash re-init failed: {e}"))
}

fn publisher_certificate_fingerprint(
    certificate_bytes: &[u8],
) -> Result<publisher_fingerprint::PublisherFingerprint, String> {
    let certificate =
        Certificate::from_pem(certificate_bytes).or_else(|_| Certificate::from_der(certificate_bytes));
    let certificate = certificate.map_err(|_| "invalid X.509 publisher certificate".to_string())?;
    let subject_public_key_info = &certificate.tbs_certificate.subject_public_key_info;

    if subject_public_key_info.algorithm.oid != ID_EC_PUBLIC_KEY {
        return Err("publisher certificate does not contain an EC public key".to_string());
    }
    let Some(curve_oid) = subject_public_key_info.algorithm.parameters.as_ref().and_then(|parameters| {
        ObjectIdentifier::try_from(x509_cert::der::asn1::AnyRef::from(parameters)).ok()
    }) else {
        return Err("publisher certificate is missing its EC curve".to_string());
    };
    if curve_oid != SECP256K1 {
        return Err("publisher certificate public key must use secp256k1".to_string());
    }

    let public_key = subject_public_key_info
        .subject_public_key
        .as_bytes()
        .ok_or_else(|| "publisher certificate has an invalid public key bit string".to_string())?;
    let compressed_public_key = secp256k1::PublicKey::from_slice(public_key)
        .map_err(|_| "publisher certificate has an invalid secp256k1 public key".to_string())?
        .serialize();

    publisher_fingerprint::PublisherFingerprint::from_compressed_public_key(&compressed_public_key)
        .map_err(|error| error.to_string())
}

fn format_rapdu(rapdu: &[u8]) -> String {
    let hex: String = rapdu.iter().map(|b| format!("{b:02x}")).collect();
    let sw = match rapdu {
        [.., a, b] => format!("{a:02x}{b:02x}"),
        _ => "(no SW)".to_string(),
    };
    format!("RAPDU ({} bytes, SW={}): {}", rapdu.len(), sw, hex)
}

#[cfg(test)]
mod publisher_certificate_tests {
    use super::*;

    #[test]
    fn fingerprint_matches_device_parser_fixture() {
        let certificate =
            include_bytes!("../../../os/app-manager/testdata/third-party-cert-with-unknown-extension.pem");

        let fingerprint = publisher_certificate_fingerprint(certificate).unwrap();

        assert_eq!(fingerprint.full, "e71fa12f4331c92985e92e7e55b85dd55e75ba22bc192db4e91f202a3f3b9452");
        assert_eq!(fingerprint.short, "e71fa12f…3f3b9452");
    }
}

// MCP server entry point

/// One physical device serves one agent at a time, so a single-threaded runtime is enough for both
/// transports.
fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Failed to build the tokio runtime")
}

/// Serve MCP over stdio until the client disconnects.
pub fn run(jail: Option<PathBuf>) -> Result<()> {
    crate::init_jail(jail)?;
    runtime()?.block_on(async {
        let service = PassportServer.serve(stdio()).await.context("Failed to start the MCP server")?;
        service.waiting().await.context("MCP server failed")?;
        Ok(())
    })
}

/// Serve MCP over Streamable HTTP until the process is killed.
///
/// Runs stateless: each request gets a fresh handler, which is safe because the device state is
/// process-wide rather than per-handler.
pub fn run_http(addr: SocketAddr, jail: Option<PathBuf>) -> Result<()> {
    crate::init_jail(jail)?;
    runtime()?.block_on(async move {
        let config = StreamableHttpServerConfig::default().with_stateful_mode(false).with_json_response(true);
        // Host validation blunts DNS rebinding, but its defaults are loopback only. The address we
        // were told to bind is by definition one clients will ask for, so allow that too.
        let mut allowed_hosts = config.allowed_hosts.clone();
        allowed_hosts.push(addr.to_string());
        let config = config.with_allowed_hosts(allowed_hosts);

        let service = StreamableHttpService::new(
            || Ok(PassportServer),
            Arc::new(LocalSessionManager::default()),
            config,
        );

        let listener = TcpListener::bind(addr).await.with_context(|| format!("Failed to bind {addr}"))?;
        eprintln!("[mcp] serving Streamable HTTP on http://{addr}");

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(accepted) => accepted,
                // A failed accept is transient, a client vanishing mid-handshake or a momentary
                // descriptor shortage, so keep serving. The pause stops an exhausted descriptor
                // table from spinning this loop at full tilt.
                Err(e) => {
                    eprintln!("[mcp] accept failed: {e}");
                    tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                    continue;
                }
            };
            let service = TowerToHyperService::new(service.clone());
            tokio::spawn(async move {
                if let Err(e) = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await {
                    eprintln!("[mcp] connection failed: {e}");
                }
            });
        }
    })
}
