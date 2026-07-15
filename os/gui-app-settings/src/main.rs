// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::RefCell,
    io::{Read, Seek},
    num::NonZeroUsize,
    rc::Rc,
    time::{Duration, SystemTime},
};

use anyhow::{self, Context};
use bip39::Mnemonic;
use ngwallet::{bdk_wallet::bitcoin::Network, bip39::MasterKey};
use quantum_link::{
    foundation_api::firmware::{FirmwareInstallEvent, InstallErrorStage},
    messages::{NotifyFirmwareInstall, SendPrimeMagicBackupEnabled, StartFirmwareUpdate},
    PairingEvent,
};
use security::{messages::Lockout, OsVersionInfo, PinEntryMode};
use slint_keyos_platform::{
    app, async_archive,
    futures_lite::StreamExt as _,
    gui_server_api::{
        msg::UpdateKioskPolicy,
        navigation::{
            filepicker::{AllowedExtensions, AllowedLocations, Location, SelectFileOptions},
            lockscreen::{VerifyPinOptions, VerifyPinResult},
        },
        InputMessage,
    },
    navigation::select_file,
    navigation::verify_pin,
    settings::{
        self,
        global::{BoardRevision, SystemTheme},
    },
    slint::{Image, ModelRc, SharedString, Timer, TimerMode, VecModel},
    spawn_local, spawn_worker, subscribe_archive, subscribe_scalar, timeout, StoredValue, TaskHandle,
};
use update::messages::ProgressUpdate;

use crate::{
    backup_permissions::BackupPermissions, gui_permissions::GuiPermissions,
    quantum_link_permissions::QuantumLinkPermissions, security_permissions::SecurityPermissions,
    settings_permissions::SettingsPermissions, state::AppState,
};

mod keycard_backup;
mod keycard_verify;
mod state;
mod timezones;

app_manager::use_api!();
backup::use_api!();
bt::use_api!();
haptics::use_api!();
keycard::use_api!();
power_manager::use_ext_api!();
quantum_link::use_api!();
security::use_api!();
update::use_api!();

const PERIODIC_UPDATE_INTERVAL: Duration = Duration::from_millis(1000);

/// Maximum number of decoded app icons kept resident at once. Icons are fetched
/// one per app over IPC, so an unbounded set would grow with the install count.
const APP_ICON_CACHE_CAP: usize = 32;

app!("Settings", role = ClaimSettingsRole);

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let state = StoredValue::new(AppState::new(cx.gui.clone(), ui.as_weak(), cx.config.clone()));

    ql_utils::on_ble_address(state.borrow().bt.clone(), move |addr| {
        log::info!("got bt address: {addr:?}");
        state.borrow_mut().ble_address = addr;
    });

    // cancel outstanding tasks, if any
    cx.router.borrow_mut().register_on_navigation_end(move |_| {
        spawn_local(async move {
            state.borrow_mut().cancel_tasks();
        })
        .detach();
    });

    setup_settings_global(state);
    setup_app_management_global(state);
    setup_datetime_globals(state);
    setup_about_global(state);
    setup_pin_global(state);
    setup_log_global(state);
    setup_backup_global(state);
    setup_keycard_backup_global(state);
    setup_ql_global(state);
    setup_update_global(state);
    setup_callbacks(state);
    setup_save_settings_global(state);
    resume_update_if_needed(state);
    setup_navigation_input(&cx, state);

    let timer = Timer::default();
    timer.start(TimerMode::Repeated, PERIODIC_UPDATE_INTERVAL, move || {
        let state = state.borrow();
        state.refresh_time();
        state.refresh_battery_stats();
        state.refresh_backup_stats();
    });

    ui.run().expect("UI running");
}

fn setup_navigation_input(cx: &AppContext, state: StoredValue<AppState>) {
    cx.set_input_handler({
        let gui = cx.gui.clone();
        move |input| {
            if input.msg != InputMessage::NavigationFocused {
                return;
            }

            let Ok(Some(nav_bytes)) = gui.navigate_pending() else {
                log::error!("Navigation focused but no pending nav request");
                return;
            };

            let Ok(route) = std::str::from_utf8(&nav_bytes) else {
                log::error!("Settings navigation request was not a UTF-8 route");
                return;
            };

            if let Some(app_id) = app_details_route_app_id(route) {
                select_installed_app(state, app_id);
            }

            let ui = state.borrow().ui();
            ui.global::<Navigate>()
                .invoke_navigate(route.into(), NavigateOptions { replace: true, animate: Animate::None });
        }
    });
}

fn app_details_route_app_id(route: &str) -> Option<&str> {
    let query = route.strip_prefix("/settings/apps/details?")?;
    query.split('&').find_map(|param| param.strip_prefix("app_id="))
}

fn setup_settings_global(state: StoredValue<AppState>) {
    spawn_local({
        let state = state.clone();
        async move {
            let mut sub = subscribe_scalar::<settings_permissions::SettingsPermissions, _>(
                settings::messages::SubscribeScreenBrightness,
            );
            while let Some(brightness) = sub.next().await {
                let state = state.borrow();
                let ui = state.ui();
                ui.global::<SettingGlobal>().set_screen_brightness(brightness.0 as f32);
            }
        }
    })
    .detach();

    spawn_local({
        let state = state.clone();
        async move {
            let mut sub = subscribe_archive::<settings_permissions::SettingsPermissions, _>(
                settings::messages::SubscribeDeviceName,
            );
            while let Some(device_name) = sub.next().await {
                let state = state.borrow();
                let ui = state.ui();
                ui.global::<SettingGlobal>().set_device_name(device_name.0.into());
            }
        }
    })
    .detach();

    spawn_local({
        let state = state.clone();
        async move {
            let mut sub = subscribe_scalar::<settings_permissions::SettingsPermissions, _>(
                settings::messages::SubscribeDeveloperMode,
            );
            while let Some(developer_mode) = sub.next().await {
                let state = state.borrow();
                let ui = state.ui();
                ui.global::<SettingGlobal>().set_developer_mode(developer_mode.0);
            }
        }
    })
    .detach();

    let ui = state.borrow().ui();
    let globals = ui.global::<SettingGlobal>();

    globals.on_set_dark_mode(move |dark_mode| {
        let theme = if dark_mode { SystemTheme::Dark } else { SystemTheme::Light };
        log::info!("Setting theme: {:?}", theme);
        state.borrow().settings.set_system_theme(theme);
    });

    globals.set_device_name(state.borrow().settings.get_device_name().0.into());
    globals.on_set_device_name(move |device_name| {
        let state = state.borrow();
        state.settings.set_device_name(device_name.as_str());
        let ui = state.ui();
        ui.global::<SettingGlobal>().set_device_name(device_name);
    });

    globals.on_set_screen_brightness(move |brightness| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<SettingGlobal>().set_screen_brightness(brightness);
        let brightness = brightness as u8;
        state.settings.set_screen_brightness(brightness);
    });

    globals.set_auto_lock(state.borrow().settings.get_auto_lock().0.as_secs() as i32);

    globals.on_set_auto_lock(move |auto_lock| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<SettingGlobal>().set_auto_lock(auto_lock);
        let auto_lock = Duration::from_secs(auto_lock as u64);
        state.settings.set_auto_lock(auto_lock);
    });
    globals.on_format_auto_lock(move |seconds| {
        // TODO: translation
        if seconds == -1 {
            return "Never".into();
        }
        let minutes = seconds / 60;

        if minutes > 59 {
            let hours = minutes / 60;
            let hours_str: SharedString = hours.to_string().into();
            if hours == 1 {
                return format!("{hours_str} {}", tr::lookup_id(TrId::CommonTimeHourFull)).into();
            }
            return format!("{hours_str} {}", tr::lookup_id(TrId::CommonTimeHoursFull)).into();
        }
        let minutes_str: SharedString = minutes.to_string().into();
        if minutes == 1 {
            return format!("{minutes_str} {}", tr::lookup_id(TrId::CommonTimeMinuteFull)).into();
        }
        return format!("{minutes_str} {}", tr::lookup_id(TrId::CommonTimeMinutesFull)).into();
    });

    globals.set_show_security_words(state.borrow().settings.get_show_security_words().0);
    globals.on_set_show_security_words(move |show_security_words| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<SettingGlobal>().set_show_security_words(show_security_words);
        state.settings.set_show_security_words(show_security_words);
    });

    globals.on_set_developer_mode(move |developer_mode| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<SettingGlobal>().set_developer_mode(developer_mode);
        state.settings.set_developer_mode(developer_mode);
    });

    globals.on_factory_reset(move || {
        spawn_local(async move {
            let ui = state.borrow().ui();
            let nav = ui.global::<Navigate>();

            nav.invoke_erase_device(
                EraseDeviceParams { status: EraseStatus::Progress },
                NavigateOptions { animate: Animate::None, replace: true },
            );

            // Best-effort goodbye to Envoy before wiping. Awaits BLE flush so
            // the bye is on the wire before erase_system_state() / Lockout reboots.
            if let Err(e) =
                async_archive::<QuantumLinkPermissions, _>(quantum_link::messages::UnpairFromEnvoy).await
            {
                log::warn!("failed to notify Envoy of unpair before factory reset: {e:?}");
            }
            erase_system_state();

            match async_archive::<SecurityPermissions, _>(Lockout {
                lockout_options: security::LockoutOptions::erase_all(),
                reboot: true,
            })
            .await
            {
                Ok(_) => {
                    // We should never get to this branch because lockout will reboot
                    log::info!("successfully reset device");
                }
                Err(_) => {
                    log::error!("failed to factory reset");
                }
            }
        })
        .detach();
    });

    let version = state.borrow().security.os_version_info().map_or_else(
        |_| "unknown".to_string(),
        |opt| {
            opt.map(|info| String::from_utf8_lossy(&info.keyos_version).to_string())
                .unwrap_or_else(|| "N/A".to_string())
        },
    );
    globals.set_current_keyos_version(SharedString::from(version));
}

fn setup_app_management_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let globals = ui.global::<AppManagementGlobal>();

    // Icon bytes are fetched on demand by app id and decoded here; the LRU caps
    // how many decoded icons stay resident so the listing never carries them.
    let icon_cache: Rc<RefCell<lru::LruCache<SharedString, Image>>> = Rc::new(RefCell::new(
        lru::LruCache::new(NonZeroUsize::new(APP_ICON_CACHE_CAP).expect("cache cap is non-zero")),
    ));
    globals.on_app_icon(move |app_id| {
        if let Some(image) = icon_cache.borrow_mut().get(&app_id).cloned() {
            return image;
        }
        let bytes = state.borrow().app_manager.get_app_icon(app_id.as_str()).unwrap_or_default();
        let image = slint_keyos_platform::raw_image::raw_image_from_bytes(&bytes);
        icon_cache.borrow_mut().put(app_id, image.clone());
        image
    });

    refresh_installed_apps(state);
    globals.on_refresh_installed_apps(move || {
        refresh_installed_apps(state);
    });
    globals.on_select_installed_app(move |app_id| select_installed_app(state, app_id.as_str()));
    globals.on_set_app_permission_subgroup_grant(move |app_id, subgroup, approved| {
        set_app_permission_subgroup_grant(state, app_id.as_str(), subgroup.as_str(), approved)
    });
    globals.on_launch_installed_app(move |app_id| {
        let requested_app_id = app_id.to_string();
        let Ok(app_id) = app_manager::decode_app_id_str(&requested_app_id) else {
            log::error!("invalid app id for manual launch: {requested_app_id}");
            return;
        };

        let state = state.borrow();
        match state.app_manager.launch_app_blocking(&app_id) {
            Ok(pid) => {
                log::info!("launched app {requested_app_id}: {pid:?}");
                if let Err(e) = state.gui.switch_to(pid, 0, 0) {
                    log::error!("failed to switch to launched app {requested_app_id}: {e:?}");
                }
            }
            Err(e) => log::error!("failed to launch app {requested_app_id}: {e:?}"),
        }
    });

    refresh_trusted_publishers(state);
    globals.on_refresh_trusted_publishers(move || {
        refresh_trusted_publishers(state);
    });
    globals.on_import_trusted_publisher(move || import_trusted_publisher(state));
    globals
        .on_remove_trusted_publisher(move |public_key| remove_trusted_publisher(state, public_key.as_str()));
    globals.on_remove_installed_app(move |app_id| remove_installed_app(state, app_id.as_str()));
}

fn refresh_installed_apps(state: StoredValue<AppState>) {
    // Use the device's active locale so localized manifest.appName values (and
    // size formatting below) are honored instead of always asking for English.
    let locale = state.borrow().settings.get_locale();
    let lang = locale.lang();
    let apps = state.borrow().app_manager.list_apps(lang, app_manager::AppFilter::third_party_only());

    let installed_apps = apps.iter().map(|app| installed_app(app.clone(), lang)).collect::<Vec<_>>();
    // Cache the full list so the details page and permission toggles read from it rather than
    // re-requesting each app.
    *state.borrow().installed_apps.borrow_mut() = apps;

    // Flatten the apps + the fixed settings section into one row model so the
    // page can render them in a single virtualized ListView (Slint can't
    // concatenate the dynamic apps model with static rows). `action_id`
    // disambiguates rows of the same kind; the delegate maps it to localized
    // text and behavior. `first`/`last`/`show_divider` drive the card chrome.
    const ACTION_TRUSTED_PUBLISHERS: i32 = 0;
    const TOGGLE_DEVELOPER_MODE: i32 = 1;
    const HEADER_INSTALLED_APPS: i32 = 0;
    const HEADER_SETTINGS: i32 = 1;

    let mut rows: Vec<AppsListRow> = Vec::with_capacity(installed_apps.len() + 4);

    rows.push(AppsListRow {
        kind: AppsListRowKind::SectionHeader,
        action_id: HEADER_INSTALLED_APPS,
        ..Default::default()
    });

    if installed_apps.is_empty() {
        rows.push(AppsListRow {
            kind: AppsListRowKind::Empty,
            first: true,
            last: true,
            ..Default::default()
        });
    } else {
        let last_index = installed_apps.len() - 1;
        for (index, app) in installed_apps.into_iter().enumerate() {
            rows.push(AppsListRow {
                kind: AppsListRowKind::App,
                app,
                first: index == 0,
                last: index == last_index,
                show_divider: index != last_index,
                ..Default::default()
            });
        }
    }

    rows.push(AppsListRow {
        kind: AppsListRowKind::SectionHeader,
        action_id: HEADER_SETTINGS,
        ..Default::default()
    });
    rows.push(AppsListRow {
        kind: AppsListRowKind::Action,
        action_id: ACTION_TRUSTED_PUBLISHERS,
        first: true,
        show_divider: true,
        ..Default::default()
    });
    rows.push(AppsListRow {
        kind: AppsListRowKind::Toggle,
        action_id: TOGGLE_DEVELOPER_MODE,
        last: true,
        ..Default::default()
    });

    let ui = state.borrow().ui();
    ui.global::<AppManagementGlobal>().set_apps_list_rows(ModelRc::new(VecModel::from(rows)));
}

fn select_installed_app(state: StoredValue<AppState>, app_id: &str) -> bool {
    let locale = state.borrow().settings.get_locale();
    let lang = locale.lang();
    let app = state.borrow().installed_apps.borrow().iter().find(|a| a.app_id == app_id).cloned();
    let Some(app) = app else {
        log::warn!("could not select installed app {app_id}");
        return false;
    };

    state.borrow().ui().global::<AppManagementGlobal>().set_selected_app(installed_app(app, lang));
    true
}

/// A revoked grant only affects future permission checks the kernel routes through the
/// broker: a running app keeps the connection permissions it was already granted. Close the
/// app gracefully so the revocation takes effect now. gui-server refuses to close essential
/// apps, and grants only exist for sideloaded apps anyway; an app without a window keeps its
/// permissions until it exits.
fn close_running_app_after_revoke(state: StoredValue<AppState>, app_id: &str) {
    let Ok(app_id) = app_manager::decode_app_id_str(app_id) else {
        return;
    };
    let Ok(Some(pid)) = server::xous::app_id_to_pid(&app_id) else {
        return;
    };
    if let Err(error) = state.borrow().gui.close_app(pid) {
        log::warn!("failed to close app 0x{app_id} (pid {pid}) after a permission revoke: {error:?}");
    }
}

/// The Settings permission toggles are a persistent Allow/Deny choice; the transient
/// "Not Now" decision only comes from the permission prompt, never from this UI.
fn grant_decision(approved: bool) -> app_manager::PermissionGrantDecision {
    if approved {
        app_manager::PermissionGrantDecision::Allow
    } else {
        app_manager::PermissionGrantDecision::Deny
    }
}

fn set_app_permission_subgroup_grant(
    state: StoredValue<AppState>,
    app_id: &str,
    subgroup: &str,
    approved: bool,
) -> bool {
    let result =
        state.borrow().app_manager.set_app_permission_grant(app_id, subgroup, grant_decision(approved));
    if result != app_manager::SetAppPermissionGrantResult::Updated {
        log::error!("failed to set permission grant for app={app_id} subgroup={subgroup}: {result:?}");
        return false;
    }

    if !approved {
        close_running_app_after_revoke(state, app_id);
    }
    // Refresh the cache before re-selecting: select_installed_app reads it, so it must hold the
    // post-toggle grant state or the details switch lags one action.
    refresh_installed_apps(state);
    select_installed_app(state, app_id);
    true
}

fn installed_app(app: app_manager::InstalledAppInfo, lang: &str) -> InstalledApp {
    InstalledApp {
        app_id: app.app_id.into(),
        name: app.name.into(),
        publisher: app.publisher.into(),
        can_launch: app.can_launch,
        can_remove: app.can_remove,
        version: app.version.into(),
        size: format_app_size(app.size_bytes, lang).into(),
        description: app.description.into(),
        basic_permissions: app_permission_groups(app.basic_permissions),
        approvable_permissions: app_permission_groups(app.approvable_permissions),
    }
}

fn app_permission_groups(
    groups: Vec<app_manager::InstalledAppPermissionGroup>,
) -> ModelRc<AppPermissionGroup> {
    let groups = groups
        .into_iter()
        .map(|group| AppPermissionGroup {
            key: group.key.into(),
            label: group.label.into(),
            subgroups: ModelRc::new(VecModel::from(
                group
                    .subgroups
                    .into_iter()
                    .map(|subgroup| AppPermissionSubgroup {
                        key: subgroup.key.into(),
                        label: subgroup.label.into(),
                        approved: subgroup.approved,
                    })
                    .collect::<Vec<_>>(),
            )),
        })
        .collect::<Vec<_>>();

    ModelRc::from(Rc::new(VecModel::from(groups)))
}

fn refresh_trusted_publishers(state: StoredValue<AppState>) {
    let trusted_publishers = state
        .borrow()
        .app_manager
        .get_third_party_certificates()
        .into_iter()
        .map(|cert| TrustedPublisher {
            name: cert.name.into(),
            company: cert.company.into(),
            contact_email: cert.contact_email.into(),
            support_url: cert.support_url.into(),
            public_key: format_hex_groups(&cert.public_key, 4, 8).into(),
            serial_number: cert.serial_number.replace(':', ": ").into(),
            subject: cert.subject.replace(',', ",\n").into(),
            basic_constraints: cert.basic_constraints.into(),
            key_usage: cert.key_usage.into(),
            extended_key_usage: cert.extended_key_usage.into(),
        })
        .collect::<Vec<_>>();

    let ui = state.borrow().ui();
    ui.global::<AppManagementGlobal>()
        .set_trusted_publishers(ModelRc::new(VecModel::from(trusted_publishers)));
}

fn format_hex_groups(value: &str, group_len: usize, groups_per_line: usize) -> String {
    let mut formatted = String::with_capacity(value.len() + (value.len() / group_len));
    for (index, ch) in value.chars().enumerate() {
        if index > 0 && index % (group_len * groups_per_line) == 0 {
            formatted.push('\n');
        } else if index > 0 && index % group_len == 0 {
            formatted.push(' ');
        }
        formatted.push(ch);
    }
    formatted
}

fn import_trusted_publisher(state: StoredValue<AppState>) -> TrustedPublisherImportResult {
    let certificate = {
        let state = state.borrow();
        read_third_party_certificate(&state)
    };

    match certificate {
        Ok(Some(certificate_pem)) => {
            let result = { state.borrow().app_manager.import_third_party_certificate(certificate_pem) };
            match result {
                Ok(app_manager::ImportThirdPartyCertificateResult::Imported(_)) => {
                    refresh_trusted_publishers(state);
                    TrustedPublisherImportResult::Installed
                }
                Ok(app_manager::ImportThirdPartyCertificateResult::Invalid) => {
                    TrustedPublisherImportResult::Failed
                }
                Err(e) => {
                    log::error!("failed to import third-party certificate: {e:?}");
                    TrustedPublisherImportResult::Failed
                }
            }
        }
        Ok(None) => TrustedPublisherImportResult::Canceled,
        Err(e) => {
            log::error!("failed to read third-party certificate: {e:?}");
            TrustedPublisherImportResult::Failed
        }
    }
}

const MAX_CERTIFICATE_BYTES: u64 = 4 * 1024;

fn read_third_party_certificate(state: &AppState) -> anyhow::Result<Option<Vec<u8>>> {
    let options = SelectFileOptions::default()
        .with_start_location(Location::External)
        .with_allowed_locations(AllowedLocations::specific([Location::External, Location::Airlock]))
        .with_allowed_extensions(AllowedExtensions::specific(["crt"]))
        .with_hidden_allowed(false)
        .with_dirs_allowed(true)
        .with_multiple_selection_mode(false);

    let Some((path, location)) = select_file::<GuiPermissions>(options)
        .context("Failed to select third-party certificate")?
        .and_then(|selected| selected.files().get(0).cloned())
    else {
        return Ok(None);
    };

    let location = match location {
        Location::Internal => fs::Location::User,
        Location::External => fs::Location::Usb,
        Location::Airlock => fs::Location::Airlock,
    };

    let file =
        state.fs.open_file(path, location, fs::OpenFlags { read: true, write: false, create: false })?;
    let mut certificate_pem = Vec::new();
    // Certs are only a few KB. Read one byte past the cap so an oversized or
    // malformed file is rejected rather than exhausting the app's heap.
    file.take(MAX_CERTIFICATE_BYTES + 1).read_to_end(&mut certificate_pem)?;
    anyhow::ensure!(
        certificate_pem.len() as u64 <= MAX_CERTIFICATE_BYTES,
        "third-party certificate exceeds {MAX_CERTIFICATE_BYTES} bytes",
    );
    Ok(Some(certificate_pem))
}

fn remove_trusted_publisher(state: StoredValue<AppState>, public_key: &str) -> SharedString {
    let locale = state.borrow().settings.get_locale();
    let result = { state.borrow().app_manager.remove_third_party_certificate(public_key, locale.lang()) };
    match result {
        Ok(app_manager::RemoveThirdPartyCertificateResult::Removed)
        | Ok(app_manager::RemoveThirdPartyCertificateResult::NotFound) => {
            refresh_trusted_publishers(state);
            SharedString::default()
        }
        Ok(app_manager::RemoveThirdPartyCertificateResult::AppRequiresKey(app_name)) => {
            i18n::replace_placeholders(
                tr::lookup_id(TrId::AppsTrustedPublisherRemoveBlocked),
                &[app_name.as_str()],
            )
            .into()
        }
        Ok(app_manager::RemoveThirdPartyCertificateResult::InternalError) => {
            tr::lookup_id(TrId::AppsTrustedPublisherRemoveFailed).into()
        }
        Err(e) => {
            log::error!("failed to remove third-party certificate: {e:?}");
            tr::lookup_id(TrId::AppsTrustedPublisherRemoveFailed).into()
        }
    }
}

fn remove_installed_app(state: StoredValue<AppState>, app_id: &str) -> SharedString {
    let app_id = match app_manager::decode_app_id_str(app_id) {
        Ok(app_id) => app_id,
        Err(e) => {
            log::error!("invalid installed app id for removal: {app_id}: {e:?}");
            return tr::lookup_id(TrId::AppsRemoveAppFailed).into();
        }
    };

    let result = { state.borrow().app_manager.remove_installed_app(&app_id) };
    match result {
        Ok(app_manager::RemoveInstalledAppResult::Removed)
        | Ok(app_manager::RemoveInstalledAppResult::NotFound) => {
            refresh_installed_apps(state);
            state.borrow().ui().global::<AppManagementGlobal>().set_selected_app(InstalledApp::default());
            SharedString::default()
        }
        Ok(app_manager::RemoveInstalledAppResult::Running) => {
            tr::lookup_id(TrId::AppsRemoveAppRunning).into()
        }
        Ok(app_manager::RemoveInstalledAppResult::NotSideloaded)
        | Ok(app_manager::RemoveInstalledAppResult::InternalError) => {
            tr::lookup_id(TrId::AppsRemoveAppFailed).into()
        }
        Err(e) => {
            log::error!("failed to remove installed app {app_id}: {e:?}");
            tr::lookup_id(TrId::AppsRemoveAppFailed).into()
        }
    }
}

fn format_app_size(size_bytes: u64, locale: &str) -> String {
    if size_bytes == 0 {
        String::new()
    } else {
        i18n::format_file_size(size_bytes, locale)
    }
}

fn setup_datetime_globals(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let dt_globals = ui.global::<DateTimeGlobal>();

    dt_globals.set_time_24(state.borrow().settings.get_use_standard_time_format().0);
    dt_globals.on_set_time_24(move |time_24| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<DateTimeGlobal>().set_time_24(time_24);
        state.settings.set_use_standard_time_format(time_24);
    });

    dt_globals.set_envoy_time_sync(state.borrow().settings.get_envoy_time_sync().0);
    dt_globals.on_set_envoy_time_sync(move |envoy_time_sync| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<DateTimeGlobal>().set_envoy_time_sync(envoy_time_sync);
        state.settings.set_envoy_time_sync(envoy_time_sync);
        if envoy_time_sync {
            ql_utils::sync_system_timezone(state.settings.clone(), state.ql_status.clone(), |e| {
                log::warn!("failed to retrieve tz from envoy {e:?}")
            })
            .detach();
        }
    });

    dt_globals.set_timezone_search_list(ModelRc::new(state.borrow().timezone.clone()));

    dt_globals.on_timezone_search_text_edited(move |search_text| {
        state.borrow().timezone.set_search(&search_text);
    });

    dt_globals.on_datetime_changed(move |y: i32, m: i32, d: i32, hh: i32, mm: i32, ss: i32| {
        state.borrow().update_system_time(|current| {
            current
                .with()
                .year(y as _)
                .month(m as _)
                .day(d as _)
                .hour(hh as _)
                .minute(mm as _)
                .second(ss as _)
                .build()
                .ok()
        });
    });
    {
        let state = state.borrow();
        let tz = state.settings.get_time_zone();
        state.update_slint_timezone(tz);
    }

    dt_globals.on_timezone_selected(move |timezone| {
        let state = state.borrow();
        let timezone = String::from(timezone.id);
        let tz = state.settings.lookup_timezone(timezone, 0);
        state.settings.set_time_zone(tz.clone());
        state.update_slint_timezone(tz);
    });
}

fn setup_pin_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let pin_global = ui.global::<PinGlobal>();

    pin_global.on_verify_pin(move |title, want_words| {
        let res = verify_pin::<GuiPermissions>(VerifyPinOptions {
            title: Some(title.into()),
            want_security_words: want_words,
        });

        if want_words {
            let ui = state.borrow().ui();
            let pin_global = ui.global::<PinGlobal>();

            match &res {
                Ok(VerifyPinResult { success: true, security_words: Some([w0, w1]), .. }) => {
                    pin_global.set_last_security_words(ModelRc::from([
                        SharedString::from(w0),
                        SharedString::from(w1),
                    ]));
                }
                _ => pin_global.set_last_security_words(Default::default()),
            }
        }

        res.map(|r| r.success).unwrap_or_else(|e| {
            log::error!("verify_pin failed: {e}");
            false
        })
    });

    pin_global.on_change_pin(move |new_pin, is_pin| {
        let state = state.borrow();
        let mode = if is_pin { PinEntryMode::Pin } else { PinEntryMode::Passphrase };
        state.ui().global::<PinGlobal>().set_is_pin_entry(is_pin);
        state.security.change_pin(new_pin.as_str().to_owned(), None, mode).is_ok()
    });

    let pin_entry_mode = state.borrow().security.get_pin_entry_mode();
    pin_global.set_is_pin_entry(pin_entry_mode == PinEntryMode::Pin);
}

fn setup_log_global(state: StoredValue<AppState>) {
    const MAX_LINE_LEN: usize = 56;

    let ui = state.borrow().ui();
    let log_global = ui.global::<LogGlobal>();
    let lines = Rc::new(VecModel::<SharedString>::default());
    let fs = FileSystem::default();
    let mut log_file_offset = 0;
    log_global.set_log_lines(ModelRc::from(lines.clone()));
    log_global.on_update_log_lines(move || {
        let mut file = match fs.open_file(
            ".log/log.0.log",
            fs::Location::User,
            fs::OpenFlags { read: true, write: false, create: false },
        ) {
            Ok(f) => f,
            Err(e) => {
                log::error!("Could not open log file: {e:?}");
                return;
            }
        };
        let size = file.metadata().unwrap().size;
        // Check if log file was rotated
        if size < log_file_offset {
            log_file_offset = 0;
        } else if log_file_offset == size {
            return;
        }
        file.seek(std::io::SeekFrom::Start(log_file_offset)).ok();
        let mut contents = vec![0u8; (size - log_file_offset) as usize];
        if let Err(e) = file.read_exact(&mut contents) {
            log::error!("Could not read log file: {e:?}");
            return;
        }
        // Manual layouting: first split into the actual found newlines, then
        // split into MAX_LINE_LEN chunks.
        // Add extra newlines after each actual log line for better readability.
        for line in contents.split(|&p| p == b'\n') {
            if line.is_empty() {
                continue;
            }
            let Ok(mut line) = str::from_utf8(line) else {
                log::error!("Log line was not utf-8");
                continue;
            };
            while !line.is_empty() {
                let (chunk, rest) = split_at_char(line, MAX_LINE_LEN);
                lines.push(chunk.into());
                line = rest;
            }
            lines.push("".into());
        }
        log_file_offset = size;
    });
}

fn split_at_char(s: &str, index: usize) -> (&str, &str) {
    let byte_index = s.char_indices().nth(index).map(|(i, _)| i).unwrap_or(s.len());

    s.split_at(byte_index)
}

fn setup_about_global(state: StoredValue<AppState>) {
    let mut state = state.borrow_mut();
    let ui = state.ui();
    let globals = ui.global::<AboutGlobal>();

    let board_revision = match state.settings.get_board_revision() {
        BoardRevision::D1 => "D1",
        BoardRevision::D6 => "D6",
    };
    globals.set_board_revision(board_revision.into());

    let Ok(version_info) = state.security.os_version_info() else {
        return;
    };

    match version_info {
        None => {
            globals.set_bootloader_version("N/A".into());
            globals.set_keyos_version("N/A".into());
        }

        Some(OsVersionInfo { bootloader_version, keyos_version }) => {
            let bootloader_version = String::from_utf8_lossy(&bootloader_version).to_string();
            let keyos_version = String::from_utf8_lossy(&keyos_version).to_string();
            globals.set_bootloader_version(bootloader_version.into());
            globals.set_keyos_version(keyos_version.into());
        }
    }
    let Some(version_info) = state.bt.get_version_info() else {
        return;
    };

    globals.set_ble_bootloader_version(version_info.bootloader_version.into());
    if let Some(firmware_version) = version_info.firmware_version {
        globals.set_ble_firmware_version(firmware_version.into());
    } else {
        globals.set_ble_firmware_version("N/A".into());
    }

    if let Ok(device_id) = &state.security.device_id() {
        globals.set_serial_number(device_id.to_string().into());
    } else {
        log::error!("Failed to get serial number");
        globals.set_serial_number("N/A".into());
    }

    if let Ok(key) = get_master_key(&state) {
        let fingerprint = key.fingerprint.to_string().to_uppercase();
        globals.set_master_fingerprint(fingerprint.clone().into());
        let reversed_fingerprint = fingerprint
            .as_bytes()
            .chunks(2)
            .rev()
            .map(|b| std::str::from_utf8(b).unwrap())
            .collect::<String>();
        globals.set_reversed_fingerprint(reversed_fingerprint.into());
    } else {
        log::error!("Failed to get fingerprint");
        globals.set_master_fingerprint("N/A".into());
        globals.set_reversed_fingerprint("N/A".into());
    }
}

fn get_master_key(app_state: &AppState) -> anyhow::Result<MasterKey> {
    let entropy = match app_state.security.seed() {
        Ok(Some(e)) => e,
        Ok(None) => anyhow::bail!("No seed found"),
        Err(e) => anyhow::bail!("Could not get seed: {:?}", e),
    };

    MasterKey::from_entropy(&app_state.secp, Network::Bitcoin, entropy.bytes(), "", None)
        .map_err(|e| anyhow::anyhow!("Could not derive seed: {}", e))
}

fn setup_callbacks(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let callbacks = ui.global::<Callbacks>();
    callbacks.on_save_log_files(move || match state.borrow().save_log_files() {
        Ok(_) => true,
        Err(e) => {
            log::error!("Failed to save log file: {}", e);
            false
        }
    });
    callbacks.on_close_settings(move || {
        if let Err(e) = state.borrow().gui.switch_to_launcher() {
            log::error!("Failed to switch to launcher: {}", e);
        }
    });

    callbacks.on_get_seed_words(move || {
        let app_state = state.borrow();
        let key = match get_master_key(&app_state) {
            Ok(k) => k,
            Err(e) => {
                log::error!("Failed to get master key: {}", e);
                return ModelRc::new(VecModel::from(vec![]));
            }
        };

        let words = key.mnemonic.split(' ').map(SharedString::from).collect::<Vec<SharedString>>();

        ModelRc::new(VecModel::from(words))
    });

    callbacks.on_get_standard_seed_qr(move || {
        let app_state = state.borrow();
        let key = match get_master_key(&app_state) {
            Ok(k) => k,
            Err(e) => {
                log::error!("Failed to get master key: {}", e);
                return Image::default();
            }
        };

        let mnemonic = match Mnemonic::parse(&key.mnemonic) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("Could not parse mnemonic: {:?}", e);
                return Image::default();
            }
        };

        let indices: String = mnemonic.word_indices().map(|idx| format!("{:04}", idx)).collect();
        slint_keyos_platform::qrcode::render(
            indices.as_bytes(),
            slint::Color::from_rgb_u8(0, 0, 0),
            slint::Color::from_rgb_u8(255, 255, 255),
        )
    });

    callbacks.on_get_compact_seed_qr(move || {
        let app_state = state.borrow();
        let key = match get_master_key(&app_state) {
            Ok(k) => k,
            Err(e) => {
                log::error!("Failed to get master key: {}", e);
                return Image::default();
            }
        };

        let mnemonic = match Mnemonic::parse(&key.mnemonic) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("Could not parse mnemonic: {:?}", e);
                return Image::default();
            }
        };

        slint_keyos_platform::qrcode::render(
            &mnemonic.to_entropy(),
            slint::Color::from_rgb_u8(0, 0, 0),
            slint::Color::from_rgb_u8(255, 255, 255),
        )
    });
}

fn setup_backup_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let backup_global = ui.global::<BackupGlobal>();

    backup_global.set_magic_backup_enabled(state.borrow().settings.get_magic_backup_enabled().0);

    backup_global.on_set_magic_backup_enabled(move |enabled| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<BackupGlobal>().set_magic_backup_enabled(enabled);
        state.settings.set_magic_backup_enabled(enabled);
    });

    backup_global.on_create_backup(move || {
        spawn_local(async move {
            let ui = state.borrow().ui();
            let global = ui.global::<BackupGlobal>();
            global.set_status(BackupStatus::Creating);
            match timeout(
                async_archive::<BackupPermissions, _>(backup::messages::CreateBackup),
                Duration::from_secs(15),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    log::warn!("create backup failed {e:?}");
                    global.set_status(BackupStatus::Error);
                }
                Err(_) => {
                    log::warn!("create backup timed out");
                    global.set_status(BackupStatus::Error);
                }
            }
        })
        .detach();
    });

    spawn_local(async move {
        let mut status_updates =
            subscribe_scalar::<backup_permissions::BackupPermissions, _>(backup::messages::StatusSubscribe);
        while let Some(status) = status_updates.next().await {
            log::info!("Backup status update: {status:?}");
            let mut state = state.borrow_mut();
            state.last_backup = status.last_backup_at;

            let ui = state.ui();
            let global = ui.global::<BackupGlobal>();
            global.set_status(if status.publish_failed { BackupStatus::Error } else { BackupStatus::Idle });
        }
    })
    .detach();

    spawn_local(async move {
        let mut sub =
            subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeMagicBackupEnabled);
        let mut task: Option<TaskHandle<()>> = None;
        while let Some(magic_backup_enabled) = sub.next().await {
            let enabled = magic_backup_enabled.0;
            let ui = state.borrow().ui();
            let global = ui.global::<BackupGlobal>();
            global.set_magic_backup_enabled(enabled);
            let publish = state
                .borrow()
                .ql_status
                .send_ql_archive_retry(SendPrimeMagicBackupEnabled { enabled }, |e| {
                    log::warn!("failed to publish magic backup enabled {e:?}")
                });
            let _ = task.insert(spawn_worker(async move {
                publish.await;
                log::info!("published magic backup enabled");
            }));
        }
    })
    .detach();

    ui.global::<VerifyKeycardBackupGlobal>().on_start(move || {
        keycard_verify::KeycardVerifyFlow::start(state);
    });
}

fn setup_keycard_backup_global(state: StoredValue<AppState>) {
    use keycard_scan::backup::BackupKind;

    let ui = state.borrow().ui();
    let backup_global = ui.global::<KeycardBackupGlobal>();

    backup_global.on_start_manual_keycard_backup(move || {
        keycard_backup::KeycardBackupFlow::start(state, BackupKind::Manual);
    });

    backup_global.on_start_magic_backup(move || {
        keycard_backup::KeycardBackupFlow::start(state, BackupKind::Magic);
    });

    backup_global.on_error_clicked(move |confirm: bool| {
        keycard_backup::KeycardBackupFlow::handle_error_click(state, confirm);
    });
}

fn setup_ql_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let ql_global = ui.global::<QlGlobal>();

    spawn_local({
        let mut ql = state.borrow().ql_status.clone();
        async move {
            while let Some(status) = ql.next().await {
                log::info!("ql_status {status:?}");
                let ui = state.borrow().ui();
                let global = ui.global::<QlGlobal>();

                global.set_bt_connected(status.bt_connected);
                global.set_ql_paired(status.ql_paired);
                global.set_ql_live(status.live);
            }
        }
    })
    .detach();

    ql_global.on_qr_data(move || {
        let state = state.borrow();
        ql_utils::static_qr(&state.settings, &state.ble_address, true).into()
    });

    ql_global.on_animated_qr_data(move || {
        let state = state.borrow();
        ql_utils::animated_qr(&state.quantum)
    });

    ql_global.on_disconnect(move || {
        spawn_local(async move {
            if let Err(e) =
                async_archive::<QuantumLinkPermissions, _>(quantum_link::messages::UnpairFromEnvoy).await
            {
                log::warn!("failed to notify Envoy of unpair: {e:?}");
            }
        })
        .detach();
    });

    spawn_local(async move {
        let mut pairing_events =
            subscribe_archive::<QuantumLinkPermissions, _>(quantum_link::messages::SubscribePairingEvent);
        while let Some(pairing_event) = pairing_events.next().await {
            log::info!("pairing event: {pairing_event:?}");
            let ui = state.borrow().ui();
            let global = ui.global::<QlGlobal>();

            match pairing_event {
                PairingEvent::PairingComplete { device_name, new } => {
                    global.set_paired_device_name(device_name.into());
                    if new {
                        let s = state.borrow();
                        if s.settings.get_envoy_time_sync().0 {
                            ql_utils::sync_system_timezone(s.settings.clone(), s.ql_status.clone(), |e| {
                                log::warn!("failed to retrieve tz from envoy {e:?}")
                            })
                            .detach();
                        }
                        drop(s);
                        ql_utils::launch_bitcoin_app::<app_manager_permissions::AppManagerPermissions>()
                            .await
                            .inspect_err(|e| log::warn!("failed to start bitcoin app {e:?}"))
                            .ok();
                    }
                }
                PairingEvent::Disconnected => {}
                PairingEvent::RequestReceived => {}
                PairingEvent::PairingFailed => {}
            }
        }
    })
    .detach();

    spawn_local(async move {
        let ql_status = state.borrow().ql_status.clone();
        loop {
            ql_status.ready().await;
            ql_status.wait_until(|s| !s.bt_connected || !s.ql_paired || !s.live).await;
            let mut state = state.borrow_mut();
            let mut status_guard = state.persisted_status.guard();
            status_guard.last_envoy_comms = Some(SystemTime::now());
        }
    })
    .detach();
}

fn setup_update_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let update_global = ui.global::<UpdateGlobal>();

    update_global.on_check_fw_update(move || {
        spawn_local(check_firmware_update_available(state)).detach();
    });

    update_global.on_download_firmware_update(move || {
        start_firmware_download(state);
    });

    ql_utils::on_update_sufficient_battery::<power_manager_ext_permissions::PowerManagerExtPermissions, _>(
        move |sufficient_battery| {
            log::info!("update sufficient_battery={}", sufficient_battery);
            let ui = state.borrow().ui();
            ui.global::<UpdateGlobal>().set_update_sufficient_battery(sufficient_battery);
        },
    )
    .detach();

    spawn_local(async move {
        let mut update_events = subscribe_archive::<update_permissions::UpdatePermissions, _>(
            update::messages::SubscribeUpdateProgress,
        );

        let ql_status = state.borrow().ql_status.clone();
        let mut disconnect_monitor: Option<TaskHandle<()>> = None;

        while let Some(event) = update_events.next().await {
            // Keep auto-lock disabled until the user leaves the update page.
            let restore_update_exit_controls = || {
                let state = state.borrow();
                state
                    .gui
                    .update_kiosk_policy(
                        UpdateKioskPolicy::default()
                            .set_home_button(true)
                            .set_power_button(true)
                            .set_control_center(true),
                    )
                    .ok();
                state.platform_config.enable_swipe_back.set(true);
            };
            let ui = state.borrow().ui();
            let update_global = ui.global::<UpdateGlobal>();

            match event {
                ProgressUpdate::DownloadProgress(progress) => {
                    update_global.set_fw_update_state(FwUpdateState::Receiving);
                    update_global.set_fw_update_progress(progress.completion_percentage() as f32);

                    // Disable auto-lock and start monitoring for disconnection when download starts
                    if progress.is_start() {
                        state
                            .borrow()
                            .gui
                            .update_kiosk_policy(UpdateKioskPolicy::default().set_auto_lock(false))
                            .ok();
                        state.borrow().platform_config.enable_swipe_back.set(false);
                        let status = ql_status.clone().into_inner().into_stream();
                        let _ = disconnect_monitor.insert(spawn_local(async move {
                            std::pin::pin!(status).any(|status| !status.live).await;
                            log::error!("QuantumLink disconnected during update");
                            handle_update_error(
                                state,
                                "Connection lost".to_string(),
                                FwUpdateError::DownloadFailed,
                                InstallErrorStage::Download,
                            );
                        }));
                    }
                }
                ProgressUpdate::DownloadComplete => {
                    log::info!("update download complete");
                    disconnect_monitor = None;
                    notify_update_progress(state, FirmwareInstallEvent::Installing);
                    state.borrow().update.apply_downloaded_update();
                    update_global.set_fw_update_state(FwUpdateState::Installing);
                }
                ProgressUpdate::InstallProgress(progress) => {
                    update_global.set_fw_update_state(FwUpdateState::Installing);

                    let percent = progress.completion_percentage();
                    let secs_remaining = progress.estimate_time_remaining_secs();
                    let mins_remaining = secs_remaining.div_ceil(60).max(1);
                    let time_str = format!("{mins_remaining}m");

                    log::info!("update install progress {percent}% {time_str}");

                    update_global.set_fw_update_progress(percent as f32);
                    update_global.set_fw_update_eta(time_str.into());
                }
                ProgressUpdate::Rebooting => {
                    log::info!("update rebooting");
                    update_global.set_fw_update_state(FwUpdateState::Restarting);
                    notify_update_progress(state, FirmwareInstallEvent::Rebooting);
                }
                ProgressUpdate::Done => {
                    log::info!("update complete. rebooting...");
                    update_global.set_fw_update_state(FwUpdateState::Restarting);
                    notify_update_progress(state, FirmwareInstallEvent::Rebooting);
                    state.borrow().set_update_kiosk_enabled(true);
                }
                ProgressUpdate::InstallError(error) => {
                    disconnect_monitor = None;
                    log::error!("failed to apply update {error:?}");
                    restore_update_exit_controls();
                    handle_update_error(
                        state,
                        error.to_string(),
                        FwUpdateError::InstallFailed,
                        InstallErrorStage::Install,
                    );
                }
                ProgressUpdate::DownloadError(error) => {
                    disconnect_monitor = None;
                    log::error!("failed to download update {error:?}");
                    restore_update_exit_controls();
                    handle_update_error(
                        state,
                        error.to_string(),
                        FwUpdateError::DownloadFailed,
                        InstallErrorStage::Download,
                    );
                }
            }
        }
    })
    .detach();
}

fn resume_update_if_needed(state: StoredValue<AppState>) {
    let state = state.borrow();
    if !state.update.update_status().needs_continue {
        return;
    }

    let ui = state.ui();
    let update_global = ui.global::<UpdateGlobal>();
    if update_global.get_fw_update_state() == FwUpdateState::Installing {
        return;
    }

    log::info!("continuing interrupted update");
    state.set_update_kiosk_enabled(false);

    update_global.set_fw_update_state(FwUpdateState::Installing);
    update_global.set_fw_update_progress(0.0);
    update_global.set_fw_update_eta(SharedString::default());

    ui.global::<Navigate>().invoke_update_progress(NavigateOptions { animate: Animate::None, replace: true });
    state.update.continue_update();
}

fn setup_save_settings_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let global = ui.global::<SaveSettingsGlobal>();

    global.on_save_settings_file(move || {
        spawn_local(async move {
            if let Err(e) = save_settings_file(state).await {
                let ui = state.borrow().ui();
                let global = ui.global::<SaveSettingsGlobal>();
                log::error!("Failed to save settings file: {:?}", e);
                global.set_status(SaveSettingsStatus::Error);
            }
        })
        .detach();
    });
}

fn start_firmware_download(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let update_global = ui.global::<UpdateGlobal>();
    update_global.set_fw_update_state(FwUpdateState::Downloading);
    update_global.set_fw_update_progress(0.0);
    update_global.set_fw_update_eta(SharedString::default());
    update_global.set_fw_update_error(FwUpdateError::DownloadFailed);

    let start_fw_update =
        state.borrow().ql_status.send_ql_archive_retry(StartFirmwareUpdate { chunk_offset: None }, |e| {
            log::warn!("failed to start fw update {e:?}, retrying...")
        });
    spawn_worker(async move {
        start_fw_update.await;
        log::info!("started fw update");
    })
    .detach();
}

fn handle_update_error(
    state: StoredValue<AppState>,
    error: String,
    fw_error: FwUpdateError,
    stage: InstallErrorStage,
) {
    let ui = state.borrow().ui();
    let update_global = ui.global::<UpdateGlobal>();
    update_global.set_fw_update_state(FwUpdateState::Failed);
    update_global.set_fw_update_error(fw_error);
    notify_update_progress(state, FirmwareInstallEvent::Error { error, stage });
}

async fn check_firmware_update_available(state: StoredValue<AppState>) {
    log::info!("Checking for firmware update");

    let ui = state.borrow().ui();
    let global = ui.global::<UpdateGlobal>();
    let ql_status = state.borrow().ql_status.clone();

    global.set_checking_fw_update(true);
    let result = timeout(
        ql_status.send_ql_archive(quantum_link::messages::CheckFirmwareUpdate),
        Duration::from_secs(10),
    )
    .await;
    global.set_checking_fw_update(false);

    let update = match result {
        Ok(Ok(update)) => update,
        Ok(Err(e)) => {
            log::error!("failed to check for firmware update {e:?}");
            global.set_new_keyos_version(SharedString::default());
            return;
        }
        Err(_) => {
            log::error!("timed out checking for firmware update");
            global.set_new_keyos_version(SharedString::default());
            return;
        }
    };

    let now = jiff::Timestamp::now();
    let tz = state.borrow().settings.get_time_zone();
    let zoned = now.to_zoned(tz.timezone());
    let last_checked =
        jiff::fmt::strtime::format("%Y-%m-%d %H:%M", &zoned).unwrap_or_else(|_| "Unknown".to_string());
    global.set_last_update_checked_on(last_checked.into());

    match update {
        Some(update) => {
            log::info!("firmware update available: {}", update.version);
            global.set_new_keyos_version(SharedString::from(&update.version));
        }
        None => {
            log::info!("no firmware update available");
            global.set_new_keyos_version(SharedString::default());
        }
    }
}

fn notify_update_progress(state: StoredValue<AppState>, event: FirmwareInstallEvent) {
    let msg = NotifyFirmwareInstall { event };
    let mut state = state.borrow_mut();
    let task = spawn_worker(state.ql_status.send_ql_archive(msg));
    state.notify_update_event = Some(task);
}

async fn save_settings_file(state: StoredValue<AppState>) -> anyhow::Result<()> {
    let state = state.borrow();
    let ui = state.ui();
    let global = ui.global::<SaveSettingsGlobal>();

    global.set_status(SaveSettingsStatus::Saving);

    let options = SelectFileOptions::default()
        .with_hidden_allowed(false)
        .with_dirs_allowed(true)
        .with_dir_selection_mode(true)
        .with_multiple_selection_mode(false)
        .with_allowed_extensions(AllowedExtensions::specific(&["tar"]));

    let (path, location) = select_file::<GuiPermissions>(options)
        .context("Failed to select a directory")?
        .and_then(|selected| selected.files().get(0).cloned())
        .ok_or(anyhow::anyhow!("No file selected"))?;

    let location = match location {
        Location::Internal => fs::Location::User,
        Location::External => fs::Location::Usb,
        Location::Airlock => fs::Location::Airlock,
    };

    let now = jiff::Timestamp::now();
    let tz = state.settings.get_time_zone();
    let zoned = now.to_zoned(tz.timezone());
    let timestamp =
        jiff::fmt::strtime::format("%Y-%m-%d_%H-%M-%S", &zoned).unwrap_or_else(|_| "unknown".to_string());
    let backup_path = format!("{}/settings-{}.tar", path, timestamp);

    state
        .backup_api
        .create_backup_file(backup_path.clone(), location)
        .context("Failed to create a backup")?;

    global.set_status(SaveSettingsStatus::Success);
    global.set_backup_path(backup_path.into());

    Ok(())
}

fn erase_system_state() {
    let fs = FileSystem::default();
    match fs.remove(fs::SYSTEM_STATE_ROOT, fs::Location::System) {
        Ok(_) | Err(fs::Error::FileNotFound) => {}
        Err(e) => log::error!("Failed to erase system state dir: {e:?}"),
    }
}
