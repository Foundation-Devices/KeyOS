// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use fs::{FileSystemEventType, Location};
use gui_server_api::{
    consts::CLOSE_TIMEOUT_EXIT_CODE,
    msg::UpdateKioskPolicy,
    navigation::alerts::{AlertResult, InvokeAlert},
    InputMessage,
};
use haptics::HapticPattern;
use slint_keyos_platform::settings::global::OnboardingStatus;
use slint_keyos_platform::slint::Timer;
use slint_keyos_platform::{
    app,
    gui_server_api::navigation::bitcoin::OpenBitcoinOptions,
    navigation::open_bitcoin_app,
    skia::PricePoint,
    slint::{ComponentHandle, VecModel},
    spawn_local, subscribe_archive, subscribe_scalar, StoredValue,
};
use update::messages::ProgressUpdate;
use xous::{app_id_to_pid, current_pid, AppId, PID};

use crate::gui_permissions::GuiPermissions;

const LAUNCH_ANIMATION_TIMEOUT: Duration = Duration::from_millis(1000);
const STALE_TIME: Duration = Duration::from_secs(60 * 2); // 2 minutes
const GRAPH_POINTS_PER_MINUTE: u64 = 1;
const EXPIRE_TIME_MINUTES: u64 = 60;
const EXPIRE_TIME: Duration = Duration::from_secs(EXPIRE_TIME_MINUTES * 60); // 1 hour
const GRAPH_WINDOW_MINUTES: u64 = 100;
const MINIMUM_GRAPH_POINTS: usize =
    ((GRAPH_WINDOW_MINUTES - EXPIRE_TIME_MINUTES + 1) * GRAPH_POINTS_PER_MINUTE) as usize;
const REDRAW_TIME_SECS: u64 = 60; // 1 minute
const GRAPH_WIDTH: u32 = 402;
const GRAPH_HEIGHT: u32 = 76;
const GRAPH_MAX_HEIGHT: u32 = GRAPH_HEIGHT - 10;
const GRAPH_WINDOW_SECS: u64 = 60 * GRAPH_WINDOW_MINUTES;
static SETTINGS_APP_ID_STR: &str = "0xc192b79230473875f159d4423d74d00f";
const FLATLINE_POINTS: [PricePoint; 2] = [
    PricePoint { price: 500, timestamp: 0, is_pad: true },
    PricePoint { price: 500, timestamp: 1, is_pad: true },
];

app_manager::use_api!();
haptics::use_api!();
quantum_link::use_api!();
update::use_api!();

pub struct AppState {
    pub gui: Arc<GuiApi>,
    pub ui: slint::Weak<AppWindow>,
    pub ql_status: QlStatus,
    pub haptics_api: HapticsApi,
    pub loading_state_timer: Timer,
    pub is_fs_unavailable: bool,
    pub show_airlock_error: bool,
    pub is_visible: bool,
    pub prices: Vec<PricePoint>,
    pub prices_dirty: bool,
    pub last_palette: Palettes,
    pub last_draw_time_secs: u64,
}

impl AppState {
    pub fn new(gui: Arc<GuiApi>, ui: slint::Weak<AppWindow>) -> Self {
        let last_palette = ui.unwrap().global::<CurrentTheme>().get_palette();
        Self {
            gui,
            ui,
            ql_status: QlStatus::new(slint_keyos_platform::worker().clone()),
            haptics_api: HapticsApi::default(),
            loading_state_timer: Timer::default(),
            is_fs_unavailable: false,
            show_airlock_error: false,
            is_visible: false,
            prices: Vec::new(),
            prices_dirty: false,
            last_palette,
            last_draw_time_secs: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
        }
    }

    pub fn ui(&self) -> AppWindow { self.ui.unwrap() }
}

app!("Launcher", kind = Launcher);

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    log::trace!("Running the launcher app");

    let state = StoredValue::new(AppState::new(cx.gui.clone(), ui.as_weak()));

    #[cfg(not(feature = "production"))]
    {
        let hidden_apps = vec![
            HiddenApp { label: "System Actions".into(), app_id: "0x6775692d6170702d73797374656d2d61".into() },
            HiddenApp { label: "Recovery".into(), app_id: "0x6775692d6170702d7265636f76657279".into() },
            HiddenApp { label: "Onboarding".into(), app_id: "0xdac5321775d449c11bc9c90f38067f8f".into() },
            HiddenApp {
                label: "File Picker Demo".into(),
                app_id: "0xd7578c121c1d86fd541a086662de6dd6".into(),
            },
            HiddenApp { label: "Image Viewer".into(), app_id: "0x0944ad38232eab9a060661c2e8dc7eb5".into() },
            HiddenApp { label: "Reg. Testing".into(), app_id: "0xc677731d8ee7380a38faa7cd97cbd3a5".into() },
            HiddenApp { label: "Playground".into(), app_id: "0x7c9f81f9bcee31425062fb0d8fbf3001".into() },
            HiddenApp { label: "Update".into(), app_id: "0x6b713041faef901f23743263a45dcb83".into() },
        ];

        ui.global::<State>().set_hidden_apps(slint::ModelRc::new(VecModel::from(hidden_apps)));
    }

    cx.set_input_handler({
        let mut needs_first_visible = true;
        move |app_input| {
            if let InputMessage::Hidden = app_input.msg {
                let mut state_borrow = state.borrow_mut();
                state_borrow.is_visible = false;
                // Clear loading state when the launcher is hidden
                let ui = state_borrow.ui();
                clear_launching_state(&ui);
                state_borrow.loading_state_timer.stop();
            }

            if let InputMessage::Visible = app_input.msg {
                let (is_fs_unavailable, show_airlock_error) = {
                    let mut state_borrow = state.borrow_mut();
                    state_borrow.is_visible = true;
                    let show_airlock_error = state_borrow.show_airlock_error;
                    state_borrow.show_airlock_error = false;
                    (state_borrow.is_fs_unavailable, show_airlock_error)
                };

                if is_fs_unavailable {
                    invoke_fs_format_alert(state);
                }
                if show_airlock_error {
                    invoke_airlock_format_alert(state);
                }

                if needs_first_visible {
                    needs_first_visible = false;
                    maybe_resume_update(state);
                }
            }
        }
    });

    spawn_local({
        async move {
            let mut sub = subscribe_archive::<app_manager_permissions::AppManagerPermissions, _>(
                app_manager::messages::SubscribeAppEvents,
            );
            while let Some(event) = sub.next().await {
                handle_app_event(state, event);
            }
        }
    })
    .detach();

    ui.global::<LauncherCallbacks>().on_app_id_to_title(move |app_id_str| {
        let Ok(app_id) = app_manager::decode_app_id_str(app_id_str.as_ref()) else {
            return "<unknown>".into();
        };

        log::info!("Requesting app name for app_id={app_id_str}");
        let locale = "en"; // TODO: i18n, get the locale from the settings
        if let Some(name) = AppManagerApi::default().app_name_by_app_id(&app_id, locale) {
            return name.into();
        }

        "<unknown>".into()
    });

    ui.global::<LauncherCallbacks>().on_scan_clicked({
        move |_pressed| {
            state.borrow().haptics_api.click();

            if let Err(e) = open_bitcoin_app::<GuiPermissions>(OpenBitcoinOptions::new()) {
                log::error!("Failed to open bitcoin app for scan: {e:?}");
            }
        }
    });

    ui.global::<LauncherCallbacks>().on_app_clicked({
        move |x: f32, y: f32, id| {
            let x = x as usize;
            let y = y as usize;
            let (gui, ui) = {
                let state_borrow = state.borrow();
                state_borrow.haptics_api.click();
                (state_borrow.gui.clone(), state_borrow.ui())
            };

            let Ok(app_id) = app_manager::decode_app_id_str(id.as_str()) else {
                log::error!("Invalid AppId hex: {id:}");
                error_message(
                    &ui,
                    tr::lookup_id(TrId::LauncherCrashHeader),
                    tr::lookup_id(TrId::LauncherCrashContent),
                    Some(format!("Invalid AppId hex: {id:}")),
                    None,
                    None,
                );
                return;
            };
            log::info!("App clicked with id={id:} at ({x}, {y})");

            // The app is already running, just switch to it
            if let Ok(Some(pid)) = app_id_to_pid(&app_id) {
                clear_launching_state(&ui);
                gui.switch_to(pid, x, y).expect("switch_to failed");
                return;
            }

            // Otherwise request the app-manager to launch the app
            ui.global::<State>().set_loading_app_id(id.clone());
            if let Err(e) = AppManagerApi::default().launch_app(&app_id) {
                error_message(
                    &ui,
                    tr::lookup_id(TrId::LauncherCrashHeader),
                    tr::lookup_id(TrId::LauncherCrashContent),
                    Some(format!("{e:?}")),
                    None,
                    None,
                );
            }
        }
    });

    spawn_local({
        async move {
            let mut sub = subscribe_scalar::<fs_permissions::FileSystemPermissions, _>(
                fs::messages::SubscribeFilesystemEvent(Location::User),
            );
            while let Some(event) = sub.next().await {
                match event.event_type {
                    FileSystemEventType::Mounted => {
                        state.borrow_mut().is_fs_unavailable = false;
                    }
                    FileSystemEventType::Unmounted => {}
                    FileSystemEventType::Error => {
                        let is_visible = {
                            let mut state_borrow = state.borrow_mut();
                            state_borrow.is_fs_unavailable = true;
                            state_borrow.is_visible
                        };
                        if is_visible {
                            invoke_fs_format_alert(state);
                        }
                    }
                }
            }
        }
    })
    .detach();

    spawn_local({
        async move {
            let mut sub = subscribe_scalar::<fs_permissions::FileSystemPermissions, _>(
                fs::messages::SubscribeFilesystemEvent(Location::Airlock),
            );
            while let Some(event) = sub.next().await {
                if let FileSystemEventType::Error = event.event_type {
                    let should_alert = {
                        let mut state_borrow = state.borrow_mut();
                        if state_borrow.is_visible {
                            true
                        } else {
                            state_borrow.show_airlock_error = true;
                            false
                        }
                    };
                    if should_alert {
                        invoke_airlock_format_alert(state);
                    }
                }
            }
        }
    })
    .detach();

    setup_ql_status(state);
    setup_update_progress(state);
    setup_bitcoin_status(state);
    refresh_bitcoin_status(state);

    let price_timer = Timer::default();
    price_timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), {
        move || {
            let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let (prices_dirty, current_palette, last_palette, last_draw_time_secs) = {
                let app_state_borrow = state.borrow();
                let ui = app_state_borrow.ui();
                (
                    app_state_borrow.prices_dirty,
                    ui.global::<CurrentTheme>().get_palette(),
                    app_state_borrow.last_palette,
                    app_state_borrow.last_draw_time_secs,
                )
            };
            let theme_changed = current_palette != last_palette;
            let timed_out = now_secs.saturating_sub(last_draw_time_secs) >= REDRAW_TIME_SECS;

            if prices_dirty || theme_changed || timed_out {
                refresh_bitcoin_status(state);
            }
        }
    });

    ui.run().expect("Platform error");
}

fn setup_ql_status(state: StoredValue<AppState>) {
    spawn_local({
        let mut ql = state.borrow().ql_status.clone();
        async move {
            while let Some(status) = ql.next().await {
                let ui = state.borrow().ui();
                let global = ui.global::<State>();

                global.set_is_envoy_connected(status.bt_connected && status.ql_paired);
            }
        }
    })
    .detach();
}

fn setup_bitcoin_status(state: StoredValue<AppState>) {
    spawn_local({
        async move {
            let mut exchange_rate = subscribe_archive::<quantum_link_permissions::QuantumLinkPermissions, _>(
                quantum_link::messages::SubscribeExchangeRate,
            );
            while let Some(exchange_rate) = exchange_rate.next().await {
                log::info!("Received quantum link exchange rate: {exchange_rate:?}");
                let mut app_state = state.borrow_mut();
                app_state.prices.push(PricePoint {
                    price: exchange_rate.rate as u32,
                    timestamp: exchange_rate.timestamp,
                    is_pad: false,
                });
                app_state.prices_dirty = true;
            }
        }
    })
    .detach();

    spawn_local({
        async move {
            let mut exchange_rate_history =
                subscribe_archive::<quantum_link_permissions::QuantumLinkPermissions, _>(
                    quantum_link::messages::SubscribeExchangeRateHistory,
                );

            while let Some(history) = exchange_rate_history.next().await {
                log::info!("Received quantum link exchange rate history");
                let mut app_state = state.borrow_mut();
                app_state.prices = history
                    .history
                    .into_iter()
                    .map(|p| PricePoint { price: p.rate as u32, timestamp: p.timestamp, is_pad: false })
                    .collect();
                app_state.prices_dirty = true;
            }
        }
    })
    .detach();
}

fn refresh_bitcoin_status(app_state: StoredValue<AppState>) {
    let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let window_start = now_secs.saturating_sub(GRAPH_WINDOW_SECS);

    let mut app_state_borrow = app_state.borrow_mut();
    app_state_borrow.prices.retain(|point| point.timestamp >= window_start);
    let ui = app_state_borrow.ui();
    let mut prices = app_state_borrow.prices.clone();

    let current_palette = ui.global::<CurrentTheme>().get_palette();
    let is_dark_mode = current_palette == Palettes::Dark;

    let ui_state = ui.global::<State>();
    let last_price_point = prices.last();

    match last_price_point {
        Some(point) => {
            let last_price_time = UNIX_EPOCH + Duration::from_secs(point.timestamp);
            let time_since_price = SystemTime::now().duration_since(last_price_time).unwrap_or_else(|e| {
                log::error!("Finding duration since time in the future: {:?}", e);
                Duration::ZERO
            });

            log::info!("checking price from {:?} ago: {}", time_since_price, point.price);

            let is_expired = time_since_price >= EXPIRE_TIME || prices.len() < MINIMUM_GRAPH_POINTS;
            let is_stale = time_since_price >= STALE_TIME || is_expired;

            ui_state.set_is_price_stale(is_stale);
            ui_state.set_is_price_expired(is_expired);
            ui_state.set_last_update_time(tr::format_duration(time_since_price).into());
            ui_state.set_bitcoin_price(fmt_price(point.price as f32).into());

            let first_price = prices.first().map(|first_point| first_point.price).unwrap_or(point.price);
            let current_price = point.price;
            let change_percentage = if first_price == 0 {
                0.0
            } else {
                ((current_price as f32 - first_price as f32) / first_price as f32) * 100.0
            };

            let sign = if change_percentage.is_sign_positive() { '+' } else { '-' };
            let change_percentage_formatted = format!("{sign}{:.2}%", change_percentage.abs());

            ui_state.set_bitcoin_price_change_percent(change_percentage_formatted.into());

            let data_vec: Vec<PricePoint> = if is_expired {
                FLATLINE_POINTS.to_vec()
            } else {
                if let (Some(first), Some(last)) = (prices.first().cloned(), prices.last().cloned()) {
                    if first.timestamp > window_start {
                        prices.insert(
                            0,
                            PricePoint {
                                price: first.price,
                                timestamp: window_start,
                                // Show left pad in grey if it accounts for more than 1 minute
                                // of data, otherwise show copper to match graph
                                is_pad: first.timestamp > (window_start + 60),
                            },
                        );
                    }
                    if last.timestamp < now_secs {
                        prices.push(PricePoint { price: last.price, timestamp: now_secs, is_pad: true });
                    }
                }
                prices
            };
            let graph_image = slint_keyos_platform::skia::draw_graph(
                &data_vec,
                GRAPH_WIDTH,
                GRAPH_HEIGHT,
                GRAPH_MAX_HEIGHT,
                is_dark_mode,
            );
            ui_state.set_bitcoin_graph_image(graph_image);
        }
        None => {
            ui_state.set_is_price_stale(true);
            ui_state.set_is_price_expired(true);

            let data_vec = FLATLINE_POINTS.to_vec();
            let graph_image = slint_keyos_platform::skia::draw_graph(
                &data_vec,
                GRAPH_WIDTH,
                GRAPH_HEIGHT,
                GRAPH_MAX_HEIGHT,
                is_dark_mode,
            );
            ui_state.set_bitcoin_graph_image(graph_image);

            ui_state.set_bitcoin_price(Default::default());
            ui_state.set_bitcoin_price_change_percent(Default::default());
        }
    }

    app_state_borrow.last_draw_time_secs = now_secs;
    app_state_borrow.last_palette = current_palette;
    app_state_borrow.prices_dirty = false;
}

fn invoke_fs_format_alert(state: StoredValue<AppState>) {
    // This alert is only going to return if the user agreed to format the volume
    // TODO: i18n
    let gui = state.borrow().gui.clone();
    let result = gui.invoke_alert(InvokeAlert::new_warning("Formatting Required",
        "The encrypted storage is unformatted. Perhaps it was previously encrypted using a different Master Key.",
        "Formatting the encrypted storage using the current Master Key will permanently erase all data previously stored, including the airlock.",
        "Format Encrypted Storage",
        tr::lookup_id(TrId::CommonButtonShutDown)
    ))
    .unwrap_or(AlertResult::Canceled);

    match result {
        AlertResult::Button1Pressed => {
            log::info!("User agreed to format the encrypted volume");
            FileSystem::default().format_encrypted_volume();
            // Assume that this corruption happened after onboarding
            SettingsApi::default().set_onboarding_status(OnboardingStatus::Complete);
            state.borrow_mut().is_fs_unavailable = false;
        }
        AlertResult::Button2Pressed => {
            log::info!("User chose to shut down the device");
            gui.shutdown().ok();
        }
        AlertResult::Button3Pressed | AlertResult::Canceled => {}
    }
}

fn invoke_airlock_format_alert(state: StoredValue<AppState>) {
    // This alert is only going to return if the user agreed to format the volume
    // TODO: i18n
    let gui = state.borrow().gui.clone();
    let result = gui
        .invoke_alert(InvokeAlert::new_warning(
            "Airlock error",
            "The filesystem on Airlock is corrupted and needs to be formatted.",
            "It may be recovered over USB if not formatted.",
            "Format Airlock",
            "Cancel",
        ))
        .unwrap_or(AlertResult::Canceled);

    if let AlertResult::Button1Pressed = result {
        log::info!("User agreed to format Airlock");
        let mut fs = FileSystem::default();
        if fs.format_airlock().is_ok() {
            fs.mount_airlock().ok();
        }
    }
}

fn clear_launching_state(ui: &AppWindow) { ui.global::<State>().set_loading_app_id("".into()); }

fn error_message(
    ui: &AppWindow,
    title: &str,
    message: &str,
    long_message: Option<String>,
    pid: Option<PID>,
    app_id: Option<AppId>,
) {
    clear_launching_state(ui);

    ui.global::<Navigate>().invoke_error(
        ErrorParams {
            crashed_app_id: app_id.map(|a| hex::encode(a.0).into()).unwrap_or("".into()),
            crashed_pid: pid.map(|p| p.get() as i32).unwrap_or(0),
            error_message: message.into(),
            error_title: title.into(),
            long_error_message: long_message.unwrap_or(String::from("")).into(),
        },
        NavigateOptions { replace: false, animate: Animate::None },
    );
}

fn handle_app_event(state: StoredValue<AppState>, event: app_manager::AppEvent) {
    let (ui, gui_api) = {
        let state_borrow = state.borrow();
        (state_borrow.ui(), state_borrow.gui.clone())
    };
    match event {
        app_manager::AppEvent::AppLaunched { app_id, pid, launched_by } => {
            // Ignore launch events that are not initiated by the launcher itself
            if launched_by != current_pid().expect("current pid") {
                return;
            }

            let app_id = AppId::from(app_id);
            log::info!("App launched: app_id={}, pid={pid}", hex::encode(app_id.0));

            // Start a timeout to clear the loading state as a fallback
            // In normal cases, the Hidden event will clear it first
            state.borrow().loading_state_timer.start(
                slint::TimerMode::SingleShot,
                LAUNCH_ANIMATION_TIMEOUT,
                {
                    let ui = ui.clone_strong();
                    move || {
                        log::debug!("Loading state timeout triggered, clearing state");
                        clear_launching_state(&ui);
                    }
                },
            );

            // Switch to an app when it's ready
            gui_api.switch_to(pid, 0, 0).expect("switch_to failed");
        }

        app_manager::AppEvent::LaunchError { app_id, error: e } => {
            log::error!("App launch error: {e:?}");
            clear_update_settings_crash(state, app_id);
            error_message(
                &ui,
                tr::lookup_id(TrId::LauncherCrashHeader),
                tr::lookup_id(TrId::LauncherCrashContent),
                Some(format!("{e:?}")),
                None,
                None,
            );
        }

        // TODO (SFT-5433): push the crash message into a log
        app_manager::AppEvent::AppCrashed { app_id, pid, exit_code, panic_message, .. } => {
            clear_update_settings_crash(state, app_id);

            let app_name = AppManagerApi::default()
                    .app_name_by_app_id(&app_id.into(), "en") // TODO: i18n, get the locale from the settings
                    .unwrap_or_else(|| "Unknown App".to_string());

            if exit_code == 0 {
                log::info!("App `{app_name}` (PID={pid}) exited normally");
            } else if exit_code == CLOSE_TIMEOUT_EXIT_CODE {
                log::warn!("{app_name} (PID={pid}) was terminated due to close timeout");
            } else {
                log::warn!("{app_name} (PID={pid}) crashed with exit code {exit_code}");

                // Vibrate to alert the user that the app crashed
                HapticsApi::default().vibrate(HapticPattern::Alert750ms);

                // Update the launcher UI to show the crash message
                error_message(
                    &ui,
                    tr::lookup_id(TrId::AppCrashHeader),
                    &i18n::replace_placeholders(
                        tr::lookup_id(TrId::AppCrashContent),
                        &[app_name.as_str(), &exit_code.to_string()],
                    ),
                    panic_message,
                    Some(pid),
                    Some(app_id.into()),
                )
            }
        }
    }
}

fn setup_update_progress(state: StoredValue<AppState>) {
    spawn_local(async move {
        let mut update_events = subscribe_archive::<update_permissions::UpdatePermissions, _>(
            update::messages::SubscribeUpdateProgress,
        );

        while let Some(event) = update_events.next().await {
            match event {
                ProgressUpdate::InstallError(_) | ProgressUpdate::DownloadError(_) => {
                    clear_update_resume_state(state);
                }
                ProgressUpdate::Rebooting | ProgressUpdate::Done => {}
                ProgressUpdate::DownloadProgress(_)
                | ProgressUpdate::DownloadComplete
                | ProgressUpdate::InstallProgress(_) => {}
            }
        }
    })
    .detach();
}

fn show_update_applied_alert(state: StoredValue<AppState>) {
    let gui = state.borrow().gui.clone();
    gui.invoke_alert(InvokeAlert {
        app_title: None,
        title: tr::lookup_id(TrId::MainUpdateAppliedHeader).to_string(),
        icon: "check-circle".to_string(),
        line1: tr::lookup_id(TrId::MainUpdateAppliedDescription).to_string(),
        line2: None,
        button1_title: tr::lookup_id(TrId::CommonButtonDone).to_string(),
        button2_title: None,
        button3_title: None,
    })
    .ok();
}

fn maybe_resume_update(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let update = UpdateApi::default();

    if update.check_update_applied() {
        show_update_applied_alert(state);
        update.clear_update_applied();
        return;
    }

    if !update.update_status().needs_continue {
        return;
    }

    if let Err(e) = try_resume_update(state) {
        log::error!("failed to resume interrupted update: {e:?}");
        clear_update_resume_state(state);
        error_message(
            &ui,
            tr::lookup_id(TrId::LauncherCrashHeader),
            tr::lookup_id(TrId::LauncherCrashContent),
            Some(format!("{e:?}")),
            None,
            None,
        );
    }
}

fn try_resume_update(state: StoredValue<AppState>) -> anyhow::Result<()> {
    let state = state.borrow();
    let ui = state.ui();

    log::info!("interrupted update detected; launching settings to resume");
    ui.global::<State>().set_update_resume_active(true);
    ui.global::<State>().set_loading_app_id(SETTINGS_APP_ID_STR.into());

    state.gui.update_kiosk_policy(UpdateKioskPolicy::all(false)).context("set kiosk policy")?;

    let settings_app_id = settings_app_id();

    let settings_pid =
        app_id_to_pid(&settings_app_id).map_err(|e| anyhow!("lookup settings app PID: {e:?}"))?;

    match settings_pid {
        Some(pid) => state.gui.switch_to(pid, 0, 0).context("switch to settings")?,
        None => AppManagerApi::default()
            .launch_app(&settings_app_id)
            .map_err(|e| anyhow!("failed to launch settings: {e:?}"))?,
    }

    Ok(())
}

fn clear_update_settings_crash(state: StoredValue<AppState>, app_id: [u32; 4]) {
    if state.borrow().ui().global::<State>().get_update_resume_active()
        && AppId::from(app_id) == settings_app_id()
    {
        clear_update_resume_state(state);
    }
}

fn clear_update_resume_state(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    ui.global::<State>().set_update_resume_active(false);
    clear_launching_state(&ui);
    if let Err(e) = state.borrow().gui.update_kiosk_policy(UpdateKioskPolicy::all(true)) {
        log::error!("failed to clear kiosk policy after update resume failure: {e:?}");
    }
}

fn settings_app_id() -> AppId { app_manager::decode_app_id_str(SETTINGS_APP_ID_STR).unwrap() }

fn fmt_price(n: f32) -> String {
    let s = n.to_string();
    let whole = s.split_once('.').map(|(w, _)| w).unwrap_or(&s);

    let mut result_chars = Vec::new();

    for (i, ch) in whole.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result_chars.push(',');
        }
        result_chars.push(ch);
    }

    result_chars.reverse();
    result_chars.into_iter().collect()
}

#[test]
fn format_price_test() {
    assert_eq!(fmt_price(1000.0), "1,000");
    assert_eq!(fmt_price(10000.0), "10,000");
    assert_eq!(fmt_price(100000.0), "100,000");
    assert_eq!(fmt_price(1000000.0), "1,000,000");
}
