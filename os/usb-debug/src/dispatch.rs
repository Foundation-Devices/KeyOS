// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Debug protocol command dispatch.
//!
//! Wire format lives in `usb-debug-protocol`. This module owns the device-side
//! mapping from decoded `Command`s to keyOS API calls.

use std::sync::mpsc;

use gui_server_api::touch::{Touch, TouchKind as GuiTouchKind};
use gui_server_api::Key;
use usb_debug_protocol::{
    Command, LaunchAppResult, LaunchAppStatus, Payload, ProtocolError, Response, TouchKind,
};

gui_server_api::use_api!();
power_manager::use_api!();
security::use_api!();
settings::use_api!();

/// Persistent debug protocol handler. Holds connections that are reused
/// across commands, rather than creating one per request.
pub struct DebugProtocol {
    gui: GuiApiLight,
    security: Security,
    settings: SettingsApi,
    /// Set by the `RebootSamba` arm; `process` honors it once the Ack has
    /// been queued for the USB writer.
    reboot_after_answering: bool,
}

impl DebugProtocol {
    pub fn new() -> Self {
        Self {
            gui: GuiApiLight::default(),
            security: Security::default(),
            settings: SettingsApi::default(),
            reboot_after_answering: false,
        }
    }

    /// Decode a USB OUT packet (`[CMD][PAYLOAD...]`), dispatch it, and forward
    /// the response. If the command requested a reboot, perform it once the
    /// response has been queued.
    pub fn process(&mut self, data: &[u8], resp_tx: &mpsc::SyncSender<Response>) {
        let cmd = match Command::decode(data) {
            Ok(cmd) => cmd,
            Err(ProtocolError::Empty) => return,
            Err(e) => {
                log::warn!("debug: {e}");
                let _ = resp_tx.send(Response::Err);
                return;
            }
        };
        let response = self.dispatch(cmd);
        let _ = resp_tx.send(response);
        if self.reboot_after_answering {
            // Give the writer thread time to deliver the Ack before we
            // tear the system down.
            std::thread::sleep(std::time::Duration::from_millis(100));
            reboot_to_samba();
        }
    }

    fn dispatch(&mut self, cmd: Command) -> Response {
        match cmd {
            Command::Screenshot => match self.gui.capture_screen() {
                Ok(pixels) => {
                    log::debug!("debug: screenshot captured");
                    let len = pixels.len();
                    Response::Screenshot(payload_from_mapped(pixels, len))
                }
                Err(e) => {
                    log::error!("Screenshot failed: {e:?}");
                    Response::Err
                }
            },
            Command::Tap { x, y, kind } => {
                let touch = Touch { kind: gui_touch_kind(kind), id: 0, x: x as usize, y: y as usize };
                log::debug!("debug: tap ({x},{y}) kind={kind:?}");
                if let Err(e) = self.gui.inject_touch(touch) {
                    log::error!("InjectTouch failed: {e:?}");
                }
                Response::Ack
            }
            Command::PowerButton { pressed } => {
                log::debug!("debug: power pressed={pressed}");
                Response::Ack
            }
            Command::RebootSamba => {
                log::debug!("debug: rebooting into SAM-BA mode");
                self.reboot_after_answering = true;
                Response::Ack
            }
            Command::CloseApp { pid } => match xous::PID::new(pid as u8) {
                Some(pid_handle) => {
                    if let Err(_) = self.gui.close_app(pid_handle) {
                        log::warn!("close_app pid={pid}: no gui window, terminating directly");
                        if let Err(e) =
                            xous::terminate_pid(pid_handle, gui_server_api::consts::CLOSE_TIMEOUT_EXIT_CODE)
                        {
                            log::error!("terminate_pid {pid} failed: {e:?}");
                            return Response::Err;
                        }
                    }
                    log::debug!("debug: close_app pid={pid}");
                    Response::Ack
                }
                None => {
                    log::error!("close_app: invalid PID {pid}");
                    Response::Err
                }
            },
            Command::InputText(text) => {
                log::debug!("debug: input_text ({} chars)", text.chars().count());
                for c in text.chars() {
                    let key = Key::Char(c as usize);
                    if let Err(e) = self.gui.inject_key(true, key) {
                        log::error!("InjectKey press failed: {e:?}");
                        break;
                    }
                    if let Err(e) = self.gui.inject_key(false, key) {
                        log::error!("InjectKey release failed: {e:?}");
                        break;
                    }
                }
                Response::Ack
            }
            Command::LaunchApp { app_id } => {
                use gui_server_api::msg::RunAppResponse;

                // Enforce the Developer Mode policy on the device, not only in
                // `foundation sideload`'s host-side preflight. The vendor debug
                // interface can be reachable while Developer Mode is off, so a
                // host calling launch_app directly (over MCP or raw USB) would
                // otherwise bypass the gate entirely.
                if !self.settings.get_developer_mode().0 {
                    log::warn!("debug: launch_app rejected: Developer Mode is disabled");
                    return Response::Err;
                }
                match self.gui.run_app(xous::AppId(app_id)) {
                    Ok(RunAppResponse::Launched { pid }) => {
                        let pid_val = pid as u16;
                        log::info!("debug: launch_app launched pid={pid_val}");
                        Response::LaunchAck(LaunchAppResult::new(pid_val, LaunchAppStatus::Launched).encode())
                    }
                    Ok(RunAppResponse::AlreadyRunning { pid }) => {
                        let pid_val = pid as u16;
                        log::info!("debug: launch_app already running pid={pid_val}");
                        Response::LaunchAck(
                            LaunchAppResult::new(pid_val, LaunchAppStatus::AlreadyRunning).encode(),
                        )
                    }
                    Ok(resp) => {
                        log::error!("launch_app rejected: {resp:?}");
                        Response::Err
                    }
                    Err(e) => {
                        log::error!("launch_app failed: {e:?}");
                        Response::Err
                    }
                }
            }
            Command::GetVersion => match self.security.os_version_info() {
                Ok(Some(info)) => {
                    let trimmed: Vec<u8> =
                        info.keyos_version.iter().take_while(|&&b| b != 0).copied().collect();
                    log::debug!("debug: get_version -> {}", String::from_utf8_lossy(&trimmed));
                    Response::Version(trimmed)
                }
                Ok(None) => {
                    log::warn!("get_version: no OS version info available in SECURAM");
                    Response::Err
                }
                Err(e) => {
                    log::error!("get_version: security API call failed: {e:?}");
                    Response::Err
                }
            },
            Command::KernelCmd { cmd_byte } => {
                let buf = match xous::map_memory(None, None, 0x40000, xous::MemoryFlags::W) {
                    Ok(buf) => xous::DropDeallocate::new(buf),
                    Err(e) => {
                        log::error!("Could not allocate debug command buffer: {e:?}");
                        return Response::Err;
                    }
                };
                let len = xous::debug_command(*buf, cmd_byte).unwrap_or(0);
                log::debug!("debug: kernel cmd '{}'", cmd_byte as char);
                Response::KernelOutput(payload_from_mapped(buf, len))
            }
            Command::GetDeveloperMode => {
                // The manifest already declares `GetDeveloperMode` permission for
                // this server — the previous host-side check just sent a kernel
                // `h` command and asserted the transport responded, which proved
                // nothing about the setting. Read it for real here.
                let enabled = self.settings.get_developer_mode().0;
                log::debug!("debug: get_developer_mode -> {enabled}");
                Response::DeveloperMode(enabled)
            }
        }
    }
}

// On keyos, the response payload borrows the gui-server-lent buffer or the
// kernel debug buffer directly so the writer thread can read from mapped pages
// without an intermediate copy. Off-target builds (simulator/tests) fall back
// to a Vec.
#[cfg(keyos)]
fn payload_from_mapped(buf: xous::DropDeallocate, len: usize) -> Payload { Payload::from_mapped(buf, len) }
#[cfg(not(keyos))]
fn payload_from_mapped(buf: xous::DropDeallocate, len: usize) -> Payload {
    Payload::from_vec(buf.as_slice::<u8>()[..len].to_vec())
}

fn gui_touch_kind(k: TouchKind) -> GuiTouchKind {
    match k {
        TouchKind::Press => GuiTouchKind::Press,
        TouchKind::Release => GuiTouchKind::Release,
        TouchKind::Drag => GuiTouchKind::Drag,
    }
}

fn reboot_to_samba() {
    #[cfg(keyos)]
    {
        const BUREG0_OFFSET: usize = 0x400;
        const BSC_CR_OFFSET: usize = 0x054;

        let bureg_page = xous::map_memory(
            xous::MemoryAddress::new(utralib::HW_BUREG_MEM),
            None,
            0x1000,
            xous::MemoryFlags::W | xous::MemoryFlags::DEV,
        )
        .expect("mapping BUREG page");
        let bsc_page = xous::map_memory(
            xous::MemoryAddress::new(utralib::HW_RSTC_BASE),
            None,
            0x1000,
            xous::MemoryFlags::W | xous::MemoryFlags::DEV,
        )
        .expect("mapping BSC page");

        log::info!("Configuring boot sequence registers for SAM-BA mode...");
        unsafe {
            let bureg0 = bureg_page.as_mut_ptr().byte_add(BUREG0_OFFSET) as *mut u32;
            let bsc_cr = bsc_page.as_mut_ptr().byte_add(BSC_CR_OFFSET) as *mut u32;
            bureg0.write_volatile(0xFFF);
            bsc_cr.write_volatile(0x6683_0004);
        }
        log::info!("Registers set, rebooting...");

        let pm = PowerManagerApi::default();
        pm.reboot();
    }
    #[cfg(not(keyos))]
    log::warn!("reboot_to_samba not supported on simulator");
}
