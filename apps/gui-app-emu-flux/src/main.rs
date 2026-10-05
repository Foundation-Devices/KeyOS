// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod flux;

use std::{cell::RefCell, collections::HashMap, collections::HashSet, rc::Rc};

use bitcoin::{bip32::Xpriv, secp256k1::Secp256k1, Network};
use slint_keyos_platform::{
    app, async_archive,
    file_backed::JsonBacked,
    gui_server_api::{
        msg::NavigateTo,
        navigation::{lockscreen::VerifyPinOptions, SETTINGS_APP_ID},
    },
    navigation::verify_pin,
    slint::{ComponentHandle as _, Model as _, ModelRc, VecModel},
    spawn,
};
#[cfg(keyos)]
use slint_keyos_platform::{gui_server_api::InputMessage, spawn_local, subscribe_archive};
use xous::PID;

use crate::flux::{FluxConn, FluxServer, LaunchChild, Touch};

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
    /// Stored inverted so the default (warning shown) needs no `Default` impl.
    #[serde(default)]
    passphrase_warning_dismissed: bool,
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

/// The mnemonic behind `config`, or `None` when the security API fails or the stored
/// entropy is corrupt.
///
/// The AppSeed is treated as BIP39 entropy, matching the words and SeedQR shown under View
/// Seed. Handing the raw AppSeed to BIP32 instead would derive a wallet that those words
/// cannot reproduce, making the displayed backup useless.
fn mnemonic_for(config: &SeedConfig, security_api: &Security) -> Option<bip39::Mnemonic> {
    match config {
        // `AppSeed` scrubs itself on drop, so this key material doesn't linger on the
        // emulator stack after the derivation returns.
        SeedConfig::DerivedFromAppSeed => bip39::Mnemonic::from_entropy(
            security_api
                .app_seed()
                .inspect_err(|e| log::error!("Failed to retrieve the AppSeed: {e:?}"))
                .ok()?
                .as_bytes(),
        ),
        SeedConfig::ManuallyEntered { entropy_hex } => bip39::Mnemonic::from_entropy(
            &hex::decode(entropy_hex).inspect_err(|e| log::error!("Corrupt stored entropy: {e:?}")).ok()?,
        ),
    }
    .inspect_err(|e| log::error!("Failed to build a mnemonic from the seed entropy: {e:?}"))
    .ok()
}

fn resolve_mnemonic(settings: &FluxSettings, security_api: &Security) -> Option<bip39::Mnemonic> {
    mnemonic_for(settings.seed_config.as_ref()?, security_api)
}

/// Install the 64-byte BIP39 seed for `config` under `passphrase` into APP_SEED so key
/// derivation is ready. Returns whether the seed was installed; false means the config is
/// present but unusable.
fn apply_seed(config: &SeedConfig, security_api: &Security, passphrase: &str) -> bool {
    let Some(mnemonic) = mnemonic_for(config, security_api) else {
        return false;
    };
    crate::flux::set_app_seed(mnemonic.to_seed(passphrase).to_vec());
    true
}

/// Uppercase BIP32 master fingerprint for the configured seed under `passphrase`, shown on the
/// passphrase details screen so a typo is visible before it is applied. Empty when there is no
/// usable seed.
fn passphrase_fingerprint(settings: &FluxSettings, security_api: &Security, passphrase: &str) -> String {
    resolve_mnemonic(settings, security_api)
        .and_then(|mnemonic| {
            Xpriv::new_master(Network::Bitcoin, &mnemonic.to_seed(passphrase))
                .inspect_err(|e| log::error!("Failed to derive the passphrase fingerprint: {e:?}"))
                .ok()
        })
        .map(|xpriv| xpriv.fingerprint(&Secp256k1::signing_only()).to_string().to_uppercase())
        .unwrap_or_default()
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

    let flux_conn: FluxConn =
        server::listen_and_connect(FluxServer::default(), xous::current_pid().unwrap()).into();

    // Client into the legacy-hid server for the Legacy USB identity toggle; FluxServer keeps
    // its own for host commands and responses.
    #[cfg(keyos)]
    let legacy_hid = Rc::new(LegacyHidApi::default());

    // One authoritative place for the installable Flux apps and the running children. The
    // Legacy USB identity is switched directly from the emulator window's Visible/Hidden
    // events below.
    let app_state = Rc::new(RefCell::new(AppState { possible: Vec::new(), running: HashMap::new() }));

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

    let flux_conn_touch = flux_conn.clone();
    ui.global::<Callbacks>().on_display_touched(move |x, y, pressed| {
        log::trace!("on_display_touched: x={x}, y={y}, pressed={pressed}");
        flux_conn_touch.send_scalar(Touch { x: x as i16, y: y as i16, pressed });
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
    let seed_loaded =
        settings.seed_config.as_ref().is_some_and(|config| apply_seed(config, &security_api, ""));

    let settings = Rc::new(RefCell::new(settings));

    // Reflect seed presence and kind in the UI global so pages can gate on them.
    let seed_is_imported = matches!(&settings.borrow().seed_config, Some(SeedConfig::ManuallyEntered { .. }));
    ui.global::<Global>().set_seed_configured(seed_loaded);
    ui.global::<Global>().set_seed_is_imported(seed_is_imported);
    ui.global::<Global>().set_show_passphrase_warning(!settings.borrow().passphrase_warning_dismissed);

    // If the seed loaded, skip the intro page.
    if seed_loaded {
        ui.global::<Navigate>().invoke_main(NavigateOptions::default());
    }

    app_state.borrow_mut().possible = fetch_flux_entries(&app_manager_api);

    fn rebuild_models(ui: &AppWindow, entries: &[app_manager::InstalledAppInfo], disabled: &HashSet<String>) {
        let all = VecModel::default();
        let enabled = VecModel::default();
        for e in entries {
            // An app whose signer isn't allowed can't launch. Don't list it at all: a launcher tile
            // would fail silently on tap, and a settings toggle for it wouldn't take effect.
            if e.launch_error.is_some() {
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

    seed_quiz::init_seed_callbacks!(ui);

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
            if !apply_seed(&SeedConfig::DerivedFromAppSeed, &security_api, "") {
                return false;
            }
            {
                let mut s = settings.borrow_mut();
                s.guard().seed_config = Some(SeedConfig::DerivedFromAppSeed);
            }
            log::info!("AppSeed derived and saved");
            if let Some(ui) = weak_ui.upgrade() {
                ui.global::<Global>().set_seed_configured(true);
                ui.global::<Global>().set_seed_is_imported(false);
            }
            true
        }
    });

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
            for challenge in seed_quiz::shuffled_challenges(&mnemonic) {
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
            let Some(challenge) = seed_quiz::word_challenge(&mnemonic, word_index as usize) else {
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

    // Re-derives APP_SEED for the configured seed under `passphrase`; an empty string restores
    // the bare seed. The passphrase is never persisted, so closing Legacy Mode drops it.
    ui.global::<Callbacks>().on_apply_passphrase({
        let settings = settings.clone();
        let security_api = security_api.clone();
        let weak_ui = weak_ui.clone();
        move |passphrase| -> bool {
            let applied = settings
                .borrow()
                .seed_config
                .as_ref()
                .is_some_and(|config| apply_seed(config, &security_api, &passphrase));
            if !applied {
                return false;
            }
            log::info!("Passphrase {}", if passphrase.is_empty() { "cleared" } else { "applied" });
            if let Some(ui) = weak_ui.upgrade() {
                ui.global::<Global>().set_passphrase_active(!passphrase.is_empty());
            }
            true
        }
    });

    ui.global::<Callbacks>().on_passphrase_fingerprint({
        let settings = settings.clone();
        let security_api = security_api.clone();
        move |passphrase| passphrase_fingerprint(&settings.borrow(), &security_api, &passphrase).into()
    });

    ui.global::<Callbacks>().on_set_show_passphrase_warning({
        let settings = settings.clone();
        let weak_ui = weak_ui.clone();
        move |show| {
            settings.borrow_mut().guard().passphrase_warning_dismissed = !show;
            if let Some(ui) = weak_ui.upgrade() {
                ui.global::<Global>().set_show_passphrase_warning(show);
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
                ui.global::<Global>().set_passphrase_active(false);
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

    // Async so the UI thread stays free to handle Hidden while Settings holds the request open.
    ui.global::<Callbacks>().on_go_to_settings(|| {
        spawn(async_archive::<gui_permissions::GuiPermissions, _>(NavigateTo {
            app_id: SETTINGS_APP_ID,
            args: b"/settings/apps".to_vec(),
        }))
        .detach();
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
    let flux_conn_clone = flux_conn.clone();
    ui.global::<Callbacks>().on_app_clicked(move |id| -> bool {
        haptics_api_clone.click();

        let Ok(app_id) = app_manager::decode_app_id_str(id.as_str()) else {
            log::error!("Invalid AppId hex: {id}");
            return false;
        };
        log::info!("App clicked with id={id}");

        // Through FluxServer, so nothing the child sends is handled before its PID is cleared.
        match flux_conn_clone.send_blocking_scalar(LaunchChild(app_id)) {
            Ok(pid) => {
                // Track the child (PID -> name) so on_pid_to_title can label it and the
                // AppCrashed subscriber recognizes it as ours.
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
