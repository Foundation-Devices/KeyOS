// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{sync::Arc, time::Duration};

use fs::DirEntry;
#[cfg(not(feature = "recovery-os"))]
use slint_keyos_platform::file_backed::JsonBacked;
use slint_keyos_platform::{
    gui_server_api::navigation::filepicker::{AllowedExtensions, SelectFileResult},
    sleep,
    slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel},
    spawn_local, StoredValue,
};

#[cfg(not(feature = "recovery-os"))]
use crate::fs_permissions::FileSystemPermissions;
use crate::{
    fsutils::list::{list_directory, ListingParams},
    FileListType,
};
use crate::{
    location::LocationKey, location::LocationMap, path::FsPath, picker::PickerState, tr, ActiveModal,
    AppWindow, BreadCrumb, BrowserGlobal, FileAction, FileEntryModel, FileListData, FileSystem, GuiApi,
    ModalGlobal, PickerGlobal, SegmentModel, SortDirection, SortMode,
};

#[cfg(not(feature = "recovery-os"))]
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct FileBrowserSettings {
    pub show_hidden_files: bool,
    pub quick_tip_dismissed: bool,
}

pub struct AppState {
    pub ui: AppWindow,
    pub gui: Arc<GuiApi>,
    pub fs: FileSystem,
    pub availability: LocationMap<Option<fs::FileSystemEventType>>,
    pub browser: BrowserState,
    pub picker: Option<PickerState>,
    pub copy_move: Option<CopyMoveState>,
    #[cfg(not(feature = "recovery-os"))]
    pub settings: JsonBacked<FileBrowserSettings, FileSystemPermissions>,
}

#[derive(Clone)]
pub struct CopyMoveState {
    pub current: LocationKey,
    pub locations: LocationMap<FsPath>,
    pub listing: ListingState,
}

#[derive(Clone)]
pub struct BrowserState {
    pub current: LocationKey,
    pub locations: LocationMap<FsPath>,
    pub listing: ListingState,
    pub selection: LocationMap<Vec<FsPath>>,
}

#[derive(Clone)]
pub struct ListingState {
    pub per_location: LocationMap<ListingSlot>,
}

#[derive(Clone)]
pub struct ListingSlot {
    #[allow(dead_code)]
    pub location: LocationKey,
    pub list_type: FileListType,
    pub num_excluded: usize,
    pub num_excluded_by_search: usize,
    pub files: ModelRc<FileEntryModel>,
}

impl AppState {
    pub fn new(ui: AppWindow, gui: Arc<GuiApi>) -> Self {
        AppState {
            ui,
            gui,
            fs: FileSystem::default(),
            availability: LocationMap::from_fn(|_| None),
            browser: BrowserState::new(),
            picker: None,
            copy_move: None,
            #[cfg(not(feature = "recovery-os"))]
            settings: JsonBacked::new("settings.json", fs::Location::AppData).0,
        }
    }

    #[cfg(not(feature = "recovery-os"))]
    pub fn show_hidden_files(&self) -> bool { self.settings.show_hidden_files }

    #[cfg(not(feature = "recovery-os"))]
    pub fn set_show_hidden_files(&mut self, show: bool) { self.settings.guard().show_hidden_files = show; }

    #[cfg(not(feature = "recovery-os"))]
    pub fn quick_tip_dismissed(&self) -> bool { self.settings.quick_tip_dismissed }

    #[cfg(not(feature = "recovery-os"))]
    pub fn set_quick_tip_dismissed(&mut self) { self.settings.guard().quick_tip_dismissed = true; }

    pub fn apply_ui(&self) {
        if self.picker.is_some() {
            self.apply_picker_ui();
        } else {
            self.apply_browser_ui();
        }
    }

    fn apply_picker_ui(&self) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let order = picker.allowed_order();

        let current = picker.current;
        let current_path = picker.locations.get(current).path.to_shared_string();
        let current_idx = order.iter().position(|key| *key == current).unwrap_or(0) as i32;

        let global = self.ui.global::<PickerGlobal>();
        global.set_is_active(true);
        global.set_location_idx(current_idx);
        global.set_is_dir_selection_mode(picker.options.dir_selection_mode);
        global.set_is_folders_allowed(picker.options.allow_dirs);
        global.set_current_path(current_path);
        global.set_allowed_locations(make_locations(&order));

        let file_lists =
            order.iter().map(|key| picker.listing.per_location.get(*key).to_file_list()).collect::<Vec<_>>();
        global.set_file_lists(ModelRc::new(VecModel::from(file_lists)));
    }

    fn apply_browser_ui(&self) {
        self.picker_global().set_is_active(false);

        let order = LocationKey::ALL;

        let current_location = self.browser.current;
        let current_path = self.browser.locations.get(current_location).to_shared_string();
        let current_idx = order.iter().position(|key| *key == current_location).unwrap_or(0) as i32;
        let allowed_locations = make_locations(&order);

        {
            let global = self.browser_global();
            global.set_location_idx(current_idx);
            global.set_allowed_locations(allowed_locations.clone());
            global.set_current_path(current_path);

            let file_lists = order
                .iter()
                .map(|key| self.browser.listing.per_location.get(*key).to_file_list())
                .collect::<Vec<_>>();
            global.set_file_lists(ModelRc::new(VecModel::from(file_lists)));
        }

        self.apply_modal_ui();
    }

    fn apply_modal_ui(&self) {
        if self.copy_move.is_some() {
            self.apply_copy_move_ui();
        }
    }

    pub fn is_mounted(&self, location: LocationKey) -> bool {
        *self.availability.get(location) == Some(fs::FileSystemEventType::Mounted)
    }

    pub fn apply_copy_move_ui(&self) {
        let Some(ctx) = &self.copy_move else {
            return;
        };

        let order = LocationKey::ALL;
        let copy_move_idx = order.iter().position(|key| *key == ctx.current).unwrap_or(0) as i32;
        let copy_move_path = ctx.locations.get(ctx.current).to_shared_string();
        let copy_move_summary = selection_summary(self.browser.selection.get(self.browser.current));

        let source_location = self.browser.current;
        let dest_location = ctx.current;
        let source_dir = self.browser.locations.get(source_location).as_str();
        let dest_dir = ctx.locations.get(dest_location).as_str();
        #[cfg(not(feature = "recovery-os"))]
        let airlock_used = source_location == LocationKey::Airlock || dest_location == LocationKey::Airlock;
        #[cfg(not(feature = "recovery-os"))]
        let airlock_mounted = self.is_mounted(LocationKey::Airlock);
        #[cfg(feature = "recovery-os")]
        let airlock_used = false;
        #[cfg(feature = "recovery-os")]
        let airlock_mounted = false;
        let action_enabled = !(source_location == dest_location && source_dir == dest_dir)
            && (!airlock_used || airlock_mounted);

        let global = self.modal_global();
        global.set_copy_move_location_idx(copy_move_idx);
        global.set_copy_move_allowed_locations(make_locations(&order));
        global.set_copy_move_current_path(copy_move_path);
        global.set_copy_move_summary(copy_move_summary.into());
        global.set_copy_move_action_enabled(action_enabled);

        let file_lists = LocationKey::ALL
            .iter()
            .map(|key| ctx.listing.per_location.get(*key).to_file_list())
            .collect::<Vec<_>>();
        global.set_copy_move_file_lists(ModelRc::new(VecModel::from(file_lists)));
    }

    fn browser_listing_params(&self) -> ListingParams {
        let key = self.browser.current;
        let path = self.browser.locations.get(key).to_string();
        let location = key.to_fs();
        let global = self.browser_global();
        let query = global.get_search_query();
        let search_query = if query.as_str().trim().is_empty() { None } else { Some(query) };

        ListingParams {
            path,
            location,
            allowed_extensions: AllowedExtensions::All,
            sort_mode: global.get_sort_mode(),
            sort_direction: global.get_sort_direction(),
            search_query,
            show_hidden: global.get_show_hidden_files(),
            allow_dirs: global.get_is_folders_allowed(),
        }
    }

    pub fn set_select_mode(&mut self, enabled: bool) {
        self.browser_global().set_is_select_mode(enabled);
        if !enabled {
            self.clear_selection();
            self.sync_selection_to_listing_all();
        } else {
            self.sync_selection_to_listing_current();
        }
    }

    pub fn toggle_selected(&mut self, name: FsPath) {
        let key = self.browser.current;
        let selected = self.browser.selection.get_mut(key);
        if let Some(pos) = selected.iter().position(|n| n == &name) {
            selected.remove(pos);
        } else {
            selected.push(name);
        }
        self.sync_selection_to_listing_current();
    }

    pub fn clear_selection(&mut self) {
        for key in LocationKey::ALL.iter().copied() {
            self.browser.selection.get_mut(key).clear();
        }
    }

    pub fn selection_len_current(&self) -> usize { self.browser.selection.get(self.browser.current).len() }

    pub fn selected_names_current(&self) -> Vec<FsPath> {
        self.browser.selection.get(self.browser.current).clone()
    }

    pub fn exit_picker(&mut self) { self.picker = None; }

    pub fn finish_picker_navigation(&mut self, result: SelectFileResult) {
        if let Err(e) = self.gui.navigate_finish(result.serialize()) {
            log::error!("failed to finish picker navigation: {e:?}");
        }
        self.exit_picker();
        self.ui.invoke_focus();
        self.apply_ui();
    }

    pub fn exit_select_mode(&mut self) {
        self.close_modal();
        self.set_select_mode(false);
        self.apply_ui();
    }

    pub fn close_modal(&mut self) {
        self.copy_move = None;
        let global = self.modal_global();
        global.set_active_modal(ActiveModal::None);
        global.set_loading(false);
    }

    pub fn open_copy_move(&mut self, action: FileAction) {
        let current = self.browser.current;
        let locations = self.browser.locations.clone();

        self.copy_move = Some(CopyMoveState { current, locations, listing: ListingState::new() });

        let global = self.modal_global();
        global.set_active_modal(ActiveModal::CopyMove);
        global.set_loading(false);
        global.set_copy_move_action(action);
        global.set_copy_move_progress(0.0);
    }

    fn sync_selection_to_listing_current(&mut self) {
        let key = self.browser.current;
        self.sync_selection_to_listing_for(key);
    }

    fn sync_selection_to_listing_all(&mut self) {
        for key in LocationKey::ALL.iter().copied() {
            self.sync_selection_to_listing_for(key);
        }
    }

    fn sync_selection_to_listing_for(&mut self, key: LocationKey) {
        let selection = self.browser.selection.get(key);
        let slot = self.browser.listing.per_location.get_mut(key);
        for i in 0..slot.files.row_count() {
            let Some(mut entry) = slot.files.row_data(i) else {
                continue;
            };
            let is_selected = selection.iter().any(|name| name.as_str() == entry.name.as_str());
            if entry.is_selected != is_selected {
                entry.is_selected = is_selected;
                slot.files.set_row_data(i, entry);
            }
        }
    }

    pub fn modal_global(&self) -> ModalGlobal<'_> { self.ui.global::<ModalGlobal>() }

    pub fn browser_global(&self) -> BrowserGlobal<'_> { self.ui.global::<BrowserGlobal>() }

    pub fn picker_global(&self) -> PickerGlobal<'_> { self.ui.global::<PickerGlobal>() }
}

/// async tasks
impl AppState {
    pub fn list_directory_browser(state: StoredValue<Self>) {
        let (fs, params, key) = {
            let state = state.borrow();
            let key = state.browser.current;
            let params = state.browser_listing_params();
            let fs = state.fs.clone();
            state.apply_ui();
            (fs, params, key)
        };

        spawn_local(Self::list_browser_task(state, fs, key, params)).detach();
    }

    pub fn list_directory_picker(state: StoredValue<Self>) {
        let (fs, params, key) = {
            let state = state.borrow();
            let Some(picker) = &state.picker else {
                return;
            };
            let user_show_hidden = state.browser_global().get_show_hidden_files();
            let key = picker.current;
            let params = picker.listing_params(user_show_hidden);
            let fs = state.fs.clone();
            state.apply_ui();
            (fs, params, key)
        };

        spawn_local(Self::list_picker_task(state, fs, key, params)).detach();
    }

    pub fn list_directory_copy_move(state: StoredValue<Self>) {
        let (fs, params, key, selected) = {
            let state = state.borrow();
            let Some(ctx) = &state.copy_move else {
                return;
            };
            let key = ctx.current;
            let params = ctx.listing_params();
            let fs = state.fs.clone();
            let selected = state.browser.selection.get(state.browser.current).clone();
            state.apply_ui();
            (fs, params, key, selected)
        };

        spawn_local(Self::list_copy_move_task(state, fs, key, params, selected)).detach();
    }

    async fn list_copy_move_task(
        state: StoredValue<Self>,
        fs: FileSystem,
        key: LocationKey,
        params: ListingParams,
        selected_names: Vec<FsPath>,
    ) {
        let result = {
            let _delayed_loading = spawn_local(async move {
                sleep(Duration::from_millis(100)).await;
                if let Some(ctx) = &mut state.borrow_mut().copy_move {
                    ctx.listing.per_location.get_mut(key).list_type = FileListType::Loading;
                }
                state.borrow().apply_ui();
            });
            list_directory(fs, params).await
        };

        let mut state = state.borrow_mut();
        let state = &mut *state;
        let Some(ctx) = &mut state.copy_move else {
            return;
        };
        let slot = ctx.listing.per_location.get_mut(key);
        Self::list_result_to_slot(result, slot, key, &state.availability, |list| {
            list.into_iter()
                .filter(|entry| entry.is_dir)
                .filter(|entry| !selected_names.iter().any(|name| name.as_str() == entry.name.as_str()))
                .map(|entry| entry.into())
        });
        state.apply_ui();
    }

    async fn list_browser_task(
        state: StoredValue<Self>,
        fs: FileSystem,
        key: LocationKey,
        params: ListingParams,
    ) {
        let result = {
            let _delayed_loading = spawn_local(async move {
                sleep(Duration::from_millis(100)).await;
                state.borrow_mut().browser.listing.per_location.get_mut(key).list_type =
                    FileListType::Loading;
                state.borrow().apply_ui();
            });
            list_directory(fs, params).await
        };

        let mut state = state.borrow_mut();
        let state = &mut *state;
        let is_select_mode = state.browser_global().get_is_select_mode();
        let selection = if is_select_mode { state.browser.selection.get(key).clone() } else { Vec::new() };

        let slot = state.browser.listing.per_location.get_mut(key);
        Self::list_result_to_slot(result, slot, key, &state.availability, |list| {
            list.into_iter().map(|entry| {
                let mut entry: FileEntryModel = entry.into();
                entry.is_selected = selection.iter().any(|name| name.as_str() == entry.name.as_str());
                entry
            })
        });
        state.apply_ui();
    }

    async fn list_picker_task(
        state: StoredValue<Self>,
        fs: FileSystem,
        key: LocationKey,
        params: ListingParams,
    ) {
        let result = {
            let _delayed_loading = spawn_local(async move {
                sleep(Duration::from_millis(100)).await;
                if let Some(picker) = &mut state.borrow_mut().picker {
                    picker.listing.per_location.get_mut(key).list_type = FileListType::Loading;
                }
                state.borrow().apply_ui();
            });
            list_directory(fs, params).await
        };

        let mut state = state.borrow_mut();
        let state = &mut *state;
        let Some(picker) = state.picker.as_mut() else {
            return;
        };
        let slot = picker.listing.per_location.get_mut(key);
        Self::list_result_to_slot(result, slot, key, &state.availability, |list| {
            list.into_iter().map(|entry| entry.into())
        });
        state.apply_ui();
    }

    fn list_result_to_slot<TI: IntoIterator<Item = FileEntryModel>>(
        result: whence::Result<(Vec<DirEntry>, crate::fsutils::list::FilterCounts), fs::Error>,
        slot: &mut ListingSlot,
        key: LocationKey,
        availability: &LocationMap<Option<fs::FileSystemEventType>>,
        entry_filter: impl FnOnce(Vec<DirEntry>) -> TI,
    ) {
        slot.clear();

        match result {
            Ok((list, filter_counts)) => {
                slot.list_type = FileListType::FileList;
                slot.num_excluded = filter_counts.excluded;
                slot.num_excluded_by_search = filter_counts.excluded_by_search;
                slot.extend(entry_filter(list));
            }
            #[cfg(not(feature = "recovery-os"))]
            Err(whence::Error { error: fs::Error::NoMedia, .. }) if key == LocationKey::Airlock => {
                if availability.get(key) == &Some(fs::FileSystemEventType::Error) {
                    slot.list_type = FileListType::NotFormatted;
                } else {
                    slot.list_type = FileListType::AirlockUsb;
                }
                slot.num_excluded = 0;
                slot.num_excluded_by_search = 0;
            }
            Err(whence::Error { error: fs::Error::NoMedia, .. }) if key == LocationKey::External => {
                if availability.get(key) == &Some(fs::FileSystemEventType::Error) {
                    slot.list_type = FileListType::NotFormatted;
                } else {
                    slot.list_type = FileListType::UsbNotConnected;
                }
                slot.num_excluded = 0;
                slot.num_excluded_by_search = 0;
            }
            Err(e) => {
                log::error!("Failed to list directory: {e:?}");
                slot.list_type = FileListType::GeneralError;
                slot.num_excluded = 0;
                slot.num_excluded_by_search = 0;
            }
        }
    }
}

impl BrowserState {
    fn new() -> Self {
        BrowserState {
            current: LocationKey::DEFAULT,
            locations: LocationMap::from_fn(|_| Default::default()),
            listing: ListingState::new(),
            selection: LocationMap::from_fn(|_| Vec::new()),
        }
    }
}

impl CopyMoveState {
    fn listing_params(&self) -> ListingParams {
        let path = self.locations.get(self.current).to_string();
        let location = self.current.to_fs();

        ListingParams {
            path,
            location,
            allowed_extensions: AllowedExtensions::All,
            sort_mode: SortMode::Alphabetical,
            sort_direction: SortDirection::Ascending,
            search_query: None,
            show_hidden: true,
            allow_dirs: true,
        }
    }
}

impl ListingState {
    pub fn new() -> Self { ListingState { per_location: LocationMap::from_fn(|l| ListingSlot::new(l)) } }
}

impl ListingSlot {
    fn new(location: LocationKey) -> Self {
        ListingSlot {
            location,
            list_type: FileListType::FileList,
            num_excluded: 0,
            num_excluded_by_search: 0,
            files: ModelRc::new(VecModel::from(Vec::<FileEntryModel>::new())),
        }
    }

    fn to_file_list(&self) -> FileListData {
        FileListData {
            #[cfg(not(feature = "recovery-os"))]
            is_airlock: self.location == LocationKey::Airlock,
            #[cfg(feature = "recovery-os")]
            is_airlock: false,
            list_type: self.list_type,
            num_files_excluded: self.num_excluded as i32,
            num_files_excluded_by_search: self.num_excluded_by_search as i32,
            files: self.files.clone(),
        }
    }

    fn vec_model(&self) -> &VecModel<FileEntryModel> { self.files.as_any().downcast_ref().unwrap() }

    fn clear(&self) {
        let model = self.vec_model();
        model.clear();
    }

    fn extend(&self, iter: impl IntoIterator<Item = FileEntryModel>) {
        let vec = self.vec_model();
        vec.clear();
        vec.extend(iter);
    }
}

fn make_locations(order: &[LocationKey]) -> ModelRc<SegmentModel> {
    let segments = order
        .iter()
        .map(|key| {
            let icon = key.icon();
            let label = key.tr_id();
            SegmentModel { icon: icon.into(), label: tr::lookup_id(label).into() }
        })
        .collect::<Vec<_>>();
    ModelRc::new(VecModel::from(segments))
}

/// if we don't have parent, something is wrong and we should navigate back to root
pub fn get_parent_path(fs: &FileSystem, path: &FsPath, location: fs::Location) -> FsPath {
    let Some(parent) = path.parent() else {
        return FsPath::default();
    };
    if parent.as_str().is_empty() || fs.metadata(parent.as_str(), location).is_ok() {
        parent
    } else {
        FsPath::default()
    }
}

fn selection_summary(names: &[FsPath]) -> String {
    names.iter().map(|name| name.as_str()).collect::<Vec<_>>().join(", ")
}

pub fn build_breadcrumbs(location: LocationKey, path: &str) -> ModelRc<BreadCrumb> {
    let mut crumbs = Vec::<BreadCrumb>::new();
    crumbs.push(BreadCrumb { label: tr::lookup_id(location.tr_id()).into(), icon: location.icon().into() });
    if !path.is_empty() {
        crumbs.extend(
            path.split('/')
                .filter(|segment| !segment.is_empty())
                .map(|segment| BreadCrumb { label: SharedString::from(segment), icon: "folder".into() }),
        );
    }
    ModelRc::new(VecModel::from(crumbs))
}

pub fn path_for_breadcrumb_index(path: &FsPath, index: i32) -> Option<FsPath> {
    if index < 0 {
        return None;
    }
    let idx = index as usize;
    if idx == 0 {
        return Some(FsPath::default());
    }
    let segments = path.as_str().split('/').filter(|segment| !segment.is_empty()).collect::<Vec<_>>();
    if idx > segments.len() {
        return None;
    }
    Some(FsPath::new(segments[..idx].join("/")))
}
