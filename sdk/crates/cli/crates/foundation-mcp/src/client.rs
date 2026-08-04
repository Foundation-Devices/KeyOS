// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Minimal JSON-RPC client for the passport-drive MCP server.

use std::{
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub struct PassportDriveMcpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl PassportDriveMcpClient {
    pub fn connect() -> Result<Self> {
        let candidates = passport_drive_candidates();
        let mut not_found = Vec::new();

        for candidate in candidates {
            match Self::spawn_candidate(&candidate) {
                Ok(mut client) => {
                    // Surface a progress message if the device doesn't respond
                    // within a second so the CLI doesn't appear to hang silently.
                    // (Full kill-on-timeout requires async or shared Child + signal
                    // handling — see the H5 follow-up note in the review.)
                    let done = Arc::new(AtomicBool::new(false));
                    let watchdog_done = done.clone();
                    thread::spawn(move || {
                        thread::sleep(Duration::from_secs(1));
                        if !watchdog_done.load(Ordering::Acquire) {
                            eprintln!("waiting for Passport Prime to respond...");
                        }
                    });
                    let result = (|| -> Result<()> {
                        client.initialize()?;
                        client.call_tool("connect", json!({}))?;
                        Ok(())
                    })();
                    done.store(true, Ordering::Release);
                    result?;
                    return Ok(client);
                }
                Err(error) if is_not_found(&error) => {
                    not_found.push(candidate.to_string_lossy().to_string());
                }
                Err(error) => return Err(error),
            }
        }

        bail!("could not start passport-drive MCP server; tried {}", not_found.join(", "));
    }

    pub fn launch_app(&mut self, app_id: &[u8; 16]) -> Result<String> {
        let result = self.call_tool("launch_app", json!({ "app_id": encode_hex(app_id) }))?;
        Ok(tool_text(&result).unwrap_or_else(|| result.to_string()))
    }

    pub fn load_app(&mut self, app_path: &Path) -> Result<String> {
        let result = self.call_tool("load_app", json!({ "app_path": app_path.display().to_string() }))?;
        Ok(tool_text(&result).unwrap_or_else(|| result.to_string()))
    }

    pub fn install_certificate(
        &mut self,
        certificate_path: &Path,
        expected_fingerprint: &str,
    ) -> Result<String> {
        let result = self.call_tool(
            "install_certificate",
            json!({
                "certificate_path": certificate_path.display().to_string(),
                "expected_fingerprint": expected_fingerprint,
            }),
        )?;
        Ok(tool_text(&result).unwrap_or_else(|| result.to_string()))
    }

    pub fn allowed_publisher_count(&mut self) -> Result<u16> {
        let result = self.call_tool("get_allowed_publisher_count", json!({}))?;
        let text = tool_text(&result).context("publisher-count query returned an empty reply")?;
        text.trim().parse::<u16>().with_context(|| format!("unexpected publisher-count reply: {text:?}"))
    }

    pub fn ensure_allowed_publisher_installed(&mut self) -> Result<()> {
        let count = self.allowed_publisher_count()?;
        if count == 0 {
            bail!("no allowed publisher certificate is installed on the device");
        }
        Ok(())
    }

    fn spawn_candidate(command: &OsStr) -> Result<Self> {
        let mut child = Command::new(command)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("starting {} mcp", command.to_string_lossy()))?;

        let stdin = child.stdin.take().context("passport-drive MCP stdin unavailable")?;
        let stdout = child.stdout.take().context("passport-drive MCP stdout unavailable")?;

        Ok(Self { child, stdin, stdout: BufReader::new(stdout), next_id: 1 })
    }

    fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "foundation",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )?;
        self.notify("notifications/initialized", json!({}))?;
        Ok(())
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let response = self.request("tools/call", json!({ "name": name, "arguments": arguments }))?;
        let result = response.get("result").cloned().context("MCP response missing result")?;

        if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            bail!(tool_text(&result).unwrap_or_else(|| format!("MCP tool {name} failed")));
        }

        Ok(result)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&request)?;

        loop {
            let mut line = String::new();
            let len = self
                .stdout
                .read_line(&mut line)
                .with_context(|| format!("reading MCP response for {method}"))?;
            if len == 0 {
                bail!("passport-drive MCP server exited while handling {method}");
            }

            let response: Value = serde_json::from_str(&line)
                .with_context(|| format!("parsing MCP response for {method}: {line}"))?;

            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }

            if let Some(error) = response.get("error") {
                bail!(format_mcp_error(error));
            }

            return Ok(response);
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&notification)
    }

    fn write_message(&mut self, message: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, message)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }
}

impl Drop for PassportDriveMcpClient {
    fn drop(&mut self) {
        let _ = self.call_tool("disconnect", json!({}));
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn passport_drive_candidates() -> Vec<OsString> {
    if let Some(command) = std::env::var_os("FOUNDATION_PASSPORT_DRIVE") {
        return vec![command];
    }

    let mut candidates = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("foundation-passport-drive").into_os_string());
        }
    }

    candidates.push(OsString::from("foundation-passport-drive"));
    candidates.push(OsString::from("passport-drive"));
    candidates
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|source| source.downcast_ref::<std::io::Error>())
        .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::NotFound)
}

fn format_mcp_error(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("MCP error: {error}"))
}

fn tool_text(result: &Value) -> Option<String> {
    let text = result
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
