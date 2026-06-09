// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use {
    simulator::{screengrab, settings, theme, MainWindow, SIMULATOR_DIR},
    slint::{winit_030::WinitWindowAccessor, ComponentHandle},
    std::{fs::create_dir_all, time::Duration},
};

gui_server_api::use_api!();

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

    screengrab::setup(&window);
    settings::setup(&window);
    theme::setup(&window);

    let _position_timer = settings::setup_window_position(&window);

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
    if let Some(gui) = GuiApiLight::try_connect_with_timeout(Duration::from_secs(1)) {
        gui.shutdown().ok();
    }
    // If the event loop exited (e.g. Cmd-Q) without on_close_requested firing,
    // tear down the rest of the hosted simulator too.
    simulator::quit_simulator();
}
