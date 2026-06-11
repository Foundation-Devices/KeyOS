// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod state;

use std::time::Duration;

use log::trace;
use slint_keyos_platform::{
    app,
    gui_server_api::InputMessage,
    slint::{ComponentHandle, Timer, TimerMode},
    spawn_local, subscribe_scalar, StoredValue,
};
use state::AppState;
use xous::MessageEnvelope;

#[cfg(not(feature = "recovery-os"))]
backup::use_api!();
#[cfg(not(feature = "recovery-os"))]
bt::use_api!();
#[cfg(keyos)]
nfc::use_api!();
#[cfg(all(keyos, not(feature = "recovery-os")))]
camera::use_api!();
#[cfg(keyos)]
haptics::use_api!();
power_manager::use_ext_api!();
#[cfg(keyos)]
usb::use_device_api!();
#[cfg(keyos)]
usb::use_host_api!();
#[cfg(not(feature = "recovery-os"))]
security::use_api!();

const STATUS_UPDATE_INTERVAL: Duration = Duration::from_millis(1000);

app!(
    "Control Center",
    role = control_center,
    height = gui_server_api::consts::CONTROL_CENTER_HEIGHT_EXPANDED_PX
);

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let state = StoredValue::new(AppState::new(ui.clone_strong()));

    cx.set_input_handler(move |mut input| {
        let state = state.borrow();
        match input.msg {
            InputMessage::Custom1 => {}
            InputMessage::Custom2 => {}
            InputMessage::Custom3 => {
                handle_is_expanded(&mut input.envelope, &state);
            }
            InputMessage::Custom4 => {
                handle_set_shut_down_mode(&mut input.envelope, &state);
            }
            _ => (),
        }
    });

    #[cfg(not(feature = "recovery-os"))]
    init_keyos(state);

    #[cfg(feature = "recovery-os")]
    {
        ui.set_is_shut_down_mode(true);
        ui.global::<State>().set_is_bluetooth_enabled(false);
        ui.global::<State>().set_is_bluetooth_in_use(false);
        ui.global::<State>().set_is_camera_enabled(false);
        ui.global::<State>().set_is_camera_in_use(false);
        ui.global::<State>().set_is_nfc_enabled(false);
        ui.global::<State>().set_is_nfc_in_use(false);
    }

    let gui = cx.gui.clone();
    spawn_local(async move {
        let mut status_updates = subscribe_scalar::<
            power_manager_ext_permissions::PowerManagerExtPermissions,
            _,
        >(power_manager::messages::StatusSubscribe);
        while let Some(status) = status_updates.next().await {
            let mut state = state.borrow_mut();
            state.battery_percent = status.battery_percent;

            // Give haptic feedback when the USB is plugged in and charging begins
            let is_charging = status.charge_status == power_manager::ChargeStatus::Charging;
            if !is_charging && status.battery_percent == 0 {
                log::info!("Battery reserve reached, forcing shutdown");
                gui.shutdown().ok();
            }
            let is_usb_attached = status.attached_state != power_manager::AttachedState::None;
            if is_charging && is_usb_attached && !state.is_usb_attached {
                #[cfg(keyos)]
                HapticsApi::default().double_click();
            }
            state.is_charging = is_charging;
            state.is_usb_attached = is_usb_attached;
        }
    })
    .detach();

    #[cfg(not(feature = "recovery-os"))]
    ui.global::<Callbacks>().on_nfc_enabled_changed(move |enabled| {
        let state = state.borrow();
        state.slint_state().set_is_nfc_enabled(enabled);
        state.settings.set_nfc_enabled(enabled);
    });

    #[cfg(not(feature = "recovery-os"))]
    ui.global::<Callbacks>().on_bluetooth_enabled_changed(move |enabled| {
        let state = state.borrow();
        state.slint_state().set_is_bluetooth_enabled(enabled);
        state.settings.set_bluetooth_enabled(enabled);
    });

    #[cfg(not(feature = "recovery-os"))]
    ui.global::<Callbacks>().on_camera_enabled_changed(move |enabled| {
        let state = state.borrow();
        state.slint_state().set_is_camera_enabled(enabled);
        state.settings.set_camera_enabled(enabled);
    });

    #[cfg(not(feature = "recovery-os"))]
    ui.global::<Callbacks>().on_usb_enabled_changed(move |enabled| {
        let state = state.borrow();
        state.slint_state().set_is_usb_enabled(enabled);
        state.settings.set_usb_enabled(enabled);
    });

    ui.global::<Callbacks>().on_shut_down({
        let gui = cx.gui.clone();
        move || {
            log::info!("Asking shutdown from GUI server");
            gui.shutdown().ok();
        }
    });

    let timer = Timer::default();
    timer.start(TimerMode::Repeated, STATUS_UPDATE_INTERVAL, move || {
        let state = state.borrow();
        #[cfg(keyos)]
        update_hw_state(&state);
        update_battery_state(&state);
        #[cfg(not(feature = "recovery-os"))]
        state.update_system_msg();
        #[cfg(not(feature = "recovery-os"))]
        state.update_time();
    });

    ui.run().expect("UI running");
}

#[cfg(not(feature = "recovery-os"))]
fn init_keyos(state: StoredValue<AppState>) {
    use bt::messages::SubscribeBleState;
    use slint_keyos_platform::{settings, spawn_local, subscribe_archive, subscribe_scalar};

    use crate::{
        backup_permissions::BackupPermissions, bt_permissions::BluetoothPermissions,
        settings_permissions::SettingsPermissions,
    };

    spawn_local(async move {
        let mut sub =
            subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeScreenBrightness);
        while let Some(brightness) = sub.next().await {
            let state = state.borrow();
            state.slint_state().set_brightness(brightness.0 as f32);
        }
    })
    .detach();

    spawn_local(async move {
        let mut sub =
            subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeUseStandardTimeFormat);
        while let Some(time_24) = sub.next().await {
            let mut state = state.borrow_mut();
            state.use_standard_time_format = time_24;
            state.update_time();
        }
    })
    .detach();

    spawn_local(async move {
        let mut sub = subscribe_archive::<SettingsPermissions, _>(settings::messages::SubscribeTimeZone);
        while let Some(timezone) = sub.next().await {
            let mut state = state.borrow_mut();
            state.timezone = timezone;
            state.update_time();
        }
    })
    .detach();

    spawn_local(async move {
        let mut sub = subscribe_scalar::<BluetoothPermissions, _>(SubscribeBleState);
        while let Some(bt_state) = sub.next().await {
            let state = state.borrow();
            let (bt_enabled, bt_rssi) = match bt_state {
                bt::State::WaitingForConnection => (true, None),
                bt::State::Connected { rssi } => (true, Some(rssi)),
                _ => (false, None),
            };
            state.slint_state().set_is_bluetooth_enabled(bt_enabled);
            // TODO: enrich UI to show RSSI ?
            state.slint_state().set_is_bluetooth_in_use(bt_rssi.is_some());
        }
    })
    .detach();

    spawn_local(async move {
        let mut status_updates = subscribe_scalar::<BackupPermissions, _>(backup::messages::StatusSubscribe);
        while let Some(status) = status_updates.next().await {
            log::info!("Backup status update: {status:?}");
            state.borrow_mut().last_backup_at = status.last_backup_at;
        }
    })
    .detach();

    state
        .borrow()
        .ui
        .global::<Callbacks>()
        .on_brightness_changed(move |value| state.borrow().settings.set_screen_brightness(value as u8));

    spawn_local(async move {
        let mut sub = subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeNfcEnabled);
        while let Some(enabled) = sub.next().await {
            let state = state.borrow();
            state.slint_state().set_is_nfc_enabled(enabled.0);
        }
    })
    .detach();

    spawn_local(async move {
        let mut sub = subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeCameraEnabled);
        while let Some(enabled) = sub.next().await {
            let state = state.borrow();
            state.slint_state().set_is_camera_enabled(enabled.0);
        }
    })
    .detach();

    spawn_local(async move {
        let mut sub = subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeUsbEnabled);
        while let Some(enabled) = sub.next().await {
            let state = state.borrow();
            state.slint_state().set_is_usb_enabled(enabled.0);
        }
    })
    .detach();

    spawn_local(async move {
        let mut sub =
            subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeBluetoothEnabled);
        while let Some(enabled) = sub.next().await {
            let state = state.borrow();
            state.slint_state().set_is_bluetooth_enabled(enabled.0);
        }
    })
    .detach();

    spawn_local(async move {
        let mut sub = subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeDebugTouch);
        while let Some(debug) = sub.next().await {
            let state = state.borrow();
            state.ui.set_debug_show_corner_deadzone(debug.0);
        }
    })
    .detach();
}

fn update_battery_state(state: &AppState) {
    state.ui.set_control_center_battery(state.battery_percent as i32);
    state.ui.set_control_center_is_charging(state.is_charging);
}

#[cfg(all(keyos, feature = "recovery-os"))]
fn update_hw_state(state: &AppState) {
    state.slint_state().set_is_usb_enabled(state.usb_host.is_enabled().unwrap_or(false));
    state.slint_state().set_is_usb_in_use(state.usb_host.is_connected().unwrap_or(false));
}

#[cfg(all(keyos, not(feature = "recovery-os")))]
fn update_hw_state(state: &AppState) {
    state.slint_state().set_is_nfc_in_use(state.nfc.is_active().unwrap_or(false));
    state.slint_state().set_is_camera_in_use(state.camera.is_in_use());
    state.slint_state().set_is_usb_in_use(
        state.usb_host.is_connected().unwrap_or(false) || state.usb_device.is_connected().unwrap_or(false),
    );
}

fn handle_is_expanded(msg: &mut MessageEnvelope, state: &AppState) {
    if let Some(xous::ScalarMessage { arg1, .. }) = msg.body.scalar_message() {
        let ui = &state.ui;
        let is_expanded = *arg1 != 0;

        trace!("Setting is_expanded = `{}`", is_expanded);
        #[cfg(not(feature = "recovery-os"))]
        if !is_expanded {
            ui.set_is_shut_down_mode(false);
        }

        ui.set_control_center_is_expanded(is_expanded);
    }
}

fn handle_set_shut_down_mode(msg: &mut MessageEnvelope, state: &AppState) {
    if let Some(xous::ScalarMessage { .. }) = msg.body.scalar_message() {
        state.ui.set_is_shut_down_mode(true);
    }
}

#[test]
fn ensure_control_center_height_matches_ui() {
    use slint::platform::software_renderer::MinimalSoftwareWindow;
    use slint_keyos_platform::gui_server_api::consts::{
        CONTROL_CENTER_HEIGHT_COLLAPSED_PX, CONTROL_CENTER_HEIGHT_EXPANDED_PX, SCREEN_HEIGHT, SCREEN_WIDTH,
    };

    struct TestPlatform(std::rc::Rc<MinimalSoftwareWindow>);

    impl slint_keyos_platform::slint::platform::Platform for TestPlatform {
        fn create_window_adapter(
            &self,
        ) -> Result<std::rc::Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }

    let window =
        MinimalSoftwareWindow::new(slint::platform::software_renderer::RepaintBufferType::SwappedBuffers);

    // Make sure the window covers our entire screen.
    window.set_size(slint::PhysicalSize::new(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32));
    slint_keyos_platform::slint::platform::set_platform(Box::new(TestPlatform(window))).unwrap();

    let ui = AppWindow::new().unwrap();

    let size = ui.global::<UISize>();

    assert_eq!(
        size.get_control_center_expanded_height(),
        CONTROL_CENTER_HEIGHT_EXPANDED_PX as f32,
        "expanded height mismatch"
    );
    assert_eq!(
        size.get_control_center_status_bar_height(),
        CONTROL_CENTER_HEIGHT_COLLAPSED_PX as f32,
        "height mismatch"
    );
}
