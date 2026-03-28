// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::settings_permissions::SettingsPermissions,
    foundation_ur::{bytewords, Decoder as UrDecoder},
    log::debug,
    rxing::{
        common::HybridBinarizer, BarcodeFormat, BinaryBitmap, DecodeHints, Luma8LuminanceSource,
        MultiFormatReader,
    },
    slint_keyos_platform::{
        app,
        gui_server_api::{
            consts::{CAMERA_HEIGHT, CAMERA_WIDTH},
            navigation::qrscanner::{ScanQrOptions, ScanQrResult},
            InputMessage,
        },
        slint::{ComponentHandle, Timer, TimerMode},
        spawn_local, subscribe_scalar, StoredValue,
    },
    std::collections::HashSet,
    std::{rc::Rc, thread, time::Duration},
};

camera::use_api!();
haptics::use_api!();

const SCAN_INTERVAL_MS: u64 = 300;

const PROGRESS_MULTIPLIER: f32 = 200.0;
const PROGRESS_NUDGE: f32 = 5.0;
const PROGRESS_MAX: f32 = 95.0;

fn fudge_progress_for_ui(progress: f32, is_empty: bool) -> f32 {
    let nudge = if is_empty { 0.0 } else { PROGRESS_NUDGE };
    (progress * PROGRESS_MULTIPLIER + nudge).min(PROGRESS_MAX)
}

enum ScanQrProgress {
    Unchanged,
    Progress(f32),
    Complete(ScanQrResult),
}

struct AppState {
    status: ScanStatus,
    scanner: MultiFormatReader,
    luma_source: Luma8LuminanceSource,
    ur_decoder: UrDecoder,
}

impl AppState {
    fn scan_qr(&mut self) -> ScanQrProgress {
        let Ok(has_code) = self
            .scanner
            .decode_with_state(&mut BinaryBitmap::new(HybridBinarizer::new(self.luma_source.clone())))
            .inspect_err(|e| {
                if !matches!(e, rxing::Exceptions::NotFoundException(_)) {
                    log::warn!("Extract error: {:?}", e)
                }
            })
        else {
            return ScanQrProgress::Unchanged;
        };

        let data = has_code.getRawBytes().iter().as_slice();

        if let Ok(data_str) = std::str::from_utf8(data) {
            match (self.ur_decoder.is_empty(), foundation_ur::UR::parse(data_str.to_lowercase().as_str())) {
                (true, Ok(part)) => {
                    let bw = match part.as_bytewords() {
                        Some(v) => v,
                        None => return ScanQrProgress::Unchanged,
                    };

                    if let Err(e) = bytewords::validate(bw, bytewords::Style::Minimal) {
                        log::warn!("Bytewords error: {:?}", e);
                        return ScanQrProgress::Unchanged;
                    };

                    if let Err(e) = self.ur_decoder.receive(part) {
                        log::warn!("Could not receive UR part: {}", e);
                        return ScanQrProgress::Unchanged;
                    }
                }
                (false, Ok(part)) => {
                    if let Err(e) = self.ur_decoder.receive(part) {
                        log::warn!("Could not receive UR part: {}", e);
                        return ScanQrProgress::Unchanged;
                    }
                }
                (true, Err(_)) => (),
                (false, Err(_)) => return ScanQrProgress::Unchanged,
            }
        }

        if self.ur_decoder.is_complete() {
            let ur_type = match self.ur_decoder.ur_type() {
                Some(t) => t,
                None => {
                    log::warn!("UR has no type");
                    self.ur_decoder.clear();
                    return ScanQrProgress::Progress(0.0);
                }
            };

            let message_opt = match self.ur_decoder.message() {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("UR Decoder state error: {}", e);
                    self.ur_decoder.clear();
                    return ScanQrProgress::Progress(0.0);
                }
            };
            let message = match message_opt {
                Some(m) => m,
                None => {
                    log::warn!("No message in UR code");
                    self.ur_decoder.clear();
                    return ScanQrProgress::Progress(0.0);
                }
            };

            let res = ScanQrResult::new_ur2(String::from(ur_type), message);
            self.ur_decoder.clear();
            return ScanQrProgress::Complete(res);
        }

        // Only reachable if the ur_decoder is empty
        // and the data wasn't a valid UR frame
        if self.ur_decoder.is_empty() {
            return ScanQrProgress::Complete(ScanQrResult::new_qr(data));
        }

        // All frames have been processed, UR decoder is not complete or empty,
        // or no frames contained a valid QR/UR frame.
        // Numbers fudged to look nicer
        return ScanQrProgress::Progress(fudge_progress_for_ui(
            self.ur_decoder.estimated_percent_complete() as f32,
            self.ur_decoder.is_empty(),
        ));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanStatus {
    Idle,
    HasPendingNavRequest,
    HasQrData,
}

app!("QR Scanner");

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();

    let haptics = Rc::new(HapticsApi::default());

    let state = {
        let hints = DecodeHints {
            PossibleFormats: Some(HashSet::from([BarcodeFormat::QR_CODE])),
            TryHarder: Some(true),
            AlsoInverted: Some(true),
            ..Default::default()
        };
        let mut scanner = MultiFormatReader::default();
        scanner.set_hints(&hints);
        let luma_source =
            Luma8LuminanceSource::with_empty_image(CAMERA_WIDTH as usize, CAMERA_HEIGHT as usize);
        let state =
            AppState { status: ScanStatus::Idle, scanner, luma_source, ur_decoder: UrDecoder::default() };
        StoredValue::new(state)
    };

    while !cx.gui.is_camera_ready().expect("can't access the gui api") {
        debug!("Waiting for the camera to become ready...");
        thread::sleep(Duration::from_millis(100));
    }
    cx.gui.show_camera(137).expect("can't show camera");

    log::info!("Running QR scanner");

    // use StoredValue due to mutability requirement of CameraApi
    let camera_api = StoredValue::new(CameraApi::default());

    ui.global::<Callbacks>().on_enable_camera_clicked({
        let gui_api = cx.gui.clone();
        let settings = SettingsApi::default();

        move || {
            settings.set_camera_enabled(true);
            gui_api.request_redraw().ok();
            true
        }
    });

    // Cancels the navigation modal request and notifies the caller
    ui.global::<Callbacks>().on_button_clicked({
        let gui_api = cx.gui.clone();
        move |action| {
            let mut state = state.borrow_mut();
            state.status = ScanStatus::Idle;
            state.ur_decoder.clear();
            gui_api.navigate_finish(ScanQrResult::from(action).serialize()).expect("finish nav");
        }
    });

    spawn_local({
        let ui = ui.clone_strong();
        async move {
            let mut camera_events =
                subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeCameraEnabled);
            while let Some(event) = camera_events.next().await {
                ui.global::<Global>().set_camera_enabled(event.0);
            }
        }
    })
    .detach();

    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(SCAN_INTERVAL_MS), {
        let haptics = haptics.clone();
        let gui_api = cx.gui.clone();
        let ui = ui.clone_strong();
        move || {
            let mut state = state.borrow_mut();
            let mut camera_api = camera_api.borrow_mut();

            // Already scanned something, stop scanning until the user press "Rescan"
            if state.status != ScanStatus::HasPendingNavRequest {
                return;
            }

            if camera_api.is_frame_ready() {
                debug!("Fetching frame for QR detection");

                {
                    let luma8 = &mut state.luma_source.get_matrix_mut();
                    #[cfg(keyos)]
                    {
                        let Ok(frame) = camera_api.get_frame_mirror() else {
                            log::error!("Could not get frame mirror");
                            return;
                        };

                        rgb565_to_luma8(frame.as_slice(), luma8);
                    }

                    #[cfg(not(keyos))]
                    {
                        let Ok(addr) = camera_api.get_frame_buffer_addr() else {
                            log::error!("Could not get frame buffer");
                            return;
                        };
                        let ptr = addr as *const u8;
                        let slice = unsafe {
                            core::slice::from_raw_parts(ptr, gui_server_api::consts::CAMERA_FB_SIZE_BYTES)
                        };
                        rgba_to_luma8(slice, luma8);
                    }
                }

                match state.scan_qr() {
                    ScanQrProgress::Unchanged => (),
                    ScanQrProgress::Progress(p) => ui.global::<Global>().set_scan_progress(p),
                    ScanQrProgress::Complete(res) => {
                        state.status = ScanStatus::HasQrData;
                        haptics.click();
                        ui.global::<Global>().set_scan_progress(0.0);
                        gui_api.navigate_finish(res.serialize()).expect("finish nav");
                    }
                }
            }
        }
    });

    cx.set_input_handler({
        let ui = ui.clone_strong();
        let gui_api = cx.gui.clone();
        move |input| {
            if input.msg == InputMessage::NavigationFocused {
                let Ok(Some(nav_bytes)) = gui_api.navigate_pending() else {
                    log::error!("Navigation focused but no pending nav request");
                    return;
                };

                let Some(options) = ScanQrOptions::from_slice(&nav_bytes) else {
                    log::error!("Failed to parse ScanQrOptions from a nav request");
                    return;
                };

                ui.global::<Global>().set_header_title(options.header_title.into());
                ui.global::<Global>().set_header_left_icon(options.header_left_icon.into());
                ui.global::<Global>().set_header_left_text(options.header_left_text.into());
                ui.global::<Global>().set_header_right_icon(options.header_right_icon.into());
                ui.global::<Global>().set_header_right_text(options.header_right_text.into());
                ui.global::<Global>().set_message(options.message.into());
                ui.global::<Global>().set_button_icon(options.button_icon.into());
                ui.global::<Global>().set_button_text(options.button_text.into());

                //TODO: remove considerable amount of lag
                gui_api.request_redraw().ok();

                state.borrow_mut().status = ScanStatus::HasPendingNavRequest;
            }
        }
    });

    ui.run().expect("UI running");
}

#[allow(dead_code)]
fn rgb565_to_luma8(from: &[u8], to: &mut [u8]) {
    for (chunk, luma) in from.chunks_exact(2).zip(to.iter_mut()) {
        let pix = rgb565::Rgb565::from_bgr565_le([chunk[0], chunk[1]]);
        let pix = pix.to_rgb888_components();
        let r = pix[2];
        let g = pix[1];
        let b = pix[0];

        *luma = rgb_components_to_luma8(r, g, b);
    }
}

#[allow(dead_code)]
fn rgba_to_luma8(from: &[u8], to: &mut [u8]) {
    for (chunk, luma) in from.chunks_exact(4).zip(to.iter_mut()) {
        let r = chunk[0];
        let g = chunk[1];
        let b = chunk[2];

        *luma = rgb_components_to_luma8(r, g, b);
    }
}

fn rgb_components_to_luma8(r: u8, g: u8, b: u8) -> u8 {
    let sum = r as u16 * 59 + g as u16 * 150 + b as u16 * 29;
    (sum >> 8) as u8
}

impl From<ScanQrAction> for ScanQrResult {
    fn from(value: ScanQrAction) -> Self {
        match value {
            ScanQrAction::LeftClicked => ScanQrResult::LeftClicked,
            ScanQrAction::RightClicked => ScanQrResult::RightClicked,
            ScanQrAction::ButtonClicked => ScanQrResult::ButtonClicked,
        }
    }
}
