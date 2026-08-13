// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{sync::Arc, time::Duration};

use jiff::fmt::strtime;
use security::{messages::RawPin, MAX_LOGIN_ATTEMPTS};
use slint_keyos_platform::{
    app,
    gui_server_api::{
        msg::UpdateKioskPolicy,
        navigation::lockscreen::{VerifyPinOptions, VerifyPinResult},
        InputMessage,
    },
    settings::{self, global},
    slint::{ModelRc, Timer, TimerMode},
    spawn_local, subscribe_archive, subscribe_scalar, StoredValue,
};

use crate::settings_permissions::SettingsPermissions;

security::use_api!();
haptics::use_api!();

pub struct AppState {
    pub timezone: global::TimeZone,
    pub use_standard_time_format: global::UseStandardTimeFormat,
    pub ui: slint::Weak<AppWindow>,

    pub gui: Arc<GuiApi>,
    pub security: Security,

    pub swipe_timeout_timer: Timer,
}

impl AppState {
    pub fn update_time(&self) {
        let ui = self.ui.unwrap();
        let ui_state = ui.global::<State>();

        let zoned = self.timezone.now();
        let time = if self.use_standard_time_format.0 { "%H:%M" } else { "%-I:%M" };
        ui_state.set_time(strtime::format(time, &zoned).unwrap().into());
        ui_state.set_date(strtime::format("%B %e, %Y", &zoned).unwrap().into());
    }
}

app!("Lock Screen", role = ClaimLockScreenRole);

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    cx.gui
        .update_kiosk_policy(UpdateKioskPolicy::default().set_home_button(false).set_control_center(false))
        .ok();

    // Fetch the visually important settings
    let settings = SettingsApi::default();
    let timezone = settings.get_time_zone();
    let use_standard_time_format = settings.get_use_standard_time_format();
    ui.global::<State>().set_show_security_words_enabled(settings.get_show_security_words().0);

    let swipe_timeout_timer = Timer::default();
    swipe_timeout_timer.start(TimerMode::SingleShot, Duration::from_millis(300), {
        let ui = ui.clone_strong();
        move || {
            ui.invoke_swipe_cancel();
        }
    });

    let state = StoredValue::new(AppState {
        timezone,
        use_standard_time_format,
        ui: ui.as_weak(),
        gui: cx.gui.clone(),
        swipe_timeout_timer,
        security: Security::default(),
    });

    set_input_handler(&cx, state);

    spawn_local(async move {
        let mut sub = subscribe_archive::<SettingsPermissions, _>(settings::messages::SubscribeTimeZone);
        while let Some(timezone) = sub.next().await {
            state.borrow_mut().timezone = timezone;
        }
    })
    .detach();

    spawn_local(async move {
        let mut sub =
            subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeUseStandardTimeFormat);
        while let Some(use_standard_time_format) = sub.next().await {
            state.borrow_mut().use_standard_time_format = use_standard_time_format;
        }
    })
    .detach();

    spawn_local(async move {
        let mut sub =
            subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeShowSecurityWords);
        while let Some(show_security_words) = sub.next().await {
            let ui = state.borrow().ui.unwrap();
            let ui_state = ui.global::<State>();
            ui_state.set_show_security_words_enabled(show_security_words.0);
        }
    })
    .detach();

    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(1000), move || {
        let s = state.borrow();
        s.update_time();
    });

    state.borrow().update_time();
    init_state(state);

    ui.run().expect("Failed to run UI");
}

fn set_input_handler(cx: &AppContext, state: StoredValue<AppState>) {
    let gui = cx.gui.clone();
    cx.set_input_handler(move |input| {
        let state = state.borrow();
        let ui = state.ui.unwrap();
        let ui_state = ui.global::<State>();
        match input.msg {
            InputMessage::NavigationFocused => {
                let Ok(Some(nav_bytes)) = state.gui.navigate_pending() else {
                    log::error!("Navigation focused but no pending nav request");
                    return;
                };

                let Some(options) = VerifyPinOptions::from_slice(&nav_bytes) else {
                    log::error!("Failed to parse VerifyPinOptions from a nav request");
                    return;
                };
                ui_state.set_is_pin_entry(state.security.get_pin_entry_mode() == security::PinEntryMode::Pin);
                ui_state.set_title(options.title.unwrap_or_default().into());
                ui_state.set_nav_request(true);
                ui_state.set_want_security_words(options.want_security_words);
                log::info!("Navigated");
            }
            InputMessage::NavigationCancelled => {
                // how is this possible?
                ui_state.set_title("".into());
                ui_state.set_nav_request(false);
                reset_input_state(&ui_state, ui_state.get_remaining_attempts() as _);
                log::info!("Navigation cancelled");
            }
            InputMessage::Hidden => {
                reset_input_state(&ui_state, ui_state.get_remaining_attempts() as _);
                ui_state.set_show_login(false);
                gui.hide_keyboard().ok();
            }
            // The mode can be changed from settings while the lock screen is off screen.
            InputMessage::Visible => {
                ui_state.set_is_pin_entry(state.security.get_pin_entry_mode() == security::PinEntryMode::Pin);
            }
            _ => {}
        };
    });
}

fn init_state(state: StoredValue<AppState>) {
    let ui = state.borrow().ui.unwrap();
    let ui_state = ui.global::<State>();

    // Configure UI with the max attempts from Rust
    ui_state.set_max_login_attempts(MAX_LOGIN_ATTEMPTS as _);

    ui_state.set_is_pin_entry(state.borrow().security.get_pin_entry_mode() == security::PinEntryMode::Pin);

    ui_state.on_show_control_center(move || {
        state.borrow().gui.update_kiosk_policy(UpdateKioskPolicy::default().set_control_center(true)).ok();
    });

    ui_state.on_hide_control_center(move || {
        state.borrow().gui.update_kiosk_policy(UpdateKioskPolicy::default().set_control_center(false)).ok();
    });

    ui_state.on_ease_swipe_offset(move |current_position, pressed_position, height| {
        let normalized_delta = (current_position - pressed_position) / (10.0 * height);
        let eased_delta = if normalized_delta < 0.0 {
            -normalized_delta.abs().powf(0.5) * height
        } else {
            normalized_delta.powf(1.5) * height
        };
        (-height).max(eased_delta.min(0.0)) / 2.0
    });

    if let Some(attempts) =
        state.borrow().security.attempts_remaining().ok().and_then(|attempts| attempts.try_into().ok())
    {
        ui_state.set_remaining_attempts(attempts);
    }

    ui_state.on_verify_pin(move || {
        spawn_local(async move {
            let state = state.borrow();
            let ui = state.ui.unwrap();
            let ui_state = ui.global::<State>();
            let pin = ui_state.get_input().to_string();
            // compute prefix before moving raw_pin into Login
            let pin_prefix: String = pin.chars().take(4).collect();
            ui_state.set_is_check_ongoing(true);
            match slint_keyos_platform::async_archive::<security_permissions::SecurityPermissions, _>(
                security::messages::Login { pin: RawPin(pin) },
            )
            .await
            {
                Ok(_) => {
                    if ui_state.get_nav_request() {
                        // Return unified result with optional words
                        let words_opt = if ui_state.get_want_security_words() {
                            match state.security.security_words(&pin_prefix) {
                                Ok([w0, w1]) => Some([w0.to_string(), w1.to_string()]),
                                Err(e) => {
                                    log::warn!("Failed to compute security words: {}", e);
                                    None
                                }
                            }
                        } else {
                            None
                        };
                        let response = VerifyPinResult { success: true, security_words: words_opt };
                        if let Err(e) = state.gui.navigate_finish(response.serialize()) {
                            log::error!("Failed to notify navigation success: {}", e);
                        }
                    } else {
                        if let Err(e) = state.gui.notify_login_success() {
                            log::error!("Failed to notify login success: {}", e);
                        }
                    }
                    reset_input_state(&ui_state, MAX_LOGIN_ATTEMPTS);
                    reset_request_state(&ui_state);
                }
                Err(e) => {
                    reset_input_state(&ui_state, e.attempts_left);
                    HapticsApi::default().triple_click();
                }
            }
        })
        .detach();
    });

    ui_state.on_cancelled(move || {
        let state = state.borrow();
        let ui = state.ui.unwrap();
        let ui_state = ui.global::<State>();
        if ui_state.get_nav_request() {
            if let Err(e) = state.gui.navigate_cancel() {
                log::error!("Failed to notify navigation cancel: {}", e);
            }
            reset_request_state(&ui_state);
        }

        reset_input_state(&ui_state, ui_state.get_remaining_attempts() as _);
    });

    ui_state.on_get_security_words(move |pin| {
        let state = state.borrow();
        // Always compute from the first 4 digits only
        let pin_prefix: String = pin.chars().take(4).collect();
        match state.security.security_words(&pin_prefix) {
            Ok(words) => {
                let words = words.map(|word| word.to_string()).map(slint::SharedString::from);
                ModelRc::from(words)
            }
            Err(e) => {
                log::error!("Failed to get security words: {}", e);
                Default::default()
            }
        }
    });

    ui_state.on_swipe_timeout_restart(move || {
        let state = state.borrow();
        state.swipe_timeout_timer.restart();
    })
}

fn reset_input_state(ui_state: &State, remaining_attempts: u32) {
    ui_state.set_remaining_attempts(remaining_attempts as _);
    ui_state.set_is_check_ongoing(false);
    ui_state.set_input("".into());
    ui_state.set_locked_prefix("".into());
    ui_state.set_security_words(Default::default());
}

fn reset_request_state(ui_state: &State) { ui_state.set_nav_request(false); }
