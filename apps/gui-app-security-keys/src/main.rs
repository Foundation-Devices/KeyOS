// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use haptics::HapticPattern;
use {
    fido::{
        messages::{OperationType, SubscribeOperationOutcomes},
        SecurityKeyView,
    },
    fuzzy_filter::FuzzyFilter,
    slint_keyos_platform::{
        app,
        gui_server_api::{
            navigation::securitykeys::{SecurityKeysNavRequest, UserPresenceResult},
            InputMessage,
        },
        sleep,
        slint::{Model, ModelRc, SharedString, VecModel},
        spawn_local, subscribe_archive, StoredValue, TaskHandle,
    },
    std::{
        io::Read,
        time::{Duration, Instant},
    },
};

/// Sort modes for the key list UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardSortMode {
    Label = 0,
    Date = 1,
}

impl From<usize> for CardSortMode {
    fn from(value: usize) -> Self {
        match value {
            1 => CardSortMode::Date,
            _ => CardSortMode::Label,
        }
    }
}

/// Old Key struct for one-time migration from the legacy local database.
/// Matches the JSON format of `security_key_database_v1.json`.
#[derive(serde::Deserialize)]
struct OldKey {
    key_index: usize,
    label: String,
    #[serde(default)]
    color: u8,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    date: u64,
    #[serde(default)]
    icon: String,
}

const OLD_DATABASE_FILE: &str = "security_key_database_v1.json";

/// Sort two SecurityKeyViews by the given mode.
fn compare_keys(a: &SecurityKeyView, b: &SecurityKeyView, mode: CardSortMode) -> std::cmp::Ordering {
    match mode {
        CardSortMode::Label => a.label.to_lowercase().cmp(&b.label.to_lowercase()),
        CardSortMode::Date => b.date.cmp(&a.date),
    }
}

/// Maximum time to wait for a presence keep-alive heartbeat from the FIDO server before the
/// modal auto-dismisses itself as cancelled. Any RP actively polling will emit a heartbeat
/// on every retry (~300 ms apart for Chrome), so 2 s is comfortably above the retry cadence.
const PRESENCE_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_millis(2000);
/// Polling interval for the auto-dismiss timer. Short enough that abandoned modals close
/// within `PRESENCE_KEEP_ALIVE_TIMEOUT + POLL` worst case.
const PRESENCE_POLL_INTERVAL: Duration = Duration::from_millis(500);

fido::use_api!();
haptics::use_api!();
nfc::use_api!();

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("Could not use negative index")]
    IndexError,
    #[error("Label is empty")]
    EmptyLabel,
    #[error("Label already in use")]
    DuplicateLabel,
}

struct AppState {
    /// Cached key list from FIDO server, updated via subscription events.
    keys: Vec<SecurityKeyView>,
    search_text: String,
    archive_mode: bool,
    sort_mode: CardSortMode,
    fido_api: FidoApi,
    haptics_api: HapticsApi,
    nfc_api: NfcApi,
    #[cfg(keyos)]
    settings_api: SettingsApi,
    /// FIDO index of the key most recently created via the Register presence modal's
    /// "Create a new key" affordance. Consumed by `on_auto_select_last_created` when the
    /// user returns from /edit so the new key is pre-selected in the dropdown.
    last_created_key_index: Option<usize>,
    /// Auto-dismiss watcher task for the user-presence modal. `Some(handle)` while the
    /// modal is on screen; assigning `None` drops the handle, which cancels the running
    /// future — that's how every modal-close path (user confirm/cancel, Hidden event, and
    /// replacement by a new presence request) tears the watcher down.
    dismiss_task: Option<TaskHandle<()>>,
    /// Timestamp of the most recent presence keep-alive heartbeat observed since the
    /// modal opened.
    last_heartbeat: Option<Instant>,
}

impl From<&SecurityKeyView> for KeyView {
    fn from(value: &SecurityKeyView) -> Self {
        Self {
            label: SharedString::from(&value.label),
            color: value.color as i32,
            live: value.live,
            icon: SharedString::from(&value.icon),
            index: value.index as i32,
        }
    }
}

impl AppState {
    fn get_key_entries(&self) -> ModelRc<KeyView> {
        let filter = if self.search_text.is_empty() {
            None
        } else {
            Some(FuzzyFilter::new(self.search_text.as_ref()))
        };

        let mut entries: Vec<&SecurityKeyView> = self
            .keys
            .iter()
            .filter(|key| {
                if key.archived != self.archive_mode {
                    return false;
                }
                match &filter {
                    Some(filter) if !filter.matches(key.label.to_lowercase().as_ref()) => false,
                    _ => true,
                }
            })
            .collect();

        entries.sort_by(|a, b| compare_keys(a, b, self.sort_mode));

        let views: Vec<KeyView> = entries.into_iter().map(KeyView::from).collect();

        ModelRc::new(VecModel::from(views))
    }

    fn get_dropdown_model(&self) -> ModelRc<DropdownModel> {
        let mut entries: Vec<&SecurityKeyView> =
            self.keys.iter().filter(|key| key.archived == self.archive_mode).collect();

        entries.sort_by(|a, b| compare_keys(a, b, self.sort_mode));

        let mut views: Vec<DropdownModel> = entries
            .iter()
            .map(|key| DropdownModel {
                label: SharedString::from(&key.label),
                value: key.index.to_string().into(),
                icon: SharedString::from("key"),
                icon_image: Default::default(),
            })
            .collect();

        // Trailing "Create a new key" entry. Value "" distinguishes it from real key
        // entries (which use stringified FIDO indices). The Slint side branches on
        // `value == ""` to invoke `CB.create-new-key()` instead of selecting a key.
        views.push(DropdownModel {
            label: SharedString::from(tr::lookup_id(TrId::RegistrationUSBNewSecurityKey)),
            value: SharedString::new(),
            icon: SharedString::from("plus"),
            icon_image: Default::default(),
        });

        ModelRc::new(VecModel::from(views))
    }

    /// Find the first non-archived key under the current sort mode. Matches the leading
    /// entry of `get_dropdown_model` so the Register modal's default selection lines up
    /// with what the user sees at the top of the dropdown.
    fn first_non_archived_key_index(&self) -> Option<usize> {
        self.keys
            .iter()
            .filter(|k| !k.archived)
            .min_by(|a, b| compare_keys(a, b, self.sort_mode))
            .map(|k| k.index)
    }

    /// Refresh the UI after key list change.
    fn refresh_ui(&self, ui_state: &SecurityKeyCallbacks) {
        ui_state.set_entries(self.get_key_entries());
        ui_state.set_dropdown_model(self.get_dropdown_model());
    }

    /// Local label-uniqueness check using the cached key list. The FIDO server re-validates
    /// on create/edit, so a stale cache here just means a duplicate slips past the inline
    /// hint and is rejected at submit time.
    fn label_is_unique(&self, exclude_index: Option<usize>, label: &str) -> bool {
        !self.keys.iter().any(|k| Some(k.index) != exclude_index && k.label == label)
    }
}

impl From<KeyError> for CallbackResult {
    fn from(error: KeyError) -> Self {
        log::warn!("{}", error);
        match error {
            KeyError::EmptyLabel => Self::failure(
                ResultLevel::Error,
                String::from("Error"),
                tr::lookup_id(TrId::AddLabelMissing).to_string(),
            ),
            KeyError::DuplicateLabel => Self::failure(
                ResultLevel::Error,
                String::from("Error"),
                tr::lookup_id(TrId::AddLabelAlreadyInUse).to_string(),
            ),
            ref other => Self::failure(ResultLevel::Error, String::from("Error"), other.to_string()),
        }
    }
}

impl CallbackResult {
    fn success() -> Self {
        Self {
            success: true,
            level: ResultLevel::Info,
            title: SharedString::new(),
            text: SharedString::new(),
        }
    }

    fn failure(level: ResultLevel, title: String, text: String) -> Self {
        Self { success: false, level, title: SharedString::from(title), text: SharedString::from(text) }
    }
}

app!("Keys");

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    cx.config.enable_swipe_back.set(false);

    let fido_api = FidoApi::default();

    // One-time migration: read old local database and push metadata to FIDO server.
    migrate_old_database(&fido_api);

    // Synchronously snapshot the key list before the app starts processing navigation
    // events. The async `SubscribeKeyChanges` subscription below keeps this updated, but
    // its first event may not have been drained by the time a presence-check navigation
    // fires — so we seed `keys` here to avoid a "No non-archived keys available" false
    // negative when FIDO launches us for a Register.
    let initial_keys = fido_api.list_security_keys();

    let app_state = AppState {
        keys: initial_keys,
        search_text: String::new(),
        archive_mode: false,
        sort_mode: CardSortMode::Label,
        fido_api,
        haptics_api: HapticsApi::default(),
        nfc_api: NfcApi::default(),
        #[cfg(keyos)]
        settings_api: SettingsApi::default(),
        last_created_key_index: None,
        dismiss_task: None,
        last_heartbeat: None,
    };

    let ui_state = ui.global::<SecurityKeyCallbacks>();
    app_state.refresh_ui(&ui_state);
    ui_state.set_sort_mode(app_state.sort_mode as i32);

    let app_state = StoredValue::new(app_state);

    // Subscribe to key changes from FIDO server.
    // The initial event delivers the current key list; subsequent events
    // arrive whenever keys are created, edited, archived, or used.
    {
        let app_state = app_state.clone();
        let ui = ui.clone_strong();
        spawn_local(async move {
            let mut events = subscribe_archive::<fido_permissions::FidoPermissions, _>(
                fido::messages::SubscribeKeyChanges,
            );
            while let Some(event) = events.next().await {
                let mut state = app_state.borrow_mut();
                log::debug!("KeysChanged: received {} keys from FIDO server", event.keys.len());
                for key in &event.keys {
                    log::debug!(
                        "  key[{}]: label='{}' archived={} live={} registered={}",
                        key.index,
                        key.label,
                        key.archived,
                        key.live,
                        key.registered_count
                    );
                }
                state.keys = event.keys;

                let ui_state = ui.global::<SecurityKeyCallbacks>();
                state.refresh_ui(&ui_state);

                // Navigate to add page if no keys exist (first launch)
                if state.keys.is_empty() {
                    ui.global::<Navigate>()
                        .invoke_add(NavigateOptions { replace: true, animate: Animate::None });
                }
            }
        })
        .detach();
    }

    // Subscribe to keep-alive heartbeats from the FIDO server. The server emits a
    // `PresenceKeepAliveEvent` every time it tells the relying party to retry
    // (ConditionNotSatisfied / UserActionPending). While the modal is on screen we record
    // the latest heartbeat; if none arrives for `PRESENCE_KEEP_ALIVE_TIMEOUT` the modal's
    // auto-dismiss watcher (armed in NavigationFocused below) cancels the request the same
    // way the user tapping cancel would.
    {
        spawn_local(async move {
            let mut events = subscribe_archive::<fido_permissions::FidoPermissions, _>(
                fido::messages::SubscribePresenceKeepAlive,
            );
            while let Some(_event) = events.next().await {
                let mut state = app_state.borrow_mut();
                if state.dismiss_task.as_ref().is_some_and(|h| !h.is_finished()) {
                    state.last_heartbeat = Some(Instant::now());
                }
            }
        })
        .detach();
    }

    // Subscribe to operation-outcome events. The success/failure modal is shown in response.
    {
        let app_state = app_state.clone();
        let ui = ui.clone_strong();
        spawn_local(async move {
            let mut events =
                subscribe_archive::<fido_permissions::FidoPermissions, _>(SubscribeOperationOutcomes);
            while let Some(event) = events.next().await {
                let view_index = event.security_key_index as i32;
                let auth_state = match event.operation {
                    OperationType::Registration => AuthenticatingState::RegistrationSuccess,
                    OperationType::Authentication => AuthenticatingState::AuthenticationSuccess,
                };
                log::debug!(
                    "OperationOutcome event: index={} op={:?} success={}",
                    event.security_key_index,
                    event.operation,
                    event.success
                );

                sleep(Duration::from_millis(500)).await;
                app_state.borrow().haptics_api.double_click();

                let ui_state = ui.global::<SecurityKeyCallbacks>();
                ui_state.set_is_outcome_mode(true);
                ui_state.set_selected_index(view_index);
                ui_state.set_auth_state(auth_state);

                ui.global::<Navigate>()
                    .invoke_authenticate(NavigateOptions { replace: false, animate: Animate::None });
            }
        })
        .detach();
    }

    ui.global::<SecurityKeyCallbacks>().on_search({
        let ui = ui.clone_strong();
        move |text| {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<SecurityKeyCallbacks>();
            app_state.search_text = text.to_string().to_lowercase();
            app_state.refresh_ui(&ui_state);
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_save({
        let ui = ui.clone_strong();
        move |label, icon, _live, color| {
            let mut app_state = app_state.borrow_mut();
            let label: String = label.into();
            let icon: String = icon.into();
            let color = color as u8;

            log::debug!("on_save: label='{}' color={}", label, color);

            let index = match app_state.fido_api.create_security_key(label, color, icon) {
                Ok(idx) => idx,
                Err(fido::error::FidoError::EmptyLabel) => {
                    return CallbackResult::from(KeyError::EmptyLabel);
                }
                Err(fido::error::FidoError::DuplicateLabel) => {
                    return CallbackResult::from(KeyError::DuplicateLabel);
                }
                Err(e) => {
                    log::warn!("create_security_key failed: {:?}", e);
                    return CallbackResult::failure(
                        ResultLevel::Error,
                        String::from("Error"),
                        String::from("Failed to create key"),
                    );
                }
            };
            log::debug!("Created security key at index {index}");

            // Stash the new index so `on_auto_select_last_created` can pre-select it when
            // the user returns from /edit to the Register presence modal.
            app_state.last_created_key_index = Some(index);

            // Synchronously refresh the key list + dropdown so the new entry is visible
            // immediately on return to /authenticate — the async subscription event may
            // not have arrived yet.
            app_state.keys = app_state.fido_api.list_security_keys();
            app_state.refresh_ui(&ui.global::<SecurityKeyCallbacks>());

            CallbackResult::success()
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_validate_new_label({
        move |label| {
            let app_state = app_state.borrow();
            let label: String = label.into();

            if label.is_empty() {
                return SharedString::from(tr::lookup_id(TrId::AddLabelMissing));
            }

            if !app_state.label_is_unique(None, &label) {
                return SharedString::from(tr::lookup_id(TrId::AddLabelAlreadyInUse));
            }

            SharedString::new()
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_validate_edit_label({
        move |index, label| {
            let app_state = app_state.borrow();
            let label: String = label.into();

            if label.is_empty() {
                return SharedString::from(tr::lookup_id(TrId::AddLabelMissing));
            }

            // Find the FIDO key index for this table position
            let key_index = if index >= 0 {
                // The index passed from UI is the view's source index (key.index from SecurityKeyView)
                Some(index as usize)
            } else {
                None
            };

            if !app_state.label_is_unique(key_index, &label) {
                return SharedString::from(tr::lookup_id(TrId::AddLabelAlreadyInUse));
            }

            SharedString::new()
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_edit({
        move |index, label, icon, _live, color| {
            let app_state = app_state.borrow();
            let Ok(index) = usize::try_from(index) else {
                return CallbackResult::from(KeyError::IndexError);
            };

            let label: String = label.into();
            let icon: String = icon.into();
            let color = color as u8;

            log::debug!("on_edit: index={} label='{}' color={}", index, label, color);

            // Pass date=0 to preserve existing date
            match app_state.fido_api.edit_security_key(index, label, color, icon, 0) {
                Ok(()) => {
                    log::debug!("on_edit: EditSecurityKey accepted by FIDO server");
                    // UI will refresh via the subscription event
                    CallbackResult::success()
                }
                Err(fido::error::FidoError::EmptyLabel) => CallbackResult::from(KeyError::EmptyLabel),
                Err(fido::error::FidoError::DuplicateLabel) => CallbackResult::from(KeyError::DuplicateLabel),
                Err(e) => {
                    log::warn!("edit_security_key failed: {:?}", e);
                    CallbackResult::failure(
                        ResultLevel::Error,
                        String::from("Error"),
                        String::from("Failed to edit key"),
                    )
                }
            }
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_set_archived({
        move |index, archived| {
            let app_state = app_state.borrow();
            let Ok(index) = usize::try_from(index) else {
                log::warn!("Invalid index for set_archived");
                return;
            };

            log::debug!("on_set_archived: index={} archived={}", index, archived);
            if let Err(e) = app_state.fido_api.set_archived(index, archived) {
                log::warn!("set_archived failed: {:?}", e);
            }
            // UI will refresh via the subscription event
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_set_archive_mode({
        let ui = ui.clone_strong();
        move |archive_mode| {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<SecurityKeyCallbacks>();
            app_state.archive_mode = archive_mode;
            app_state.refresh_ui(&ui_state);
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_set_sort_mode({
        let ui = ui.clone_strong();
        move |sort_mode| {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<SecurityKeyCallbacks>();
            ui_state.set_selected_index(-1);
            app_state.sort_mode = CardSortMode::from(sort_mode as usize);
            app_state.refresh_ui(&ui_state);
        }
    });

    cx.set_input_handler({
        let ui = ui.clone_strong();
        let gui_api = cx.gui.clone();
        let router = cx.router.clone();
        move |input| {
            if input.msg == InputMessage::NavigationFocused {
                let Ok(Some(nav_bytes)) = gui_api.navigate_pending() else {
                    log::error!("Navigation focused but no pending nav request");
                    return;
                };

                let (selected_view_index, auth_state) = match SecurityKeysNavRequest::from_slice(&nav_bytes) {
                    Some(SecurityKeysNavRequest::UserPresence(options)) => {
                        let mut app_state_local = app_state.borrow_mut();

                        let view_index = match options.security_key_index {
                            Some(key_index) => {
                                if !app_state_local.keys.iter().any(|k| k.index == key_index) {
                                    log::warn!("No key with index: {}", key_index);
                                    gui_api
                                        .navigate_finish(UserPresenceResult::new_cancelled().serialize())
                                        .unwrap_or_else(|e| {
                                            log::warn!("could not finish navigation: {}", e);
                                        });
                                    return;
                                }
                                key_index as i32
                            }
                            None => {
                                if options.authentication {
                                    log::warn!("Fido server should tell keys app which key to use");
                                    gui_api
                                        .navigate_finish(UserPresenceResult::new_cancelled().serialize())
                                        .unwrap_or_else(|e| {
                                            log::warn!("could not finish navigation: {}", e);
                                        });
                                    return;
                                }

                                match app_state_local.first_non_archived_key_index() {
                                    Some(idx) => {
                                        log::debug!("No key pre-selected, defaulting to first non-archived key index {}", idx);
                                        idx as i32
                                    }
                                    None => {
                                        // No non-archived keys: fall through with -1 so the
                                        // dropdown shows only the trailing "Create a new key"
                                        // entry. The user can register their first key directly
                                        // from the presence modal.
                                        log::debug!(
                                            "No non-archived keys available; Register modal will offer Create a new key"
                                        );
                                        -1
                                    }
                                }
                            }
                        };

                        let auth_state = if options.authentication {
                            AuthenticatingState::AuthenticationConfirm
                        } else {
                            AuthenticatingState::RegistrationConfirm
                        };

                        ui.global::<SecurityKeyCallbacks>().set_is_outcome_mode(false);
                        app_state_local.haptics_api.vibrate(HapticPattern::Alert750ms);
                        log::debug!("Got user presence request: {:?}", options);

                        // Arm the keep-alive timer and spawn the auto-dismiss guard. A fresh
                        // heartbeat timestamp is seeded so the 2 s window starts from now,
                        // giving the FIDO server one retry cycle to emit its first heartbeat.
                        // Storing the handle (rather than detaching) lets every modal-close
                        // path tear the watcher down by assigning `dismiss_task = None`.
                        let dismiss_task = {
                            let gui_api = gui_api.clone();
                            let ui = ui.clone_strong();
                            let router = router.clone();
                            spawn_local(async move {
                                loop {
                                    sleep(PRESENCE_POLL_INTERVAL).await;
                                    let on_authenticate = router.borrow().with_history(|history| {
                                        history.get_current_path().map(|path| path == "/authenticate").unwrap_or(false)
                                    });
                                    if !on_authenticate {
                                        // User navigated to /edit via "Create a new key" — the
                                        // FIDO worker is tied up serving our synchronous
                                        // CreateSecurityKey call and no heartbeats flow. Pause
                                        // the staleness check until they return.
                                        continue;
                                    }
                                    if let Some(t) = app_state.borrow().last_heartbeat {
                                        if t.elapsed() <= PRESENCE_KEEP_ALIVE_TIMEOUT {
                                            // Still fresh — keep watching.
                                            continue;
                                        }
                                    }
                                    break;
                                }

                                log::info!(
                                    "presence modal auto-dismissed: no keep-alive in {}ms",
                                    PRESENCE_KEEP_ALIVE_TIMEOUT.as_millis()
                                );

                                // Mirror the Slint `back()` function: resolve the nav
                                // with a cancelled result, deselect the key, then
                                // drive the UI backward. `navigate_finish` alone
                                // unblocks the FIDO worker but doesn't close the
                                // Slint page.
                                gui_api
                                    .navigate_finish(UserPresenceResult::new_cancelled().serialize())
                                    .unwrap_or_else(|e| {
                                        log::warn!("auto-dismiss navigate_finish failed: {}", e);
                                    });

                                let ui_state = ui.global::<SecurityKeyCallbacks>();
                                ui_state.set_selected_index(-1);
                                ui_state.set_is_outcome_mode(false);
                                app_state.borrow().fido_api.select_security_key(None);

                                // Pop the presence page off the nav stack. Safe because
                                // the route check earlier this iteration already gated
                                // the dismiss path on being on /authenticate, and no
                                // `await` runs between that check and here, so the
                                // route can't have changed underneath.
                                let nav = ui.global::<Navigate>();
                                if nav.get_has_backward() {
                                    nav.invoke_backward();
                                } else {
                                    log::warn!(
                                        "auto-dismiss: no backward nav available; modal may stay visible"
                                    );
                                }
                            })
                        };
                        app_state_local.last_heartbeat = Some(Instant::now());
                        app_state_local.dismiss_task = Some(dismiss_task);

                        (view_index, auth_state)
                    }
                    None => {
                        log::error!("Failed to deserialize SecurityKeysNavRequest");
                        gui_api
                            .navigate_finish(UserPresenceResult::new_cancelled().serialize())
                            .unwrap_or_else(|e| {
                                log::warn!("could not finish navigation: {}", e);
                            });
                        return;
                    }
                };

                let ui_state = ui.global::<SecurityKeyCallbacks>();
                ui_state.set_selected_index(selected_view_index);
                ui_state.set_auth_state(auth_state);

                ui.global::<Navigate>().invoke_authenticate(
                    NavigateOptions { replace: false, animate: Animate::None },
                );
            } else if input.msg == InputMessage::Hidden {
                let is_authenticate_page = router.borrow().with_history(|history| {
                    history.get_current_path().map(|path| path == "/authenticate").unwrap_or(false)
                });

                let mut app_state = app_state.borrow_mut();

                if is_authenticate_page {
                    ui.global::<Navigate>().invoke_backward();
                }

                app_state.fido_api.select_security_key(None);
                log::debug!("deselected current key");

                // Cancel any running keep-alive guard — the modal is gone.
                app_state.dismiss_task = None;

                let ui_state = ui.global::<SecurityKeyCallbacks>();
                ui_state.set_is_outcome_mode(false);
            }
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_user_presence_check({
        let gui_api = cx.gui.clone();
        let ui = ui.clone_strong();
        move |confirmed| {
            // User interacted explicitly — disarm the keep-alive guard.
            app_state.borrow_mut().dismiss_task = None;
            let presence_result = if confirmed {
                let ui_state = ui.global::<SecurityKeyCallbacks>();
                let selected_index = ui_state.get_selected_index();

                // selected_index is now the FIDO key index directly
                let selected_key_index =
                    if selected_index >= 0 { Some(selected_index as usize) } else { None };

                UserPresenceResult::new_checked(selected_key_index)
            } else {
                UserPresenceResult::new_cancelled()
            };

            gui_api.navigate_finish(presence_result.serialize()).unwrap_or_else(|e| {
                log::warn!("could not finish navigation: {}", e);
            });
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_dismiss_outcome({
        let ui = ui.clone_strong();
        move || {
            let ui_state = ui.global::<SecurityKeyCallbacks>();
            ui_state.set_is_outcome_mode(false);
            ui_state.set_auth_state(AuthenticatingState::Wait);
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_get_view_index({
        move |entries, source_index| match entries.iter().position(|entry| entry.index == source_index) {
            Some(i) => i as i32,
            None => {
                log::warn!("Could not find index of key that should exist");
                0
            }
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_get_dropdown_index({
        // Gate against non-numeric values so the trailing "Create a new key" entry
        // (value == "") never matches a real FIDO index.
        move |entries, source_index| match entries
            .iter()
            .position(|entry| entry.value.as_str().parse::<i32>().map(|v| v == source_index).unwrap_or(false))
        {
            Some(i) => i as i32,
            None => {
                log::warn!("Could not find index of key that should exist");
                0
            }
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_select_key({
        let ui = ui.clone_strong();
        move |index| {
            let app_state = app_state.borrow();
            let ui_state = ui.global::<SecurityKeyCallbacks>();
            ui_state.set_selected_index(index);

            // index is the FIDO key index directly
            if index >= 0 {
                app_state.fido_api.select_security_key(Some(index as usize));
                log::debug!("Selected key: {}", index);
            }
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_deselect_key({
        let ui = ui.clone_strong();
        move || {
            let app_state = app_state.borrow();
            let ui_state = ui.global::<SecurityKeyCallbacks>();
            ui_state.set_selected_index(-1);

            app_state.fido_api.select_security_key(None);
            log::debug!("deselected current key");
        }
    });

    // "Create a new key" entry in the Register presence dropdown.
    // Flow: user taps the entry → we mark the keep-alive as paused (so the modal
    // auto-dismiss doesn't fire while the user is on /edit) and navigate to /edit with
    // EditCaller::Register. On save, the Slint side invokes `auto_select_last_created`;
    // on cancel, it invokes `create_new_key_cancelled`.
    ui.global::<SecurityKeyCallbacks>().on_create_new_key({
        let ui = ui.clone_strong();
        move || {
            // The auto-dismiss watcher pauses while the route is /edit (route check, not a
            // flag), so we just navigate — Slint updates the route synchronously under
            // Animate::None before the watcher gets another chance to poll.
            ui.global::<Navigate>().invoke_edit(
                EditParams {
                    caller: EditCaller::Register,
                    key: KeyView {
                        label: SharedString::new(),
                        icon: SharedString::new(),
                        live: false,
                        color: 0,
                        index: 0,
                    },
                    version: EditPageVersion::Add,
                },
                NavigateOptions { replace: false, animate: Animate::None },
            );
            log::debug!("Register: navigating to /edit to create a new key");
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_create_new_key_cancelled({
        move || {
            // User backed out of /edit — reseed the heartbeat so the dismissal watcher
            // doesn't fire immediately against the timestamp from before the /edit detour.
            app_state.borrow_mut().last_heartbeat = Some(Instant::now());
            log::debug!("Register: /edit cancelled, resuming presence keep-alive");
        }
    });

    ui.global::<SecurityKeyCallbacks>().on_auto_select_last_created({
        let ui = ui.clone_strong();
        move || {
            let mut state = app_state.borrow_mut();
            let ui_state = ui.global::<SecurityKeyCallbacks>();

            // Reseed the heartbeat so the dismissal watcher doesn't fire immediately
            // against the timestamp from before the /edit detour.
            state.last_heartbeat = Some(Instant::now());

            if let Some(idx) = state.last_created_key_index {
                ui_state.set_selected_index(idx as i32);
                state.fido_api.select_security_key(Some(idx));
                log::debug!("Register: auto-selected newly created key at index {}", idx);
            } else {
                log::warn!("auto_select_last_created invoked but no index stashed");
            }
        }
    });

    #[cfg(keyos)]
    ui.global::<SecurityKeyCallbacks>()
        .on_is_usb_on(move || app_state.borrow().settings_api.get_usb_enabled().0);

    #[cfg(not(keyos))]
    ui.global::<SecurityKeyCallbacks>().on_is_usb_on(|| true);

    ui.global::<SecurityKeyCallbacks>()
        .on_is_nfc_on(move || app_state.borrow().nfc_api.is_enabled().unwrap_or(false));

    ui.run().expect("UI running");
}

/// One-time migration: read the old local key database and push metadata to the FIDO server.
/// After migration, the old file is deleted. Safe to call multiple times (no-op if file is gone).
fn migrate_old_database(fido_api: &FidoApi) {
    let fs = FileSystem::default();

    // Try to open the old database file
    let mut file = match fs.open_file(OLD_DATABASE_FILE, fs::Location::AppData, fs::OpenFlags::READ_ONLY) {
        Ok(file) => file,
        Err(_) => {
            log::debug!("Migration: no old database found, skipping");
            return;
        }
    };

    // Read file contents
    let mut content = String::new();
    if let Err(e) = file.read_to_string(&mut content) {
        log::warn!("Migration: failed to read old database: {:?}", e);
        return;
    }
    drop(file);

    // Parse the old JSON (plain Vec<OldKey>)
    let old_keys: Vec<OldKey> = match serde_json::from_str(&content) {
        Ok(keys) => keys,
        Err(err) => {
            log::warn!("Migration: failed to parse old database: {}", err);
            return;
        }
    };

    log::debug!("Migration: found old database with {} keys", old_keys.len());

    // edit_security_key and set_archived are now blocking and return Result, so we let the
    // server tell us about missing/invalid indices instead of pre-snapshotting via
    // list_security_keys.
    let mut all_applied = true;
    for key in &old_keys {
        log::debug!(
            "Migration: key[{}] label='{}' color={} archived={} date={}",
            key.key_index,
            key.label,
            key.color,
            key.archived,
            key.date
        );

        if let Err(e) = fido_api.edit_security_key(
            key.key_index,
            key.label.clone(),
            key.color,
            key.icon.clone(),
            key.date,
        ) {
            log::warn!("Migration: edit_security_key for key[{}] failed: {:?}", key.key_index, e);
            all_applied = false;
            continue;
        }
        log::debug!("Migration: pushed metadata for key[{}]", key.key_index);

        if key.archived {
            if let Err(e) = fido_api.set_archived(key.key_index, true) {
                log::warn!("Migration: set_archived for key[{}] failed: {:?}", key.key_index, e);
                all_applied = false;
                continue;
            }
            log::debug!("Migration: set archived for key[{}]", key.key_index);
        }
    }

    if !all_applied {
        // Quarantine the legacy DB instead of leaving it in place. Re-running migration on
        // every app start would re-push the keys that did succeed (wasted IPC round-trips,
        // and once one fails we've shown the failure isn't transient anyway). Renaming with
        // a suffix preserves the data for forensics without re-attempting.
        let failed_path = format!("{OLD_DATABASE_FILE}.failed");
        let _ = fs.remove(failed_path.clone(), fs::Location::AppData);
        match fs.rename(OLD_DATABASE_FILE.to_string(), failed_path.clone(), fs::Location::AppData) {
            Ok(()) => log::warn!(
                "Migration: not all keys could be applied to FIDO server; quarantined legacy DB to {}",
                failed_path
            ),
            Err(e) => log::error!(
                "Migration: not all keys could be applied AND quarantining the legacy DB failed: {:?}",
                e
            ),
        }
        return;
    }

    // edit_security_key and set_archived are blocking, so by the time we reach this point
    // every mutation has been ack'd by the server and persisted; safe to drop the legacy
    // source-of-truth.
    if let Err(e) = fs.remove(OLD_DATABASE_FILE.to_string(), fs::Location::AppData) {
        log::warn!("Migration: failed to delete old database: {:?}", e);
    } else {
        log::debug!("Migration: deleted old database file");
    }

    log::debug!("Migration: complete, migrated {} keys", old_keys.len());
}
