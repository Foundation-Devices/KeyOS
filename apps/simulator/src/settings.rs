// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use {
    crate::{MainWindow, Settings, DEP_SETTINGS_FILE, SETTINGS_FILE},
    anyhow::Context,
    gui_server_api::{msg, simulator::SimulatorApi},
    server::{CheckedPermissions, MessageAllowed},
    slint::{winit_030::WinitWindowAccessor, ComponentHandle},
    std::{cell::Cell, path::PathBuf, rc::Rc, time::Duration},
};

/// Permission to set the simulator's display scale factor.
pub trait ScalePermissions: CheckedPermissions + MessageAllowed<msg::SetScaleFactor> {}

impl<P> ScalePermissions for P where P: CheckedPermissions + MessageAllowed<msg::SetScaleFactor> {}

pub fn setup<P: ScalePermissions>(window: &MainWindow, sim: SimulatorApi<P>) {
    let settings = match read_settings_file() {
        Ok(s) => s,
        Err(error) => {
            log::warn!("Failed to read settings file: {}", error);
            let s = Settings { scale: 1.0 };
            write_settings_file(&s).unwrap_or_else(|error| {
                log::warn!("Failed to write default settings file: {}", error);
            });
            s
        }
    };

    set_scale(&sim, settings.scale);
    window.set_settings(settings.into());

    window.on_scale_set({
        let window = window.clone_strong();
        move |scale_factor| {
            set_scale(&sim, scale_factor);
            change_setting(&window, |settings| settings.scale = scale_factor);
        }
    });
}

pub fn change_setting(window: &MainWindow, f: impl Fn(&mut Settings)) {
    let mut settings: Settings = window.get_settings();
    f(&mut settings);
    write_settings_file(&settings).unwrap_or_else(|error| {
        log::warn!("Failed to write settings file: {}", error);
    });
    window.set_settings(settings);
}

pub fn read_settings_file() -> anyhow::Result<Settings> {
    let settings_string = std::fs::read_to_string(SETTINGS_FILE).context("Error reading settings file")?;

    serde_json::from_str(settings_string.as_str()).map_err(|error| {
        log::warn!(
            "Unable to parse settings: {}\nThis may be due to settings structure changes. Moving old settings file to {}.",
            error,
            DEP_SETTINGS_FILE
        );
        std::fs::rename(SETTINGS_FILE, DEP_SETTINGS_FILE).unwrap_or_else(|error| {
            log::warn!("Unable to move old settings file: {}", error);
        });

        anyhow::Error::new(error).context("Error parsing settings file")
    })
}

pub fn write_settings_file(settings: &Settings) -> anyhow::Result<()> {
    let settings_string = serde_json::to_string(&settings).context("Could not serialize settings")?;
    std::fs::write(SETTINGS_FILE, settings_string.as_str()).context("Could not write settings file")?;
    Ok(())
}

pub fn set_scale<P: ScalePermissions>(sim: &SimulatorApi<P>, scale_factor: f32) {
    sim.set_scale_factor(scale_factor).unwrap_or_else(|error| {
        log::warn!("Could not set scale factor: {:?}", error);
    })
}

// Stored under ~/.foundation so the position survives SDK bundle reinstalls (the
// bundle working tree is wiped on reinstall). Kept separate from the scale
// settings so an older settings.json without a position still deserializes, and
// so restoring it before run() lets the winit backend apply it at window creation
// (macOS ignores a position set after the window shows).
fn window_position_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".foundation/simulator/control-panel-position.json"))
}

pub fn restore_window_position(window: &MainWindow) {
    let Some(path) = window_position_file() else { return };
    let Ok(contents) = std::fs::read_to_string(&path) else { return };
    match serde_json::from_str::<slint::PhysicalPosition>(&contents) {
        Ok(pos) => window.window().set_position(pos),
        Err(error) => log::warn!("Failed to parse control panel window position: {}", error),
    }
}

// Slint's `position()`/`set_position()` nominally operate on the outer frame,
// but on macOS the winit backend lands a position set before `run()` on the
// window's content rect. Persisting the outer position therefore reopened the
// window one title-bar height too high. Read and store the inner (content)
// position via the winit backend so save and restore use the same reference and
// round-trip exactly.
fn read_inner_position(window: &MainWindow) -> Option<slint::PhysicalPosition> {
    let pos = window.window().with_winit_window(|w| w.inner_position().ok()).flatten()?;
    Some(slint::PhysicalPosition::new(pos.x, pos.y))
}

pub fn save_window_position(window: &MainWindow) {
    let Some(path) = window_position_file() else { return };
    let Some(pos) = read_inner_position(window) else {
        log::warn!("Skipping control panel position save; winit window unavailable");
        return;
    };
    let contents = match serde_json::to_string(&pos) {
        Ok(contents) => contents,
        Err(error) => {
            log::warn!("Failed to serialize control panel window position: {}", error);
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            log::warn!("Failed to create control panel position directory: {}", error);
            return;
        }
    }
    std::fs::write(&path, contents).unwrap_or_else(|error| {
        log::warn!("Failed to save control panel window position: {}", error);
    });
}

// Persist only genuine user moves. macOS does not round-trip a window position
// exactly: the value read back after setting one differs slightly, so blindly
// re-saving the freshly restored position would walk the window across the
// screen a little more on every launch. Track the last position treated as
// settled and save only when it actually changes.
fn save_window_position_if_moved(window: &MainWindow, last: &Rc<Cell<Option<slint::PhysicalPosition>>>) {
    let Some(pos) = read_inner_position(window) else { return };
    match last.get() {
        // First observation is the just-restored position; adopt it as the
        // baseline without saving so the restore round-trip isn't persisted.
        None => last.set(Some(pos)),
        Some(prev) if prev != pos => {
            save_window_position(window);
            last.set(Some(pos));
        }
        Some(_) => {}
    }
}

// Restores the saved position, then keeps it persisted across both the normal
// close (on_close_requested) and Ctrl-C (SIGINT, which doesn't fire that
// callback) by polling on a timer. Both paths save only real moves via a shared
// baseline. The returned timer must be held alive as long as the window runs.
pub fn setup_window_position(window: &MainWindow) -> slint::Timer {
    restore_window_position(window);

    let last_pos: Rc<Cell<Option<slint::PhysicalPosition>>> = Rc::new(Cell::new(None));

    window.window().on_close_requested({
        let window_weak = window.as_weak();
        let last_pos = last_pos.clone();
        move || {
            if let Some(window) = window_weak.upgrade() {
                save_window_position_if_moved(&window, &last_pos);
            }
            // Stop the event loop so run() returns and main can send gui-server the
            // Shutdown that tears the whole hosted simulator down.
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        }
    });

    let timer = slint::Timer::default();
    let window_weak = window.as_weak();
    timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), move || {
        let Some(window) = window_weak.upgrade() else { return };
        save_window_position_if_moved(&window, &last_pos);
    });
    timer
}
