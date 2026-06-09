// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod flux;

use std::{cell::RefCell, collections::HashMap, collections::HashSet, rc::Rc};

use slint_keyos_platform::{
    app,
    file_backed::JsonBacked,
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
    DerivedFromAppSeed,
    /// A manually entered 24-word mnemonic.
    /// Stored as the 32-byte BIP39 entropy, hex-encoded.
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

/// Tracks whether the device should currently advertise the Legacy Mode USB
/// identity. The desired state is `is_emu_visible || !running_children.is_empty()`
/// — i.e. the emulator UI is on screen, OR a Flux child app it launched is
/// still alive.
#[cfg(keyos)]
struct LegacyState {
    is_emu_visible: bool,
    running_children: HashSet<PID>,
    is_legacy_active: bool,
}

/// Recompute the desired Legacy Mode state and apply it synchronously via
/// the `legacy-hid` server. Synchronous because once the emulator app
/// becomes Hidden, the slint event loop blocks waiting for input and won't
/// drive `spawn_local` futures (`should_block` short-circuits to `true`
/// when `!visible`); a debounced timer-based apply would never fire until
/// the next user input.
#[cfg(keyos)]
fn recompute_legacy(state: &Rc<RefCell<LegacyState>>, legacy_hid: &LegacyHidApi) {
    let (desired, current, visible, n_children) = {
        let s = state.borrow();
        (
            s.is_emu_visible || !s.running_children.is_empty(),
            s.is_legacy_active,
            s.is_emu_visible,
            s.running_children.len(),
        )
    };
    log::debug!(
        "Legacy Mode: recompute visible={visible} children={n_children} desired={desired} current={current}"
    );
    if desired == current {
        return;
    }
    legacy_hid.set_legacy_mode(desired);
    state.borrow_mut().is_legacy_active = desired;
}

#[cfg(keyos)]
fn kill_running_children(state: &Rc<RefCell<LegacyState>>, reason: &str) {
    // Force-kill any child Flux apps before we go away. They would otherwise be
    // orphaned, talking to a FluxServer that's gone, with no framebuffer or touch source.
    let children: Vec<PID> = state.borrow().running_children.iter().copied().collect();
    for pid in children {
        log::info!("Legacy Mode: force-killing child PID {pid} on {reason}");
        if let Err(e) = xous::terminate_pid(pid, 0) {
            log::warn!("terminate_pid({pid}) failed: {e:?}");
        }
    }
    state.borrow_mut().running_children.clear();
}

app!("Flux Emulator", kind = App);

/// Apply a saved seed config: retrieve or decode the seed bytes and push them
/// into the global APP_SEED so key derivation works immediately.
fn apply_seed_config(config: &SeedConfig, security_api: &Security) {
    match config {
        SeedConfig::DerivedFromAppSeed => match security_api.app_seed() {
            Ok(seed) => crate::flux::set_app_seed(seed.to_vec()),
            Err(e) => log::error!("Failed to retrieve AppSeed from security API: {e:?}"),
        },
        SeedConfig::ManuallyEntered { entropy_hex } => match hex::decode(entropy_hex) {
            Ok(entropy) => match bip39::Mnemonic::from_entropy(&entropy) {
                Ok(mnemonic) => crate::flux::set_app_seed(mnemonic.to_seed("").to_vec()),
                Err(e) => log::error!("Failed to reconstruct mnemonic from stored entropy: {e:?}"),
            },
            Err(e) => log::error!("Failed to decode stored entropy hex: {e:?}"),
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

    // Pump inbound HID APDUs from legacy-hid into the in-process SEPH_FIFO,
    // mirroring the behaviour of the old in-process out_thread. We also stash
    // the channel id so the `Rapdu` handler can frame outgoing replies on the
    // same HID channel.
    #[cfg(keyos)]
    {
        spawn_local(async move {
            let mut sub = subscribe_archive::<legacy_hid_permissions::LegacyHidPermissions, _>(
                legacy_hid::messages::SubscribeIncomingApdu,
            );
            while let Some(event) = sub.next().await {
                let legacy_hid::messages::IncomingApdu { channel_id, data } = event;
                crate::flux::push_incoming_apdu(channel_id, &data);
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

    // Legacy Mode visibility tracker. Drives the USB VID:PID switch from
    // Visible/Hidden events plus child-app lifecycle, so the device only
    // advertises the Legacy identity while the user is actually inside the
    // emulator (or one of its Flux children).
    #[cfg(keyos)]
    let legacy_state = Rc::new(RefCell::new(LegacyState {
        is_emu_visible: false,
        running_children: HashSet::new(),
        is_legacy_active: false,
    }));

    #[cfg(keyos)]
    {
        let legacy_state = legacy_state.clone();
        let legacy_hid_for_input = legacy_hid.clone();
        cx.set_input_handler(move |input| match input.msg {
            InputMessage::Visible => {
                log::debug!("Legacy Mode: input handler got Visible");
                legacy_state.borrow_mut().is_emu_visible = true;
                recompute_legacy(&legacy_state, &legacy_hid_for_input);
            }
            InputMessage::Hidden => {
                log::debug!("Legacy Mode: input handler got Hidden");
                kill_running_children(&legacy_state, "dismiss");
                legacy_state.borrow_mut().is_emu_visible = false;
                recompute_legacy(&legacy_state, &legacy_hid_for_input);
                // Exit the process so we free the (heavy) Slint runtime.
                // Next launch goes through gui-server NavigateTo or the
                // launcher tile.
                log::info!("Legacy Mode: dismissing — terminating gui-app-emu-flux");
                xous::terminate_process(0);
            }
            _ => {}
        });
    }

    #[cfg(keyos)]
    {
        let legacy_state = legacy_state.clone();
        let legacy_hid_for_events = legacy_hid.clone();
        spawn_local(async move {
            let mut sub = subscribe_archive::<app_manager_permissions::AppManagerPermissions, _>(
                app_manager::messages::SubscribeAppEvents,
            );
            while let Some(event) = sub.next().await {
                if let app_manager::AppEvent::AppCrashed { pid, .. } = event {
                    let removed = legacy_state.borrow_mut().running_children.remove(&pid);
                    if removed {
                        // Clear the framebuffer so the emulator menu replaces
                        // the child's last frame; FluxServer's Disconnected
                        // hook isn't reliable on every exit path.
                        crate::flux::display::reset();
                        recompute_legacy(&legacy_state, &legacy_hid_for_events);
                    }
                }
            }
        })
        .detach();
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
    if let Some(config) = &settings.seed_config {
        apply_seed_config(config, &*security_api);
    }

    let settings = Rc::new(RefCell::new(settings));

    // Reflect seed presence in the UI global so pages can gate on it.
    ui.global::<Global>().set_seed_configured(settings.borrow().seed_config.is_some());

    // If a seed is already configured, skip the intro page.
    if settings.borrow().seed_config.is_some() {
        ui.global::<Navigate>().invoke_main(NavigateOptions::default());
    }

    let flux_entries: Vec<_> = app_manager_api.list_flux_apps("en");

    fn rebuild_models(ui: &AppWindow, entries: &[app_manager::AppEntry], disabled: &HashSet<String>) {
        let all = VecModel::default();
        let enabled = VecModel::default();
        for e in entries {
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

    rebuild_models(&ui, &flux_entries, &settings.borrow().disabled_apps);

    ui.global::<Callbacks>().on_back_from_running(|| {
        crate::flux::display::reset();
    });

    ui.global::<Callbacks>()
        .on_validate_seed_word(|word| bip39::Language::English.word_list().contains(&word.as_str()));

    ui.global::<Callbacks>().on_validate_full_seed(|words| {
        let is_24_words = words.iter().count() == 24;
        let mnemonic_str = words.iter().map(|w| w.to_string()).collect::<Vec<_>>().join(" ");
        is_24_words && bip39::Mnemonic::parse_normalized(&mnemonic_str).is_ok()
    });

    // "Enter 24 words" flow: parse the mnemonic, derive the 64-byte BIP39 seed,
    // persist it as ManuallyEntered, and update the in-memory APP_SEED.
    let weak_ui = ui.as_weak();
    ui.global::<Callbacks>().on_confirm_seed_import({
        let settings = settings.clone();
        let weak_ui = weak_ui.clone();
        move |words| {
            let mnemonic_str = words.iter().map(|w| w.to_string()).collect::<Vec<_>>().join(" ");
            match bip39::Mnemonic::parse_normalized(&mnemonic_str) {
                Ok(mnemonic) => {
                    // Store the entropy for word display and quiz; use the 64-byte BIP39
                    // PBKDF2 seed in APP_SEED so key derivation is standards-compatible.
                    let entropy_hex = hex::encode(mnemonic.to_entropy());
                    crate::flux::set_app_seed(mnemonic.to_seed("").to_vec());
                    let mut s = settings.borrow_mut();
                    s.set_auto_save(false);
                    {
                        let mut guard = s.guard();
                        guard.seed_config = Some(SeedConfig::ManuallyEntered { entropy_hex });
                    }
                    s.set_auto_save(true);
                    if let Err(e) = s.try_save() {
                        log::error!("Failed to save seed config: {e:?}");
                    } else {
                        log::info!("Manual seed saved");
                        if let Some(ui) = weak_ui.upgrade() {
                            ui.global::<Global>().set_seed_configured(true);
                        }
                    }
                }
                Err(e) => {
                    log::error!("Failed to parse mnemonic: {e:?}");
                }
            }
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
        move || match security_api.app_seed() {
            Ok(seed) => {
                crate::flux::set_app_seed(seed.to_vec());
                let mut s = settings.borrow_mut();
                s.set_auto_save(false);
                {
                    let mut guard = s.guard();
                    guard.seed_config = Some(SeedConfig::DerivedFromAppSeed);
                }
                s.set_auto_save(true);
                if let Err(e) = s.try_save() {
                    log::error!("Failed to save seed config: {e:?}");
                } else {
                    log::info!("AppSeed derived and saved");
                    if let Some(ui) = weak_ui.upgrade() {
                        ui.global::<Global>().set_seed_configured(true);
                    }
                }
            }
            Err(e) => {
                log::error!("Failed to get app seed from security API: {e:?}");
            }
        }
    });

    // Helper: resolve the current seed config to a bip39::Mnemonic.
    fn resolve_mnemonic(settings: &FluxSettings, security_api: &Security) -> Option<bip39::Mnemonic> {
        match &settings.seed_config {
            Some(SeedConfig::DerivedFromAppSeed) => {
                let seed = security_api.app_seed().ok()?;
                bip39::Mnemonic::from_entropy(&seed).ok()
            }
            Some(SeedConfig::ManuallyEntered { entropy_hex }) => {
                let entropy = hex::decode(entropy_hex).ok()?;
                bip39::Mnemonic::from_entropy(&entropy).ok()
            }
            None => None,
        }
    }

    // Returns the 24 mnemonic words for the currently configured seed.
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

    // Generates word-position challenges for the seed-deletion quiz.
    ui.global::<Callbacks>().on_generate_seed_word_challenges({
        let settings = settings.clone();
        let security_api = security_api.clone();
        move |count| {
            let model = slint::VecModel::default();
            let Some(mnemonic) = resolve_mnemonic(&settings.borrow(), &*security_api) else {
                return slint::ModelRc::new(model);
            };
            let entropy = mnemonic.to_entropy();
            let words: Vec<&str> = mnemonic.words().collect();
            let word_list = bip39::Language::English.word_list();
            let num = (count as usize).min(words.len());

            // Deterministically shuffle word positions using a hash of the entropy.
            let mut rng_state: u64 = {
                let mut h: u64 = 0xcbf29ce484222325u64; // FNV offset basis
                for (i, b) in entropy.iter().enumerate() {
                    h ^= i as u64;
                    h = h.wrapping_mul(0x100000001b3);
                    h ^= *b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                // Mix in some OS randomness so challenges vary across quiz attempts.
                let mut rand_bytes = [0u8; 8];
                let _ = getrandom::getrandom(&mut rand_bytes);
                h ^ u64::from_le_bytes(rand_bytes)
            };
            let xorshift = |s: &mut u64| -> u64 {
                *s ^= *s << 13;
                *s ^= *s >> 7;
                *s ^= *s << 17;
                *s
            };

            // Partial Fisher–Yates to pick `num` distinct positions from 0..24.
            let mut indices: Vec<usize> = (0..words.len()).collect();
            for i in 0..num {
                let j = i + (xorshift(&mut rng_state) as usize % (words.len() - i));
                indices.swap(i, j);
            }

            for &word_index in &indices[..num] {
                let correct_word = words[word_index];

                // Pick 3 decoys distinct from the correct word and each other.
                let mut options: Vec<String> = vec![correct_word.to_string()];
                while options.len() < 4 {
                    let idx = xorshift(&mut rng_state) as usize % word_list.len();
                    let candidate = word_list[idx];
                    if !options.iter().any(|o| o.as_str() == candidate) {
                        options.push(candidate.to_string());
                    }
                }

                // Shuffle options.
                for i in (1..options.len()).rev() {
                    let j = xorshift(&mut rng_state) as usize % (i + 1);
                    options.swap(i, j);
                }

                let correct_option_index =
                    options.iter().position(|o| o.as_str() == correct_word).unwrap_or(0);

                let options_model = slint::VecModel::default();
                for opt in &options {
                    options_model.push(slint::SharedString::from(opt.as_str()));
                }
                model.push(SeedWordChallenge {
                    word_index: word_index as i32,
                    options: slint::ModelRc::new(options_model),
                    correct_option_index: correct_option_index as i32,
                });
            }
            slint::ModelRc::new(model)
        }
    });

    // Clears the seed config from persistent storage and navigates to intro.
    ui.global::<Callbacks>().on_remove_seed({
        let settings = settings.clone();
        let weak_ui = weak_ui.clone();
        move || {
            let mut s = settings.borrow_mut();
            s.set_auto_save(false);
            {
                let mut guard = s.guard();
                guard.seed_config = None;
            }
            s.set_auto_save(true);
            if let Err(e) = s.try_save() {
                log::error!("Failed to remove seed config: {e:?}");
            } else {
                log::info!("Seed removed");
            }
            if let Some(ui) = weak_ui.upgrade() {
                ui.global::<Global>().set_seed_configured(false);
                ui.global::<Navigate>().invoke_intro(NavigateOptions { replace: true, ..Default::default() });
            }
        }
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
    let entries_toggle = flux_entries.clone();
    ui.global::<Callbacks>().on_app_toggled(move |app_id, enabled| {
        let mut s = settings_toggle.borrow_mut();
        s.set_auto_save(false);
        {
            let mut guard = s.guard();
            if enabled {
                guard.disabled_apps.remove(app_id.as_str());
            } else {
                guard.disabled_apps.insert(app_id.to_string());
            }
        }
        s.set_auto_save(true);
        if let Err(e) = s.try_save() {
            log::error!("Failed to save flux settings: {e:?}");
        } else {
            log::info!("Saved flux settings: disabled={:?}", s.disabled_apps);
        }
        if let Some(ui) = weak_toggle.upgrade() {
            rebuild_models(&ui, &entries_toggle, &s.disabled_apps);
        }
    });

    let app_names = Rc::new(RefCell::new(HashMap::<PID, String>::new()));
    let app_names_clone = app_names.clone();
    ui.global::<Callbacks>().on_pid_to_title(move |pid| {
        if pid != 0 {
            if let Some(pid) = PID::new(pid as u8) {
                if let Some(app_name) = app_names_clone.borrow().get(&pid) {
                    return app_name.into();
                }
            }
        }
        "<unknown>".into()
    });

    let haptics_api_clone = haptics_api.clone();
    let app_manager_api_clone = app_manager_api.clone();
    let app_names_clone = app_names.clone();
    #[cfg(keyos)]
    let legacy_state_clone = legacy_state.clone();
    #[cfg(keyos)]
    let legacy_hid_for_click = legacy_hid.clone();
    ui.global::<Callbacks>().on_app_clicked(move |id| {
        haptics_api_clone.click();

        let Ok(app_id) = app_manager::decode_app_id_str(id.as_str()) else {
            log::error!("Invalid AppId hex: {id}");
            return;
        };
        log::info!("App clicked with id={id}");

        // The Flux child is already running; keep the emulator UI as its GUI.
        if let Ok(Some(_pid)) = xous::app_id_to_pid(&app_id) {
            return;
        }

        // Launch the app via app-manager
        match app_manager_api_clone.launch_app_blocking(&app_id) {
            Ok(pid) => {
                // Store the app name for later reference
                if let Some(name) = app_manager_api_clone.app_name_by_pid(pid, "en") {
                    app_names_clone.borrow_mut().insert(pid, name);
                }
                // Track the new Flux child so Legacy Mode stays active while
                // it runs, even when the emulator itself becomes Hidden.
                // Insert the PID before the Hidden event arrives (this closure
                // runs on the slint event loop, and the queued Hidden event
                // can't be dispatched until we return), so the recompute that
                // fires on Hidden sees a non-empty children set and stays in
                // Legacy. The AppEvents subscriber removes the PID on exit.
                #[cfg(keyos)]
                {
                    legacy_state_clone.borrow_mut().running_children.insert(pid);
                    recompute_legacy(&legacy_state_clone, &legacy_hid_for_click);
                }
            }
            Err(e) => {
                log::error!("Error launching app: {e:?}");
            }
        }
    });

    ui.run().expect("UI running");

    #[cfg(keyos)]
    {
        kill_running_children(&legacy_state, "graceful shutdown");
        legacy_state.borrow_mut().is_emu_visible = false;
        recompute_legacy(&legacy_state, &legacy_hid);
    }
}
