// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    app_manager::decode_app_id_str,
    gui_server_api::{msg::RunAppResponse, GuiServerError},
    simulator::{
        screengrab::{recording, screenshot, RecordingMessage},
        settings::{read_settings_file, set_scale, write_settings_file},
        theme,
    },
    std::io::{self, Write},
};

gui_server_api::use_api!();

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap_or_else(|error| {
        println!("Failed to initialize log server: {:?}", error);
    });

    log::set_max_level(log::LevelFilter::Info);

    let stdin = io::stdin();

    let recording = match recording() {
        Ok(recording) => recording,
        Err(e) => {
            log::warn!("Failed to set up screen recording system: {}", e);
            return;
        }
    };

    let recording_copy = recording.clone();

    loop {
        let mut input = String::new();

        if stdin.read_line(&mut input).is_err() {
            log::info!("read line error");
            break;
        }

        match input.trim() {
            "ping" => emit_line("ok ping proto=1 caps=run"),
            "d" => {
                screenshot(true).unwrap_or_else(|error| {
                    log::warn!("Failed to take screenshot: {}", error);
                });
            }
            "s" => {
                screenshot(false).unwrap_or_else(|error| {
                    log::warn!("Failed to take screenshot: {}", error);
                });
            }
            "rd" => {
                recording.send(RecordingMessage::Start(true)).unwrap_or_else(|error| {
                    log::warn!("Failed to start recording: {}", error);
                });
            }
            "rs" => {
                recording.send(RecordingMessage::Start(false)).unwrap_or_else(|error| {
                    log::warn!("Failed to start recording: {}", error);
                });
            }
            "c" => {
                recording_copy.send(RecordingMessage::Stop).unwrap_or_else(|error| {
                    log::warn!("Failed to stop recording: {}", error);
                });
            }
            "1x" => update_scale(1.0),
            "1.5x" => update_scale(1.5),
            "2x" => update_scale(2.0),
            "t" => {
                let is_dark = theme::get_system_theme();
                theme::set_system_theme(!is_dark);
            }
            "td" => theme::set_system_theme(true),
            "tl" => theme::set_system_theme(false),
            "help" | "h" => {
                log::info!(
                    "Simulator commands:
    ping   - Print control protocol status
    run    - Launch or foreground an app by AppId
    d      - Device Screenshot
    s      - Screen-only Screenshot
    rd     - Record Device
    rs     - Record Screen
    c      - Cut recording
    1x     - scale to 1x
    1.5x   - scale to 1.5x
    2x     - scale to 2x
    t      - Toggle Theme
    td     - Theme Dark
    tl     - Theme Light
    h/help - show all commands"
                );
            }
            command if command.starts_with("run ") => handle_run_command(command),
            _ => log::info!("unknown command"),
        }
    }
}

fn update_scale(scale: f32) {
    set_scale(scale);

    let mut settings = match read_settings_file() {
        Ok(s) => s,
        Err(_) => {
            log::warn!("failed to read settings");
            return;
        }
    };
    settings.scale = scale;
    write_settings_file(&settings).unwrap_or_else(|error| {
        log::warn!("Failed to write settings file: {}", error);
    });
}

fn emit_line(line: &str) {
    println!("{line}");
    let _ = io::stdout().flush();
}

fn handle_run_command(command: &str) {
    let mut parts = command.split_whitespace();
    let _ = parts.next();
    let Some(app_id_str) = parts.next() else {
        emit_line("err run bad-request");
        return;
    };

    let app_id = match decode_app_id_str(app_id_str) {
        Ok(app_id) => app_id,
        Err(error) => {
            emit_line(&format!("err run invalid-app-id {error}"));
            return;
        }
    };

    let Some(gui) = GuiApiLight::try_connect() else {
        emit_line("err run not-ready");
        return;
    };

    match gui.run_app(app_id) {
        Ok(RunAppResponse::Launched { pid }) => emit_line(&format!("ok run launched pid={pid}")),
        Ok(RunAppResponse::AlreadyRunning { pid }) => emit_line(&format!("ok run already-running pid={pid}")),
        Ok(RunAppResponse::Locked) => emit_line("err run locked"),
        Ok(RunAppResponse::NotReady) => emit_line("err run not-ready"),
        Ok(RunAppResponse::AppIdNotFound) => emit_line("err run app-not-found"),
        Ok(RunAppResponse::LaunchFailed { reason }) => {
            emit_line(&format!("err run launch-failed reason={reason:?}"))
        }
        Err(error) => log_run_error(error),
    }
}

fn log_run_error(error: GuiServerError) {
    log::warn!("Failed to run app through gui-server: {error:?}");
    emit_line("err run launch-failed");
}
