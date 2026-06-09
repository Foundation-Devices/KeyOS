// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
//
// SPDX-FileCopyrightText: 2026 immz https://github.com/immz4
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{camera_permissions::CameraPermissions, settings_permissions::SettingsPermissions},
    app_manifest::{QrMatchRule, QrMatchSubRule},
    camera::{Frame, CAMERA_HEIGHT, CAMERA_WIDTH},
    foundation_ur::{bytewords, Decoder as UrDecoder},
    regex::Regex,
    rxing::{
        common::HybridBinarizer, BarcodeFormat, BinaryBitmap, DecodeHints, Exceptions, Luma8LuminanceSource,
        MultiFormatReader,
    },
    serde_json::from_slice,
    slint_keyos_platform::{
        app,
        gui_server_api::{
            navigation::qrscanner::{ScanQrMatchedRule, ScanQrMatchingApp, ScanQrOptions, ScanQrResult},
            InputMessage,
        },
        slint::{ComponentHandle, Timer, TimerMode},
        spawn_local, subscribe_scalar, try_subscribe_scalar, StoredValue,
    },
    std::{collections::HashSet, rc::Rc, time::Duration},
};

camera::use_api!();
haptics::use_api!();
app_manager::use_api!();

const SCAN_INTERVAL_MS: u64 = 300;

const PROGRESS_MULTIPLIER: f32 = 200.0;
const PROGRESS_NUDGE: f32 = 5.0;
const PROGRESS_MAX: f32 = 95.0;

/// Camera Y position for the QR scanner view
const CAMERA_Y_POS: u16 = 137;

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
    frame_raw: Option<Frame>,
    luma_source: Luma8LuminanceSource,
    ur_decoder: UrDecoder,
    request_matching_apps: bool,
    app_manager: AppManagerApi,
}

impl AppState {
    fn scan_qr(&mut self) -> ScanQrProgress {
        let mut bitmap = BinaryBitmap::new(HybridBinarizer::new(self.luma_source.clone()));
        let Ok(has_code) = self.scanner.decode_with_state(&mut bitmap).inspect_err(|e| {
            if !matches!(e, Exceptions::NotFoundException(_)) {
                log::warn!("Extract error: {:?}", e)
            }
        }) else {
            return ScanQrProgress::Unchanged;
        };

        let raw_data = has_code.getRawBytes();
        let data = if raw_data.is_empty() { has_code.getText().as_bytes() } else { raw_data };

        if let Ok(data_str) = std::str::from_utf8(data) {
            match (self.ur_decoder.is_empty(), foundation_ur::UR::parse(data_str.to_lowercase().as_str())) {
                // Single-part URs aren't fountain-encoded; the multi-part decoder rejects them.
                (_, Ok(part)) if part.is_single_part() => {
                    let bw = match part.as_bytewords() {
                        Some(v) => v,
                        None => return ScanQrProgress::Unchanged,
                    };

                    let message = match bytewords::decode(bw, bytewords::Style::Minimal) {
                        Ok(m) => m,
                        Err(e) => {
                            log::warn!("Bytewords error: {:?}", e);
                            return ScanQrProgress::Unchanged;
                        }
                    };

                    let ur_type = String::from(part.as_type());
                    self.ur_decoder.clear();
                    return ScanQrProgress::Complete(ScanQrResult::new_ur2(ur_type, &message));
                }
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

        // No code was found, or code found but wasn't complete and failed UR parse.
        // Numbers fudged to look nicer
        return ScanQrProgress::Progress(fudge_progress_for_ui(
            self.ur_decoder.estimated_percent_complete() as f32,
            self.ur_decoder.is_empty(),
        ));
    }

    fn matching_apps(&self, scan_result: &ScanQrResult) -> Vec<ScanQrMatchingApp> {
        let app_match_rules = self.app_manager.get_qr_match_rules();

        let matching_apps = app_match_rules
            .iter()
            .filter_map(|entry| {
                let rules = match from_slice::<Vec<QrMatchRule>>(&entry.rules_json) {
                    Ok(r) if !r.is_empty() => r,
                    Ok(_) => return None,
                    Err(e) => {
                        log::warn!("Skipping QR match rules entry due to parse error: {:?}", e);
                        return None;
                    }
                };

                let matched_rules: Vec<ScanQrMatchedRule> = rules
                    .into_iter()
                    .filter_map(|rule| {
                        let sub_rule_id = first_matching_sub_rule(scan_result, &rule)?;
                        Some(ScanQrMatchedRule { rule_id: rule.id, priority: rule.priority, sub_rule_id })
                    })
                    .collect();

                if matched_rules.is_empty() {
                    return None;
                }

                Some(ScanQrMatchingApp { id: entry.id, matched_rules })
            })
            .collect::<Vec<ScanQrMatchingApp>>();

        log::info!("Matched {} apps for scanned QR", matching_apps.len());
        matching_apps
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
        let state = AppState {
            status: ScanStatus::Idle,
            scanner,
            ur_decoder: UrDecoder::default(),
            frame_raw: None,
            luma_source,
            request_matching_apps: false,
            app_manager: AppManagerApi::default(),
        };
        StoredValue::new(state)
    };

    cx.gui.show_camera(CAMERA_Y_POS).expect("can't show camera");

    log::info!("Running QR scanner");

    // use StoredValue due to mutability requirement of CameraApi
    let camera_api = StoredValue::new(CameraApi::default());

    ui.global::<Callbacks>().on_enable_camera_clicked({
        let settings = SettingsApi::default();

        move || {
            settings.set_camera_enabled(true);
            true
        }
    });

    // Cancels the navigation modal request and notifies the caller
    ui.global::<Callbacks>().on_button_clicked({
        let ui = ui.clone_strong();
        let gui_api = cx.gui.clone();
        move |action| {
            let mut state = state.borrow_mut();
            state.status = ScanStatus::Idle;
            state.ur_decoder.clear();
            ui.global::<Global>().set_scan_progress(0.0);
            state.request_matching_apps = false;
            gui_api.navigate_finish(ScanQrResult::from(action).serialize()).expect("finish nav");
        }
    });

    ui.global::<Callbacks>().on_unknown_qr_modal_confirmed({
        let ui = ui.clone_strong();
        move || {
            let mut state = state.borrow_mut();
            state.status = ScanStatus::HasPendingNavRequest;
            state.ur_decoder.clear();
            ui.global::<Global>().set_scan_progress(0.0);
        }
    });

    #[cfg(not(feature = "production"))]
    ui.global::<Callbacks>().on_title_tapped({
        let ui = ui.clone_strong();
        let gui_api = cx.gui.clone();
        let _camera_api = camera_api.clone();
        move || {
            log::info!("Opening camera config page");

            // Load current camera params BEFORE hiding the camera
            #[cfg(keyos)]
            {
                let camera = _camera_api.borrow();
                if let Ok(params) = camera.get_params() {
                    // Load auto control states
                    ui.global::<Global>().set_config_aec_enabled((params.auto_controls & 0x01) != 0);
                    ui.global::<Global>().set_config_awb_enabled((params.auto_controls & 0x02) != 0);
                    ui.global::<Global>().set_config_agc_enabled((params.auto_controls & 0x04) != 0);
                    ui.global::<Global>().set_config_agc_ceiling(params.agc_ceiling as i32);
                    // Load SDE controls
                    ui.global::<Global>().set_config_brightness(params.brightness as i32);
                    ui.global::<Global>().set_config_contrast(params.contrast as i32);
                    ui.global::<Global>().set_config_saturation(params.saturation as i32);
                }
            }

            // Hide the camera while config is open
            log::info!("Hiding camera");
            gui_api.hide_camera().ok();

            log::info!("Showing camera config page");
            ui.global::<Global>().set_show_config(true);
        }
    });

    // Camera config callbacks (only available in non-production keyos builds)
    #[cfg(all(keyos, not(feature = "production")))]
    {
        fn param_setter<T: 'static>(
            camera_api: StoredValue<CameraApi>,
            f: impl Fn(&mut camera::messages::CameraParams, T) + 'static,
        ) -> impl Fn(T) {
            move |value| {
                if let Ok(camera) = camera_api.try_borrow() {
                    let mut params = camera.get_params().unwrap_or_default();
                    f(&mut params, value);
                    if let Err(e) = camera.set_params(params) {
                        log::error!("Failed to set params: {:?}", e);
                    }
                }
            }
        }

        ui.global::<Callbacks>().on_config_aec_toggled(param_setter(camera_api, |params, value| {
            if value {
                params.auto_controls |= 0x01;
            } else {
                params.auto_controls &= !0x01;
            }
        }));

        ui.global::<Callbacks>().on_config_agc_toggled(param_setter(camera_api, |params, value| {
            if value {
                params.auto_controls |= 0x04;
            } else {
                params.auto_controls &= !0x04;
            }
        }));

        ui.global::<Callbacks>().on_config_awb_toggled(param_setter(camera_api, |params, value| {
            if value {
                params.auto_controls |= 0x02;
            } else {
                params.auto_controls &= !0x02;
            }
        }));

        ui.global::<Callbacks>().on_config_agc_ceiling_changed(param_setter(camera_api, |params, value| {
            params.agc_ceiling = value as u8;
        }));

        ui.global::<Callbacks>().on_config_brightness_changed(param_setter(camera_api, |params, value| {
            params.brightness = value as u8;
        }));

        ui.global::<Callbacks>().on_config_contrast_changed(param_setter(camera_api, |params, value| {
            params.contrast = value as u8;
        }));

        ui.global::<Callbacks>().on_config_saturation_changed(param_setter(camera_api, |params, value| {
            params.saturation = value as u8;
        }));
    }

    #[cfg(not(feature = "production"))]
    ui.global::<Callbacks>().on_config_reset_clicked({
        let ui = ui.clone_strong();
        let _camera_api = camera_api.clone();
        move || {
            // Reset to defaults
            let defaults = camera::messages::CameraParams::default();
            // Reset UI controls
            ui.global::<Global>().set_config_aec_enabled((defaults.auto_controls & 0x01) != 0);
            ui.global::<Global>().set_config_awb_enabled((defaults.auto_controls & 0x02) != 0);
            ui.global::<Global>().set_config_agc_enabled((defaults.auto_controls & 0x04) != 0);
            ui.global::<Global>().set_config_agc_ceiling(defaults.agc_ceiling as i32);
            ui.global::<Global>().set_config_brightness(defaults.brightness as i32);
            ui.global::<Global>().set_config_contrast(defaults.contrast as i32);
            ui.global::<Global>().set_config_saturation(defaults.saturation as i32);

            #[cfg(keyos)]
            {
                let camera = _camera_api.borrow();
                if let Err(e) = camera.set_params(defaults) {
                    log::error!("Failed to reset params: {:?}", e);
                }
            }
        }
    });

    #[cfg(not(feature = "production"))]
    ui.global::<Callbacks>().on_config_close_clicked({
        let ui = ui.clone_strong();
        let gui_api = cx.gui.clone();
        move || {
            ui.global::<Global>().set_show_config(false);
            // Resume the camera
            gui_api.show_camera(CAMERA_Y_POS).ok();
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

    spawn_local({
        async move {
            let mut frames =
                try_subscribe_scalar::<CameraPermissions, _>(camera::messages::Subscribe).await.unwrap();
            while let Some(frame) = frames.next().await {
                state.borrow_mut().frame_raw = Some(frame);
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

            // Already scanned something, stop scanning until the user press "Rescan"
            if state.status != ScanStatus::HasPendingNavRequest {
                return;
            }

            if let Some(frame_range) = state.frame_raw.as_ref().map(|s| s.content()) {
                log::debug!("Fetching frame for QR detection");

                {
                    let luma8 = state.luma_source.get_matrix_mut().as_mut();
                    #[cfg(keyos)]
                    {
                        rgb565_to_luma8(frame_range.as_slice(), luma8);
                    }

                    #[cfg(not(keyos))]
                    {
                        rgba_to_luma8(frame_range.as_slice(), luma8);
                    }
                }

                match state.scan_qr() {
                    ScanQrProgress::Unchanged => (),
                    ScanQrProgress::Progress(p) => ui.global::<Global>().set_scan_progress(p),
                    ScanQrProgress::Complete(res) => {
                        let res = if state.request_matching_apps {
                            let matching_apps = state.matching_apps(&res);
                            if matching_apps.is_empty() {
                                state.status = ScanStatus::HasQrData;
                                haptics.click();
                                ui.global::<Global>().set_scan_progress(0.0);
                                ui.global::<Global>().set_show_unknown_qr_modal(true);
                                return;
                            }
                            res.with_matching_apps(matching_apps)
                        } else {
                            res
                        };
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
                let mut state = state.borrow_mut();
                state.request_matching_apps = options.request_matching_apps;
                state.status = ScanStatus::HasPendingNavRequest;
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

/// Returns the ID of the first sub-rule that matched, or `None` if the rule did not match.
/// Stops at the first match to avoid redundant regex evaluations.
fn first_matching_sub_rule(scan_result: &ScanQrResult, rule: &QrMatchRule) -> Option<String> {
    rule.sub_rules.iter().find_map(|(id, sub_rule)| {
        if sub_rule_matches(scan_result, sub_rule) {
            Some(id.clone())
        } else {
            None
        }
    })
}

fn sub_rule_matches(scan_result: &ScanQrResult, sub_rule: &QrMatchSubRule) -> bool {
    match sub_rule {
        QrMatchSubRule::QR { min_len, max_len, regex_pattern } => {
            let ScanQrResult::Qr { data, .. } = scan_result else {
                return false;
            };

            let len = data.len();
            if let Some(min_len) = min_len {
                if len < *min_len {
                    return false;
                }
            }
            if let Some(max_len) = max_len {
                if len > *max_len {
                    return false;
                }
            }

            if let Some(pattern) = regex_pattern {
                let Ok(text) = std::str::from_utf8(data.as_slice()) else {
                    return false;
                };
                let Ok(regex) = Regex::new(pattern.as_str()) else {
                    log::error!("Regex failed to parse: {}", pattern);
                    return false;
                };
                if !regex.is_match(text) {
                    return false;
                }
            }

            true
        }
        QrMatchSubRule::UR { ur_type } => {
            let ScanQrResult::Ur2 { ur_type: scanned_type, .. } = scan_result else {
                return false;
            };

            normalize_ur_type(ur_type.as_str()) == normalize_ur_type(scanned_type.as_str())
        }
    }
}

fn normalize_ur_type(ur_type: &str) -> String {
    let ur_type = ur_type.to_ascii_lowercase();
    match ur_type.as_str() {
        "hdkey" | "crypto-hdkey" => "hdkey".to_string(),
        "psbt" | "crypto-psbt" => "psbt".to_string(),
        "x-passport-request" | "crypto-request" => "x-passport-request".to_string(),
        "x-passport-response" | "crypto-response" => "x-passport-response".to_string(),
        _ => ur_type,
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use app_manifest::{QrMatchRule, QrMatchSubRule, QrPriority};
    use slint_keyos_platform::gui_server_api::navigation::qrscanner::ScanQrResult;

    use super::{first_matching_sub_rule, normalize_ur_type, sub_rule_matches};

    fn qr_result(data: &[u8]) -> ScanQrResult { ScanQrResult::new_qr(data) }

    fn ur_result(ur_type: &str) -> ScanQrResult { ScanQrResult::new_ur2(ur_type.to_string(), &[]) }

    fn make_rule(id: &str, sub_rules: BTreeMap<String, QrMatchSubRule>) -> QrMatchRule {
        QrMatchRule {
            id: id.to_string(),
            id_localizations: BTreeMap::new(),
            sub_rules,
            priority: QrPriority::default(),
        }
    }

    // --- normalize_ur_type ---

    #[test]
    fn normalize_ur_type_aliases() {
        assert_eq!(normalize_ur_type("crypto-hdkey"), "hdkey");
        assert_eq!(normalize_ur_type("HDKEY"), "hdkey");
        assert_eq!(normalize_ur_type("crypto-psbt"), "psbt");
        assert_eq!(normalize_ur_type("PSBT"), "psbt");
        assert_eq!(normalize_ur_type("crypto-request"), "x-passport-request");
        assert_eq!(normalize_ur_type("crypto-response"), "x-passport-response");
        assert_eq!(normalize_ur_type("unknown-type"), "unknown-type");
    }

    // --- sub_rule_matches: QR variant ---

    #[test]
    fn sub_rule_qr_matches_no_constraints() {
        let sub_rule = QrMatchSubRule::QR { min_len: None, max_len: None, regex_pattern: None };
        assert!(sub_rule_matches(&qr_result(b"anything"), &sub_rule));
    }

    #[test]
    fn sub_rule_qr_rejects_ur_result() {
        let sub_rule = QrMatchSubRule::QR { min_len: None, max_len: None, regex_pattern: None };
        assert!(!sub_rule_matches(&ur_result("psbt"), &sub_rule));
    }

    #[test]
    fn sub_rule_qr_min_len_enforced() {
        let sub_rule = QrMatchSubRule::QR { min_len: Some(10), max_len: None, regex_pattern: None };
        assert!(!sub_rule_matches(&qr_result(b"short"), &sub_rule));
        assert!(sub_rule_matches(&qr_result(b"exactly_ten"), &sub_rule));
    }

    #[test]
    fn sub_rule_qr_max_len_enforced() {
        let sub_rule = QrMatchSubRule::QR { min_len: None, max_len: Some(5), regex_pattern: None };
        assert!(sub_rule_matches(&qr_result(b"hi"), &sub_rule));
        assert!(!sub_rule_matches(&qr_result(b"too_long_string"), &sub_rule));
    }

    #[test]
    fn sub_rule_qr_regex_match() {
        let sub_rule =
            QrMatchSubRule::QR { min_len: None, max_len: None, regex_pattern: Some("^UR:".to_string()) };
        assert!(sub_rule_matches(&qr_result(b"UR:psbt/..."), &sub_rule));
        assert!(!sub_rule_matches(&qr_result(b"not a ur"), &sub_rule));
    }

    #[test]
    fn sub_rule_qr_rejects_non_utf8_when_regex_set() {
        let sub_rule =
            QrMatchSubRule::QR { min_len: None, max_len: None, regex_pattern: Some(".*".to_string()) };
        assert!(!sub_rule_matches(&qr_result(&[0xFF, 0xFE]), &sub_rule));
    }

    // --- sub_rule_matches: UR variant ---

    #[test]
    fn sub_rule_ur_matches_normalized_type() {
        let sub_rule = QrMatchSubRule::UR { ur_type: "psbt".to_string() };
        assert!(sub_rule_matches(&ur_result("crypto-psbt"), &sub_rule));
        assert!(sub_rule_matches(&ur_result("PSBT"), &sub_rule));
        assert!(!sub_rule_matches(&ur_result("hdkey"), &sub_rule));
    }

    #[test]
    fn sub_rule_ur_rejects_qr_result() {
        let sub_rule = QrMatchSubRule::UR { ur_type: "psbt".to_string() };
        assert!(!sub_rule_matches(&qr_result(b"UR:PSBT/..."), &sub_rule));
    }

    // --- first_matching_sub_rule ---

    #[test]
    fn first_matching_sub_rule_returns_id_on_match() {
        let mut sub_rules = BTreeMap::new();
        sub_rules.insert(
            "by-regex".to_string(),
            QrMatchSubRule::QR { min_len: None, max_len: None, regex_pattern: Some("^BC1".to_string()) },
        );
        let rule = make_rule("bitcoin-address", sub_rules);
        let matched = first_matching_sub_rule(&qr_result(b"BC1xxxx"), &rule);
        assert_eq!(matched.as_deref(), Some("by-regex"));
    }

    #[test]
    fn first_matching_sub_rule_returns_none_when_nothing_matches() {
        let mut sub_rules = BTreeMap::new();
        sub_rules.insert("ur-only".to_string(), QrMatchSubRule::UR { ur_type: "psbt".to_string() });
        let rule = make_rule("psbt-rule", sub_rules);
        assert!(first_matching_sub_rule(&qr_result(b"some bytes"), &rule).is_none());
    }

    #[test]
    fn first_matching_sub_rule_returns_one_id_even_if_multiple_would_match() {
        // BTreeMap iteration order is alphabetical — "a-match" comes before "z-match".
        let mut sub_rules = BTreeMap::new();
        sub_rules.insert(
            "a-match".to_string(),
            QrMatchSubRule::QR { min_len: None, max_len: None, regex_pattern: None },
        );
        sub_rules.insert(
            "z-match".to_string(),
            QrMatchSubRule::QR { min_len: None, max_len: None, regex_pattern: None },
        );
        let rule = make_rule("multi", sub_rules);
        let result = first_matching_sub_rule(&qr_result(b"data"), &rule);
        assert_eq!(result.as_deref(), Some("a-match"));
    }
}
