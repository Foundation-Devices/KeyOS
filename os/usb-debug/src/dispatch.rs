// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Debug protocol command dispatch.
//!
//! Wire format lives in `usb-debug-protocol`. This module owns the device-side
//! mapping from decoded `Command`s to keyOS API calls.

use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs::{Error as FsError, Location, OpenFlags};
use gui_server_api::msg::{LaunchFailureReason, RunAppResponse};
use gui_server_api::touch::{Touch, TouchKind as GuiTouchKind};
use gui_server_api::Key;
use usb_debug_protocol::{
    Command, LaunchAppResult, LaunchAppStatus, Payload, ProtocolError, Response,
    PUBLISHER_FINGERPRINT_HEX_LEN,
};

app_manager::use_api!();
fs::use_api!();
gui_server_api::use_api!();
power_manager::use_api!();
security::use_api!();
settings::use_api!();

use xous_ticktimer::TicktimerPrivileged;

const POWER_BUTTON_SHORT_PRESS_MS: u64 = 200;
const POWER_BUTTON_LONG_PRESS_MS: u64 = 3000;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Persistent debug protocol handler. Holds connections that are reused
/// across commands, rather than creating one per request.
pub struct DebugProtocol {
    app_manager: AppManagerApi,
    gui: GuiApiLight,
    security: Security,
    settings: SettingsApi,
    fs: FileSystem,
    ticktimer: TicktimerPrivileged,
    upload: Option<UploadSession>,
    /// Set by the `RebootSamba` arm; `process` honors it once the Ack has
    /// been queued for the USB writer.
    reboot_after_answering: bool,
}

struct UploadSession {
    app_id: xous::AppId,
    /// The currently installed bundle. It remains intact until the replacement upload completes.
    app_dir: String,
    /// The untrusted replacement bundle, invisible to app-manager until `LOAD_APP_END` swaps it in.
    staging_dir: String,
    current_file: Option<UploadFile>,
}

struct UploadFile {
    final_path: String,
    temp_path: String,
    file: File,
    expected_size: u64,
    written: u64,
}

impl DebugProtocol {
    pub fn new() -> Self {
        Self {
            app_manager: AppManagerApi::default(),
            gui: GuiApiLight::default(),
            security: Security::default(),
            settings: SettingsApi::default(),
            fs: FileSystem::default(),
            ticktimer: TicktimerPrivileged::default(),
            upload: None,
            reboot_after_answering: false,
        }
    }

    /// Decode a command packet (`[CMD][PAYLOAD...]`), dispatch it, and forward
    /// the response. If the command requested a reboot, perform it once the
    /// response has been queued.
    pub fn process(&mut self, data: &[u8], send: impl Fn(Response)) {
        let cmd = match Command::decode(data) {
            Ok(cmd) => cmd,
            Err(ProtocolError::Empty) => return,
            Err(e) => {
                log::warn!("debug: {e}");
                send(Response::Err);
                return;
            }
        };
        if !command_allowed(&cmd) {
            log::warn!("debug: command 0x{:02x} is not available in production firmware", cmd.cmd_byte());
            send(Response::Err);
            return;
        }
        #[cfg(feature = "production")]
        if requires_unlock(&cmd) {
            match self.gui.is_locked() {
                Ok(false) => {}
                Ok(true) => {
                    log::warn!("debug: command 0x{:02x} rejected: device is locked", cmd.cmd_byte());
                    send(Response::Locked);
                    return;
                }
                Err(e) => {
                    log::error!("debug: lock-state check failed: {e:?}");
                    send(Response::Err);
                    return;
                }
            }
        }
        send(self.dispatch(cmd));
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
            Command::Swipe { start_x, start_y, end_x, end_y, duration_ms, steps } => {
                self.swipe(start_x, start_y, end_x, end_y, duration_ms, steps)
            }
            Command::PowerButton { long } => self.power_button(long),
            Command::RebootSamba => {
                log::debug!("debug: rebooting into SAM-BA mode");
                self.reboot_after_answering = true;
                Response::Ack
            }
            Command::CloseApp { pid } => self.close_app(pid),
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
            Command::LaunchApp { app_id } => match self.gui.run_app(xous::AppId(app_id)) {
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
                Ok(RunAppResponse::Locked) => {
                    log::warn!("debug: launch_app rejected: device is locked");
                    Response::Locked
                }
                Ok(RunAppResponse::AppIdNotFound) => {
                    log::warn!("debug: launch_app rejected: app id not found");
                    Response::LaunchAck(LaunchAppResult::new(0, LaunchAppStatus::AppIdNotFound).encode())
                }
                Ok(RunAppResponse::LaunchFailed { reason }) => {
                    log::warn!("debug: launch_app rejected: {reason:?}");
                    let status = launch_status(reason);
                    Response::LaunchAck(LaunchAppResult::new(0, status).encode())
                }
                Ok(RunAppResponse::NotReady) => {
                    log::warn!("debug: launch_app rejected: gui not ready");
                    Response::LaunchAck(LaunchAppResult::new(0, LaunchAppStatus::NotReady).encode())
                }
                Err(e) => {
                    log::error!("debug: launch_app failed: {e:?}");
                    Response::LaunchAck(LaunchAppResult::new(0, LaunchAppStatus::InternalError).encode())
                }
            },
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
            Command::LoadAppBegin { app_id } => self.load_app_begin(app_id, app_manager::SIDELOADED_APPS_DIR),
            Command::LoadAppFileBegin { filename, size } => self.load_app_file_begin(&filename, size),
            Command::LoadAppChunk(data) => self.load_app_chunk(&data),
            Command::LoadAppEnd => self.load_app_end(),
            Command::GetProcessList => kernel_debug_command(b't'),
            Command::InstallCertificate { expected_fingerprint, certificate_pem } => {
                self.install_certificate(expected_fingerprint, certificate_pem)
            }
            Command::GetAllowedPublisherCount => self.get_allowed_publisher_count(),
            Command::GetSystemTime => self.get_system_time(),
            Command::SetSystemTime { unix_seconds } => self.set_system_time(unix_seconds),
            Command::KernelCmd { cmd_byte } => kernel_debug_command(cmd_byte),
            Command::GetDeveloperMode => {
                // Being reachable at all is the answer on hardware, where the interface itself is
                // gated. The simulator has no such gate, so there the setting is what governs the
                // gated commands, and what a caller needs told.
                let enabled = cfg!(keyos) || self.developer_mode_enabled();
                log::debug!("debug: get_developer_mode -> {enabled}");
                Response::DeveloperMode(enabled)
            }
        }
    }

    fn developer_mode_enabled(&self) -> bool {
        self.settings.get_developer_mode().map(|developer_mode| developer_mode.0).unwrap_or_else(|| {
            log::warn!("debug: Developer Mode setting unavailable; treating it as disabled");
            false
        })
    }

    fn reject_load_app_if_locked(&mut self, command: &'static str) -> Option<Response> {
        match self.gui.is_locked() {
            Ok(false) => None,
            Ok(true) => {
                log::warn!("{command}: rejected: device is locked");
                self.abort_load_app_session(command);
                Some(Response::Locked)
            }
            Err(e) => {
                log::error!("{command}: lock-state check failed: {e:?}");
                self.abort_load_app_session(command);
                Some(Response::Err)
            }
        }
    }

    fn abort_load_app_session(&mut self, reason: &'static str) {
        let Some(session) = self.upload.take() else {
            return;
        };

        log::warn!("{reason}: aborting unfinished load_app session for {}", session.app_dir);
        if let Some(upload_file) = session.current_file {
            let UploadFile { temp_path, file, .. } = upload_file;
            drop(file);
            match self.fs.remove(&temp_path, Location::System) {
                Ok(()) | Err(FsError::FileNotFound) => {}
                Err(e) => log::warn!("{reason}: remove temp file {temp_path} failed: {e:?}"),
            }
        }
        match self.fs.remove(&session.staging_dir, Location::System) {
            Ok(()) | Err(FsError::FileNotFound) => {}
            Err(e) => log::warn!("{reason}: remove staged app {} failed: {e:?}", session.staging_dir),
        }
    }

    fn install_certificate(
        &mut self,
        expected_fingerprint: [u8; PUBLISHER_FINGERPRINT_HEX_LEN],
        certificate_pem: Vec<u8>,
    ) -> Response {
        match self.gui.is_locked() {
            Ok(false) => {}
            Ok(true) => {
                log::warn!("debug: install_certificate rejected: device is locked");
                return Response::Locked;
            }
            Err(e) => {
                log::error!("debug: install_certificate lock-state check failed: {e:?}");
                return Response::Err;
            }
        }

        if !self.developer_mode_enabled() {
            log::warn!("debug: install_certificate rejected: Developer Mode is disabled");
            return Response::Err;
        }
        let expected_fingerprint =
            std::str::from_utf8(&expected_fingerprint).expect("protocol validates fingerprint ASCII");
        match self.app_manager.import_third_party_certificate(certificate_pem, expected_fingerprint) {
            Ok(Ok(cert)) => {
                log::info!(
                    "debug: installed allowed publisher certificate with fingerprint {}",
                    cert.fingerprint
                );
                Response::Ack
            }
            Ok(Err(error)) => {
                log::warn!("debug: install_certificate rejected: {error:?}");
                Response::Err
            }
            Err(e) => {
                log::error!("debug: install_certificate app-manager request failed: {e:?}");
                Response::Err
            }
        }
    }

    fn get_allowed_publisher_count(&mut self) -> Response {
        let certificates = self.app_manager.get_third_party_certificates();
        let usable = certificates.iter().filter(|cert| cert.is_usable()).count();
        let usable = u16::try_from(usable).unwrap_or(u16::MAX);
        let installed = u16::try_from(certificates.len()).unwrap_or(u16::MAX);

        log::debug!("debug: allowed publisher count -> {usable} usable of {installed}");
        let mut payload = usable.to_le_bytes().to_vec();
        payload.extend_from_slice(&installed.to_le_bytes());
        Response::AllowedPublisherCount(payload)
    }

    /// Readable while locked and in production builds: the clock is not a secret, and it is the
    /// first thing to check when a certificate or a signed app is rejected.
    fn get_system_time(&mut self) -> Response {
        let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) else {
            log::error!("debug: get_system_time: the clock reads before the Unix epoch");
            return Response::Err;
        };
        let unix_seconds = elapsed.as_secs();

        log::debug!("debug: system time -> {unix_seconds} (unix seconds, UTC)");
        Response::SystemTime(unix_seconds.to_le_bytes().to_vec())
    }

    /// Moving the clock moves what every certificate validity window is judged against, so this
    /// takes the same gate as importing one: Developer Mode, and not while locked.
    fn set_system_time(&mut self, unix_seconds: u64) -> Response {
        match self.gui.is_locked() {
            Ok(false) => {}
            Ok(true) => {
                log::warn!("debug: set_system_time rejected: device is locked");
                return Response::Locked;
            }
            Err(e) => {
                log::error!("debug: set_system_time lock-state check failed: {e:?}");
                return Response::Err;
            }
        }

        if !self.developer_mode_enabled() {
            log::warn!("debug: set_system_time rejected: Developer Mode is disabled");
            return Response::Err;
        }

        // The RTC stores a 32 bit Unix timestamp and the ticktimer reads a pending value of zero as
        // "nothing to set", so seconds outside this range would be acknowledged here and then
        // wrapped or dropped instead of reaching the clock.
        if unix_seconds == 0 || unix_seconds > u64::from(u32::MAX) {
            log::warn!("debug: set_system_time rejected: {unix_seconds} is outside the RTC range");
            return Response::Err;
        }

        self.ticktimer.set_system_time(unix_seconds * NANOS_PER_SECOND);
        log::info!("debug: system time set to {unix_seconds} (unix seconds, UTC)");
        Response::Ack
    }

    fn close_app(&mut self, pid: u16) -> Response {
        if !self.developer_mode_enabled() {
            log::warn!("debug: close_app rejected: Developer Mode is disabled");
            return Response::Err;
        }

        let Ok(pid_u8) = u8::try_from(pid) else {
            log::error!("close_app: invalid PID {pid}");
            return Response::Err;
        };
        let Some(pid_handle) = xous::PID::new(pid_u8) else {
            log::error!("close_app: invalid PID {pid}");
            return Response::Err;
        };

        if let Err(e) = self.gui.close_app(pid_handle) {
            log::error!("close_app pid={pid}: gui-server request failed: {e:?}");
            return Response::Err;
        }

        log::debug!("debug: close_app pid={pid}");
        Response::Ack
    }

    fn power_button(&mut self, long: bool) -> Response {
        let duration_ms = if long { POWER_BUTTON_LONG_PRESS_MS } else { POWER_BUTTON_SHORT_PRESS_MS };
        log::debug!("debug: power {} press ({duration_ms}ms)", if long { "long" } else { "short" });

        if let Err(e) = self.gui.inject_power_button(true) {
            log::error!("InjectPowerButton press failed: {e:?}");
            return Response::Err;
        }
        std::thread::sleep(Duration::from_millis(duration_ms));
        if let Err(e) = self.gui.inject_power_button(false) {
            log::error!("InjectPowerButton release failed: {e:?}");
            return Response::Err;
        }

        Response::Ack
    }

    fn swipe(
        &mut self,
        start_x: u16,
        start_y: u16,
        end_x: u16,
        end_y: u16,
        duration_ms: u16,
        steps: u8,
    ) -> Response {
        log::debug!(
            "debug: swipe ({start_x},{start_y}) -> ({end_x},{end_y}) duration={duration_ms}ms steps={steps}"
        );

        if self.inject_touch_event(GuiTouchKind::Press, start_x, start_y).is_err() {
            return Response::Err;
        }

        if steps == 0 {
            std::thread::sleep(Duration::from_millis(duration_ms as u64));
        } else {
            let mut elapsed_ms = 0;
            for step in 1..=steps {
                let target_ms = u64::from(duration_ms) * u64::from(step) / u64::from(steps);
                let delay_ms = target_ms.saturating_sub(elapsed_ms);
                if delay_ms > 0 {
                    std::thread::sleep(Duration::from_millis(delay_ms));
                    elapsed_ms = target_ms;
                }

                let x = interpolate_coord(start_x, end_x, step, steps);
                let y = interpolate_coord(start_y, end_y, step, steps);
                if self.inject_touch_event(GuiTouchKind::Drag, x, y).is_err() {
                    return Response::Err;
                }
            }
        }

        if self.inject_touch_event(GuiTouchKind::Release, end_x, end_y).is_err() {
            return Response::Err;
        }

        Response::Ack
    }

    fn inject_touch_event(&mut self, kind: GuiTouchKind, x: u16, y: u16) -> Result<(), ()> {
        let touch = Touch { kind, id: 0, x: x as usize, y: y as usize };
        if let Err(e) = self.gui.inject_touch(touch) {
            log::error!("InjectTouch failed: {e:?}");
            return Err(());
        }
        Ok(())
    }

    fn load_app_begin(&mut self, app_id: [u8; 16], sideload_root: &str) -> Response {
        if let Some(response) = self.reject_load_app_if_locked("LOAD_APP_BEGIN") {
            return response;
        }
        if !self.developer_mode_enabled() {
            log::warn!("debug: load_app rejected: Developer Mode is disabled");
            return Response::Err;
        }

        self.abort_load_app_session("LOAD_APP_BEGIN");

        let app_id_dir = app_id_dir_name(&app_id);
        let app_dir = format!("{sideload_root}/{app_id_dir}");
        let staging_dir = format!("{app_dir}.part");
        match self.fs.remove(&staging_dir, Location::System) {
            Ok(()) | Err(FsError::FileNotFound) => {}
            Err(e) => {
                log::error!("LOAD_APP_BEGIN: remove stale staged app {staging_dir} failed: {e:?}");
                return Response::Err;
            }
        }

        self.upload = Some(UploadSession {
            app_id: xous::AppId(app_id),
            app_dir: app_dir.clone(),
            staging_dir,
            current_file: None,
        });
        log::info!("debug: load_app begin {app_dir}");
        Response::Ack
    }

    fn load_app_file_begin(&mut self, filename: &str, expected_size: u64) -> Response {
        if let Some(response) = self.reject_load_app_if_locked("LOAD_APP_FILE_BEGIN") {
            return response;
        }
        if !is_valid_upload_relative_path(filename) {
            log::warn!("LOAD_APP_FILE_BEGIN: invalid file path: {filename:?}");
            return Response::Err;
        }

        let app_dir = match self.upload.as_ref() {
            Some(session) if session.current_file.is_none() => session.staging_dir.clone(),
            Some(_) => {
                log::warn!("LOAD_APP_FILE_BEGIN: previous file still open");
                return Response::Err;
            }
            None => {
                log::warn!("LOAD_APP_FILE_BEGIN: no active load_app session");
                return Response::Err;
            }
        };

        self.start_upload_file(format!("{app_dir}/{filename}"), expected_size, "LOAD_APP_FILE_BEGIN")
    }

    fn start_upload_file(
        &mut self,
        final_path: String,
        expected_size: u64,
        command: &'static str,
    ) -> Response {
        let temp_path = format!("{final_path}.part");
        if let Err(e) = self.fs.ensure_parent_dir_exists(&temp_path, Location::System) {
            log::error!("{command}: create parent for {temp_path} failed: {e:?}");
            return Response::Err;
        }
        match self.fs.remove(&temp_path, Location::System) {
            Ok(()) | Err(FsError::FileNotFound) => {}
            Err(e) => {
                log::error!("{command}: remove stale temp file {temp_path} failed: {e:?}");
                return Response::Err;
            }
        }

        let file = match self.fs.open_file(
            temp_path.clone(),
            Location::System,
            OpenFlags { read: false, write: true, create: true },
        ) {
            Ok(file) => file,
            Err(e) => {
                log::error!("{command}: open {temp_path} failed: {e:?}");
                return Response::Err;
            }
        };

        if expected_size == 0 {
            drop(file);
            let _ = self.fs.remove(&final_path, Location::System);
            if let Err(e) = self.fs.rename(&temp_path, &final_path, Location::System) {
                log::error!("{command}: rename {temp_path} to {final_path} failed: {e:?}");
                return Response::Err;
            }
            log::info!("debug: load_app wrote {final_path}");
            return Response::Ack;
        }

        let Some(session) = self.upload.as_mut() else {
            return Response::Err;
        };
        session.current_file =
            Some(UploadFile { final_path, temp_path: temp_path.clone(), file, expected_size, written: 0 });
        log::info!("debug: load_app writing {temp_path} ({expected_size} bytes)");
        Response::Ack
    }

    fn load_app_chunk(&mut self, payload: &[u8]) -> Response {
        if let Some(response) = self.reject_load_app_if_locked("LOAD_APP_CHUNK") {
            return response;
        }
        let is_file_complete = {
            let Some(session) = self.upload.as_mut() else {
                log::warn!("LOAD_APP_CHUNK: no active load_app session");
                return Response::Err;
            };
            let Some(upload_file) = session.current_file.as_mut() else {
                log::warn!("LOAD_APP_CHUNK: no open file");
                return Response::Err;
            };

            let new_written = upload_file.written.saturating_add(payload.len() as u64);
            if new_written > upload_file.expected_size {
                log::warn!(
                    "LOAD_APP_CHUNK: {} would exceed expected size {} > {}",
                    upload_file.temp_path,
                    new_written,
                    upload_file.expected_size
                );
                return Response::Err;
            }

            if let Err(e) = upload_file.file.write_all(payload) {
                log::error!("LOAD_APP_CHUNK: write {} failed: {e:?}", upload_file.temp_path);
                return Response::Err;
            }
            upload_file.written = new_written;
            new_written == upload_file.expected_size
        };

        if is_file_complete {
            self.finish_load_app_file("LOAD_APP_CHUNK")
        } else {
            Response::Ack
        }
    }

    fn finish_load_app_file(&mut self, command: &'static str) -> Response {
        let upload_file = {
            let Some(session) = self.upload.as_mut() else {
                log::warn!("{command}: no active load_app session");
                return Response::Err;
            };
            let Some(upload_file) = session.current_file.take() else {
                log::warn!("{command}: no open file");
                return Response::Err;
            };
            upload_file
        };

        let UploadFile { final_path, temp_path, mut file, expected_size, written } = upload_file;
        if written != expected_size {
            log::warn!("{command}: {temp_path} size mismatch, wrote {written} expected {expected_size}");
            return Response::Err;
        }
        if let Err(e) = file.flush() {
            log::error!("{command}: flush {temp_path} failed: {e:?}");
            return Response::Err;
        }
        drop(file);

        let _ = self.fs.remove(&final_path, Location::System);
        if let Err(e) = self.fs.rename(&temp_path, &final_path, Location::System) {
            log::error!("{command}: rename {temp_path} to {final_path} failed: {e:?}");
            return Response::Err;
        }

        log::info!("debug: load_app wrote {final_path}");
        Response::Ack
    }

    fn load_app_end(&mut self) -> Response {
        if let Some(response) = self.reject_load_app_if_locked("LOAD_APP_END") {
            return response;
        }
        let (app_id, app_dir, staging_dir) = {
            let Some(session) = self.upload.as_ref() else {
                log::warn!("LOAD_APP_END: no active load_app session");
                return Response::Err;
            };
            if session.current_file.is_some() {
                log::warn!("LOAD_APP_END: incomplete session current_file=true");
                return Response::Err;
            }
            (session.app_id, session.app_dir.clone(), session.staging_dir.clone())
        };

        match self.fs.remove(&app_dir, Location::System) {
            Ok(()) | Err(FsError::FileNotFound) => {}
            Err(e) => {
                log::error!("LOAD_APP_END: remove existing app {app_dir} failed: {e:?}");
                self.upload = None;
                return Response::Err;
            }
        }
        if let Err(e) = self.fs.rename(&staging_dir, &app_dir, Location::System) {
            log::error!("LOAD_APP_END: rename staged app {staging_dir} to {app_dir} failed: {e:?}");
            self.upload = None;
            return Response::Err;
        }

        if let Err(e) = self.app_manager.refresh_installed_app(app_id) {
            log::error!("LOAD_APP_END: refresh installed apps after {app_dir} failed: {e:?}");
            self.upload = None;
            return Response::Err;
        }

        self.upload = None;
        log::info!("debug: load_app complete {app_dir}");
        Response::Ack
    }
}

fn launch_status(reason: LaunchFailureReason) -> LaunchAppStatus {
    match reason {
        LaunchFailureReason::SignatureRejected => LaunchAppStatus::SignatureRejected,
        LaunchFailureReason::NoCertificate => LaunchAppStatus::NoCertificate,
        LaunchFailureReason::PublisherCertificateExpired => LaunchAppStatus::PublisherCertificateExpired,
        LaunchFailureReason::PublisherCertificateNotYetActive => {
            LaunchAppStatus::PublisherCertificateNotYetActive
        }
        LaunchFailureReason::KeyOsVersionTooOld => LaunchAppStatus::KeyOsVersionTooOld,
        LaunchFailureReason::RunningKeyOsVersionUnavailable => {
            LaunchAppStatus::RunningKeyOsVersionUnavailable
        }
        LaunchFailureReason::Internal => LaunchAppStatus::InternalError,
    }
}

fn is_valid_upload_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.ends_with('/')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

fn app_id_dir_name(app_id: &[u8; 16]) -> String {
    use core::fmt::Write as _;

    let mut name = String::with_capacity(32);
    for byte in app_id {
        write!(&mut name, "{byte:02x}").expect("writing to String cannot fail");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::{app_id_dir_name, is_valid_upload_relative_path};

    #[test]
    fn app_id_dir_name_is_lowercase_hex() {
        assert_eq!(
            app_id_dir_name(&[
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
                0xff,
            ]),
            "00112233445566778899aabbccddeeff"
        );
    }

    #[test]
    fn upload_relative_path_rejects_traversal_and_absolute_paths() {
        assert!(is_valid_upload_relative_path("app.elf"));
        assert!(is_valid_upload_relative_path("resources/.foundation/icon.raw"));
        assert!(is_valid_upload_relative_path("resources/images/logo.raw"));

        assert!(!is_valid_upload_relative_path(""));
        assert!(!is_valid_upload_relative_path("/icon.raw"));
        assert!(!is_valid_upload_relative_path("images/"));
        assert!(!is_valid_upload_relative_path("images//logo.raw"));
        assert!(!is_valid_upload_relative_path("../icon.raw"));
        assert!(!is_valid_upload_relative_path("images/../icon.raw"));
        assert!(!is_valid_upload_relative_path("./icon.raw"));
    }
}

#[cfg(feature = "production")]
fn command_allowed(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Screenshot
            | Command::Swipe { .. }
            | Command::PowerButton { .. }
            | Command::CloseApp { .. }
            | Command::InputText(_)
            | Command::GetVersion
            | Command::LaunchApp { .. }
            | Command::GetDeveloperMode
            | Command::LoadAppBegin { .. }
            | Command::LoadAppFileBegin { .. }
            | Command::LoadAppChunk(_)
            | Command::LoadAppEnd
            | Command::GetProcessList
            | Command::InstallCertificate { .. }
            | Command::GetAllowedPublisherCount
            // Reading the clock is a diagnostic; moving it changes what every certificate validity
            // window is judged against, so SetSystemTime stays out of production firmware.
            | Command::GetSystemTime
    )
}

#[cfg(not(feature = "production"))]
fn command_allowed(_cmd: &Command) -> bool { true }

/// Commands that capture the screen or drive input, which a locked production unit must refuse so
/// USB access cannot read the display or the PIN keypad. Non-production firmware keeps them
/// available while locked, which the KeyOS development workflow relies on. LaunchApp,
/// InstallCertificate and SetSystemTime carry their own lock checks in every build.
#[cfg(feature = "production")]
fn requires_unlock(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Screenshot
            | Command::Swipe { .. }
            | Command::PowerButton { .. }
            | Command::InputText(_)
            | Command::CloseApp { .. }
    )
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

/// Run a kernel debug command and return its output. `t` is the process list.
#[cfg(keyos)]
fn kernel_debug_command(cmd_byte: u8) -> Response {
    let buf = match xous::map_memory(None, None, 0x40000, xous::MemoryFlags::W) {
        Ok(buf) => xous::DropDeallocate::new(buf),
        Err(e) => {
            log::error!("Could not allocate debug command buffer: {e:?}");
            return Response::Err;
        }
    };

    match xous::debug_command(*buf, cmd_byte) {
        Ok(len) => {
            log::debug!("debug: kernel cmd '{}'", cmd_byte as char);
            Response::KernelOutput(payload_from_mapped(buf, len))
        }
        Err(e) => {
            log::error!("debug: kernel cmd '{}' failed: {e:?}", cmd_byte as char);
            Response::Err
        }
    }
}

/// The kernel only answers debug commands on hardware, so the simulator has no process list to
/// report and no `p`/`m`/`s` dumps to run.
#[cfg(not(keyos))]
fn kernel_debug_command(cmd_byte: u8) -> Response {
    log::warn!("debug: kernel cmd '{}' needs the hardware kernel", cmd_byte as char);
    Response::Err
}

fn interpolate_coord(start: u16, end: u16, step: u8, steps: u8) -> u16 {
    let start = i32::from(start);
    let delta = i32::from(end) - start;
    (start + delta * i32::from(step) / i32::from(steps)) as u16
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
