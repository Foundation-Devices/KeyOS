// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use app_manager::IconVariant;
use fs::{FileSystemEventType, Location};
use gui_server_api::{
    consts::CLOSE_TIMEOUT_EXIT_CODE,
    msg::UpdateKioskPolicy,
    navigation::{
        alerts::{AlertResult, InvokeAlert},
        qrscanner::{MatchedQrResult, ScanQrMatchedRule, ScanQrOptions, ScanQrResult},
        QR_SCANNER_APP_ID, SETTINGS_APP_ID,
    },
    InputMessage,
};
use haptics::HapticPattern;
use jiff::fmt::strtime;
use layout::{
    page_collection_id, LauncherAction, LauncherCollection, LauncherConfig, LauncherItem, LauncherTarget,
    SlotItem, BITCOIN_APP_ID, SCAN_QR_ACTION_ID, SETTINGS_APP_ID_STR,
};
use slint_keyos_platform::{
    app,
    file_backed::JsonBacked,
    navigation::async_open_qr_scanner,
    raw_image::raw_image_from_bytes,
    settings::{
        global::{OnboardingStatus, SystemTheme, TimeZone, UseStandardTimeFormat},
        messages::SubscribeSystemTheme,
    },
    skia::{scale_image, GraphPoint, GraphStyle, PricePoint},
    sleep,
    slint::{ComponentHandle, Image, ModelRc, SharedString, Timer, VecModel},
    spawn_local, subscribe_archive, subscribe_scalar, StoredValue,
};
use update::messages::ProgressUpdate;
use xous::{app_id_to_pid, current_pid, AppId, PID};

use crate::{fs_permissions::FileSystemPermissions, gui_permissions::GuiPermissions};

mod layout;

const LAUNCH_ANIMATION_TIMEOUT: Duration = Duration::from_millis(1000);
const STALE_TIME: Duration = Duration::from_secs(60 * 2); // 2 minutes
const EXPIRE_TIME_MINUTES: u64 = 60;
const EXPIRE_TIME: Duration = Duration::from_secs(EXPIRE_TIME_MINUTES * 60); // 1 hour
const GRAPH_WINDOW_MINUTES: u64 = 100;
const REDRAW_TIME_SECS: u64 = 60; // 1 minute
const GRAPH_WIDTH: u32 = 432;
const GRAPH_HEIGHT: u32 = 88;
const GRAPH_MAX_HEIGHT: u32 = GRAPH_HEIGHT - 10;
const APP_ICON_SIZE: u32 = 110;
/// Pixel size for the per-app icons shown in the universal-QR match dropdown.
const DROPDOWN_ICON_SIZE: u32 = 40;
const GRAPH_WINDOW_SECS: u64 = 60 * GRAPH_WINDOW_MINUTES;
const PERSISTENT_STATE_PATH: &str = "persistent-state.json";
const FILES_APP_ID: &str = "0x46696c652042726f7773657200000000";
const AUTHENTICATOR_APP_ID: &str = "0x41757468656e74696361746f72203246";
const KEYS_APP_ID: &str = "0x5365637572697479204b657973000000";
const VAULT_APP_ID: &str = "0x53656564205661756c74000000000000";
const LEGACY_APP_ID: &str = "0x6775692d6170702d656d752d666c7578";

/// Built-in user-facing apps that should appear in the launcher, each with the
/// TrId of its label. Declaration order sets the built-ins' launcher rank.
const KNOWN_APPS: &[(&str, TrId)] = &[
    (FILES_APP_ID, TrId::MainFileBrowser),
    (VAULT_APP_ID, TrId::MainSeedVault),
    (KEYS_APP_ID, TrId::MainSecurityKeys),
    (AUTHENTICATOR_APP_ID, TrId::Main2FaCodes),
    (BITCOIN_APP_ID, TrId::MainBitcoin),
    (SETTINGS_APP_ID_STR, TrId::MainSettings),
    (LEGACY_APP_ID, TrId::MainLegacy),
];

fn known_app_label(app_id: &str) -> Option<String> {
    KNOWN_APPS.iter().find(|(id, _)| *id == app_id).map(|(_, tr_id)| tr::lookup_id(*tr_id).to_string())
}

const FLATLINE_POINTS: [PricePoint; 2] = [
    PricePoint { price: 500, timestamp: 0, is_pad: true },
    PricePoint { price: 500, timestamp: 1, is_pad: true },
];
const BITCOIN_SCRUB_MARKER_SIZE: f32 = 10.0;
const BITCOIN_SCRUB_UPDATE_INTERVAL: Duration = Duration::from_millis(100);
const BITCOIN_SCRUB_MOVE_DEADBAND_PX: f32 = 2.0;

app_manager::use_api!();
haptics::use_api!();
quantum_link::use_api!();
update::use_api!();

pub struct UniversalQrMatch {
    app_id: AppId,
    matched_rules: Vec<ScanQrMatchedRule>,
}

/// Sort apps matched by a universal QR scan for display.
///
/// Primary order: highest matched-rule priority descending (app with the highest-priority rule
/// appears first). Ties are broken by app label, case-insensitively, ascending.
fn sort_universal_qr_matches(matched: &mut Vec<(DropdownModel, UniversalQrMatch)>) {
    matched.sort_by(|(a, a_match), (b, b_match)| {
        let a_max = a_match.matched_rules.iter().map(|rule| rule.priority).max().unwrap_or_default();
        let b_max = b_match.matched_rules.iter().map(|rule| rule.priority).max().unwrap_or_default();
        b_max.cmp(&a_max).then_with(|| a.label.as_str().to_lowercase().cmp(&b.label.as_str().to_lowercase()))
    });
}

pub struct AppState {
    pub gui: Arc<GuiApi>,
    pub app_manager: AppManagerApi,
    pub ui: slint::Weak<AppWindow>,
    pub ql_status: QlStatus,
    pub haptics_api: HapticsApi,
    pub loading_state_timer: Timer,
    pub is_fs_unavailable: bool,
    pub is_visible: bool,
    pub persistent: JsonBacked<PersistentState, FileSystemPermissions>,
    pub(crate) launcher: LauncherConfig,
    pub rendered_prices: Vec<PricePoint>,
    pub last_scrubbed_point: Option<(u64, u32)>,
    pub last_scrub_update_time: Option<Instant>,
    pub last_scrub_x: Option<f32>,
    pub prices_dirty: bool,
    pub timezone: TimeZone,
    pub use_standard_time_format: UseStandardTimeFormat,
    pub last_palette: Palettes,
    pub last_draw_time_secs: u64,
    pub pending_universal_scan: Option<ScanQrResult>,
    pub universal_qr_matches: Vec<UniversalQrMatch>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PersistentState {
    pub prices: Vec<PricePoint>,
    /// ISO-4217 code of the fiat the cached price points are denominated in.
    /// Empty on first launch / for legacy persisted state.
    #[serde(default)]
    pub currency_code: String,
    /// Saved item ordering by page index.
    #[serde(default)]
    pub page_orders: Vec<Vec<String>>,
    /// Saved dock item ordering.
    #[serde(default)]
    pub dock_order: Vec<String>,
    /// Version of the layout scheme `page_orders`/`dock_order` was written with;
    /// orders from older schemes are ignored (see [`layout::LAYOUT_VERSION`]).
    #[serde(default)]
    pub layout_version: u32,
}

impl AppState {
    pub fn new(gui: Arc<GuiApi>, ui: slint::Weak<AppWindow>) -> Self {
        let last_palette = ui.unwrap().global::<CurrentTheme>().get_palette();
        let mut persistent: JsonBacked<PersistentState, FileSystemPermissions> =
            JsonBacked::new(PERSISTENT_STATE_PATH, Location::SystemAppData).0;
        persistent.set_auto_save(false);
        // Pre-currency-tagged caches are untrusted — could be any fiat. Drop until QL re-seeds.
        if persistent.currency_code.is_empty() && !persistent.prices.is_empty() {
            persistent.guard().prices.clear();
        }
        let app_manager = AppManagerApi::default();
        let launcher = LauncherConfig::from_persistent(&persistent, discover_launcher_items(&app_manager));
        let settings = SettingsApi::default();
        Self {
            gui,
            app_manager,
            ui,
            ql_status: QlStatus::new(slint_keyos_platform::worker().clone()),
            haptics_api: HapticsApi::default(),
            loading_state_timer: Timer::default(),
            is_fs_unavailable: false,
            is_visible: false,
            persistent,
            launcher,
            rendered_prices: Vec::new(),
            last_scrubbed_point: None,
            last_scrub_update_time: None,
            last_scrub_x: None,
            prices_dirty: false,
            timezone: settings.get_time_zone(),
            use_standard_time_format: settings.get_use_standard_time_format(),
            last_palette,
            last_draw_time_secs: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            pending_universal_scan: None,
            universal_qr_matches: Vec::new(),
        }
    }

    pub fn ui(&self) -> AppWindow { self.ui.unwrap() }

    fn launcher_item_by_id(&self, item_id: &str) -> Option<&LauncherItem> {
        self.launcher.item_by_id(item_id)
    }

    fn move_item(
        &mut self,
        source_collection_id: &str,
        from_index: usize,
        target_collection_id: &str,
        to_index: usize,
    ) {
        if self.launcher.move_item(source_collection_id, from_index, target_collection_id, to_index) {
            self.sync_layout_orders();
        }
    }

    fn sync_layout_orders(&mut self) {
        let mut persistent = self.persistent.guard();
        self.launcher.sync_persistent(&mut persistent);
    }
}

app!("Launcher", role = ClaimLauncherRole);

/// Items the launcher can display: the fixed built-in app allowlist, sideloaded
/// apps reported by the app manager, plus the launcher's own Scan QR action.
fn discover_launcher_items(app_manager: &AppManagerApi) -> Vec<LauncherItem> {
    let locale = SettingsApi::default().get_locale();
    let lang = locale.lang();

    let mut items: Vec<LauncherItem> = Vec::new();
    for entry in app_manager.list_apps(lang, app_manager::AppFilter::standard_only()) {
        let is_known = KNOWN_APPS.iter().any(|(id, _)| *id == entry.app_id.as_str());
        let is_sideloaded = entry.can_remove;
        // Registry entries that are neither known user apps nor sideloaded are
        // hidden support/dev apps, even in simulator builds.
        if !is_known && !is_sideloaded {
            continue;
        }
        items.push(LauncherItem {
            id: entry.app_id.clone(),
            label: known_app_label(&entry.app_id).unwrap_or(entry.name),
            icon_key: entry.app_id.clone(),
            target: LauncherTarget::App { app_id: entry.app_id },
            enabled: entry.can_launch,
            can_remove: entry.can_remove,
        });
    }

    items.push(LauncherItem {
        id: SCAN_QR_ACTION_ID.to_string(),
        label: tr::lookup_id(TrId::MainScanQr).to_string(),
        icon_key: format!("0x{QR_SCANNER_APP_ID}"),
        target: LauncherTarget::Action { action: LauncherAction::ScanQr },
        enabled: true,
        can_remove: false,
    });
    items.sort_by(|a, b| {
        default_rank(&a.id)
            .cmp(&default_rank(&b.id))
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    items
}

/// Canonical placement order for fresh layouts; apps not listed here sort
/// after the built-ins, alphabetically by label.
fn default_rank(item_id: &str) -> usize {
    if let Some(index) = KNOWN_APPS.iter().position(|(id, _)| *id == item_id) {
        index
    } else if item_id == SCAN_QR_ACTION_ID {
        KNOWN_APPS.len()
    } else {
        usize::MAX
    }
}

/// Re-list installed apps and rebuild the launcher layout. Saved ordering is
/// preserved; newly installed apps appear in the first open slot and removed
/// apps drop out.
fn refresh_launcher_apps(state: StoredValue<AppState>) {
    let app_manager = state.borrow().app_manager.clone();
    let discovered = discover_launcher_items(&app_manager);
    let mut state = state.borrow_mut();
    state.launcher = LauncherConfig::from_persistent(&state.persistent, discovered);
    let ui = state.ui();
    sync_launcher_ui(&ui, &state.launcher);
}

fn launcher_item_data(slot: usize, item: &LauncherItem) -> LauncherItemData {
    LauncherItemData {
        id: SharedString::from(item.id.as_str()),
        label: SharedString::from(item.label.as_str()),
        icon_key: SharedString::from(item.icon_key.as_str()),
        enabled: item.enabled,
        slot_index: slot as i32,
        can_remove: item.can_remove,
    }
}

fn build_slot_item_model(items: &[SlotItem]) -> ModelRc<LauncherItemData> {
    ModelRc::new(VecModel::from(
        items.iter().map(|slot_item| launcher_item_data(slot_item.slot, &slot_item.item)).collect::<Vec<_>>(),
    ))
}

fn build_page_model(pages: &[LauncherCollection]) -> ModelRc<LauncherPageData> {
    ModelRc::new(VecModel::from(
        pages
            .iter()
            .enumerate()
            .map(|(page_index, page)| LauncherPageData {
                id: SharedString::from(page_collection_id(page_index)),
                items: build_slot_item_model(&page.items),
            })
            .collect::<Vec<_>>(),
    ))
}

fn sync_launcher_ui(ui: &AppWindow, launcher: &LauncherConfig) {
    let state = ui.global::<State>();
    state.set_pages(build_page_model(&launcher.pages));
    state.set_dock_items(build_slot_item_model(&launcher.dock.items));
    if state.get_current_page() >= launcher.pages.len() as i32 {
        state.set_current_page(launcher.pages.len().saturating_sub(1) as i32);
    }
}

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let state = StoredValue::new(AppState::new(cx.gui.clone(), ui.as_weak()));

    cx.set_input_handler({
        move |app_input| match app_input.msg {
            InputMessage::CloseRequested => {
                state.borrow_mut().persistent.save();
            }
            InputMessage::Hidden => {
                let mut state = state.borrow_mut();
                state.is_visible = false;
                clear_transient_state(&mut state);
            }
            InputMessage::Visible => {
                state.borrow_mut().is_visible = true;
                // Pick up apps installed or removed while the launcher was hidden.
                refresh_launcher_apps(state);
                if state.borrow().is_fs_unavailable {
                    invoke_fs_format_alert(state);
                }
            }
            InputMessage::Custom1 => {
                let ui = state.borrow().ui();
                clear_rearrange_state(&ui);
            }
            InputMessage::Custom2 => {
                let mut state = state.borrow_mut();
                clear_transient_state(&mut state);
            }
            _ => {}
        }
    });

    ui.global::<LauncherCallbacks>().on_universal_qr_continue({
        move |selected_app_id| {
            let Ok(app_id) = app_manager::decode_app_id_str(selected_app_id.as_str()) else {
                log::error!("Invalid universal QR selected app ID: {}", selected_app_id);
                return;
            };

            let (gui, ui, scan_result, matched_rules) = {
                let mut state_mut = state.borrow_mut();
                let Some(scan_result) = state_mut.pending_universal_scan.take() else {
                    log::error!("No pending universal QR scan result to continue with");
                    return;
                };

                let matched_rules = state_mut
                    .universal_qr_matches
                    .iter()
                    .find(|app| app.app_id == app_id)
                    .map(|app| app.matched_rules.clone())
                    .unwrap_or_else(|| {
                        log::error!("Selected app_id not found in universal QR matches");
                        Vec::new()
                    });

                state_mut.universal_qr_matches.clear();
                let ui = state_mut.ui();
                clear_dropdown_model(&ui);
                (state_mut.gui.clone(), ui, scan_result, matched_rules)
            };
            ui.global::<Navigate>().invoke_return_home_animate(Animate::None);

            log::info!("Launching universal QR handoff");
            let options = MatchedQrResult { scan_result, matched_rules };
            if let Err(e) = gui.navigate_to(app_id, &options.serialize()) {
                log::error!("Failed to navigate to universal QR target app: {e:?}");
            };
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

    spawn_local({
        async move {
            let mut theme_updates =
                subscribe_scalar::<settings_permissions::SettingsPermissions, _>(SubscribeSystemTheme);
            while let Some(theme) = theme_updates.next().await {
                let new_palette = if theme == SystemTheme::Dark { Palettes::Dark } else { Palettes::Light };

                // Immediately redraw bitcoin graph upon theme change
                if state.borrow().last_palette != new_palette {
                    refresh_bitcoin_status(state);
                    state.borrow_mut().last_palette = new_palette;
                }
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
        if let Some(name) = state.borrow().app_manager.app_name_by_app_id(&app_id, locale) {
            return name.into();
        }

        "<unknown>".into()
    });

    ui.global::<LauncherCallbacks>().on_scan_clicked({
        move |_pressed| {
            state.borrow().haptics_api.click();
            spawn_local(open_universal_qr_scan(state)).detach();
        }
    });

    ui.global::<LauncherCallbacks>().on_bitcoin_scrub_update({
        move |x: f32, width: f32| {
            let mut state_borrow = state.borrow_mut();
            if state_borrow.rendered_prices.len() < 2 {
                return;
            }
            let now = Instant::now();
            if let (Some(last_x), Some(last_update_time)) =
                (state_borrow.last_scrub_x, state_borrow.last_scrub_update_time)
            {
                let moved = (x - last_x).abs();
                if moved < BITCOIN_SCRUB_MOVE_DEADBAND_PX
                    || now.duration_since(last_update_time) < BITCOIN_SCRUB_UPDATE_INTERVAL
                {
                    return;
                }
            }
            let Some(sample) = graph_sample_for_touch_x(&state_borrow.rendered_prices, x, width) else {
                return;
            };
            let sample_price = sample.price.round() as u32;
            let sample_key = (sample.timestamp, sample_price);
            if state_borrow.last_scrubbed_point == Some(sample_key) {
                return;
            }
            state_borrow.last_scrubbed_point = Some(sample_key);
            state_borrow.last_scrub_update_time = Some(now);
            state_borrow.last_scrub_x = Some(x);

            let ui = state_borrow.ui();
            let ui_state = ui.global::<State>();
            ui_state.set_bitcoin_scrub_active(true);
            ui_state.set_bitcoin_scrub_price(fmt_price(sample_price as f32).into());
            set_bitcoin_point_datetime(
                &ui,
                sample.timestamp,
                &state_borrow.timezone,
                state_borrow.use_standard_time_format.0,
            );

            let marker_half = BITCOIN_SCRUB_MARKER_SIZE / 2.0;
            let marker_x =
                (sample.x - marker_half).clamp(0.0, GRAPH_WIDTH as f32 - BITCOIN_SCRUB_MARKER_SIZE);
            let marker_y =
                (sample.y - marker_half).clamp(0.0, GRAPH_HEIGHT as f32 - BITCOIN_SCRUB_MARKER_SIZE);
            ui_state.set_bitcoin_scrub_marker_x(marker_x);
            ui_state.set_bitcoin_scrub_marker_y(marker_y);
        }
    });

    ui.global::<LauncherCallbacks>().on_bitcoin_scrub_end({
        move || {
            let mut state_borrow = state.borrow_mut();
            state_borrow.last_scrubbed_point = None;
            state_borrow.last_scrub_update_time = None;
            state_borrow.last_scrub_x = None;
            let ui = state_borrow.ui();
            clear_bitcoin_scrub(&ui);
            set_bitcoin_point_datetime_from_latest(
                &ui,
                &state_borrow.rendered_prices,
                &state_borrow.timezone,
                state_borrow.use_standard_time_format.0,
            );
        }
    });

    ui.global::<LauncherCallbacks>().on_item_clicked({
        move |x: f32, y: f32, item_id| {
            let x = x as usize;
            let y = y as usize;
            let (gui, ui, item) = {
                let state = state.borrow();
                let Some(item) = state.launcher_item_by_id(item_id.as_str()).cloned() else {
                    log::warn!("Unknown launcher item clicked: {item_id}");
                    return;
                };
                (state.gui.clone(), state.ui(), item)
            };

            // Ignore item clicks when an app is already being launched (SFT-6854)
            if is_launching(&ui) {
                return;
            }

            if !item.enabled {
                log::info!("Ignoring disabled launcher item click: {}", item.id);
                return;
            }

            state.borrow().haptics_api.click();

            match item.target {
                LauncherTarget::Action { action: LauncherAction::ScanQr } => {
                    ui.global::<State>().set_loading_item_id(item.id.clone().into());
                    spawn_local(open_universal_qr_scan(state)).detach();
                }
                LauncherTarget::App { app_id } => {
                    let Ok(app_id) = app_manager::decode_app_id_str(app_id.as_str()) else {
                        log::error!("Invalid AppId hex configured for item {}: {}", item.id, app_id);
                        launcher_crash_error(
                            &ui,
                            format!("Invalid AppId hex configured for item {}: {}", item.id, app_id),
                        );
                        return;
                    };

                    log::info!("Launcher item clicked with id={} at ({x}, {y})", item.id);

                    // The app is already running, just switch to it
                    if let Ok(Some(pid)) = app_id_to_pid(&app_id) {
                        clear_launching_state(&ui);
                        if let Err(e) = gui.switch_to(pid, x, y) {
                            log::error!("Failed to switch to running app: {e:?}");
                            launcher_crash_error(&ui, format!("{e:?}"));
                        }
                        return;
                    }

                    // Otherwise request the app-manager to launch the app
                    ui.global::<State>().set_loading_item_id(item.id.clone().into());
                    if let Err(e) = state.borrow().app_manager.launch_app(&app_id) {
                        launcher_crash_error(&ui, format!("{e:?}"));
                    }
                }
            }
        }
    });

    {
        let app_manager = state.borrow().app_manager.clone();
        let app_icon_cache = RefCell::new(lru::LruCache::<String, Image>::new(
            NonZeroUsize::new(32).expect("cache cap is non-zero"),
        ));
        ui.global::<LauncherCallbacks>().on_app_icon(move |icon_key, is_dark| {
            let (variant, theme_key) =
                if is_dark { (IconVariant::Dark, "dark") } else { (IconVariant::Light, "light") };
            let cache_key =
                format!("{}@{}x{}:smooth:{theme_key}", icon_key.as_str(), APP_ICON_SIZE, APP_ICON_SIZE);
            if let Some(image) = app_icon_cache.borrow_mut().get(cache_key.as_str()).cloned() {
                log::debug!("app icon cache hit on {cache_key}");
                return image;
            }

            let icon_bytes = app_manager.get_app_icon(icon_key.as_str(), variant).unwrap_or_default();
            let image = scale_image(
                raw_image_from_bytes(&icon_bytes),
                APP_ICON_SIZE as f32,
                APP_ICON_SIZE as f32,
                true,
            );
            // Cache the result even when the app ships no icon: the empty image is a
            // valid answer, and caching it avoids re-issuing the blocking `get_app_icon`
            // IPC + decode every time an icon-less tile subtree rebuilds.
            app_icon_cache.borrow_mut().put(cache_key, image.clone());
            image
        });
    }

    ui.global::<LauncherCallbacks>().on_drop_slide_target({
        let state = state.clone();
        move |collection_id, slot| {
            state
                .borrow()
                .launcher
                .drop_slide_target(collection_id.as_str(), slot.max(0) as usize)
                .map_or(-1, |slot| slot as i32)
        }
    });

    ui.global::<LauncherCallbacks>().on_displaced_item({
        let state = state.clone();
        move |collection_id, slot| {
            state
                .borrow()
                .launcher
                .displaced_item(collection_id.as_str(), slot.max(0) as usize)
                .map_or_else(LauncherItemData::default, |item| launcher_item_data(0, item))
        }
    });

    ui.global::<LauncherCallbacks>().on_move_item({
        move |source_collection_id, from_index, target_collection_id, to_index| {
            let mut state = state.borrow_mut();
            state.move_item(
                source_collection_id.as_str(),
                from_index.max(0) as usize,
                target_collection_id.as_str(),
                to_index.max(0) as usize,
            );
            let ui = state.ui();
            sync_launcher_ui(&ui, &state.launcher);
            state.persistent.save();
        }
    });

    ui.global::<LauncherCallbacks>().on_remove_item_requested({
        move |item_id| {
            confirm_and_remove_launcher_item(state, item_id.as_str());
        }
    });

    ui.global::<LauncherCallbacks>().on_delete_legacy_confirmed({
        move || {
            delete_legacy_confirmed(state);
        }
    });

    {
        let state = state.borrow();
        sync_launcher_ui(&state.ui(), &state.launcher);
    }

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
                        state.borrow_mut().is_fs_unavailable = true;
                        if state.borrow().is_visible {
                            invoke_fs_format_alert(state);
                        }
                    }
                }
            }
        }
    })
    .detach();

    setup_ql_status(state);
    setup_update_resume(state);
    setup_bitcoin_status(state);
    refresh_bitcoin_status(state);

    let price_timer = Timer::default();
    price_timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), {
        move || {
            let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let (prices_dirty, last_draw_time_secs) = {
                let state = state.borrow();
                (state.prices_dirty, state.last_draw_time_secs)
            };
            let timed_out = now_secs.saturating_sub(last_draw_time_secs) >= REDRAW_TIME_SECS;

            if prices_dirty || timed_out {
                refresh_bitcoin_status(state);
            }
        }
    });

    ui.run().expect("Platform error");
}

/// Load an app's bundled icon, scaled for the universal-QR match dropdown. Returns
/// an empty image (falls back to no/themed icon) when the app ships no icon.
fn dropdown_app_icon(app_manager: &AppManagerApi, app_id_hex: &str, variant: IconVariant) -> Image {
    let icon_bytes = app_manager.get_app_icon(app_id_hex, variant).unwrap_or_default();
    if icon_bytes.is_empty() {
        return Image::default();
    }
    scale_image(raw_image_from_bytes(&icon_bytes), DROPDOWN_ICON_SIZE as f32, DROPDOWN_ICON_SIZE as f32, true)
}

/// Open the universal QR scanner and route the result: auto-forward when a single
/// app matches, otherwise populate the dropdown and navigate to the selection page.
async fn open_universal_qr_scan(state: StoredValue<AppState>) {
    {
        let mut state_mut = state.borrow_mut();
        state_mut.pending_universal_scan = None;
        state_mut.universal_qr_matches.clear();
        let ui = state_mut.ui();
        clear_dropdown_model(&ui);
    }

    // This sleep yields to allow the user to see a spinner early
    sleep(Duration::from_millis(1)).await;

    log::info!("Opening universal QR scanner");
    let open_result = async_open_qr_scanner::<GuiPermissions>(ScanQrOptions {
        request_matching_apps: true,
        header_left_icon: String::new(),
        header_right_icon: String::from("close"),
        message: tr::lookup_id(TrId::ScanContent).to_string(),
        ..ScanQrOptions::default()
    })
    .await;

    // Clear the loading spinner now that the scanner has closed (or failed to open)
    clear_launching_state(&state.borrow().ui());
    let scan_result = match open_result {
        Ok(Some(result)) => result,
        Ok(None) => {
            log::info!("No result returned from QR scanner");
            return;
        }
        Err(e) => {
            log::error!("Failed to open QR scanner: {e:?}");
            return;
        }
    };

    let matching_apps = match &scan_result {
        ScanQrResult::Qr { matching_apps, .. } | ScanQrResult::Ur2 { matching_apps, .. } => {
            matching_apps.clone()
        }
        ScanQrResult::LeftClicked | ScanQrResult::RightClicked | ScanQrResult::ButtonClicked => {
            log::info!("QR scanner dismissed");
            return;
        }
    };

    let matching_apps = matching_apps.unwrap_or_default();

    if matching_apps.is_empty() {
        log::info!("No installed apps accept the scanned QR payload");
        return;
    }

    // Auto-forward when only one app matches — skip the selection UI entirely
    if matching_apps.len() == 1 {
        let single_match = &matching_apps[0];
        let app_id = AppId::from(single_match.id);
        let gui = state.borrow().gui.clone();
        log::info!("Single app match — auto-forwarding universal QR");
        let options = MatchedQrResult { scan_result, matched_rules: single_match.matched_rules.clone() };
        if let Err(e) = gui.navigate_to(app_id, &options.serialize()) {
            log::error!("Failed to navigate to universal QR target app: {e:?}");
        }
        return;
    }

    let locale = "en"; // TODO: i18n, get the locale from the settings
    let app_manager = state.borrow().app_manager.clone();
    let variant = if state.borrow().ui().global::<CurrentTheme>().get_is_dark() {
        IconVariant::Dark
    } else {
        IconVariant::Light
    };
    let mut matched: Vec<_> = matching_apps
        .iter()
        .filter_map(|app| {
            let app_id = AppId::from(app.id);
            let Some(app_name) = app_manager.app_name_by_app_id(&app_id, locale) else {
                log::warn!("Could not fetch app name for app_id=0x{app_id}");
                return None;
            };
            let app_id_hex = format!("0x{app_id}");
            let icon_image = dropdown_app_icon(&app_manager, &app_id_hex, variant);
            Some((
                DropdownModel {
                    label: app_name.clone().into(),
                    value: app_id_hex.into(),
                    icon: "".into(),
                    icon_image,
                },
                UniversalQrMatch { app_id, matched_rules: app.matched_rules.clone() },
            ))
        })
        .collect();

    sort_universal_qr_matches(&mut matched);

    let (dropdown_model, universal_qr_matches): (Vec<_>, Vec<_>) = matched.into_iter().unzip();

    let mut state_mut = state.borrow_mut();
    state_mut.pending_universal_scan = Some(scan_result);
    state_mut.universal_qr_matches = universal_qr_matches;

    let ui = state_mut.ui();
    ui.global::<State>().set_dropdown_model(slint::ModelRc::new(VecModel::from(dropdown_model)));
    ui.global::<Navigate>()
        .invoke_universal_qr(NavigateOptions { replace: false, animate: Animate::Forward });
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
                log::info!("Received QuantumLink exchange rate: {exchange_rate:?}");
                {
                    let mut state = state.borrow_mut();
                    {
                        let mut persistent = state.persistent.guard();
                        if persistent.currency_code != exchange_rate.currency_code {
                            // Currency changed under us — drop the old series so we don't mix
                            // points from two different fiats on the same chart.
                            persistent.prices.clear();
                            persistent.currency_code = exchange_rate.currency_code.clone();
                        }
                        persistent.prices.push(PricePoint {
                            price: exchange_rate.rate as u32,
                            timestamp: exchange_rate.timestamp,
                            is_pad: false,
                        });
                        persistent.prices.sort_by_key(|point| point.timestamp);
                    }
                    state.prices_dirty = true;
                }
                refresh_bitcoin_status(state);
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
                log::info!("Received QuantumLink exchange rate history");
                {
                    let mut state = state.borrow_mut();
                    {
                        let mut persistent = state.persistent.guard();
                        let currency_changed = persistent.currency_code != history.currency_code;
                        persistent.currency_code = history.currency_code.clone();
                        let mut new_points: Vec<PricePoint> = history
                            .history
                            .into_iter()
                            .map(|point| PricePoint {
                                price: point.rate as u32,
                                timestamp: point.timestamp,
                                is_pad: false,
                            })
                            .collect();
                        new_points.sort_by_key(|point| point.timestamp);
                        // Carry forward live points newer than this history's
                        // newest sample so a late transfer can't go backwards.
                        if !currency_changed {
                            let new_tail = new_points.last().map(|p| p.timestamp).unwrap_or(0);
                            let live_extras: Vec<PricePoint> = persistent
                                .prices
                                .iter()
                                .filter(|p| p.timestamp > new_tail)
                                .copied()
                                .collect();
                            new_points.extend(live_extras);
                            new_points.sort_by_key(|point| point.timestamp);
                        }
                        persistent.prices = new_points;
                    }
                    state.prices_dirty = true;
                }
                refresh_bitcoin_status(state);
            }
        }
    })
    .detach();
}

fn refresh_bitcoin_status(state: StoredValue<AppState>) {
    let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let window_start = now_secs.saturating_sub(GRAPH_WINDOW_SECS);

    let mut state = state.borrow_mut();
    {
        let mut persistent = state.persistent.guard();
        persistent.prices.retain(|point| point.timestamp >= window_start);
    }
    let ui = state.ui();
    let mut prices = state.persistent.prices.clone();

    #[cfg(not(target_os = "xous"))]
    if prices.is_empty() {
        prices = simulator_price_history(window_start, now_secs);
    }

    let current_palette = ui.global::<CurrentTheme>().get_palette();
    let is_dark_mode = current_palette == Palettes::Dark;

    let ui_state = ui.global::<State>();
    let currency_code = if state.persistent.currency_code.is_empty() {
        "USD".to_string()
    } else {
        state.persistent.currency_code.clone()
    };
    ui_state.set_bitcoin_currency_code(currency_code.into());
    let last_price_point = prices.last().copied();

    match last_price_point {
        Some(point) => {
            let last_price_time = UNIX_EPOCH + Duration::from_secs(point.timestamp);
            let time_since_price = SystemTime::now().duration_since(last_price_time).unwrap_or_default();

            log::info!("checking price from {:?} ago: {}", time_since_price, point.price);

            let is_expired = time_since_price >= EXPIRE_TIME;
            let is_stale = time_since_price >= STALE_TIME || is_expired;

            ui_state.set_is_price_stale(is_stale);
            ui_state.set_is_price_expired(is_expired);
            ui_state.set_last_update_time(tr::format_duration(time_since_price).into());
            ui_state.set_bitcoin_price(fmt_price(point.price as f32).into());
            if !ui_state.get_bitcoin_scrub_active() {
                set_bitcoin_point_datetime(
                    &ui,
                    point.timestamp,
                    &state.timezone,
                    state.use_standard_time_format.0,
                );
            }

            let first_price = prices.first().map(|first_point| first_point.price).unwrap_or(point.price);
            let current_price = point.price;
            let change_percentage = if first_price == 0 {
                0.0
            } else {
                ((current_price as f32 - first_price as f32) / first_price as f32) * 100.0
            };
            let sign = if change_percentage.is_sign_positive() { '+' } else { '-' };
            let change_percentage_formatted = format!("{sign}{:.2}%", change_percentage.abs());

            ui_state.set_is_price_positive(change_percentage >= 0.0);
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
                GraphStyle { is_price_positive: change_percentage >= 0.0, is_dark_mode },
            );
            ui_state.set_bitcoin_graph_image(graph_image);

            if is_expired {
                state.rendered_prices.clear();
                state.last_scrubbed_point = None;
                state.last_scrub_update_time = None;
                state.last_scrub_x = None;
                if ui_state.get_bitcoin_scrub_active() {
                    clear_bitcoin_scrub(&ui);
                    set_bitcoin_point_datetime(
                        &ui,
                        point.timestamp,
                        &state.timezone,
                        state.use_standard_time_format.0,
                    );
                }
                ui_state.set_bitcoin_scrub_enabled(false);
            } else {
                state.rendered_prices = data_vec;
                ui_state.set_bitcoin_scrub_enabled(state.rendered_prices.len() >= 2);
            }
        }
        None => {
            ui_state.set_is_price_stale(true);
            ui_state.set_is_price_expired(true);
            ui_state.set_is_price_positive(true);

            let data_vec = FLATLINE_POINTS.to_vec();
            let graph_image = slint_keyos_platform::skia::draw_graph(
                &data_vec,
                GRAPH_WIDTH,
                GRAPH_HEIGHT,
                GRAPH_MAX_HEIGHT,
                GraphStyle { is_price_positive: true, is_dark_mode },
            );
            ui_state.set_bitcoin_graph_image(graph_image);

            ui_state.set_bitcoin_price(Default::default());
            ui_state.set_bitcoin_price_change_percent(Default::default());
            state.rendered_prices.clear();
            state.last_scrubbed_point = None;
            state.last_scrub_update_time = None;
            state.last_scrub_x = None;
            clear_bitcoin_scrub(&ui);
            clear_bitcoin_point_datetime(&ui);
            ui_state.set_bitcoin_scrub_enabled(false);
        }
    }

    state.last_draw_time_secs = now_secs;
    state.prices_dirty = false;
}

#[cfg(not(target_os = "xous"))]
fn simulator_price_history(window_start: u64, now_secs: u64) -> Vec<PricePoint> {
    let point_count = (GRAPH_WINDOW_SECS / 60) as usize + 1;
    let last_index = point_count.saturating_sub(1).max(1) as f32;

    (0..point_count)
        .map(|index| {
            let progress = index as f32 / last_index;
            let chop = ((((index * 17) % 7) as f32) - 3.0) * 82.0;
            let saw = ((progress * 21.0 + 0.19).fract() - 0.5) * 320.0;
            let price = 76_250.0
                + 6_450.0 * progress
                + 520.0 * (progress * std::f32::consts::PI * 4.6).sin()
                + 210.0 * (progress * std::f32::consts::PI * 12.7 + 0.45).sin()
                + saw
                + chop;

            PricePoint {
                price: price.clamp(75_000.0, 85_000.0).round() as u32,
                timestamp: (window_start + index as u64 * 60).min(now_secs),
                is_pad: false,
            }
        })
        .collect()
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

fn confirm_and_remove_launcher_item(state: StoredValue<AppState>, item_id: &str) {
    let Some(item) = state.borrow().launcher_item_by_id(item_id).cloned() else {
        log::warn!("Unknown launcher item remove requested: {item_id}");
        return;
    };

    if !item.can_remove {
        log::warn!("Ignoring remove request for non-removable launcher item: {}", item.id);
        return;
    }

    let LauncherTarget::App { app_id } = item.target.clone() else {
        log::warn!("Ignoring remove request for non-app launcher item: {}", item.id);
        return;
    };

    {
        let ui = state.borrow().ui();
        clear_rearrange_state(&ui);
    }

    // Legacy is a built-in exception: deleting it is irreversible and cascades to its emulated
    // apps, so it confirms with a centered destructive modal (matching the Figma pop-up) instead
    // of the generic sideloaded bottom-sheet alert. The removal runs once the user confirms.
    if app_id.as_str() == LEGACY_APP_ID {
        let ui = state.borrow().ui();
        let launcher_state = ui.global::<State>();
        launcher_state.set_pending_delete_item_id(item.id.clone().into());
        launcher_state.set_delete_legacy_active(true);
        return;
    }

    let result = {
        let gui = state.borrow().gui.clone();
        gui.invoke_alert(InvokeAlert {
            app_title: None,
            title: tr::lookup_id(TrId::RemoveAppHeader).to_string(),
            icon: "alert".to_string(),
            line1: i18n::replace_placeholders(tr::lookup_id(TrId::RemoveAppContent), &[item.label.as_str()]),
            line2: None,
            button1_title: tr::lookup_id(TrId::CommonButtonDelete).to_string(),
            button2_title: Some(tr::lookup_id(TrId::CommonButtonCancel).to_string()),
            button3_title: None,
        })
        .unwrap_or(AlertResult::Canceled)
    };

    if !matches!(result, AlertResult::Button1Pressed) {
        return;
    }

    remove_launcher_item(state, &item);
}

/// Decode a removable launcher item's AppId and ask app-manager to remove it, refreshing the
/// launcher on success and surfacing an error modal on failure. The item must be a removable app.
fn remove_launcher_item(state: StoredValue<AppState>, item: &LauncherItem) {
    let LauncherTarget::App { app_id } = item.target.clone() else {
        log::warn!("Ignoring removal for non-app launcher item: {}", item.id);
        return;
    };

    let app_id = match app_manager::decode_app_id_str(app_id.as_str()) {
        Ok(app_id) => app_id,
        Err(e) => {
            log::error!("Invalid removable launcher AppId {}: {e:?}", item.id);
            show_remove_app_error(state, &item.label);
            return;
        }
    };

    log::info!("Requesting removal of launcher item {} (app_id={app_id})", item.id);
    let remove_result = state.borrow().app_manager.remove_installed_app(&app_id);

    match remove_result {
        Ok(app_manager::RemoveInstalledAppResult::Removed)
        | Ok(app_manager::RemoveInstalledAppResult::NotFound) => {
            let ui = state.borrow().ui();
            clear_rearrange_state(&ui);
            refresh_launcher_apps(state);
            let mut state = state.borrow_mut();
            state.sync_layout_orders();
            state.persistent.save();
        }
        Ok(app_manager::RemoveInstalledAppResult::Running) => {
            log::warn!("failed to remove launcher item {}: app is still running", item.id);
            show_remove_app_error(state, &item.label);
        }
        Ok(app_manager::RemoveInstalledAppResult::NotSideloaded) => {
            log::warn!("failed to remove launcher item {}: app is not sideloaded", item.id);
            show_remove_app_error(state, &item.label);
        }
        Ok(app_manager::RemoveInstalledAppResult::InternalError) => {
            log::error!("failed to remove launcher item {}: app-manager internal error", item.id);
            show_remove_app_error(state, &item.label);
        }
        Err(e) => {
            log::error!("failed to request launcher item removal {}: {e:?}", item.id);
            show_remove_app_error(state, &item.label);
        }
    }
}

/// Run the Legacy removal after the user confirms in the centered delete modal, dismissing the
/// modal first. Operates on the item snapshotted when the modal was shown.
fn delete_legacy_confirmed(state: StoredValue<AppState>) {
    let item_id = {
        let ui = state.borrow().ui();
        let launcher_state = ui.global::<State>();
        launcher_state.set_delete_legacy_active(false);
        let id = launcher_state.get_pending_delete_item_id().to_string();
        launcher_state.set_pending_delete_item_id("".into());
        id
    };

    let Some(item) = state.borrow().launcher_item_by_id(&item_id).cloned() else {
        log::warn!("delete-legacy confirmed but launcher item {item_id} is gone");
        return;
    };

    remove_launcher_item(state, &item);
}

fn show_remove_app_error(state: StoredValue<AppState>, app_label: &str) {
    let gui = state.borrow().gui.clone();
    let _ = gui.invoke_alert(InvokeAlert {
        app_title: None,
        title: tr::lookup_id(TrId::RemoveAppFailedHeader).to_string(),
        icon: "alert".to_string(),
        line1: i18n::replace_placeholders(tr::lookup_id(TrId::RemoveAppFailedContent), &[app_label]),
        line2: None,
        button1_title: tr::lookup_id(TrId::CommonButtonDone).to_string(),
        button2_title: None,
        button3_title: None,
    });
}

fn clear_launching_state(ui: &AppWindow) { ui.global::<State>().set_loading_item_id("".into()); }

fn clear_transient_state(state: &mut AppState) {
    state.last_scrubbed_point = None;
    state.last_scrub_update_time = None;
    state.last_scrub_x = None;
    let ui = state.ui();
    clear_launching_state(&ui);
    clear_rearrange_state(&ui);
    clear_bitcoin_scrub(&ui);
    state.loading_state_timer.stop();
}

fn clear_dropdown_model(ui: &AppWindow) {
    ui.global::<State>().set_dropdown_model(ModelRc::new(VecModel::from(Vec::<DropdownModel>::new())));
}

fn clear_rearrange_state(ui: &AppWindow) {
    let state = ui.global::<State>();
    state.set_rearrange_mode(false);
    state.set_drag_source_collection_id("".into());
    state.set_drag_overlay_visible(false);
    state.set_drag_hover_collection_id("".into());
    state.set_drag_hover_index(-1);
}

fn clear_bitcoin_scrub(ui: &AppWindow) {
    let ui_state = ui.global::<State>();
    ui_state.set_bitcoin_scrub_active(false);
    ui_state.set_bitcoin_scrub_price("".into());
    ui_state.set_bitcoin_scrub_marker_x(0.0);
    ui_state.set_bitcoin_scrub_marker_y(0.0);
}

fn clear_bitcoin_point_datetime(ui: &AppWindow) {
    let ui_state = ui.global::<State>();
    ui_state.set_bitcoin_point_date("".into());
    ui_state.set_bitcoin_point_time("".into());
}

fn set_bitcoin_point_datetime(
    ui: &AppWindow,
    timestamp_secs: u64,
    timezone: &TimeZone,
    use_standard_time_format: bool,
) {
    let Ok(timestamp) = jiff::Timestamp::from_second(timestamp_secs as i64) else {
        clear_bitcoin_point_datetime(ui);
        return;
    };
    let zoned = timestamp.to_zoned(timezone.timezone());

    let date = strtime::format("%B %e, %Y", &zoned).unwrap_or_default();
    let time = if use_standard_time_format {
        strtime::format("%H:%M:%S", &zoned).unwrap_or_default()
    } else {
        strtime::format("%I:%M:%S %p", &zoned).unwrap_or_default()
    };

    let ui_state = ui.global::<State>();
    ui_state.set_bitcoin_point_date(date.into());
    ui_state.set_bitcoin_point_time(time.into());
}

fn set_bitcoin_point_datetime_from_latest(
    ui: &AppWindow,
    data: &[PricePoint],
    timezone: &TimeZone,
    use_standard_time_format: bool,
) {
    let Some(point) = data.last() else {
        clear_bitcoin_point_datetime(ui);
        return;
    };
    set_bitcoin_point_datetime(ui, point.timestamp, timezone, use_standard_time_format);
}

fn is_launching(ui: &AppWindow) -> bool { !ui.global::<State>().get_loading_item_id().is_empty() }

fn launcher_crash_error(ui: &AppWindow, long_message: String) {
    error_message(
        ui,
        tr::lookup_id(TrId::LauncherCrashHeader),
        tr::lookup_id(TrId::LauncherCrashContent),
        Some(long_message),
        None,
        None,
    );
}

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
            crashed_app_id: app_id.map(|a| a.to_string().into()).unwrap_or("".into()),
            crashed_pid: pid.map(|p| p.get() as i32).unwrap_or(0),
            error_message: message.into(),
            error_title: title.into(),
            long_error_message: long_message.unwrap_or(String::from("")).into(),
        },
        NavigateOptions { replace: false, animate: Animate::None },
    );
}

fn handle_app_event(state: StoredValue<AppState>, event: app_manager::AppEvent) {
    let (ui, gui_api, app_manager) = {
        let state_borrow = state.borrow();
        (state_borrow.ui(), state_borrow.gui.clone(), state_borrow.app_manager.clone())
    };
    match event {
        app_manager::AppEvent::AppLaunched { app_id, pid, launched_by } => {
            // Ignore launch events that are not initiated by the launcher itself
            if launched_by != current_pid().expect("current pid") {
                return;
            }

            log::info!("App launched: app_id={app_id}, pid={pid}");

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
            if let Err(e) = gui_api.switch_to(pid, 0, 0) {
                log::error!("Failed to switch to launched app: {e:?}");
                launcher_crash_error(&ui, format!("{e:?}"));
            }
        }

        app_manager::AppEvent::LaunchError { app_id, error: e } => {
            log::error!("App launch error: {e:?}");
            clear_update_settings_crash(state, app_id);
            launcher_crash_error(&ui, format!("{e:?}"));
        }

        // TODO (SFT-5433): push the crash message into a log
        app_manager::AppEvent::AppCrashed { app_id, pid, exit_code, panic_message, .. } => {
            clear_update_settings_crash(state, app_id);

            let app_name = app_manager
                .app_name_by_app_id(&app_id, "en") // TODO: i18n, get the locale from the settings
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
                    Some(app_id),
                )
            }
        }

        app_manager::AppEvent::AppSetChanged { installed, removed } => {
            log::info!("App set changed: {} installed, {} removed", installed.len(), removed.len());
            refresh_launcher_apps(state);
        }

        app_manager::AppEvent::TrustedPublishersChanged => refresh_launcher_apps(state),
    }
}

fn setup_update_resume(state: StoredValue<AppState>) {
    spawn_local(async move {
        let mut fs_events = subscribe_scalar::<fs_permissions::FileSystemPermissions, _>(
            fs::messages::SubscribeFilesystemEvent(Location::AppData),
        );

        while let Some(event) = fs_events.next().await {
            match event.event_type {
                FileSystemEventType::Mounted => {
                    maybe_resume_update(state);
                    return;
                }
                FileSystemEventType::Unmounted => {}
                FileSystemEventType::Error => {
                    log::error!("app data filesystem error while waiting to resume update")
                }
            }
        }
    })
    .detach();

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
        launcher_crash_error(&ui, format!("{e:?}"));
    }
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

fn try_resume_update(state: StoredValue<AppState>) -> anyhow::Result<()> {
    let state = state.borrow();
    let ui = state.ui();

    log::info!("interrupted update detected; launching settings to resume");
    ui.global::<State>().set_update_resume_active(true);
    ui.global::<State>().set_loading_item_id(SETTINGS_APP_ID_STR.into());

    state.gui.update_kiosk_policy(UpdateKioskPolicy::all(false)).context("set kiosk policy")?;

    let settings_pid =
        app_id_to_pid(&SETTINGS_APP_ID).map_err(|e| anyhow!("lookup settings app PID: {e:?}"))?;

    match settings_pid {
        Some(pid) => state.gui.switch_to(pid, 0, 0).context("switch to settings")?,
        None => state
            .app_manager
            .launch_app(&SETTINGS_APP_ID)
            .map_err(|e| anyhow!("failed to launch settings: {e:?}"))?,
    }

    Ok(())
}

fn clear_update_settings_crash(state: StoredValue<AppState>, app_id: AppId) {
    if state.borrow().ui().global::<State>().get_update_resume_active() && app_id == SETTINGS_APP_ID {
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

fn graph_sample_for_touch_x(data: &[PricePoint], touch_x: f32, card_width: f32) -> Option<GraphPoint> {
    if card_width <= f32::EPSILON {
        return None;
    }
    let normalized_x = (touch_x / card_width).clamp(0.0, 1.0);
    let graph_x = normalized_x * GRAPH_WIDTH as f32;
    let samples = slint_keyos_platform::skia::graph_points(data, GRAPH_WIDTH, GRAPH_HEIGHT, GRAPH_MAX_HEIGHT);
    nearest_sample_at_x(&samples, graph_x)
}

fn nearest_sample_at_x(samples: &[GraphPoint], x: f32) -> Option<GraphPoint> {
    samples.iter().copied().min_by(|a, b| (a.x - x).abs().total_cmp(&(b.x - x).abs()))
}

#[cfg(test)]
mod tests {
    use app_manifest::QrPriority;

    use super::*;

    #[test]
    fn format_price_test() {
        assert_eq!(fmt_price(1000.0), "1,000");
        assert_eq!(fmt_price(10000.0), "10,000");
        assert_eq!(fmt_price(100000.0), "100,000");
        assert_eq!(fmt_price(1000000.0), "1,000,000");
    }

    #[test]
    fn nearest_graph_sample_test() {
        let samples = vec![
            GraphPoint { x: 0.0, y: 60.0, price: 100.0, timestamp: 100 },
            GraphPoint { x: 100.0, y: 20.0, price: 200.0, timestamp: 200 },
        ];
        let sample = nearest_sample_at_x(&samples, 60.0).expect("sample");
        assert_eq!(sample.timestamp, 200);
        assert!((sample.y - 20.0).abs() <= f32::EPSILON);
    }

    // --- Universal QR sort tests ---

    fn make_qr_app(name: &str, priorities: &[u8]) -> (DropdownModel, UniversalQrMatch) {
        let rules = priorities
            .iter()
            .enumerate()
            .map(|(i, &priority)| ScanQrMatchedRule {
                rule_id: format!("rule-{i}"),
                priority: QrPriority::new(priority).unwrap(),
                sub_rule_id: format!("sub-{i}"),
            })
            .collect();
        (
            DropdownModel {
                label: name.into(),
                value: "".into(),
                icon: "".into(),
                icon_image: Default::default(),
            },
            UniversalQrMatch { app_id: AppId([0u8; 16]), matched_rules: rules },
        )
    }

    #[test]
    fn qr_apps_sorted_by_highest_priority_first() {
        let mut apps = vec![make_qr_app("Low", &[1]), make_qr_app("High", &[5]), make_qr_app("Mid", &[3])];
        sort_universal_qr_matches(&mut apps);
        let names: Vec<_> = apps.iter().map(|(d, _)| d.label.as_str()).collect();
        assert_eq!(names, ["High", "Mid", "Low"]);
    }

    #[test]
    fn qr_apps_priority_ties_broken_alphabetically_case_insensitive() {
        let mut apps =
            vec![make_qr_app("Zebra", &[5]), make_qr_app("apple", &[5]), make_qr_app("Bitcoin", &[5])];
        sort_universal_qr_matches(&mut apps);
        let names: Vec<_> = apps.iter().map(|(d, _)| d.label.as_str()).collect();
        assert_eq!(names, ["apple", "Bitcoin", "Zebra"]);
    }

    #[test]
    fn qr_apps_sort_uses_max_priority_across_multiple_rules() {
        // App A has rules with priorities [1, 2]; App B has a single rule with priority 3.
        // App A's best rule (2) loses to App B (3), so B should sort first.
        let mut apps = vec![make_qr_app("App A", &[1, 2]), make_qr_app("App B", &[3])];
        sort_universal_qr_matches(&mut apps);
        let names: Vec<_> = apps.iter().map(|(d, _)| d.label.as_str()).collect();
        assert_eq!(names, ["App B", "App A"]);
    }

    #[test]
    fn qr_apps_sort_zero_priority_is_lowest() {
        // The empty matched-rules fallback is 3, so an explicit 5 still sorts ahead of it.
        let mut apps = vec![make_qr_app("Default", &[]), make_qr_app("Explicit", &[5])];
        sort_universal_qr_matches(&mut apps);
        let names: Vec<_> = apps.iter().map(|(d, _)| d.label.as_str()).collect();
        assert_eq!(names, ["Explicit", "Default"]);
    }
}
