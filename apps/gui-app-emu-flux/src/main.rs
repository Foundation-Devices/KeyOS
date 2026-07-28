// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod flux;

use std::{cell::RefCell, collections::HashMap, collections::HashSet, rc::Rc};

use slint_keyos_platform::{
    app,
    file_backed::JsonBacked,
    gui_server_api::navigation::lockscreen::VerifyPinOptions,
    navigation::verify_pin,
    slint::{ComponentHandle as _, Model as _, ModelRc, VecModel},
    spawn,
};
#[cfg(keyos)]
use slint_keyos_platform::{gui_server_api::InputMessage, spawn_local, subscribe_archive};
use xous::PID;

use crate::flux::FluxServer;

/// How the Legacy Mode seed was configured.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(tag = "kind")]
enum SeedConfig {
    /// Derived from the device's AppSeed at runtime via the security API.
    /// No seed bytes are stored; they are re-derived on every launch.
    /// The AppSeed is used as BIP39 entropy, so APP_SEED is the 64-byte PBKDF2 seed as
    /// below and the words shown under View Seed restore the same wallet elsewhere.
    DerivedFromAppSeed,
    /// A manually entered mnemonic (12 or 24 words).
    /// Stored as the BIP39 entropy (16 bytes for 12 words, 32 for 24), hex-encoded.
    /// APP_SEED is set to the 64-byte PBKDF2 seed derived from this entropy.
    ManuallyEntered { entropy_hex: String },
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct FluxSettings {
    disabled_apps: HashSet<String>,
    #[serde(default)]
    seed_config: Option<SeedConfig>,
}

app_manager::use_api!();
haptics::use_api!();
#[cfg(keyos)]
legacy_hid::use_api!();
security::use_api!();

/// The emulator's app state in one place: the installable Flux apps for the launcher
/// grid (`possible`) and the running Flux children (`running`, PID -> display name).
struct AppState {
    possible: Vec<app_manager::InstalledAppInfo>,
    running: HashMap<PID, String>,
}

app!("Legacy Mode");

/// The Flux apps app-manager knows, built-in and sideloaded alike. list_apps iterates a
/// HashMap, so sort for a stable tile order (and stable tap coordinates in the smoke
/// tests). Name first keeps Ethereum before Solana.
fn fetch_flux_entries(app_manager_api: &AppManagerApi) -> Vec<app_manager::InstalledAppInfo> {
    let mut entries = app_manager_api.list_apps(tr::get_locale(), app_manager::AppFilter::flux_only());
    entries.sort_by(|a, b| (&a.name, &a.app_id).cmp(&(&b.name, &b.app_id)));
    entries
}

/// The 64-byte BIP39 seed for the device AppSeed, or `None` if it is unavailable.
///
/// The AppSeed is treated as BIP39 entropy, matching how `resolve_mnemonic` turns it into
/// the words and SeedQR shown under View Seed. Handing the raw AppSeed to BIP32 instead
/// would derive a wallet that those words cannot reproduce, making the displayed backup
/// useless.
fn derived_bip39_seed(security_api: &Security) -> Option<Vec<u8>> {
    // `AppSeed` scrubs itself on drop, so this key material doesn't linger on the emulator
    // stack after the derivation returns.
    let app_seed = match security_api.app_seed() {
        Ok(seed) => seed,
        Err(e) => {
            log::error!("Failed to retrieve AppSeed from security API: {e:?}");
            return None;
        }
    };
    match bip39::Mnemonic::from_entropy(app_seed.as_bytes()) {
        Ok(mnemonic) => Some(mnemonic.to_seed("").to_vec()),
        Err(e) => {
            log::error!("Failed to build a mnemonic from the AppSeed: {e:?}");
            None
        }
    }
}

/// Apply a saved seed config: retrieve or decode the seed bytes and push them
/// into the global APP_SEED so key derivation works immediately. Returns whether
/// the seed was installed; false means the config is present but unusable (the
/// security API failed or the stored entropy is corrupt).
fn apply_seed_config(config: &SeedConfig, security_api: &Security) -> bool {
    match config {
        SeedConfig::DerivedFromAppSeed => match derived_bip39_seed(security_api) {
            Some(seed) => {
                crate::flux::set_app_seed(seed);
                true
            }
            None => false,
        },
        SeedConfig::ManuallyEntered { entropy_hex } => match hex::decode(entropy_hex) {
            Ok(entropy) => match bip39::Mnemonic::from_entropy(&entropy) {
                Ok(mnemonic) => {
                    crate::flux::set_app_seed(mnemonic.to_seed("").to_vec());
                    true
                }
                Err(e) => {
                    log::error!("Failed to reconstruct mnemonic from stored entropy: {e:?}");
                    false
                }
            },
            Err(e) => {
                log::error!("Failed to decode stored entropy hex: {e:?}");
                false
            }
        },
    }
}

fn display_frame_to_image(frame: crate::flux::display::Frame) -> slint::Image {
    let mut pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
        crate::flux::display::DISPLAY_WIDTH as u32,
        crate::flux::display::DISPLAY_HEIGHT as u32,
    );
    for (dst, pixel) in pixel_buf.make_mut_slice().iter_mut().zip(frame) {
        let r = ((pixel >> 16) & 0xFF) as u8;
        let g = ((pixel >> 8) & 0xFF) as u8;
        let b = (pixel & 0xFF) as u8;
        *dst = slint::Rgba8Pixel::new(r, g, b, 0xFF);
    }
    slint::Image::from_rgba8(pixel_buf)
}

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    // Wait for encrypted user filesystem (mounted after PIN unlock)
    cx.fs.wait_for_filesystem(fs::Location::AppData);

    let _conn = server::listen_and_connect(FluxServer::default(), xous::current_pid().unwrap());

    // Client into the legacy-hid server for outbound APDUs and the Legacy USB
    // identity toggle. Held by the Slint event loop; FluxServer keeps its
    // own copy for the `Rapdu` write path.
    #[cfg(keyos)]
    let legacy_hid = Rc::new(LegacyHidApi::default());

    // One authoritative place for the installable Flux apps and the running children. The
    // Legacy USB identity is switched directly from the emulator window's Visible/Hidden
    // events below; the inbound-APDU pump reads `running` as its "is one of ours on screen?"
    // gate.
    let app_state = Rc::new(RefCell::new(AppState { possible: Vec::new(), running: HashMap::new() }));

    // Pump inbound HID APDUs from legacy-hid into the in-process SEPH_FIFO,
    // mirroring the behaviour of the old in-process out_thread. We also stash
    // the channel id so the `Rapdu` handler can frame outgoing replies on the
    // same HID channel.
    #[cfg(keyos)]
    {
        let legacy_hid_for_apdu = legacy_hid.clone();
        let app_state_for_apdu = app_state.clone();
        spawn_local(async move {
            let mut sub = subscribe_archive::<legacy_hid_permissions::LegacyHidPermissions, _>(
                legacy_hid::messages::SubscribeIncomingApdu,
            );
            while let Some(event) = sub.next().await {
                let legacy_hid::messages::IncomingApdu { channel_id, data } = event;
                // App-identification probes are answered here; app commands are
                // forwarded to the child via the SEPH FIFO.
                let child_running = !app_state_for_apdu.borrow().running.is_empty();
                if let Some(reply) = crate::flux::push_incoming_apdu(channel_id, &data, child_running) {
                    if let Err(e) = legacy_hid_for_apdu.write_apdu(channel_id, reply) {
                        log::error!("Failed to write app-identification reply: {e:?}");
                    }
                }
            }
        })
        .detach();
    }

    // Push the current dark mode state immediately so background threads see the
    // correct value from the very first refresh() call.
    crate::flux::display::set_dark_mode(ui.global::<CurrentTheme>().get_is_dark());

    let weak = ui.as_weak();
    crate::flux::display::init(move |frame| {
        let weak = weak.clone();
        spawn(async move {
            if let Some(ui) = weak.upgrade() {
                ui.global::<Global>().set_display_image(display_frame_to_image(frame));
                ui.global::<Global>().set_app_running(true);
            }
        })
        .detach();
    });

    let weak_for_reset = ui.as_weak();
    crate::flux::display::on_reset(move || {
        let weak = weak_for_reset.clone();
        spawn(async move {
            if let Some(ui) = weak.upgrade() {
                ui.global::<Global>().set_display_image(slint::Image::default());
                ui.global::<Global>().set_app_running(false);
            }
        })
        .detach();
    });

    ui.global::<Callbacks>().on_display_touched(|x, y, pressed| {
        log::trace!("on_display_touched: x={x}, y={y}, pressed={pressed}");
        let state = if pressed { 1u8 } else { 0u8 };
        crate::flux::display::push_touch_event(x as i16, y as i16, state);
    });

    let haptics_api = Rc::new(HapticsApi::default());
    let app_manager_api = Rc::new(AppManagerApi::default());
    let security_api = Rc::new(Security::default());

    #[cfg(keyos)]
    {
        let legacy_hid_for_input = legacy_hid.clone();
        cx.set_input_handler(move |input| match input.msg {
            InputMessage::Visible => {
                log::debug!("Legacy Mode: input handler got Visible");
                legacy_hid_for_input.set_legacy_mode(true);
            }
            InputMessage::Hidden => {
                log::debug!("Legacy Mode: input handler got Hidden");
                // Just drop the Legacy USB identity. The process and its Flux
                // children keep running; if memory gets tight the OOM killer
                // reaps this (least-recently-used) emulator, and the children
                // then self-exit when its server vanishes.
                legacy_hid_for_input.set_legacy_mode(false);
            }
            _ => {}
        });
    }

    let (settings, restored) = JsonBacked::<FluxSettings, fs_permissions::FileSystemPermissions>::new(
        "settings.json",
        fs::Location::AppData,
    );
    log::info!(
        "Flux settings restored={restored}, disabled={:?}, seed_configured={}",
        settings.disabled_apps,
        settings.seed_config.is_some()
    );

    // Apply any previously saved seed config so key derivation is ready immediately.
    // A present-but-unusable config (security API failure or corrupt entropy) counts
    // as not loaded, so the user is kept on setup rather than launching children that
    // cannot derive keys.
    let seed_loaded = match &settings.seed_config {
        Some(config) => apply_seed_config(config, &*security_api),
        None => false,
    };

    let settings = Rc::new(RefCell::new(settings));

    // Reflect seed presence and kind in the UI global so pages can gate on them.
    let seed_is_imported = matches!(&settings.borrow().seed_config, Some(SeedConfig::ManuallyEntered { .. }));
    ui.global::<Global>().set_seed_configured(seed_loaded);
    ui.global::<Global>().set_seed_is_imported(seed_is_imported);

    // If the seed loaded, skip the intro page.
    if seed_loaded {
        ui.global::<Navigate>().invoke_main(NavigateOptions::default());
    }

    app_state.borrow_mut().possible = fetch_flux_entries(&app_manager_api);

    fn rebuild_models(ui: &AppWindow, entries: &[app_manager::InstalledAppInfo], disabled: &HashSet<String>) {
        let all = VecModel::default();
        let enabled = VecModel::default();
        for e in entries {
            // An app whose signer isn't trusted can't launch (app-manager reports
            // can_launch=false). Don't list it at all: a launcher tile would fail
            // silently on tap, and a settings toggle for it wouldn't take effect.
            if !e.can_launch {
                continue;
            }
            let is_on = !disabled.contains(&e.app_id);
            all.push(SettingsEntryValue {
                name: e.name.clone().into(),
                app_id: e.app_id.clone().into(),
                enabled: is_on,
            });
            if is_on {
                enabled.push(AppEntryValue { name: e.name.clone().into(), app_id: e.app_id.clone().into() });
            }
        }
        ui.global::<Global>().set_all_apps(ModelRc::new(all));
        ui.global::<Global>().set_apps(ModelRc::new(enabled));
    }

    rebuild_models(&ui, &app_state.borrow().possible, &settings.borrow().disabled_apps);

    #[cfg(keyos)]
    {
        let app_state = app_state.clone();
        let weak_ui_for_events = ui.as_weak();
        let app_manager_for_events = app_manager_api.clone();
        let settings_for_events = settings.clone();
        spawn_local(async move {
            let mut sub = subscribe_archive::<app_manager_permissions::AppManagerPermissions, _>(
                app_manager::messages::SubscribeAppEvents,
            );
            while let Some(event) = sub.next().await {
                match event {
                    app_manager::AppEvent::AppCrashed { pid, exit_code, .. } => {
                        let removed = app_state.borrow_mut().running.remove(&pid).is_some();
                        if removed {
                            // Clear the framebuffer so the emulator menu replaces
                            // the child's last frame; FluxServer's Disconnected
                            // hook isn't reliable on every exit path.
                            crate::flux::display::reset();
                            // The dead child won't drain its queued APDUs; drop them so the
                            // next child doesn't inherit its stale input.
                            crate::flux::clear_fifos();
                            // Surface a non-zero exit in the crash panel rather than
                            // silently dropping the user back to the launcher.
                            if exit_code != 0 {
                                if let Some(ui) = weak_ui_for_events.upgrade() {
                                    let g = ui.global::<Global>();
                                    g.set_crashed_pid(pid.get() as i32);
                                    g.set_exit_code(exit_code as i32);
                                }
                            }
                        }
                    }
                    // A sideload or removal changed the installed set. That can happen
                    // while this emulator is open: usb-debug stays reachable on the
                    // Legacy USB identity. Refresh the launcher grid to match.
                    app_manager::AppEvent::AppSetChanged { .. } => {
                        app_state.borrow_mut().possible = fetch_flux_entries(&app_manager_for_events);
                        if let Some(ui) = weak_ui_for_events.upgrade() {
                            rebuild_models(
                                &ui,
                                &app_state.borrow().possible,
                                &settings_for_events.borrow().disabled_apps,
                            );
                        }
                    }
                    _ => {}
                }
            }
        })
        .detach();
    }

    ui.global::<Callbacks>().on_back_from_running(|| {
        crate::flux::display::reset();
    });

    ui.global::<Callbacks>()
        .on_validate_seed_word(|word| bip39::Language::English.word_list().contains(&word.as_str()));

    ui.global::<Callbacks>().on_validate_full_seed(|words| {
        // The UI only ever submits a 12- or 24-length array, and parse_normalized rejects
        // any array still holding empty words, so this accepts a complete 12- or 24-word seed.
        let mnemonic_str = words.iter().map(|w| w.to_string()).collect::<Vec<_>>().join(" ");
        bip39::Mnemonic::parse_normalized(&mnemonic_str).is_ok()
    });

    // Seed-import flow: parse the entered mnemonic (12 or 24 words), derive the 64-byte BIP39
    // seed, persist it as ManuallyEntered, and update the in-memory APP_SEED.
    let weak_ui = ui.as_weak();
    ui.global::<Callbacks>().on_confirm_seed_import({
        let settings = settings.clone();
        let weak_ui = weak_ui.clone();
        move |words| -> bool {
            let mnemonic_str = words.iter().map(|w| w.to_string()).collect::<Vec<_>>().join(" ");
            let mnemonic = match bip39::Mnemonic::parse_normalized(&mnemonic_str) {
                Ok(mnemonic) => mnemonic,
                Err(e) => {
                    log::error!("Failed to parse mnemonic: {e:?}");
                    return false;
                }
            };
            // Store the entropy for word display and quiz; use the 64-byte BIP39
            // PBKDF2 seed in APP_SEED so key derivation is standards-compatible.
            let entropy_hex = hex::encode(mnemonic.to_entropy());
            {
                let mut s = settings.borrow_mut();
                s.guard().seed_config = Some(SeedConfig::ManuallyEntered { entropy_hex });
            }
            log::info!("Manual seed saved");
            // Persisted: now activate the seed in memory and reflect it in the UI.
            crate::flux::set_app_seed(mnemonic.to_seed("").to_vec());
            if let Some(ui) = weak_ui.upgrade() {
                ui.global::<Global>().set_seed_configured(true);
                ui.global::<Global>().set_seed_is_imported(true);
            }
            true
        }
    });

    // "Create new Seed" button: navigate to the seed-derive confirmation page.
    ui.global::<Callbacks>().on_derive_new_seed({
        let ui = ui.clone_strong();
        move || {
            ui.global::<Navigate>().invoke_seed_derive(NavigateOptions::default());
            log::info!("Navigating to seed-derive confirmation");
        }
    });

    // Seed-derive confirmation: fetch AppSeed from the security API, persist it,
    // and update the in-memory APP_SEED. The UI navigates to main afterward.
    ui.global::<Callbacks>().on_confirm_seed_derive({
        let settings = settings.clone();
        let security_api = security_api.clone();
        let weak_ui = weak_ui.clone();
        move || -> bool {
            // Leave seed_config unset if the seed is unavailable, so the intro page keeps
            // offering setup rather than stranding the user on an unusable launcher.
            let Some(seed) = derived_bip39_seed(&security_api) else {
                return false;
            };
            {
                let mut s = settings.borrow_mut();
                s.guard().seed_config = Some(SeedConfig::DerivedFromAppSeed);
            }
            log::info!("AppSeed derived and saved");
            crate::flux::set_app_seed(seed);
            if let Some(ui) = weak_ui.upgrade() {
                ui.global::<Global>().set_seed_configured(true);
                ui.global::<Global>().set_seed_is_imported(false);
            }
            true
        }
    });

    // Helper: resolve the current seed config to a bip39::Mnemonic.
    fn resolve_mnemonic(settings: &FluxSettings, security_api: &Security) -> Option<bip39::Mnemonic> {
        match &settings.seed_config {
            Some(SeedConfig::DerivedFromAppSeed) => {
                let seed = security_api.app_seed().ok()?;
                bip39::Mnemonic::from_entropy(seed.as_bytes()).ok()
            }
            Some(SeedConfig::ManuallyEntered { entropy_hex }) => {
                let entropy = hex::decode(entropy_hex).ok()?;
                bip39::Mnemonic::from_entropy(&entropy).ok()
            }
            None => None,
        }
    }

    // Returns the mnemonic words (12 or 24) for the currently configured seed.
    ui.global::<Callbacks>().on_get_seed_words({
        let settings = settings.clone();
        let security_api = security_api.clone();
        move || {
            let s = settings.borrow();
            let model = slint::VecModel::default();
            if let Some(mnemonic) = resolve_mnemonic(&s, &*security_api) {
                for w in mnemonic.words() {
                    model.push(slint::SharedString::from(w));
                }
            }
            slint::ModelRc::new(model)
        }
    });

    // Standard SeedQR: concatenated 4-digit BIP39 word indices.
    ui.global::<Callbacks>().on_get_standard_seed_qr({
        let settings = settings.clone();
        let security_api = security_api.clone();
        move || {
            let s = settings.borrow();
            let Some(mnemonic) = resolve_mnemonic(&s, &*security_api) else {
                return slint::Image::default();
            };
            let indices: String = mnemonic.word_indices().map(|idx| format!("{:04}", idx)).collect();
            slint_keyos_platform::qrcode::render(
                indices.as_bytes(),
                slint::Color::from_rgb_u8(0, 0, 0),
                slint::Color::from_rgb_u8(255, 255, 255),
            )
        }
    });

    // Compact SeedQR: raw entropy bytes.
    ui.global::<Callbacks>().on_get_compact_seed_qr({
        let settings = settings.clone();
        let security_api = security_api.clone();
        move || {
            let s = settings.borrow();
            let Some(mnemonic) = resolve_mnemonic(&s, &*security_api) else {
                return slint::Image::default();
            };
            slint_keyos_platform::qrcode::render(
                &mnemonic.to_entropy(),
                slint::Color::from_rgb_u8(0, 0, 0),
                slint::Color::from_rgb_u8(255, 255, 255),
            )
        }
    });

    // Generates one word-position challenge per seed word (12 or 24) for the deletion quiz.
    ui.global::<Callbacks>().on_generate_seed_word_challenges({
        let settings = settings.clone();
        let security_api = security_api.clone();
        move || {
            let model = slint::VecModel::default();
            let Some(mnemonic) = resolve_mnemonic(&settings.borrow(), &*security_api) else {
                return slint::ModelRc::new(model);
            };
            for challenge in seed_quiz::shuffled_challenges(&mnemonic, &mut rand::thread_rng()) {
                let options_model = slint::VecModel::default();
                for opt in &challenge.options {
                    options_model.push(slint::SharedString::from(opt.as_str()));
                }
                model.push(SeedWordChallenge {
                    word_index: challenge.word_index as i32,
                    options: slint::ModelRc::new(options_model),
                    correct_option_index: challenge.correct_option_index as i32,
                });
            }
            slint::ModelRc::new(model)
        }
    });

    // Re-rolls a single word's challenge after a wrong guess, so a retry offers fresh
    // decoys instead of the same four options.
    ui.global::<Callbacks>().on_regenerate_seed_word_challenge({
        let settings = settings.clone();
        let security_api = security_api.clone();
        move |word_index| {
            // Fail closed: -1 matches no on-screen option (0..NUM_OPTIONS), so an empty challenge
            // can never be tapped "correct" to satisfy the seed-deletion gate.
            let empty = || SeedWordChallenge {
                word_index,
                options: slint::ModelRc::new(slint::VecModel::default()),
                correct_option_index: -1,
            };
            let Some(mnemonic) = resolve_mnemonic(&settings.borrow(), &*security_api) else {
                log::error!("no mnemonic available to regenerate a seed word challenge");
                return empty();
            };
            let Some(challenge) =
                seed_quiz::word_challenge(&mnemonic, word_index as usize, &mut rand::thread_rng())
            else {
                log::error!("word index {word_index} out of range for regenerated challenge");
                return empty();
            };
            let options_model = slint::VecModel::default();
            for opt in &challenge.options {
                options_model.push(slint::SharedString::from(opt.as_str()));
            }
            SeedWordChallenge {
                word_index: challenge.word_index as i32,
                options: slint::ModelRc::new(options_model),
                correct_option_index: challenge.correct_option_index as i32,
            }
        }
    });

    // Clears the seed config from persistent storage; the caller navigates only
    // on a reported success.
    ui.global::<Callbacks>().on_remove_seed({
        let settings = settings.clone();
        let weak_ui = weak_ui.clone();
        move || -> bool {
            {
                let mut s = settings.borrow_mut();
                s.guard().seed_config = None;
            }
            log::info!("Seed removed");
            // Removal is persisted: drop the in-memory seed so the deleted seed
            // can't keep signing this session.
            crate::flux::clear_app_seed();
            if let Some(ui) = weak_ui.upgrade() {
                ui.global::<Global>().set_seed_configured(false);
            }
            true
        }
    });

    ui.global::<Callbacks>().on_verify_pin(|title| {
        verify_pin::<gui_permissions::GuiPermissions>(VerifyPinOptions {
            title: Some(title.into()),
            want_security_words: false,
        })
        .map(|r| r.success)
        .unwrap_or_else(|e| {
            log::error!("verify_pin failed: {e}");
            false
        })
    });

    let weak_theme_changed = ui.as_weak();
    ui.global::<Callbacks>().on_theme_changed(move || {
        if let Some(ui) = weak_theme_changed.upgrade() {
            let is_dark = ui.global::<CurrentTheme>().get_is_dark();
            crate::flux::display::set_dark_mode(is_dark);
            if ui.global::<Global>().get_app_running() {
                crate::flux::display::refresh();
            }
        }
    });

    let weak_toggle = ui.as_weak();
    let settings_toggle = settings.clone();
    let app_state_toggle = app_state.clone();
    ui.global::<Callbacks>().on_app_toggled(move |app_id, enabled| {
        let mut s = settings_toggle.borrow_mut();
        {
            let mut guard = s.guard();
            if enabled {
                guard.disabled_apps.remove(app_id.as_str());
            } else {
                guard.disabled_apps.insert(app_id.to_string());
            }
        }
        log::info!("Saved flux settings: disabled={:?}", s.disabled_apps);
        if let Some(ui) = weak_toggle.upgrade() {
            rebuild_models(&ui, &app_state_toggle.borrow().possible, &s.disabled_apps);
        }
    });

    let app_state_title = app_state.clone();
    ui.global::<Callbacks>().on_pid_to_title(move |pid| {
        if pid != 0 {
            if let Some(pid) = PID::new(pid as u8) {
                if let Some(app_name) = app_state_title.borrow().running.get(&pid) {
                    return app_name.into();
                }
            }
        }
        "<unknown>".into()
    });

    let haptics_api_clone = haptics_api.clone();
    let app_manager_api_clone = app_manager_api.clone();
    let app_state_clone = app_state.clone();
    ui.global::<Callbacks>().on_app_clicked(move |id| -> bool {
        haptics_api_clone.click();

        let Ok(app_id) = app_manager::decode_app_id_str(id.as_str()) else {
            log::error!("Invalid AppId hex: {id}");
            return false;
        };
        log::info!("App clicked with id={id}");

        // The Flux child is already running (e.g. the emulator reopened while the
        // child stayed alive); keep the emulator UI as its GUI and track it like a
        // fresh launch so the AppCrashed subscriber recognizes it as one of ours.
        if let Ok(Some(pid)) = xous::app_id_to_pid(&app_id) {
            if let Some(name) = app_manager_api_clone.app_name_by_pid(pid, tr::get_locale()) {
                app_state_clone.borrow_mut().running.insert(pid, name);
            }
            return true;
        }

        // Launch the app via app-manager
        match app_manager_api_clone.launch_app_blocking(&app_id) {
            Ok(pid) => {
                // Track the new Flux child (PID -> name) so on_pid_to_title can
                // label it and the AppCrashed subscriber recognizes it as ours.
                if let Some(name) = app_manager_api_clone.app_name_by_pid(pid, tr::get_locale()) {
                    app_state_clone.borrow_mut().running.insert(pid, name);
                }
                true
            }
            Err(e) => {
                log::error!("Error launching app: {e:?}");
                false
            }
        }
    });

    ui.run().expect("UI running");

    #[cfg(keyos)]
    legacy_hid.set_legacy_mode(false);
}
