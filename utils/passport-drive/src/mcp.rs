// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! MCP (Model Context Protocol) server mode for passport-drive.
//!
//! Speaks MCP JSON-RPC 2.0 over stdin/stdout.
//! Provides tools for AI integration (Claude Code).

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use hidapi::HidDevice;
use rusb::UsbContext as _;
use serde_json::{json, Value};
use usb_debug_protocol::{Command, LaunchAppResult, LaunchAppStatus, UsbDebugClient};

use crate::{LOG_TERMINATOR, SCREEN_HEIGHT, SCREEN_WIDTH};

const MAX_LOG_LINES: usize = 2000;
const TAP_HOLD_MS: u16 = 50;

// Log drain helper

/// Drain pending log frames from the USB transport and parse 0x1E-terminated
/// records into the MCP state's log buffer.
fn drain_logs(state: &mut McpState) {
    let transport = match state.device.as_ref() {
        Some(t) => t,
        None => return,
    };

    while let Ok(data) = transport.read_logs(Duration::ZERO) {
        for &b in &data {
            if b == LOG_TERMINATOR {
                let text = String::from_utf8_lossy(&state.record_buf).trim_end().to_string();
                state.record_buf.clear();
                if text.is_empty() {
                    continue;
                }
                state.log_buffer.push_back(text);
                while state.log_buffer.len() > MAX_LOG_LINES {
                    state.log_buffer.pop_front();
                }
            } else {
                state.record_buf.push(b);
                if state.record_buf.len() > 16384 {
                    state.record_buf.drain(..state.record_buf.len() - 4096);
                }
            }
        }
    }
}

// MCP state

struct McpState {
    device: Option<UsbDebugClient>,
    log_buffer: VecDeque<String>,
    record_buf: Vec<u8>,
    sambuca: Option<sambuca::Sambuca>,
    flash_params: Option<FlashParams>,
    hid_device: Option<HidDevice>,
}

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

    fn require_device(&self) -> Result<&UsbDebugClient, String> {
        self.device.as_ref().ok_or_else(|| "Not connected. Call connect first.".to_string())
    }

    fn require_sambuca(&mut self) -> Result<&mut sambuca::Sambuca, String> {
        self.sambuca.as_mut().ok_or_else(|| "SAM-BA not connected. Call samba_connect first.".to_string())
    }
}

// Tool definitions

fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_ports",
            "description": "List connected Passport Prime USB devices (normal, Flux/legacy, and SAM-BA modes).",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "connect",
            "description": "Connect to the Passport Prime device over USB vendor interface. Auto-detects the device.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "disconnect",
            "description": "Disconnect from the device.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "get_logs",
            "description": "Get recent log lines streamed from the device.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "max_lines": { "type": "number", "description": "Max lines to return (default 100)" },
                    "filter": { "type": "string", "description": "Optional substring filter" }
                },
                "required": []
            }
        },
        {
            "name": "clear_logs",
            "description": "Clear the in-memory log buffer.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "screenshot",
            "description": format!("Capture a screenshot from the device screen ({SCREEN_WIDTH}×{SCREEN_HEIGHT} px). Returns a base64-encoded PNG."),
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "tap",
            "description": format!("Tap (press + release) at the given screen coordinates. Screen is {SCREEN_WIDTH}×{SCREEN_HEIGHT} px, origin at top-left."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": { "type": "number", "description": "X coordinate (0–479)" },
                    "y": { "type": "number", "description": "Y coordinate (0–799)" }
                },
                "required": ["x", "y"]
            }
        },
        {
            "name": "swipe",
            "description": "Send a timed swipe gesture. Coordinates are physical touch coordinates; the LCD is y=0..799 and the virtual button strip extends below it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "start_x": { "type": "number", "description": "Start X coordinate" },
                    "start_y": { "type": "number", "description": "Start Y coordinate" },
                    "end_x": { "type": "number", "description": "End X coordinate" },
                    "end_y": { "type": "number", "description": "End Y coordinate" },
                    "duration_ms": { "type": "number", "description": "Gesture duration in milliseconds (default 300)" },
                    "steps": { "type": "number", "description": "Number of drag events (default 15)" }
                },
                "required": ["start_x", "start_y", "end_x", "end_y"]
            }
        },
        {
            "name": "power_button",
            "description": "Simulate a short or long power button press on the device.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "long": { "type": "boolean", "description": "true = long press, false = short press" }
                },
                "required": ["long"]
            }
        },
        {
            "name": "send_debug_command",
            "description": "Send a single-byte kernel debug command via USB. Commands: h=help i=irqs m=mmu p=processes t=processes(compact) s=servers c=cache a=appids o=memory-ownership k=consistency-check. Returns the kernel output directly.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Single character, e.g. \"p\" for process list" }
                },
                "required": ["command"]
            }
        },
        {
            "name": "reboot_to_samba",
            "description": "Reboot the device into SAM-BA bootloader mode. Device will disconnect and reappear as SAM-BA device (VID:PID 03eb:6124).",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "input_text",
            "description": "Type text into the focused input field on the device. Injects key press/release events for each character, bypassing the on-screen keyboard.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Text to type into the focused input field" }
                },
                "required": ["text"]
            }
        },
        {
            "name": "launch_app",
            "description": "Launch a Flux app by its 16-byte hex app ID. Returns whether it was launched or already running, plus the PID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "app_id": { "type": "string", "description": "32-character hex app ID (with optional 0x prefix)" }
                },
                "required": ["app_id"]
            }
        },
        {
            "name": "close_app",
            "description": "Close/kill an app by PID. Uses gui-server's graceful close mechanism. Only works for app processes (not system services).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pid": { "type": "number", "description": "Process ID to close" }
                },
                "required": ["pid"]
            }
        },
        {
            "name": "load_app",
            "description": "Upload an arbitrary app directory into keyos/sideloaded-apps/<app-id> on the device over usb-debug. The directory must contain app.elf and manifest.json; icon.bin and resources/ are uploaded when present. Replaces those files if the app already exists.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "app_path": { "type": "string", "description": "Local app bundle directory containing app.elf, manifest.json, and optional icon.bin/resources" }
                },
                "required": ["app_path"]
            }
        },
        // SAM-BA tools
        {
            "name": "samba_list_devices",
            "description": "List SAM-BA bootloader devices (SAMA5D2, VID:PID 03eb:6124). Device must be in SAM-BA mode.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "samba_connect",
            "description": "Connect to a SAM-BA bootloader device. Auto-detects the port.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "samba_disconnect",
            "description": "Disconnect from the SAM-BA device.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "samba_version",
            "description": "Get SAM-BA bootloader version string.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "samba_read_u32",
            "description": "Read a 32-bit value from a memory address.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "address": { "type": "number", "description": "Memory address" }
                },
                "required": ["address"]
            }
        },
        {
            "name": "samba_write_u32",
            "description": "Write a 32-bit value to a memory address.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "address": { "type": "number", "description": "Memory address" },
                    "value": { "type": "number", "description": "Value to write (32-bit)" }
                },
                "required": ["address", "value"]
            }
        },
        {
            "name": "samba_init_flash",
            "description": "Initialize the SDMMC flash applet. Must be called before flash read/write/verify.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "instance": { "type": "number", "description": "SDMMC instance (default: 0)" },
                    "partition": { "type": "number", "description": "Partition number (default: 0)" }
                },
                "required": []
            }
        },
        {
            "name": "samba_flash_info",
            "description": "Get flash applet information (buffer address, buffer size, page size).",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "samba_read_flash",
            "description": "Read data from flash. Requires samba_init_flash first. Returns base64-encoded data.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "offset": { "type": "number", "description": "Byte offset in flash (page-aligned)" },
                    "length": { "type": "number", "description": "Number of bytes to read (page-aligned)" }
                },
                "required": ["offset", "length"]
            }
        },
        {
            "name": "samba_write_flash",
            "description": "Write data to flash. Requires samba_init_flash first. Data must be base64-encoded.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "offset": { "type": "number", "description": "Byte offset in flash (page-aligned)" },
                    "data_base64": { "type": "string", "description": "Data to write (base64-encoded)" }
                },
                "required": ["offset", "data_base64"]
            }
        },
        {
            "name": "samba_verify_flash",
            "description": "Verify flash contents against provided data. Returns true if match.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "offset": { "type": "number", "description": "Byte offset in flash" },
                    "data_base64": { "type": "string", "description": "Expected data (base64-encoded)" }
                },
                "required": ["offset", "data_base64"]
            }
        },
        {
            "name": "samba_reboot",
            "description": "Reboot the device to normal mode (exit SAM-BA mode).",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        // HID APDU tool
        {
            "name": "send_apdu",
            "description": "Send an ISO 7816 APDU over USB HID. Auto-detects CTAP/FIDO mode (VID=0x1307, CTAPHID_MSG framing) or Ledger mode (VID=0x2c97, Ledger HID framing). Returns hex-encoded RAPDU.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "apdu_hex": { "type": "string", "description": "Hex-encoded APDU bytes, e.g. '0003000000' for U2F_VERSION" }
                },
                "required": ["apdu_hex"]
            }
        },
        // Device info
        {
            "name": "get_version",
            "description": "Get the KeyOS version string running on the device (same value shown on Settings → About → KeyOS, e.g. \"1.3.0\"). Useful for SDK compatibility checks.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        // Process monitoring
        {
            "name": "get_process_list",
            "description": "Get the list of running processes on the device with PID, name, CPU%, RAM usage, and thread states. Returns the compact process list.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        // Developer Mode probe (used by `foundation sideload` to fail early
        // when the gated usb-debug interface is not reachable).
        {
            "name": "get_developer_mode",
            "description": "Probe the Developer Mode gated usb-debug interface. Returns 'enabled' if reachable; otherwise the request fails.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        // Kernel debug
        {
            "name": "send_kernel_command",
            "description": "Send a single-byte kernel debug command via USB and return the output. Commands: h=help, i=IRQ stats, m=MMU state, p=process list (verbose), t=process list (compact), s=server list, c=cache stats, a=app IDs (maps app IDs to PIDs), o=memory ownership, k=consistency check. Note: 't' output is also available via get_process_list.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Single character kernel debug command (h/i/m/p/t/s/c/a/o/k)" }
                },
                "required": ["command"]
            }
        }
    ])
}

// Result helpers

fn text_result(s: &str) -> Value { json!({ "content": [{ "type": "text", "text": s }] }) }

fn image_result(base64_data: &str) -> Value {
    json!({ "content": [{ "type": "image", "data": base64_data, "mimeType": "image/png" }] })
}

fn error_result(msg: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": format!("Error: {msg}") }], "isError": true })
}

fn format_rapdu(rapdu: &[u8]) -> Value {
    let hex: String = rapdu.iter().map(|b| format!("{b:02x}")).collect();
    let sw = if rapdu.len() >= 2 {
        format!("{:02x}{:02x}", rapdu[rapdu.len() - 2], rapdu[rapdu.len() - 1])
    } else {
        "(no SW)".to_string()
    };
    text_result(&format!("RAPDU ({} bytes, SW={}): {}", rapdu.len(), sw, hex))
}

fn bgra_to_png_base64(bgra: &[u8]) -> Result<String, String> {
    let png_data = crate::screenshot::bgra_to_png(bgra)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&png_data))
}

// Tool dispatch

fn handle_tool(state: &mut McpState, name: &str, args: &Value) -> Value {
    match name {
        "list_ports" => handle_list_ports(),
        "connect" => handle_connect(state, args),
        "disconnect" => handle_disconnect(state),
        "get_logs" => handle_get_logs(state, args),
        "clear_logs" => handle_clear_logs(state),
        "screenshot" => handle_screenshot(state, args),
        "tap" => handle_tap(state, args),
        "swipe" => handle_swipe(state, args),
        "power_button" => handle_power_button(state, args),
        "send_debug_command" => handle_send_debug_command(state, args),
        "send_kernel_command" => handle_send_kernel_command(state, args),
        "reboot_to_samba" => handle_reboot_to_samba(state, args),
        "input_text" => handle_input_text(state, args),
        "launch_app" => handle_launch_app(state, args),
        "close_app" => handle_close_app(state, args),
        "load_app" => handle_load_app(state, args),
        "samba_list_devices" => handle_samba_list_devices(),
        "samba_connect" => handle_samba_connect(state),
        "samba_disconnect" => handle_samba_disconnect(state),
        "samba_version" => handle_samba_version(state),
        "samba_read_u32" => handle_samba_read_u32(state, args),
        "samba_write_u32" => handle_samba_write_u32(state, args),
        "samba_init_flash" => handle_samba_init_flash(state, args),
        "samba_flash_info" => handle_samba_flash_info(state),
        "samba_read_flash" => handle_samba_read_flash(state, args),
        "samba_write_flash" => handle_samba_write_flash(state, args),
        "samba_verify_flash" => handle_samba_verify_flash(state, args),
        "samba_reboot" => handle_samba_reboot(state),
        "send_apdu" => handle_send_apdu(state, args),
        "get_process_list" => handle_get_process_list(state),
        "get_version" => handle_get_version(state),
        "get_developer_mode" => handle_get_developer_mode(state),
        _ => error_result(&format!("Unknown tool: {name}")),
    }
}

// Runtime tool handlers

fn handle_list_ports() -> Value {
    let context = match rusb::Context::new() {
        Ok(context) => context,
        Err(e) => return error_result(&format!("Failed to initialize USB context: {e}")),
    };
    let devices = match context.devices() {
        Ok(d) => d,
        Err(e) => return error_result(&format!("Failed to enumerate USB devices: {e}")),
    };

    let mut lines = Vec::new();
    for dev in devices.iter() {
        if let Ok(desc) = dev.device_descriptor() {
            let vid = desc.vendor_id();
            let pid = desc.product_id();
            let label = match (vid, pid) {
                (0x1307, 0x0165) => "Passport Prime",
                (0x2c97, 0x0007) => "Passport Prime (Flux/legacy)",
                (0x03eb, 0x6124) => "SAM-BA bootloader",
                _ => continue,
            };
            lines.push(format!(
                "Bus {:03} Device {:03} — {label} (VID:{vid:04x} PID:{pid:04x})",
                dev.bus_number(),
                dev.address()
            ));
        }
    }

    if lines.is_empty() {
        text_result("No Passport Prime USB devices found.")
    } else {
        text_result(&lines.join("\n"))
    }
}

fn handle_connect(state: &mut McpState, _args: &Value) -> Value {
    if state.device.is_some() {
        return error_result("Already connected. Call disconnect first.");
    }

    match UsbDebugClient::open() {
        Ok(client) => {
            state.device = Some(client);
            text_result("Connected via USB vendor interface. Logs and debug commands on single interface.")
        }
        Err(e) => error_result(&format!("Failed to connect: {e}")),
    }
}

fn handle_disconnect(state: &mut McpState) -> Value {
    state.device.take();
    state.log_buffer.clear();
    state.record_buf.clear();
    text_result("Disconnected.")
}

fn handle_get_logs(state: &mut McpState, args: &Value) -> Value {
    drain_logs(state);

    let max_lines = args.get("max_lines").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
    let filter = args.get("filter").and_then(|v| v.as_str());

    let iter = state.log_buffer.iter().filter(|l| filter.map_or(true, |f| l.contains(f)));
    let logs: Vec<String> = if max_lines > 0 {
        iter.rev().take(max_lines).cloned().collect::<Vec<_>>().into_iter().rev().collect()
    } else {
        iter.cloned().collect()
    };

    if logs.is_empty() {
        return text_result("(no logs)");
    }
    text_result(&logs.join("\n"))
}

fn handle_clear_logs(state: &mut McpState) -> Value {
    // Drain pending logs first, then clear everything.
    if let Some(transport) = &state.device {
        while transport.read_logs(Duration::ZERO).is_ok() {}
    }
    state.log_buffer.clear();
    state.record_buf.clear();
    text_result("Log buffer cleared.")
}

fn handle_screenshot(state: &McpState, _args: &Value) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    let payload = match dev.send(Command::Screenshot, Duration::from_secs(20)) {
        Ok(p) => p,
        Err(e) => return error_result(&format!("Screenshot failed: {e}")),
    };

    match bgra_to_png_base64(&payload) {
        Ok(b64) => image_result(&b64),
        Err(e) => error_result(&e),
    }
}

fn handle_tap(state: &McpState, args: &Value) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    let x = match args.get("x").and_then(|v| v.as_u64()) {
        Some(v) => v as u16,
        None => return error_result("Missing required parameter: x"),
    };
    let y = match args.get("y").and_then(|v| v.as_u64()) {
        Some(v) => v as u16,
        None => return error_result("Missing required parameter: y"),
    };
    if let Err(e) = dev.send(
        Command::Swipe { start_x: x, start_y: y, end_x: x, end_y: y, duration_ms: TAP_HOLD_MS, steps: 0 },
        Duration::from_millis(u64::from(TAP_HOLD_MS) + 5_000),
    ) {
        return error_result(&format!("Tap failed: {e}"));
    }

    text_result(&format!("Tapped at ({x}, {y})."))
}

fn required_u16_arg(args: &Value, name: &str) -> Result<u16, String> {
    let value = args
        .get(name)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| format!("Missing required parameter: {name}"))?;
    u16::try_from(value).map_err(|_| format!("{name} must fit in u16"))
}

fn optional_u16_arg(args: &Value, name: &str, default: u16) -> Result<u16, String> {
    let Some(value) = args.get(name).and_then(|v| v.as_u64()) else {
        return Ok(default);
    };
    u16::try_from(value).map_err(|_| format!("{name} must fit in u16"))
}

fn optional_u8_arg(args: &Value, name: &str, default: u8) -> Result<u8, String> {
    let Some(value) = args.get(name).and_then(|v| v.as_u64()) else {
        return Ok(default);
    };
    u8::try_from(value).map_err(|_| format!("{name} must fit in u8"))
}

fn handle_swipe(state: &McpState, args: &Value) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    let start_x = match required_u16_arg(args, "start_x") {
        Ok(v) => v,
        Err(e) => return error_result(&e),
    };
    let start_y = match required_u16_arg(args, "start_y") {
        Ok(v) => v,
        Err(e) => return error_result(&e),
    };
    let end_x = match required_u16_arg(args, "end_x") {
        Ok(v) => v,
        Err(e) => return error_result(&e),
    };
    let end_y = match required_u16_arg(args, "end_y") {
        Ok(v) => v,
        Err(e) => return error_result(&e),
    };
    let duration_ms = match optional_u16_arg(args, "duration_ms", 300) {
        Ok(v) => v,
        Err(e) => return error_result(&e),
    };
    let steps = match optional_u8_arg(args, "steps", 15) {
        Ok(v) => v,
        Err(e) => return error_result(&e),
    };

    if let Err(e) = dev.send(
        Command::Swipe { start_x, start_y, end_x, end_y, duration_ms, steps },
        Duration::from_millis(u64::from(duration_ms) + 5_000),
    ) {
        return error_result(&format!("Swipe failed: {e}"));
    }

    text_result(&format!(
        "Swipe ({start_x}, {start_y}) -> ({end_x}, {end_y}) over {duration_ms}ms with {steps} steps."
    ))
}

fn handle_power_button(state: &McpState, args: &Value) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    let long = match args.get("long").and_then(|v| v.as_bool()) {
        Some(p) => p,
        None => return error_result("Missing required parameter: long"),
    };

    if let Err(e) = dev.send(Command::PowerButton { long }, Duration::from_secs(5)) {
        return error_result(&format!("Power button failed: {e}"));
    }

    text_result(&format!("Power button {} press.", if long { "long" } else { "short" }))
}

fn handle_input_text(state: &McpState, args: &Value) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return error_result("Missing required parameter: text"),
    };
    match dev.send(Command::InputText(text.to_string()), Duration::from_secs(10)) {
        Ok(_) => text_result(&format!("Typed {} character(s).", text.chars().count())),
        Err(e) => error_result(&format!("Input text failed: {e}")),
    }
}

fn handle_send_debug_command(state: &McpState, args: &Value) -> Value {
    // Forward to send_kernel_command for backwards compatibility.
    handle_send_kernel_command(state, args)
}

fn handle_send_kernel_command(state: &McpState, args: &Value) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    let command = match args.get("command").and_then(|v| v.as_str()) {
        Some(c) if c.len() == 1 => c.as_bytes()[0],
        _ => return error_result("command must be a single character (h/i/m/p/t/s/c/a/o/k)"),
    };

    match dev.send(Command::KernelCmd { cmd_byte: command }, Duration::from_secs(5)) {
        Ok(payload) => text_result(&String::from_utf8_lossy(&payload)),
        Err(e) => error_result(&format!("Kernel command failed: {e}")),
    }
}

fn handle_get_version(state: &McpState) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    match dev.send(Command::GetVersion, Duration::from_secs(5)) {
        Ok(payload) => text_result(&String::from_utf8_lossy(&payload)),
        Err(e) => error_result(&format!("get_version request failed: {e}")),
    }
}

fn handle_get_developer_mode(state: &McpState) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    match dev.send(Command::GetDeveloperMode, Duration::from_secs(5)) {
        Ok(payload) => {
            // Wire format: single-byte payload, 0x00 = off, 0x01 = on.
            // Anything else is a protocol-level violation; treat as error.
            match payload.first().copied() {
                Some(0) => text_result("disabled"),
                Some(1) => text_result("enabled"),
                Some(other) => {
                    error_result(&format!("get_developer_mode: unexpected payload byte 0x{other:02x}"))
                }
                None => error_result("get_developer_mode: empty payload"),
            }
        }
        Err(e) => error_result(&format!("get_developer_mode request failed: {e}")),
    }
}

fn handle_get_process_list(state: &McpState) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    match dev.send(Command::GetProcessList, Duration::from_secs(5)) {
        Ok(payload) => text_result(&String::from_utf8_lossy(&payload)),
        Err(e) => error_result(&format!("Process list request failed: {e}")),
    }
}

fn handle_reboot_to_samba(state: &mut McpState, _args: &Value) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    let _ = dev.send(Command::RebootSamba, Duration::from_secs(5));
    state.device.take();

    text_result("Device rebooting to SAM-BA mode. Use samba_connect to connect to it.")
}

fn handle_launch_app(state: &McpState, args: &Value) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    let hex_str = match args.get("app_id").and_then(|v| v.as_str()) {
        Some(s) => s.trim().strip_prefix("0x").unwrap_or(s.trim()),
        None => return error_result("app_id is required (32-character hex string)"),
    };

    let bytes = match hex::decode(hex_str) {
        Ok(b) if b.len() == 16 => {
            let mut arr = [0u8; 16];
            arr.copy_from_slice(&b);
            arr
        }
        Ok(b) => {
            return error_result(&format!("app_id must be 16 bytes (32 hex chars), got {} bytes", b.len()))
        }
        Err(e) => return error_result(&format!("Invalid hex app_id: {e}")),
    };

    match dev.send(Command::LaunchApp { app_id: bytes }, Duration::from_secs(10)) {
        Ok(payload) => match LaunchAppResult::decode(&payload) {
            Ok(result) => match result.status {
                LaunchAppStatus::Launched => {
                    text_result(&format!("App launched successfully with PID {}", result.pid))
                }
                LaunchAppStatus::AlreadyRunning => text_result(&format!(
                    "App is already running with PID {}. Newly uploaded code will not run until the app is closed and launched again.",
                    result.pid
                )),
            },
            Err(e) => error_result(&format!("Invalid launch_app response: {e}")),
        },
        Err(e) => error_result(&format!("Failed to launch app: {e}")),
    }
}

fn handle_close_app(state: &McpState, args: &Value) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    let pid = match args.get("pid").and_then(|v| v.as_u64()) {
        Some(p) if p > 0 && p <= 0xFFFF => p as u16,
        _ => return error_result("pid must be a positive integer (1-65535)"),
    };

    match dev.send(Command::CloseApp { pid }, Duration::from_secs(5)) {
        Ok(_) => text_result(&format!("Process {pid} close requested successfully")),
        Err(e) => error_result(&format!("Failed to close app: {e}")),
    }
}

fn handle_load_app(state: &McpState, args: &Value) -> Value {
    let dev = match state.require_device() {
        Ok(d) => d,
        Err(e) => return error_result(&e),
    };

    let app_path = match args.get("app_path").and_then(|v| v.as_str()) {
        Some(path) => PathBuf::from(path),
        None => return error_result("Missing required parameter: app_path"),
    };

    match crate::load_app::load_app(dev, &app_path) {
        Ok(report) => text_result(&format!(
            "Loaded {} into keyos/sideloaded-apps/{} (app.elf: {} bytes, manifest.json: {} bytes, icon.bin: {} bytes, resources: {} files / {} bytes).",
            report.app_id,
            report.app_id,
            report.elf_bytes,
            report.manifest_bytes,
            report.icon_bytes.unwrap_or(0),
            report.resource_files,
            report.resource_bytes
        )),
        Err(e) => error_result(&format!("load_app failed: {e:#}")),
    }
}

// HID APDU handler

fn handle_send_apdu(state: &mut McpState, args: &Value) -> Value {
    let apdu_hex = match args.get("apdu_hex").and_then(|v| v.as_str()) {
        Some(h) => h.trim(),
        None => return error_result("Missing required parameter: apdu_hex"),
    };
    let timeout_ms: i32 = 10000;

    // Parse hex string into bytes
    let hex_clean: String = apdu_hex.chars().filter(|c| !c.is_whitespace()).collect();
    if hex_clean.len() % 2 != 0 {
        return error_result("apdu_hex must have an even number of hex characters");
    }
    let apdu_bytes: Vec<u8> = match (0..hex_clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex_clean[i..i + 2], 16))
        .collect::<std::result::Result<Vec<u8>, _>>()
    {
        Ok(b) => b,
        Err(e) => return error_result(&format!("Invalid hex in apdu_hex: {e}")),
    };

    if apdu_bytes.len() < 4 {
        return error_result("APDU must be at least 4 bytes (CLA INS P1 P2)");
    }

    // Auto-open HID device on first call (or reopen if stale)
    if state.hid_device.is_none() {
        match crate::hid::open_hid() {
            Ok((dev, mode)) => {
                let mode_str = match mode {
                    crate::hid::HidMode::Ledger => "Ledger",
                    crate::hid::HidMode::Fido => "CTAP/FIDO",
                };
                eprintln!("[mcp] HID device opened in {mode_str} mode");
                state.hid_device = Some(dev);
            }
            Err(e) => return error_result(&format!("Failed to open HID device: {e}")),
        }
    }

    let device = state.hid_device.as_ref().unwrap();
    match crate::hid::exchange_apdu(device, &apdu_bytes, timeout_ms) {
        Ok(rapdu) => format_rapdu(&rapdu),
        Err(_) => {
            // Handle might be stale — retry once with a fresh device.
            state.hid_device = None;
            match crate::hid::open_hid() {
                Ok((dev, _)) => state.hid_device = Some(dev),
                Err(e) => return error_result(&format!("Failed to reopen HID device: {e}")),
            }
            let device = state.hid_device.as_ref().unwrap();
            match crate::hid::exchange_apdu(device, &apdu_bytes, timeout_ms) {
                Ok(rapdu) => format_rapdu(&rapdu),
                Err(e) => error_result(&format!("APDU exchange failed: {e}")),
            }
        }
    }
}

// SAM-BA tool handlers

fn handle_samba_list_devices() -> Value {
    let context = match rusb::Context::new() {
        Ok(context) => context,
        Err(e) => return error_result(&format!("Failed to initialize USB context: {e}")),
    };
    let devices = match context.devices() {
        Ok(d) => d,
        Err(e) => return error_result(&format!("Failed to enumerate USB devices: {e}")),
    };

    let samba: Vec<String> = devices
        .iter()
        .filter_map(|dev| {
            let desc = dev.device_descriptor().ok()?;
            if desc.vendor_id() == 0x03eb && desc.product_id() == 0x6124 {
                Some(format!(
                    "Bus {:03} Device {:03} (VID:{:04x} PID:{:04x})",
                    dev.bus_number(),
                    dev.address(),
                    desc.vendor_id(),
                    desc.product_id()
                ))
            } else {
                None
            }
        })
        .collect();

    if samba.is_empty() {
        text_result("No SAM-BA devices found. Is the device in bootloader mode?")
    } else {
        text_result(&samba.join("\n"))
    }
}

fn handle_samba_connect(state: &mut McpState) -> Value {
    if state.sambuca.is_some() {
        return error_result("Already connected to SAM-BA. Call samba_disconnect first.");
    }

    match sambuca::Sambuca::new() {
        Ok(s) => {
            state.sambuca = Some(s);
            text_result("Connected to SAM-BA device.")
        }
        Err(e) => error_result(&format!("Failed to connect to SAM-BA device: {e}")),
    }
}

fn handle_samba_disconnect(state: &mut McpState) -> Value {
    state.sambuca = None;
    state.flash_params = None;
    text_result("Disconnected from SAM-BA device.")
}

fn handle_samba_version(state: &mut McpState) -> Value {
    let sambuca = match state.require_sambuca() {
        Ok(s) => s,
        Err(e) => return error_result(&e),
    };
    match sambuca.version() {
        Ok(v) => text_result(&format!("SAM-BA version: {v}")),
        Err(e) => error_result(&format!("Failed to read version: {e}")),
    }
}

fn handle_samba_read_u32(state: &mut McpState, args: &Value) -> Value {
    let address = match args.get("address").and_then(|v| v.as_u64()) {
        Some(a) => a as u32,
        None => return error_result("Missing required parameter: address"),
    };

    let sambuca = match state.require_sambuca() {
        Ok(s) => s,
        Err(e) => return error_result(&e),
    };

    match sambuca.read_u32(address) {
        Ok(val) => text_result(&format!("0x{address:08x}: 0x{val:08x} ({val})")),
        Err(e) => error_result(&format!("Read failed: {e}")),
    }
}

fn handle_samba_write_u32(state: &mut McpState, args: &Value) -> Value {
    let address = match args.get("address").and_then(|v| v.as_u64()) {
        Some(a) => a as u32,
        None => return error_result("Missing required parameter: address"),
    };
    let value = match args.get("value").and_then(|v| v.as_u64()) {
        Some(v) => v as u32,
        None => return error_result("Missing required parameter: value"),
    };

    let sambuca = match state.require_sambuca() {
        Ok(s) => s,
        Err(e) => return error_result(&e),
    };

    match sambuca.write_u32(address, value) {
        Ok(()) => text_result(&format!("Wrote 0x{value:08x} to 0x{address:08x}.")),
        Err(e) => error_result(&format!("Write failed: {e}")),
    }
}

fn handle_samba_init_flash(state: &mut McpState, args: &Value) -> Value {
    let instance = args.get("instance").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let partition = args.get("partition").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

    let sambuca = match state.require_sambuca() {
        Ok(s) => s,
        Err(e) => return error_result(&e),
    };

    let params = FlashParams { instance, ioset: 1, partition, bus_width: 8, voltage: 3 };

    match sambuca.initialize_flash_applet(
        params.instance,
        params.ioset,
        params.partition,
        params.bus_width,
        params.voltage,
    ) {
        Ok(_applet) => {
            let msg = format!(
                "Flash applet initialized.\n  Instance: {}\n  IO set: {}\n  Partition: {}\n  Bus width: {}\n  Voltage: {}",
                params.instance, params.ioset, params.partition, params.bus_width, params.voltage,
            );
            state.flash_params = Some(params);
            text_result(&msg)
        }
        Err(e) => error_result(&format!("Flash init failed: {e}")),
    }
}

fn handle_samba_flash_info(state: &mut McpState) -> Value {
    match &state.flash_params {
        Some(params) => {
            text_result(&format!(
                "Flash applet initialized:\n  Instance: {}\n  IO set: {}\n  Partition: {}\n  Bus width: {}\n  Voltage: {}",
                params.instance, params.ioset, params.partition, params.bus_width, params.voltage,
            ))
        }
        None => text_result("Flash applet not initialized. Call samba_init_flash first."),
    }
}

/// Copy flash params out of state so we can mutably borrow sambuca without conflicts.
fn copy_flash_params(state: &McpState) -> Option<FlashParams> {
    state.flash_params.as_ref().map(|p| FlashParams {
        instance: p.instance,
        ioset: p.ioset,
        partition: p.partition,
        bus_width: p.bus_width,
        voltage: p.voltage,
    })
}

fn handle_samba_read_flash(state: &mut McpState, args: &Value) -> Value {
    let offset = match args.get("offset").and_then(|v| v.as_u64()) {
        Some(o) => o,
        None => return error_result("Missing required parameter: offset"),
    };
    let length = match args.get("length").and_then(|v| v.as_u64()) {
        Some(l) => l as usize,
        None => return error_result("Missing required parameter: length"),
    };

    let params = match copy_flash_params(state) {
        Some(p) => p,
        None => return error_result("Flash not initialized. Call samba_init_flash first."),
    };
    let sambuca = match state.require_sambuca() {
        Ok(s) => s,
        Err(e) => return error_result(&e),
    };

    let mut applet = match sambuca.initialize_flash_applet(
        params.instance,
        params.ioset,
        params.partition,
        params.bus_width,
        params.voltage,
    ) {
        Ok(a) => a,
        Err(e) => return error_result(&format!("Flash re-init failed: {e}")),
    };

    let mut buf = Vec::new();
    match applet.read_flash(offset, length, &mut buf, |_| {}) {
        Ok(()) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
            text_result(&format!("Read {} bytes from offset 0x{offset:x}.\nData (base64): {b64}", buf.len()))
        }
        Err(e) => error_result(&format!("Flash read failed: {e}")),
    }
}

fn handle_samba_write_flash(state: &mut McpState, args: &Value) -> Value {
    let offset = match args.get("offset").and_then(|v| v.as_u64()) {
        Some(o) => o,
        None => return error_result("Missing required parameter: offset"),
    };
    let data_b64 = match args.get("data_base64").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return error_result("Missing required parameter: data_base64"),
    };

    let data = match base64::engine::general_purpose::STANDARD.decode(data_b64) {
        Ok(d) => d,
        Err(e) => return error_result(&format!("Invalid base64: {e}")),
    };

    let params = match copy_flash_params(state) {
        Some(p) => p,
        None => return error_result("Flash not initialized. Call samba_init_flash first."),
    };
    let sambuca = match state.require_sambuca() {
        Ok(s) => s,
        Err(e) => return error_result(&e),
    };

    let mut applet = match sambuca.initialize_flash_applet(
        params.instance,
        params.ioset,
        params.partition,
        params.bus_width,
        params.voltage,
    ) {
        Ok(a) => a,
        Err(e) => return error_result(&format!("Flash re-init failed: {e}")),
    };

    match applet.write_flash(offset, &data, |_| {}) {
        Ok(()) => text_result(&format!("Wrote {} bytes to offset 0x{offset:x}.", data.len())),
        Err(e) => error_result(&format!("Flash write failed: {e}")),
    }
}

fn handle_samba_verify_flash(state: &mut McpState, args: &Value) -> Value {
    let offset = match args.get("offset").and_then(|v| v.as_u64()) {
        Some(o) => o,
        None => return error_result("Missing required parameter: offset"),
    };
    let data_b64 = match args.get("data_base64").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return error_result("Missing required parameter: data_base64"),
    };

    let data = match base64::engine::general_purpose::STANDARD.decode(data_b64) {
        Ok(d) => d,
        Err(e) => return error_result(&format!("Invalid base64: {e}")),
    };

    let params = match copy_flash_params(state) {
        Some(p) => p,
        None => return error_result("Flash not initialized. Call samba_init_flash first."),
    };
    let sambuca = match state.require_sambuca() {
        Ok(s) => s,
        Err(e) => return error_result(&e),
    };

    let mut applet = match sambuca.initialize_flash_applet(
        params.instance,
        params.ioset,
        params.partition,
        params.bus_width,
        params.voltage,
    ) {
        Ok(a) => a,
        Err(e) => return error_result(&format!("Flash re-init failed: {e}")),
    };

    match applet.verify_flash(offset, &data, |_| {}, true) {
        Ok(stats) => {
            if stats.num_chunks_patched == 0 {
                text_result("Verification PASSED: flash matches data.")
            } else {
                text_result(&format!(
                    "Verification FAILED: patched {} chunk(s) ({} attempts).",
                    stats.num_chunks_patched, stats.num_attempts
                ))
            }
        }
        Err(e) => error_result(&format!("Verify failed: {e}")),
    }
}

fn handle_samba_reboot(state: &mut McpState) -> Value {
    let sambuca = match state.require_sambuca() {
        Ok(s) => s,
        Err(e) => return error_result(&e),
    };

    // Reset boot bits and kick reset controller
    if let Err(e) = sambuca.write_u32(0xF804_8054, 0x6683_0000) {
        return error_result(&format!("Failed to reset boot bits: {e}"));
    }
    if let Err(e) = sambuca.write_u32(0xF804_8000, 0xA500_0001) {
        return error_result(&format!("Failed to kick reset controller: {e}"));
    }

    state.sambuca = None;
    state.flash_params = None;
    text_result("Device rebooting to normal mode.")
}

// MCP server main loop

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StdioMode {
    Unknown,
    Line,
    Framed,
}

fn read_stdio_message<R: BufRead>(reader: &mut R, mode: &mut StdioMode) -> Result<Option<String>> {
    loop {
        match *mode {
            StdioMode::Line => return read_line_message(reader),
            StdioMode::Framed => return read_framed_message(reader),
            StdioMode::Unknown => {
                let buf = reader.fill_buf().context("Failed to read stdin")?;
                if buf.is_empty() {
                    return Ok(None);
                }

                let whitespace_len = buf.iter().take_while(|b| b.is_ascii_whitespace()).count();
                if whitespace_len > 0 {
                    reader.consume(whitespace_len);
                    continue;
                }

                *mode = if matches!(buf[0], b'{' | b'[') { StdioMode::Line } else { StdioMode::Framed };
            }
        }
    }
}

fn read_line_message<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line).context("Failed to read stdin")?;
        if bytes == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            return Ok(Some(line.clone()));
        }
    }
}

fn read_framed_message<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line).context("Failed to read MCP headers")?;
        if bytes == 0 {
            if content_length.is_some() {
                anyhow::bail!("Unexpected EOF while reading MCP headers");
            }
            return Ok(None);
        }

        let header = line.trim_end_matches(&['\r', '\n'][..]);
        if header.is_empty() {
            break;
        }

        let (name, value) =
            header.split_once(':').with_context(|| format!("Malformed MCP header: {header}"))?;
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .trim()
                    .parse()
                    .with_context(|| format!("Invalid MCP Content-Length header: {}", value.trim()))?,
            );
        }
    }

    let len = content_length.context("Missing MCP Content-Length header")?;
    let mut body = vec![0; len];
    reader.read_exact(&mut body).context("Failed to read MCP body")?;
    String::from_utf8(body).context("MCP body was not valid UTF-8").map(Some)
}

fn write_stdio_response<W: Write>(writer: &mut W, mode: StdioMode, response: &Value) -> Result<()> {
    let body = serde_json::to_vec(response).context("Failed to encode response")?;
    match mode {
        StdioMode::Framed => {
            write!(writer, "Content-Length: {}\r\n\r\n", body.len()).context("Failed to write MCP header")?;
            writer.write_all(&body).context("Failed to write MCP response")?;
        }
        StdioMode::Line | StdioMode::Unknown => {
            writer.write_all(&body).context("Failed to write response")?;
            writer.write_all(b"\n").context("Failed to write newline")?;
        }
    }
    writer.flush().context("Failed to flush stdout")
}

pub fn run() -> Result<()> {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout();
    let mut mode = StdioMode::Unknown;
    let mut state = McpState::new();

    while let Some(message) = read_stdio_message(&mut reader, &mut mode)? {
        let request: Value = match serde_json::from_str(&message) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[mcp] Invalid JSON: {e}");
                continue;
            }
        };

        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = request.get("id").cloned();

        // Notifications (no id) — don't send a response
        if id.is_none() {
            // e.g. "notifications/initialized", "notifications/cancelled"
            continue;
        }

        let id = id.unwrap();
        let params = request.get("params").cloned().unwrap_or(json!({}));

        let response = match method {
            "initialize" => {
                // Echo back the protocol version from the client
                let protocol_version =
                    params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("2024-11-05");
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": protocol_version,
                        "capabilities": {
                            "tools": {}
                        },
                        "serverInfo": {
                            "name": "passport-drive",
                            "version": "0.1.0"
                        }
                    }
                })
            }

            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),

            "tools/list" => {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": tool_definitions()
                    }
                })
            }

            "tools/call" => {
                let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let tool_args = params.get("arguments").cloned().unwrap_or(json!({}));
                let result = handle_tool(&mut state, tool_name, &tool_args);
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                })
            }

            "resources/list" => {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "resources": [] }
                })
            }

            "prompts/list" => {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "prompts": [] }
                })
            }

            _ => {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Method not found: {method}")
                    }
                })
            }
        };

        write_stdio_response(&mut stdout, mode, &response)?;
    }

    // Clean up
    state.device.take();

    Ok(())
}
