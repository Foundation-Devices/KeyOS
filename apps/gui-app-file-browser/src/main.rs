// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![feature(must_not_suspend)]
#![deny(must_not_suspend)]

use std::time::Duration;

use fs::{messages::FormatUsb, DirEntry};
pub use i18n::{format_currency, format_date, format_float, format_int, format_time};
#[cfg(not(feature = "recovery-os"))]
use slint_keyos_platform::settings;
use slint_keyos_platform::{
    app, async_scalar,
    gui_server_api::{
        navigation::filepicker::{SelectFileOptions, SelectFileResult},
        InputMessage,
    },
    sleep,
    slint::{ComponentHandle, Model},
    spawn_local, AppInput, StoredValue,
};

use crate::fsutils::copy_move::{copy_entries, move_entries};
use crate::fsutils::{create_dir, delete_all, rename_entry};
use crate::location::LocationKey;
use crate::path::FsPath;
use crate::picker::{PickerOptions, PickerState};
use crate::state::AppState;

mod fsutils;
mod location;
mod path;
mod picker;
mod state;

app!("Files");

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    tr::set_locale("en");

    let state = StoredValue::new(AppState::new(ui.clone_strong(), cx.gui.clone()));

    cx.set_input_handler(move |app_input| input_handler_fn(app_input, state));

    subscribe_fs_events(state, fs::Location::Usb);
    #[cfg(not(feature = "recovery-os"))]
    {
        subscribe_fs_events(state, fs::Location::User);
        subscribe_fs_events(state, fs::Location::Airlock);
    }

    init_picker(&ui, state);
    init_browser(&ui, state);
    init_modals(&ui, state);

    ui.run().expect("UI running");
}

fn init_picker(ui: &AppWindow, state: StoredValue<AppState>) {
    let global = ui.global::<PickerGlobal>();

    global.on_select_directory(move || {
        let mut state = state.borrow_mut();
        let Some(picker) = state.picker.as_ref() else {
            log::error!("tried to select directory when not in picker mode");
            return;
        };
        if !picker.options.dir_selection_mode {
            log::error!("tried to select directory when not in directory select mode");
            return;
        }

        let path = picker.locations.get(picker.current).path.to_string();
        let location = picker.current.to_nav();
        let result = SelectFileResult::new(vec![(path, location)]);
        state.finish_picker_navigation(result);
    });

    global.on_entry_pressed(move |entry_name, is_folder| {
        {
            let mut state = state.borrow_mut();
            let Some(picker) = state.picker.as_mut() else {
                return;
            };
            if is_folder {
                let state_loc = picker.locations.get_mut(picker.current);
                state_loc.path.join(entry_name.as_str());
                state.apply_ui();
            } else {
                if picker.options.dir_selection_mode {
                    return;
                }
                let mut path = picker.locations.get(picker.current).path.clone();
                let location = picker.current.to_nav();
                path.join(entry_name.as_str());
                let result = SelectFileResult::new(vec![(path.to_string(), location)]);
                state.finish_picker_navigation(result);
            }
        }
        AppState::list_directory_picker(state);
    });

    global.on_back(move || {
        {
            let mut state = state.borrow_mut();
            let fs = state.fs.clone();
            let Some(picker) = state.picker.as_mut() else {
                return;
            };
            let location = picker.current.to_fs();
            let state_loc = picker.locations.get_mut(picker.current);
            let path = state::get_parent_path(&fs, &state_loc.path, location);
            state_loc.path = path;
            state.apply_ui();
        }
        AppState::list_directory_picker(state);
    });

    global.on_location_changed(move |new_location_index| {
        {
            let mut state = state.borrow_mut();
            if new_location_index < 0 {
                log::warn!("location index out of range: {new_location_index}");
                return;
            }
            let Some(picker) = state.picker.as_mut() else {
                return;
            };
            let order = picker.allowed_order();
            let idx = new_location_index as usize;
            let Some(key) = order.get(idx).copied() else {
                log::warn!("location index out of range: {new_location_index}");
                return;
            };
            picker.current = key;
            state.apply_ui();
        };
        AppState::list_directory_picker(state);
    });

    global.on_breadcrumb_selected(move |_, index| {
        {
            let mut state = state.borrow_mut();
            let Some(picker) = state.picker.as_mut() else {
                return;
            };
            let key = picker.current;
            let current_path = picker.locations.get(key).path.clone();
            let Some(new_path) = state::path_for_breadcrumb_index(&current_path, index) else {
                log::warn!("picker breadcrumb index out of range: {index}");
                return;
            };
            picker.locations.get_mut(key).path = new_path;
            state.apply_ui();
        }
        AppState::list_directory_picker(state);
    });

    global.on_build_breadcrumbs(move |location_idx, path| {
        let idx: usize = location_idx.try_into().unwrap_or_default();
        let state = state.borrow();
        let Some(picker) = state.picker.as_ref() else {
            return Default::default();
        };
        let order = picker.allowed_order();
        let Some(key) = order.get(idx).copied() else {
            return Default::default();
        };
        state::build_breadcrumbs(key, path.as_str())
    });
}

fn init_browser(ui: &AppWindow, state: StoredValue<AppState>) {
    let global = ui.global::<BrowserGlobal>();

    global.on_entry_pressed(move |entry_name, is_folder| {
        {
            let mut state = state.borrow_mut();
            if state.browser_global().get_is_select_mode() {
                state.toggle_selected(FsPath::new(entry_name.as_str()));
                return;
            }
            if !is_folder {
                return;
            }
            let key = state.browser.current;
            let loc_path = state.browser.locations.get_mut(key);
            loc_path.join(entry_name.as_str());
            state.apply_ui();
        };
        AppState::list_directory_browser(state);
    });

    global.on_entry_long_pressed(move |entry_name, _is_folder| {
        let mut state = state.borrow_mut();
        state.set_select_mode(true);
        state.toggle_selected(FsPath::new(entry_name.as_str()));
        state.apply_ui();
    });

    global.on_back(move || {
        {
            let mut state = state.borrow_mut();
            let fs = state.fs.clone();
            let key = state.browser.current;
            let location = key.to_fs();
            let state_loc = state.browser.locations.get_mut(key);
            let path = state::get_parent_path(&fs, state_loc, location);
            *state_loc = path;
            state.apply_ui();
        };
        AppState::list_directory_browser(state);
    });

    global.on_location_changed(move |new_location_index| {
        {
            let mut state = state.borrow_mut();
            if new_location_index < 0 {
                log::warn!("location index out of range: {new_location_index}");
                return;
            }
            let idx = new_location_index as usize;
            let Some(key) = LocationKey::ALL.get(idx).copied() else {
                log::warn!("location index out of range: {new_location_index}");
                return;
            };
            state.browser.current = key;
            state.apply_ui();
        };
        AppState::list_directory_browser(state);
    });

    global.on_breadcrumb_selected(move |_, index| {
        {
            let mut state = state.borrow_mut();
            let key = state.browser.current;
            let loc_path = state.browser.locations.get_mut(key);
            let Some(new_path) = state::path_for_breadcrumb_index(loc_path, index) else {
                log::warn!("breadcrumb index out of range: {index}");
                return;
            };
            *loc_path = new_path;
            state.apply_ui();
        }
        AppState::list_directory_browser(state);
    });

    global.on_reload_files(move || {
        AppState::list_directory_browser(state);
    });

    global.on_select_mode_toggled(move |enabled| {
        let mut state = state.borrow_mut();
        state.set_select_mode(enabled);
        state.apply_ui();
    });

    global.on_menu_done(move || {
        let mut state = state.borrow_mut();
        state.set_select_mode(false);
        state.apply_ui();
    });

    global.on_menu_copy(move || {
        {
            let mut state = state.borrow_mut();
            state.open_copy_move(crate::FileAction::Copy);
            state.apply_ui();
        };
        AppState::list_directory_copy_move(state);
    });

    global.on_menu_move(move || {
        {
            let mut state = state.borrow_mut();
            state.open_copy_move(crate::FileAction::Move);
            state.apply_ui();
        };
        AppState::list_directory_copy_move(state);
    });

    global.on_menu_rename(move || {
        let state = state.borrow();
        let Some(name) = state.browser.selection.get(state.browser.current).iter().next() else {
            return;
        };
        let global = state.modal_global();
        global.set_rename_text(name.as_str().into());
        global.set_active_modal(ActiveModal::Rename);
        global.set_loading(false);
    });

    global.on_menu_create_folder(move || {
        let state = state.borrow();
        let global = state.modal_global();
        global.set_active_modal(ActiveModal::CreateFolder);
        global.set_loading(false);
    });

    global.on_menu_delete(move || {
        let state = state.borrow();
        if state.selection_len_current() == 0 {
            return;
        }
        let global = state.modal_global();
        global.set_active_modal(ActiveModal::Delete);
        global.set_loading(false);
    });

    global.on_files_selected_count(|files| files.iter().filter(|f| f.is_selected).count() as i32);
    global.on_build_breadcrumbs(|location_idx, path| {
        let idx: usize = location_idx.try_into().unwrap_or_default();
        let Some(key) = LocationKey::ALL.get(idx).copied() else {
            return Default::default();
        };
        state::build_breadcrumbs(key, path.as_str())
    });

    global.on_format_clicked(move || {
        let state = state.borrow();
        let global = state.modal_global();
        global.set_active_modal(ActiveModal::Format);
        global.set_loading(false);
    });

    global.on_format_ok_acknowledged(move || {
        AppState::list_directory_browser(state);
    });

    #[cfg(not(feature = "recovery-os"))]
    global.on_airlock_mode_changed(move |am| {
        let settings = SettingsApi::default();
        settings.set_airlock_mode(am);
    });

    #[cfg(not(feature = "recovery-os"))]
    global.on_hidden_files_toggled(move |show| {
        state.borrow_mut().set_show_hidden_files(show);
    });

    #[cfg(not(feature = "recovery-os"))]
    global.on_quick_tip_dismissed(move || {
        state.borrow_mut().set_quick_tip_dismissed();
    });

    #[cfg(not(feature = "recovery-os"))]
    {
        let state = state.borrow();
        global.set_show_hidden_files(state.show_hidden_files());
        if state.quick_tip_dismissed() {
            global.set_show_quick_tip(false);
        }
    }

    #[cfg(not(feature = "recovery-os"))]
    spawn_local({
        let ui = ui.clone_strong();
        async move {
            let mut sub = slint_keyos_platform::subscribe_scalar::<
                settings_permissions::SettingsPermissions,
                _,
            >(settings::messages::SubscribeAirlockMode);
            while let Some(am) = sub.next().await {
                ui.global::<BrowserGlobal>().set_airlock_mode(am.into());
            }
        }
    })
    .detach();
}

fn init_modals(ui: &AppWindow, state: StoredValue<AppState>) {
    let global = ui.global::<ModalGlobal>();

    global.on_cancel(move || {
        let mut state = state.borrow_mut();
        state.close_modal();
    });

    global.on_validate_rename(move |text| is_valid_filename(text.as_str()));

    global.on_validate_create_folder(move |text| is_valid_filename(text.as_str()));

    global.on_build_breadcrumbs(|location_idx, path| {
        let idx: usize = location_idx.try_into().unwrap_or_default();
        let Some(key) = LocationKey::ALL.get(idx).copied() else {
            return Default::default();
        };
        state::build_breadcrumbs(key, path.as_str())
    });

    global.on_rename_confirmed(move |new_name| {
        if !is_valid_filename(new_name.as_str()) {
            log::warn!("rename to invalid file name {new_name}");
            return;
        }

        let (fs, location, dir, from_name) = {
            let state = state.borrow();
            let Some(name) = state.selected_names_current().into_iter().next() else {
                log::warn!("rename with no selected file present");
                return;
            };
            let key = state.browser.current;
            let dir = state.browser.locations.get(key).to_string();
            state.modal_global().set_loading(true);
            (state.fs.clone(), key.to_fs(), dir, name.to_string())
        };

        let rename = async move { rename_entry(fs, location, &dir, &from_name, new_name.as_str()).await };

        spawn_local(async move {
            match rename.await {
                Ok((from, to)) => {
                    log::info!("rename request success {from} {to}");
                }
                Err(e) => {
                    log::error!("rename request failed: {e:?}");
                }
            }
            state.borrow_mut().exit_select_mode();
            AppState::list_directory_browser(state);
        })
        .detach();
    });

    global.on_create_folder_confirmed(move |name| {
        if !is_valid_filename(name.as_str()) {
            log::warn!("create folder with invalid name {name}");
            return;
        }

        let (fs, location, dir) = {
            let state = state.borrow();
            let key = state.browser.current;
            let dir = state.browser.locations.get(key).to_string();
            state.modal_global().set_loading(true);
            (state.fs.clone(), key.to_fs(), dir)
        };

        spawn_local(async move {
            match create_dir(fs, location, &dir, name.as_str()).await {
                Ok(path) => {
                    log::info!("create folder success {path}");
                }
                Err(e) => {
                    log::error!("create folder failed: {e:?}");
                }
            }

            state.with(|state| {
                state.close_modal();
            });

            AppState::list_directory_browser(state);
        })
        .detach();
    });

    global.on_delete_confirmed(move || {
        let (fs, location, paths) = {
            let state = state.borrow();
            if state.selection_len_current() == 0 {
                return;
            }
            let key = state.browser.current;
            let dir = state.browser.locations.get(key).as_str();
            let paths: Vec<_> = state
                .selected_names_current()
                .into_iter()
                .map(|name| fsutils::join_path(dir, name.as_str()))
                .collect();
            state.modal_global().set_loading(true);
            (state.fs.clone(), key.to_fs(), paths)
        };

        if paths.is_empty() {
            state.borrow_mut().exit_select_mode();
            AppState::list_directory_browser(state);
            return;
        }

        spawn_local(async move {
            delete_all(fs, location, paths).await;
            state.borrow_mut().exit_select_mode();
            AppState::list_directory_browser(state);
        })
        .detach();
    });

    global.on_copy_move_close(move || {
        let mut state = state.borrow_mut();
        state.close_modal();
    });

    global.on_copy_move_location_changed(move |idx| {
        {
            let mut state = state.borrow_mut();
            let idx = idx.max(0) as usize;
            let Some(selected) = LocationKey::ALL.get(idx).copied() else {
                return;
            };
            let Some(ctx) = &mut state.copy_move else {
                return;
            };
            ctx.current = selected;
            state.apply_ui();
        };
        AppState::list_directory_copy_move(state);
    });

    global.on_copy_move_breadcrumb_selected(move |_, index| {
        {
            let mut state = state.borrow_mut();
            let Some(ctx) = &mut state.copy_move else {
                return;
            };
            let loc_path = ctx.locations.get_mut(ctx.current);
            let Some(new_path) = state::path_for_breadcrumb_index(loc_path, index) else {
                log::warn!("breadcrumb index out of range: {index}");
                return;
            };
            *loc_path = new_path;
            state.apply_ui();
        }
        AppState::list_directory_copy_move(state);
    });

    global.on_copy_move_enter_folder(move |name| {
        {
            let mut state = state.borrow_mut();
            let Some(ctx) = &mut state.copy_move else {
                return;
            };
            let state_loc = ctx.locations.get_mut(ctx.current);
            state_loc.join(name.as_str());
            state.apply_ui();
        };
        AppState::list_directory_copy_move(state);
    });

    global.on_copy_move_exit_folder(move || {
        {
            let mut state = state.borrow_mut();
            let fs = state.fs.clone();
            let Some(ctx) = &mut state.copy_move else {
                return;
            };
            let location_path = ctx.locations.get_mut(ctx.current);
            let location = ctx.current.to_fs();
            let path = state::get_parent_path(&fs, location_path, location);
            *location_path = path;
            state.apply_ui();
        };
        AppState::list_directory_copy_move(state);
    });

    global.on_copy_move_confirmed(move || {
        let (action, source_location, dest_location, source_dir, dest_dir, names) = {
            let state = state.borrow();
            let Some(ctx) = &state.copy_move else {
                return;
            };
            let global = state.modal_global();
            let action = global.get_copy_move_action();
            let source_location = state.browser.current.to_fs();
            let dest_location = ctx.current.to_fs();
            let source_dir = state.browser.locations.get(state.browser.current).to_string();
            let dest_dir = ctx.locations.get(ctx.current).to_string();
            let names: Vec<_> =
                state.selected_names_current().into_iter().map(|name| name.to_string()).collect();

            global.set_copy_move_progress(0.0);
            global.set_loading(true);
            (action, source_location, dest_location, source_dir, dest_dir, names)
        };

        spawn_local(async move {
            let fs = state.borrow().fs.clone();
            let handler = move |done, total| {
                let progress = if total == 0 { 0.0 } else { (done as f32) * 100.0 / (total as f32) };
                state.borrow().modal_global().set_copy_move_progress(progress);
            };
            let result = match action {
                FileAction::Copy => {
                    copy_entries(fs, source_location, dest_location, &source_dir, &dest_dir, &names, handler)
                        .await
                }
                FileAction::Move => {
                    move_entries(fs, source_location, dest_location, &source_dir, &dest_dir, &names, handler)
                        .await
                }
            };

            match result {
                Ok(()) => {
                    log::info!("copy/move {action:?} succeeded")
                }
                Err(e) => log::error!("copy/move {action:?} failed {e:?}"),
            }
            sleep(Duration::from_millis(200)).await;
            state.with(|state| {
                state.close_modal();
                state.exit_select_mode();
            });
            AppState::list_directory_browser(state);
        })
        .detach();
    });

    global.on_format_confirmed(move || {
        let key = state.borrow().browser.current;
        spawn_local(async move {
            state.with(|state| {
                state.close_modal();
                state.browser.listing.per_location.get_mut(key).list_type = FileListType::Formatting;
                state.apply_ui();
            });
            let format_result = match key {
                LocationKey::External => {
                    async_scalar::<fs_permissions::FileSystemPermissions, _>(FormatUsb).await
                }
                #[cfg(not(feature = "recovery-os"))]
                LocationKey::Internal => {
                    log::error!("Format confirmed on internal location");
                    return;
                }
                #[cfg(not(feature = "recovery-os"))]
                LocationKey::Airlock => {
                    use fs::messages::{FormatAirlock, MountAirlock};

                    match async_scalar::<fs_permissions::FileSystemPermissions, _>(FormatAirlock).await {
                        Ok(()) => {
                            async_scalar::<fs_permissions::FileSystemPermissions, _>(MountAirlock(true)).await
                        }
                        Err(e) => Err(e),
                    }
                }
            };
            let result = match format_result {
                Ok(()) => FileListType::FormatSuccessful,
                Err(e) => {
                    log::error!("Format failed: {e:?}");
                    FileListType::FormatFailed
                }
            };

            state.with(|state| {
                state.browser.listing.per_location.get_mut(key).list_type = result;
                state.apply_ui();
            });
        })
        .detach();
    })
}

fn input_handler_fn(app_input: AppInput<gui_permissions::GuiPermissions>, state: StoredValue<AppState>) {
    match app_input.msg {
        InputMessage::Visible => {
            let should_list = state.borrow().picker.is_none();
            if should_list {
                state.borrow().apply_ui();
                AppState::list_directory_browser(state);
            }
        }

        InputMessage::NavigationCancelled => {
            state.with(|state| {
                state.exit_picker();
                state.ui.invoke_focus();
                state.apply_ui();
            });
        }

        InputMessage::NavigationFocused => {
            let gui = state.borrow().gui.clone();
            let Ok(Some(nav_bytes)) = gui.navigate_pending() else {
                log::error!("Navigation focused but no pending nav request");
                return;
            };

            let Some(options) = SelectFileOptions::from_slice(&nav_bytes) else {
                log::error!("Failed to parse SelectFileOptions from a nav request");
                return;
            };

            let start_location = LocationKey::from_nav(options.start_location());
            let allowed_locations = PickerState::allowed_map_from_request(options.allowed_locations());
            let picker_options = PickerOptions {
                allowed_extensions: options.allowed_extensions().clone(),
                allow_dirs: options.dirs_allowed() || options.dir_selection_mode(),
                allow_hidden: options.hidden_allowed(),
                dir_selection_mode: options.dir_selection_mode(),
            };
            let picker = PickerState::new(picker_options, allowed_locations, start_location);

            if picker.allowed_order().is_empty() {
                log::warn!("picker requested with no allowed locations");
                let result = SelectFileResult::new(Vec::new());
                state.with(|state| state.finish_picker_navigation(result));
                return;
            }

            state.with(|state| {
                state.ui.invoke_focus();
                state.picker = Some(picker);
                state.apply_ui();
            });

            AppState::list_directory_picker(state);
        }

        _ => (),
    }
}

fn fs_event_handler(state: StoredValue<AppState>, event: fs::FileSystemEvent) {
    let Some(key) = LocationKey::from_fs(event.location) else {
        log::debug!("Ignoring fs event for location: {:?}", event.location);
        return;
    };

    state.with(|state| {
        state.availability.insert(key, Some(event.event_type));
    });

    if state.borrow().picker.is_some() {
        AppState::list_directory_picker(state)
    } else if state.borrow().copy_move.is_some() {
        if !state.with(|state| state.is_mounted(state.browser.current)) {
            state.borrow_mut().close_modal();
            state.borrow_mut().apply_ui();
        } else {
            AppState::list_directory_copy_move(state)
        }
    } else {
        let current_list_type =
            state.with(|state| state.browser.listing.per_location.get(state.browser.current).list_type);
        // Don't switch away from the format dialog. An fs event is expected and handled differently there.
        let expected_event = match current_list_type {
            FileListType::Formatting => true,
            FileListType::FormatSuccessful => event.event_type == fs::FileSystemEventType::Mounted,
            FileListType::FormatFailed => event.event_type == fs::FileSystemEventType::Error,
            _ => false,
        };
        if !expected_event {
            AppState::list_directory_browser(state)
        }
    }
}

fn subscribe_fs_events(state: StoredValue<AppState>, location: fs::Location) {
    spawn_local(async move {
        let mut sub = slint_keyos_platform::subscribe_scalar::<fs_permissions::FileSystemPermissions, _>(
            fs::messages::SubscribeFilesystemEvent(location),
        );
        while let Some(event) = sub.next().await {
            fs_event_handler(state, event);
        }
    })
    .detach();
}

impl From<DirEntry> for FileEntryModel {
    fn from(value: DirEntry) -> Self {
        FileEntryModel {
            info: "".into(),
            is_folder: value.is_dir,
            name: value.name.clone().into(),
            is_selected: false,
        }
    }
}

#[cfg(not(feature = "recovery-os"))]
impl From<AirlockMode> for settings::global::AirlockMode {
    fn from(value: AirlockMode) -> Self {
        match value {
            AirlockMode::Disabled => settings::global::AirlockMode::Disabled,
            AirlockMode::ReadOnly => settings::global::AirlockMode::ReadOnly,
            AirlockMode::ReadWrite => settings::global::AirlockMode::ReadWrite,
        }
    }
}

#[cfg(not(feature = "recovery-os"))]
impl From<settings::global::AirlockMode> for AirlockMode {
    fn from(value: settings::global::AirlockMode) -> Self {
        match value {
            settings::global::AirlockMode::Disabled => AirlockMode::Disabled,
            settings::global::AirlockMode::ReadOnly => AirlockMode::ReadOnly,
            settings::global::AirlockMode::ReadWrite => AirlockMode::ReadWrite,
        }
    }
}

fn is_valid_filename(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with(' ')
        && !name.ends_with('.')
        && !name.chars().any(|ch| {
            ch.is_control() || matches!(ch, '\0' | '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        && name.encode_utf16().count() <= 255
}
