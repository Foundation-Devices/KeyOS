// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::OnceCell,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
#[cfg(keyos)]
use std::{io::Write, time::Instant};

#[cfg(keyos)]
use fs::{Location, OpenFlags};
use {
    bt::AdvChannel,
    slint_keyos_platform::{app, gui_server_api::InputMessage},
};

bt::use_api!();
#[cfg(keyos)]
nfc::use_api!();
camera::use_api!();

const CAMERA_Y_POS: u16 = 528;

const BT_ENABLED: usize = 1 << 0;
const NFC_ENABLED: usize = 1 << 1;
const USB_ENABLED: usize = 1 << 2;
const CAM_ENABLED: usize = 1 << 3;

static TEST_END: AtomicBool = AtomicBool::new(false);
thread_local! {
    static UI: OnceCell<AppWindow> = OnceCell::new();
}

const APP_NAME: &'static str = "Regulatory Testing";

fn queue_with_ui(f: impl FnOnce(&AppWindow) + Send + 'static) {
    slint_keyos_platform::spawn(async move { UI.with(|ui| f(ui.get().unwrap())) }).detach();
}

app!(APP_NAME);

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Debug);

    // disable everything by default
    BluetoothApi::default().disable_ble().expect("disable Bluetooth");
    #[cfg(keyos)]
    NfcApi::default().set_enabled(false).expect("disable NFC");
    #[cfg(keyos)]
    usb::host::api::UsbHost::default().set_enabled(false).expect("disable USB host");
    #[cfg(keyos)]
    CameraApi::default().set_enabled(false);

    // Handle the app being hidden.
    // In the real test setting there wouldn't be an option to dismiss the app (similar to onboarding),
    // but the app is still receives a hidden notification if the screen is turned off, so this
    // should be properly handled.
    cx.set_input_handler({
        let ui = ui.clone_strong();
        let gui_api = cx.gui.clone();
        move |input| {
            if input.msg == InputMessage::Hidden {
                // Stop the currently running test (if any)
                ui.global::<State>().set_is_emissions_test_running(false);
                ui.global::<State>().set_camera_message("Idle".into());
                gui_api.hide_camera().expect("hide camera");
                TEST_END.store(true, Ordering::SeqCst);
            }
        }
    });

    let ui_cloned = ui.clone_strong();
    let gui_api = cx.gui.clone();
    ui.global::<Callbacks>().on_start_emissions_test(move || {
        log::info!("Starting emissions test");
        TEST_END.store(false, Ordering::SeqCst);
        ui_cloned.global::<State>().set_is_emissions_test_running(true);
        if ui_cloned.global::<State>().get_bluetooth_enabled() {
            let mut bt_api = BluetoothApi::default();
            loop {
                if let Ok(state) = bt_api.state() {
                    if state.is_booted() {
                        break;
                    }
                }
                ui_cloned.global::<State>().set_bluetooth_message("Wait for BT chip to boot".into());
                std::thread::sleep(Duration::from_millis(1000));
            }
        }
        #[cfg(keyos)]
        if ui_cloned.global::<State>().get_nfc_enabled() {
            NfcApi::default().set_enabled(true).expect("enable NFC");
        }
        if ui_cloned.global::<State>().get_test_mode() == 0 {
            // Cycle mode
            let interval = ui_cloned.global::<State>().get_cycle_interval();
            let enabled = if ui_cloned.global::<State>().get_bluetooth_enabled() { BT_ENABLED } else { 0 }
                | if ui_cloned.global::<State>().get_nfc_enabled() { NFC_ENABLED } else { 0 }
                | if ui_cloned.global::<State>().get_usb_enabled() { USB_ENABLED } else { 0 }
                | if ui_cloned.global::<State>().get_camera_enabled() { CAM_ENABLED } else { 0 };
            let gui_api = gui_api.clone();
            std::thread::spawn(move || {
                let interval = Duration::from_millis(interval as u64);
                loop {
                    if TEST_END.load(Ordering::SeqCst) {
                        cleanup(enabled);
                        log::trace!("return thread emission cycle");
                        return;
                    }
                    if enabled & BT_ENABLED != 0 {
                        queue_with_ui(move |ui| {
                            log::info!("Advertising on Bluetooth...");
                            ui.global::<State>().set_bluetooth_message("Advertising on Bluetooth...".into())
                        });
                        let mut bt_api = BluetoothApi::default();
                        bt_api.enable_ble().expect("enable Bluetooth");
                        std::thread::sleep(interval);
                        bt_api.disable_ble().expect("disable Bluetooth");
                        queue_with_ui(move |ui| ui.global::<State>().set_bluetooth_message("Idle".into()));
                    }
                    if TEST_END.load(Ordering::SeqCst) {
                        cleanup(enabled);
                        log::trace!("return thread emission cycle 2");
                        return;
                    }
                    if enabled & NFC_ENABLED != 0 {
                        queue_with_ui(move |ui| {
                            log::info!("Reading from NFC card...");
                            ui.global::<State>().set_nfc_message("Reading from NFC card...".into())
                        });
                        #[cfg(not(keyos))]
                        std::thread::sleep(interval);
                        #[cfg(keyos)]
                        {
                            let start = Instant::now();
                            let mut nfc_api = NfcApi::default();
                            while start.elapsed() < interval {
                                match nfc_api.read_ndef_raw_msg(Duration::from_millis(300)) {
                                    Ok((_, raw_msg)) => {
                                        log::debug!("Read NDEF message ({} bytes)", raw_msg.len());
                                    }
                                    Err(e) => {
                                        log::warn!("Failed to read NDEF message: {:?}", e);
                                    }
                                }
                            }
                        }
                        queue_with_ui(move |ui| ui.global::<State>().set_nfc_message("Idle".into()));
                    }
                    if TEST_END.load(Ordering::SeqCst) {
                        cleanup(enabled);
                        log::trace!("return thread emission cycle 3");
                        return;
                    }
                    if enabled & USB_ENABLED != 0 {
                        queue_with_ui(move |ui| {
                            log::info!("Writing file on USB...");
                            ui.global::<State>().set_usb_message("Writing file on USB...".into())
                        });
                        #[cfg(not(keyos))]
                        std::thread::sleep(interval);
                        #[cfg(keyos)]
                        {
                            let start = Instant::now();
                            const TEST_FILE_NAME: &str = "regulatory_file.bin";
                            let fs = FileSystem::default();
                            usb::host::api::UsbHost::default().set_enabled(true).expect("enable USB host");
                            while fs.open_dir("/", Location::Usb).is_err() {
                                queue_with_ui(move |ui| {
                                    ui.global::<State>().set_usb_message("Insert USB drive".into())
                                });
                                log::info!("Waiting for Usb drive insertion");
                                std::thread::sleep(Duration::from_secs(2));
                                if start.elapsed() > interval {
                                    break;
                                }
                            }
                            if start.elapsed() < interval {
                                let data: Vec<u8> = (0..1024).map(|i| i as u8).collect();
                                let mut cnt = 0usize;
                                while start.elapsed() < interval {
                                    fs.remove(TEST_FILE_NAME, Location::Usb).ok();
                                    if let Ok(mut file) = fs.open_file(
                                        TEST_FILE_NAME,
                                        Location::Usb,
                                        OpenFlags { read: false, write: true, create: true },
                                    ) {
                                        file.write_all(&data).ok();
                                        file.flush().ok();
                                        drop(file);
                                    }
                                    cnt += 1;
                                    queue_with_ui(move |ui| {
                                        ui.global::<State>().set_usb_message(
                                            format!("Writing file on USB ({})...", cnt).into(),
                                        )
                                    });
                                }
                            }
                        }
                        queue_with_ui(move |ui| ui.global::<State>().set_usb_message("Idle".into()));
                    }
                    if TEST_END.load(Ordering::SeqCst) {
                        cleanup(enabled);
                        log::trace!("return thread emission cycle 4");
                        return;
                    }
                    if enabled & CAM_ENABLED != 0 {
                        queue_with_ui(move |ui| {
                            log::info!("Showing Camera stream...");
                            ui.global::<State>().set_camera_message("Showing Camera stream...".into())
                        });
                        gui_api.show_camera(CAMERA_Y_POS).expect("show camera");
                        let camera_api = CameraApi::default();
                        camera_api.set_enabled(true);
                        std::thread::sleep(interval);
                        camera_api.set_enabled(false);
                        gui_api.hide_camera().expect("hide camera");

                        queue_with_ui(move |ui| ui.global::<State>().set_camera_message("Idle".into()));
                    }
                }
            });
        } else {
            // Continuous mode
            if ui_cloned.global::<State>().get_bluetooth_enabled() {
                BluetoothApi::default().enable_ble().expect("enable Bluetooth");
                log::info!("Advertising on Bluetooth...");
                ui_cloned.global::<State>().set_bluetooth_message("Advertising on Bluetooth...".into());
            }
            if ui_cloned.global::<State>().get_nfc_enabled() {
                #[cfg(keyos)]
                std::thread::spawn(|| {
                    let mut nfc_api = NfcApi::default();
                    nfc_api.set_enabled(true).expect("enable NFC");
                    let mut cnt = 0usize;
                    loop {
                        if TEST_END.load(Ordering::SeqCst) {
                            NfcApi::default().set_enabled(false).expect("disable NFC");
                            log::debug!("disabling NFC");
                            queue_with_ui(move |ui| {
                                ui.global::<State>().set_nfc_message(format!("Idle...").into())
                            });
                            log::trace!("return thread nfc from read loop");
                            return;
                        } else {
                            queue_with_ui(move |ui| {
                                ui.global::<State>()
                                    .set_nfc_message(format!("Reading from NFC card ({})...", cnt).into())
                            });
                        }
                        match nfc_api.read_ndef_raw_msg(Duration::from_millis(300)) {
                            Ok((_, raw_msg)) => {
                                log::debug!("Read NDEF message ({} bytes)", raw_msg.len());
                            }
                            Err(e) => {
                                log::warn!("Failed to read NDEF message: {:?}", e);
                            }
                        }
                        cnt += 1;
                    }
                });
                log::info!("Reading from NFC card...");
                ui_cloned.global::<State>().set_nfc_message("Reading from NFC card...".into());
            }
            if ui_cloned.global::<State>().get_usb_enabled() {
                #[cfg(keyos)]
                std::thread::spawn(|| {
                    const TEST_FILE_NAME: &str = "regulatory_file.bin";
                    let fs = FileSystem::default();
                    usb::host::api::UsbHost::default().set_enabled(true).expect("enable USB host");
                    while fs.open_dir("/", Location::Usb).is_err() {
                        queue_with_ui(move |ui| {
                            ui.global::<State>().set_usb_message("Insert USB drive".into())
                        });
                        log::info!("Waiting for Usb drive insertion");
                        std::thread::sleep(Duration::from_secs(2));
                        if TEST_END.load(Ordering::SeqCst) {
                            log::trace!("return thread usb from connect loop");
                            return;
                        }
                    }
                    let data: Vec<u8> = (0..1024).map(|i| i as u8).collect();
                    let mut cnt = 0usize;
                    loop {
                        if TEST_END.load(Ordering::SeqCst) {
                            log::trace!("return thread usb from write loop");
                            return;
                        }
                        fs.remove(TEST_FILE_NAME, Location::Usb).ok();
                        if let Ok(mut file) = fs.open_file(
                            TEST_FILE_NAME,
                            Location::Usb,
                            OpenFlags { read: false, write: true, create: true },
                        ) {
                            file.write_all(&data).ok();
                            file.flush().ok();
                            drop(file);
                        }
                        if TEST_END.load(Ordering::SeqCst) {
                            log::trace!("return thread usb from write loop 2");
                            return;
                        }
                        cnt += 1;
                        queue_with_ui(move |ui| {
                            ui.global::<State>()
                                .set_usb_message(format!("Writing file on USB ({})...", cnt).into())
                        });
                    }
                });
                log::info!("Writing file on USB...");
                ui_cloned.global::<State>().set_usb_message("Writing file on USB...".into());
            }
            if ui_cloned.global::<State>().get_camera_enabled() {
                gui_api.show_camera(CAMERA_Y_POS).expect("show camera");
                CameraApi::default().set_enabled(true);
                log::info!("Showing Camera stream...");
                ui_cloned.global::<State>().set_camera_message("Showing Camera stream...".into());
            }
        }
    });

    let ui_cloned = ui.clone_strong();
    ui.global::<Callbacks>().on_start_fcc_test(move || {
        log::info!("Starting FCC test");
        TEST_END.store(false, Ordering::SeqCst);
        ui_cloned.global::<State>().set_is_fcc_test_running(true);
        if ui_cloned.global::<State>().get_bluetooth_enabled() {
            let mut bt_api = BluetoothApi::default();
            loop {
                if let Ok(state) = bt_api.state() {
                    if state.is_booted() {
                        break;
                    }
                }
                ui_cloned.global::<State>().set_bluetooth_message("Wait for BT chip to boot".into());
                std::thread::sleep(Duration::from_millis(1000));
            }
            bt_api
                .disable_adv_channels(match ui_cloned.global::<State>().get_bluetooth_channel() {
                    37 => AdvChannel::C38 | AdvChannel::C39,
                    38 => AdvChannel::C37 | AdvChannel::C39,
                    39 => AdvChannel::C37 | AdvChannel::C38,
                    _ => AdvChannel::empty(),
                })
                .expect("disable adv channels");
        }
        #[cfg(keyos)]
        if ui_cloned.global::<State>().get_nfc_enabled() {
            NfcApi::default().set_enabled(true).expect("enable NFC");
        }

        if ui_cloned.global::<State>().get_test_mode() == 0 {
            // Cycle mode
            let interval = ui_cloned.global::<State>().get_cycle_interval();
            let enabled = if ui_cloned.global::<State>().get_bluetooth_enabled() { BT_ENABLED } else { 0 }
                | if ui_cloned.global::<State>().get_nfc_enabled() { NFC_ENABLED } else { 0 };
            let bluetooth_channel = ui_cloned.global::<State>().get_bluetooth_channel();
            std::thread::spawn(move || {
                let interval = Duration::from_millis(interval as u64);
                loop {
                    if TEST_END.load(Ordering::SeqCst) {
                        cleanup(enabled);
                        log::trace!("return thread fcc cycle 1");
                        return;
                    }
                    if enabled & BT_ENABLED != 0 {
                        queue_with_ui(move |ui| {
                            let bt_text = match bluetooth_channel {
                                37 => "Advertising on Bluetooth (37)...",
                                38 => "Advertising on Bluetooth (38)...",
                                39 => "Advertising on Bluetooth (39)...",
                                _ => "Advertising on Bluetooth (all)...",
                            };
                            log::info!("{}", bt_text);
                            ui.global::<State>().set_bluetooth_message(bt_text.into())
                        });
                        let mut bt_api = BluetoothApi::default();
                        bt_api.enable_ble().expect("enable BT");
                        std::thread::sleep(interval);
                        bt_api.disable_ble().expect("disable BT");
                        queue_with_ui(move |ui| ui.global::<State>().set_bluetooth_message("Idle".into()));
                    }
                    if TEST_END.load(Ordering::SeqCst) {
                        cleanup(enabled);
                        log::trace!("return thread fcc cycle 2");
                        return;
                    }
                    if enabled & NFC_ENABLED != 0 {
                        log::info!("Reading from NFC card...");
                        queue_with_ui(move |ui| {
                            ui.global::<State>().set_nfc_message("Reading from NFC card...".into())
                        });
                        #[cfg(not(keyos))]
                        std::thread::sleep(interval);
                        #[cfg(keyos)]
                        {
                            let start = Instant::now();
                            let mut nfc_api = NfcApi::default();
                            while start.elapsed() < interval {
                                match nfc_api.read_ndef_raw_msg(Duration::from_millis(300)) {
                                    Ok((_, raw_msg)) => {
                                        log::debug!("Read NDEF message ({} bytes)", raw_msg.len());
                                    }
                                    Err(e) => {
                                        log::warn!("Failed to read NDEF message: {:?}", e);
                                    }
                                }
                            }
                        }
                        queue_with_ui(move |ui| ui.global::<State>().set_nfc_message("Idle".into()));
                    }
                }
            });
        } else {
            // Continuous mode
            if ui_cloned.global::<State>().get_bluetooth_enabled() {
                BluetoothApi::default().enable_ble().expect("enable BT");
                let bt_text = match ui_cloned.global::<State>().get_bluetooth_channel() {
                    37 => "Advertising on Bluetooth (37)...".to_string(),
                    38 => "Advertising on Bluetooth (38)...".to_string(),
                    39 => "Advertising on Bluetooth (39)...".to_string(),
                    _ => "Advertising on Bluetooth (all)...".to_string(),
                };
                log::info!("{}", bt_text);
                ui_cloned.global::<State>().set_bluetooth_message(bt_text.into());
            }
            if ui_cloned.global::<State>().get_nfc_enabled() {
                #[cfg(keyos)]
                std::thread::spawn(|| {
                    let mut nfc_api = NfcApi::default();
                    nfc_api.set_enabled(true).expect("enable NFC");
                    let mut cnt = 0usize;
                    loop {
                        if TEST_END.load(Ordering::SeqCst) {
                            NfcApi::default().set_enabled(false).expect("disable NFC");
                            log::debug!("disabling NFC");
                            queue_with_ui(move |ui| {
                                ui.global::<State>().set_nfc_message(format!("Idle...").into())
                            });
                            return;
                        } else {
                            queue_with_ui(move |ui| {
                                ui.global::<State>()
                                    .set_nfc_message(format!("Reading from NFC card ({})...", cnt).into())
                            });
                        }
                        match nfc_api.read_ndef_raw_msg(Duration::from_millis(300)) {
                            Ok((_, raw_msg)) => {
                                log::debug!("Read NDEF message ({} bytes)", raw_msg.len());
                            }
                            Err(e) => {
                                log::warn!("Failed to read NDEF message: {:?}", e);
                            }
                        }
                        cnt += 1;
                    }
                });
                log::info!("Reading from NFC card...");
                ui_cloned.global::<State>().set_nfc_message("Reading from NFC card...".into());
            }
        }
    });

    let ui_cloned = ui.clone_strong();
    let gui_api = cx.gui.clone();
    ui.global::<Callbacks>().on_stop_emissions_test(move || {
        log::info!("Stopping emissions test");
        TEST_END.store(true, Ordering::SeqCst);
        ui_cloned.global::<State>().set_is_emissions_test_running(false);
        if ui_cloned.global::<State>().get_usb_enabled() {
            #[cfg(keyos)]
            usb::host::api::UsbHost::default().set_enabled(false).expect("disable USB host");
            log::debug!("disabling USB host");
            ui_cloned.global::<State>().set_usb_message("Idle".into());
        }
        if ui_cloned.global::<State>().get_camera_enabled() {
            log::debug!("disabling camera");
            ui_cloned.global::<State>().set_camera_message("Idle".into());
            CameraApi::default().set_enabled(false);
            gui_api.hide_camera().expect("hide camera");
        }
    });

    let ui_cloned = ui.clone_strong();
    ui.global::<Callbacks>().on_stop_fcc_test(move || {
        log::info!("Stopping FCC test");
        TEST_END.store(true, Ordering::SeqCst);
        ui_cloned.global::<State>().set_is_fcc_test_running(false);
        if ui_cloned.global::<State>().get_bluetooth_enabled() {
            BluetoothApi::default().disable_ble().expect("disable Bluetooth");
            log::debug!("disabling Bluetooth");
            ui_cloned.global::<State>().set_bluetooth_message("Idle".into());
        }
    });

    UI.with(|ui_global| ui_global.set(ui.clone_strong()).ok());

    ui.run().expect("UI running");
}

fn cleanup(enabled: usize) {
    if enabled & NFC_ENABLED != 0 {
        #[cfg(keyos)]
        NfcApi::default().set_enabled(false).expect("disable NFC");
        log::debug!("disabling NFC");
        queue_with_ui(move |ui| ui.global::<State>().set_nfc_message(format!("Idle...").into()));
    }
    if enabled & BT_ENABLED != 0 {
        BluetoothApi::default().disable_ble().expect("disable Bluetooth");
        log::debug!("disabling Bluetooth");
        queue_with_ui(move |ui| ui.global::<State>().set_bluetooth_message("Idle".into()));
    }
}
