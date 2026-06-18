// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use {
    simulator::{screengrab, settings as sim_settings, theme, MainWindow, SIMULATOR_DIR},
    slint::{winit_030::WinitWindowAccessor, ComponentHandle},
    std::{fs::create_dir_all, time::Duration},
};

gui_server_api::use_api!();
settings::use_api!();

type Sim = gui_server_api::simulator::SimulatorApi<gui_permissions::GuiPermissions>;

fn main() {
    //slint::platform::set_platform(Box::new(i_slint_backend_winit::Backend::new().unwrap())).unwrap();
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap_or_else(|error| {
        println!("Failed to initialize log server: {:?}", error);
    });

    log::set_max_level(log::LevelFilter::Info);

    create_dir_all(SIMULATOR_DIR).unwrap_or_else(|error| {
        log::warn!("Failed to create simulator directory: {:?}", error);
    });

    // Critical error, nothing can happen without a window
    let window = MainWindow::new().unwrap();

    let gui = GuiApiLight::connect();
    let sim = Sim::connect();
    let settings_api = SettingsApi::default();

    screengrab::setup(&window, gui.clone(), sim.clone());
    sim_settings::setup(&window, sim);
    theme::setup(&window, settings_api);

    let _position_timer = sim_settings::setup_window_position(&window);

    // Launched from the `foundation` CLI, so on macOS the control panel can open
    // behind the terminal. Once the event loop is up and the window is visible,
    // pull it to the front.
    {
        let window_weak = window.as_weak();
        slint::Timer::single_shot(Duration::from_millis(200), move || {
            if let Some(window) = window_weak.upgrade() {
                window.window().with_winit_window(|w| w.focus_window());
            }
        });
    }

    log::info!("Simulator starting");
    window.run().unwrap();
    // gui-server's Shutdown terminates every hosted process, so this also closes the
    // device window; harmless if gui-server is already gone.
    gui.shutdown().ok();
}
