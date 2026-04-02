// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(test))]
use slint_keyos_platform::file_backed::JsonBacked;
use {
    crate::gui_permissions::GuiPermissions,
    fuzzy_filter::FuzzyFilter,
    gui_app_authenticator::{
        get_timestamp_in_seconds,
        google_migration::{self, MigrationError},
        make_import_label, Auth, AuthDuplicateReason, AuthEditField, AuthValidationError, DATABASE_FILE,
    },
    i18n::replace_placeholders,
    ordered_table::{CardSortMode, FilePersistence, OrderedTable, OrderedTableError, SortableCard},
    slint_keyos_platform::{
        app,
        gui_server_api::{
            navigation::qrscanner::{ScanQrOptions, ScanQrResult},
            GuiServerError,
        },
        navigation::open_qr_scanner,
        slint::{Model, ModelRc, SharedString, Timer, TimerMode, VecModel},
        StoredValue,
    },
    std::{collections::HashMap, rc::Rc, time::Duration},
};

use crate::fs_permissions::FileSystemPermissions;

const UPDATE_INTERVAL_MS: u64 = 1000;
const TOTP_TIMESTEP: i32 = 30;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("QR scanning was canceled")]
    ScanQrCanceledError,
    #[error("QR scanning failed")]
    ScanQrFailedError,
    #[error("Could not navigate to QR scanner: {0:?}")]
    NavigateToQrError(GuiServerError),
    #[error("Could not decode QR data: {0:?}")]
    DecodeQrError(std::str::Utf8Error),
    #[error("Could not scan QR, unknown QR action")]
    UnknownQrResultError,
    #[error("No new auth code to save")]
    NoNewAuthError,
    #[error("OrderedTableError: {0:?}")]
    OrderedTableError(OrderedTableError<Auth>),
    #[error("ValidationError: {0:?}")]
    ValidationError(AuthValidationError),
    #[error("DuplicateError: {0:?}")]
    DuplicateError(AuthDuplicateReason),
    #[error("Could not use negative index")]
    IndexError,
    #[error("Could not move auth code to {0:?}, only {1:?} non-archived")]
    MovePositionError(usize, usize),
    #[error("Code {0:?} is already archived")]
    RedundantArchivalError(usize),
    #[error("Migration parse error: {0}")]
    MigrationParseError(String),
    #[error("No pending imports")]
    NoPendingImportsError,
}

impl From<OrderedTableError<Auth>> for AuthError {
    fn from(value: OrderedTableError<Auth>) -> Self { AuthError::OrderedTableError(value) }
}

impl From<AuthValidationError> for AuthError {
    fn from(value: AuthValidationError) -> Self { AuthError::ValidationError(value) }
}

impl From<AuthDuplicateReason> for AuthError {
    fn from(value: AuthDuplicateReason) -> Self { AuthError::DuplicateError(value) }
}

impl From<MigrationError> for AuthError {
    fn from(value: MigrationError) -> Self { AuthError::MigrationParseError(value.to_string()) }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct AuthSettings {
    sort_mode: CardSortMode,
}

impl Default for AuthSettings {
    fn default() -> Self { Self { sort_mode: CardSortMode::Label } }
}

struct AppState {
    auth_table: OrderedTable<Auth, FilePersistence<FileSystemPermissions>>,
    search_text: String,
    new_code: Option<Auth>,
    pending_imports: Option<Vec<Auth>>,
    archive_mode: bool,
    model: Rc<VecModel<AuthView>>,
    #[cfg(not(test))]
    settings: JsonBacked<AuthSettings, FileSystemPermissions>,
    #[cfg(test)]
    sort_mode: CardSortMode,
    last_time: u64,
}

impl AuthView {
    fn new(value: &Auth, time: u64) -> Self {
        Self {
            label: SharedString::from(value.get_label()),
            account: SharedString::from(value.get_account()),
            issuer: SharedString::from(value.get_issuer()),
            color: value.color as i32,
            code: SharedString::from(format_totp_code(&value.get_code(time))),
            index: -1,
        }
    }

    fn with_index(mut self, index: i32) -> Self {
        self.index = index;
        self
    }
}

impl AppState {
    fn get_time_to_refresh(&self) -> i32 {
        let system_time = get_timestamp_in_seconds();
        let time_to_refresh = TOTP_TIMESTEP - (system_time % TOTP_TIMESTEP as u64) as i32;
        time_to_refresh
    }

    fn detect_time_jump(&mut self) -> bool {
        let system_time = get_timestamp_in_seconds();
        let res = self.last_time.abs_diff(system_time) > 1;
        self.last_time = system_time;
        res
    }

    fn get_auth_entries(&self) -> ModelRc<AuthView> {
        self.model.clear();

        let sort_mode = self.get_sort_mode();
        let filter = if self.search_text.is_empty() {
            None
        } else {
            Some(FuzzyFilter::new(self.search_text.as_ref()))
        };

        let time = get_timestamp_in_seconds();

        let entries = self
            .auth_table
            .view_sorted(|a, b| Auth::compare_by(a, b, sort_mode))
            .filter(|(_i, entry)| {
                if entry.archived != self.archive_mode {
                    return false;
                }

                match &filter {
                    Some(filter) if !filter.matches(entry.get_label().to_lowercase().as_ref()) => false,
                    _ => true,
                }
            })
            .map(|(i, entry)| AuthView::new(entry, time).with_index(i as i32))
            .collect::<Vec<AuthView>>();

        self.model.extend(entries);
        ModelRc::from(self.model.clone())
    }

    fn get_sort_mode(&self) -> CardSortMode {
        #[cfg(not(test))]
        return self.settings.sort_mode.clone();
        #[cfg(test)]
        return self.sort_mode;
    }

    #[cfg(not(test))]
    fn set_sort_mode(&mut self, mode: CardSortMode) { self.settings.guard().sort_mode = mode; }

    #[cfg(test)]
    fn set_sort_mode(&mut self, mode: CardSortMode) { self.sort_mode = mode; }
}

trait ToValidationString {
    fn to_validation_string(&self) -> SharedString;
}

impl ToValidationString for AuthError {
    fn to_validation_string(&self) -> SharedString {
        match self {
            AuthError::OrderedTableError(e) => e.to_validation_string(),
            AuthError::ValidationError(e) => e.to_validation_string(),
            AuthError::DuplicateError(e) => e.to_validation_string(),
            AuthError::DecodeQrError(_) => {
                SharedString::from(tr::lookup_id(TrId::MainAdd2FAModalInvalidSecretContent))
            }
            AuthError::UnknownQrResultError => {
                SharedString::from(tr::lookup_id(TrId::MainAdd2FAModalInvalidSecretContent))
            }
            ref other => other.to_string().into(),
        }
    }
}

impl ToValidationString for OrderedTableError<Auth> {
    fn to_validation_string(&self) -> SharedString {
        match self {
            OrderedTableError::PushInvalidError(validation_error) => validation_error.to_validation_string(),
            OrderedTableError::PushDuplicateError((duplicate_reason, _i)) => {
                duplicate_reason.to_validation_string()
            }
            OrderedTableError::EditInvalidOperationError(validation_error) => {
                validation_error.to_validation_string()
            }
            OrderedTableError::EditInvalidResultError(validation_error) => {
                validation_error.to_validation_string()
            }
            OrderedTableError::EditDuplicateError((duplicate_reason, _i)) => {
                duplicate_reason.to_validation_string()
            }
            ref other => {
                log::warn!("{}", self);
                other.to_string().into()
            }
        }
    }
}

impl ToValidationString for AuthValidationError {
    fn to_validation_string(&self) -> SharedString {
        match self {
            AuthValidationError::InvalidLabelError => {
                SharedString::from(tr::lookup_id(TrId::MainAddCodeLabelMissing))
            }
            AuthValidationError::EmptyAccountError => {
                SharedString::from(tr::lookup_id(TrId::MainAddCodeAccountMissing))
            }
            AuthValidationError::InvalidAccountError => {
                SharedString::from(tr::lookup_id(TrId::MainAddCodeSpecialCharacter))
            }
            AuthValidationError::InvalidIssuerError => {
                SharedString::from(tr::lookup_id(TrId::MainAddCodeSpecialCharacter))
            }
            AuthValidationError::InvalidTotpError(_e) => {
                SharedString::from(tr::lookup_id(TrId::MainAdd2FAModalInvalidSecretContent))
            }
            AuthValidationError::InvalidTimestepError(_e) => {
                SharedString::from(tr::lookup_id(TrId::MainAdd2FAModalInvalidTimerContent))
            }
        }
    }
}

impl ToValidationString for AuthDuplicateReason {
    fn to_validation_string(&self) -> SharedString {
        match self {
            AuthDuplicateReason::Label(_other) => {
                SharedString::from(tr::lookup_id(TrId::MainAddCodeLabelAlreadyInUse))
            }
            AuthDuplicateReason::Totp(other) => SharedString::from(replace_placeholders(
                tr::lookup_id(TrId::MainAdd2FAModalAlreadyUsedContent),
                &[other],
            )),
        }
    }
}

impl From<AuthError> for CallbackResult {
    fn from(error: AuthError) -> Self {
        log::warn!("{}", error);
        match error {
            AuthError::ScanQrCanceledError => Self::success(),
            AuthError::OrderedTableError(e) => Self::from(e),
            AuthError::ValidationError(e) => Self::from(e),
            AuthError::DuplicateError(reason) => Self::from(reason),
            // Other AuthErrors should never be seen because they
            // only result from unexpected behavior like system errors
            ref other => {
                Self::failure(ResultLevel::Error, String::from("Error"), other.to_validation_string().into())
            }
        }
    }
}

impl From<OrderedTableError<Auth>> for CallbackResult {
    fn from(error: OrderedTableError<Auth>) -> Self {
        log::warn!("{}", error);
        match error {
            OrderedTableError::PushInvalidError(validation_error) => CallbackResult::from(validation_error),
            OrderedTableError::PushDuplicateError((duplicate_reason, _i)) => {
                CallbackResult::from(duplicate_reason)
            }
            OrderedTableError::EditInvalidOperationError(validation_error) => {
                CallbackResult::from(validation_error)
            }
            OrderedTableError::EditInvalidResultError(validation_error) => {
                CallbackResult::from(validation_error)
            }
            OrderedTableError::EditDuplicateError((duplicate_reason, _i)) => {
                CallbackResult::from(duplicate_reason)
            }
            ref other => Self::failure(ResultLevel::Error, String::from("Error"), other.to_string()),
        }
    }
}

impl From<AuthValidationError> for CallbackResult {
    fn from(error: AuthValidationError) -> Self {
        log::warn!("{}", error);
        match error {
            AuthValidationError::InvalidTotpError(_e) => Self::invalid_totp_error(),
            AuthValidationError::InvalidTimestepError(_step) => Self::invalid_time_period_error(),
            // Other ValidationErrors should never be seen because save buttons
            // are disabled in case of these validation errors.
            ref other => Self::failure(
                ResultLevel::Error,
                String::from("Error"),
                other.to_validation_string().to_string(),
            ),
        }
    }
}

impl From<AuthDuplicateReason> for CallbackResult {
    fn from(reason: AuthDuplicateReason) -> Self {
        log::warn!("{}", reason);
        match reason {
            AuthDuplicateReason::Totp(other) => Self::duplicate_code_error(other),
            // Other DuplicateReasons should never be seen because save buttons
            // are disabled in case of these validation errors.
            ref other => Self::failure(
                ResultLevel::Error,
                String::from("Error"),
                other.to_validation_string().to_string(),
            ),
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

    fn duplicate_code_error(label: String) -> Self {
        Self::failure(
            ResultLevel::Info,
            tr::lookup_id(TrId::MainAdd2FAModalAlreadyUsedTitle).to_string(),
            replace_placeholders(tr::lookup_id(TrId::MainAdd2FAModalAlreadyUsedContent), &[label]),
        )
    }

    fn invalid_totp_error() -> Self {
        Self::failure(
            ResultLevel::Error,
            tr::lookup_id(TrId::MainAdd2FAModalInvalidSecretTitle).to_string(),
            tr::lookup_id(TrId::MainAdd2FAModalInvalidSecretContent).to_string(),
        )
    }

    fn invalid_time_period_error() -> Self {
        Self::failure(
            ResultLevel::Info,
            tr::lookup_id(TrId::MainAdd2FAModalInvalidTimerTitle).to_string(),
            tr::lookup_id(TrId::MainAdd2FAModalInvalidTimerContent).to_string(),
        )
    }

    fn navigate_from_scan_qr(&self, from_edit: bool, ui_nav: Navigate<'_>) {
        if from_edit && self.success {
            // Go back to page before qr scan, no action if not coming from edit
            ui_nav.invoke_backward_animate(Animate::None);
            return;
        }

        if !self.success {
            // Navigate to edit to show the error, replace on the stack if coming from edit
            ui_nav.invoke_edit(
                EditParams {
                    auth: AuthView::default(),
                    label_validation: SharedString::new(),
                    result: self.clone(),
                    version: EditPageVersion::Add,
                },
                NavigateOptions { replace: from_edit, animate: Animate::None },
            );
        }
    }
}

app!("2FA");

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    cx.config.enable_swipe_back.set(false);

    // All errors encountered here are unrecoverable.
    // The app cannot function without auth_table.
    let app_state = AppState {
        auth_table: OrderedTable::new()
            .with_persistence(FilePersistence::new(String::from(DATABASE_FILE), fs::Location::AppData))
            .expect("failed to create authenticator database"),
        search_text: String::new(),
        new_code: None,
        pending_imports: None,
        archive_mode: false,
        model: Rc::new(VecModel::default()),
        #[cfg(not(test))]
        settings: JsonBacked::new("settings.json", fs::Location::AppData).0,
        #[cfg(test)]
        sort_mode: CardSortMode::Label,
        last_time: 0,
    };

    if app_state.auth_table.len() == 0 {
        ui.global::<Navigate>().invoke_add(NavigateOptions { replace: true, animate: Animate::None });
    }

    let ui_state = ui.global::<AuthenticatorCallbacks>();
    ui_state.set_entries(app_state.get_auth_entries());
    ui_state.set_sort_mode(app_state.get_sort_mode() as i32);
    ui_state.set_time_until_refresh(app_state.get_time_to_refresh());

    let app_state = StoredValue::new(app_state);

    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(UPDATE_INTERVAL_MS), {
        let ui = ui.clone_strong();
        move || {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();
            let time_until_refresh = app_state.get_time_to_refresh();
            let time_jump = app_state.detect_time_jump();

            if time_until_refresh == TOTP_TIMESTEP || time_jump {
                ui_state.set_entries(app_state.get_auth_entries());
            }

            ui_state.set_time_until_refresh(time_until_refresh);
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_scan_qr({
        let ui = ui.clone_strong();
        move |caller| {
            let mut app_state = app_state.borrow_mut();
            let ui_nav = ui.global::<Navigate>();
            let ui_callbacks = ui.global::<AuthenticatorCallbacks>();
            let from_edit = caller == ScanQrCaller::Edit;

            let url = match scan_qr_request() {
                Ok(u) => u,
                Err(e) => {
                    CallbackResult::from(e).navigate_from_scan_qr(from_edit, ui_nav);
                    return;
                }
            };

            if google_migration::is_migration_uri(&url) {
                match google_migration::parse_migration_uri(&url) {
                    Ok(entries) => {
                        let count = entries.len();
                        if count == 0 {
                            CallbackResult::from(AuthError::MigrationParseError(
                                "No accounts found".to_string(),
                            ))
                            .navigate_from_scan_qr(from_edit, ui_nav);
                            return;
                        }

                        log::info!("Parsed {} accounts from Google Authenticator migration", count);
                        app_state.pending_imports = Some(entries);
                        ui_callbacks.set_pending_import_count(count as i32);

                        ui_nav.invoke_main(
                            MainParams { version: CardPageVersion::Main },
                            NavigateOptions { replace: true, animate: Animate::None },
                        );
                        return;
                    }
                    Err(e) => {
                        log::warn!("Failed to parse migration URI: {}", e);
                        CallbackResult::from(AuthError::from(e)).navigate_from_scan_qr(from_edit, ui_nav);
                        return;
                    }
                }
            }

            match adapt_scan_qr(url, &mut app_state) {
                Ok((auth, label_validation)) => {
                    ui_nav.invoke_edit(
                        EditParams {
                            auth,
                            label_validation,
                            result: CallbackResult::success(),
                            version: EditPageVersion::Add,
                        },
                        NavigateOptions { replace: from_edit, animate: Animate::None },
                    );
                }
                Err(e) => {
                    CallbackResult::from(e).navigate_from_scan_qr(from_edit, ui_nav);
                    return;
                }
            }
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_search({
        let ui = ui.clone_strong();
        move |text| {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();
            app_state.search_text = text.to_string().to_lowercase();
            ui_state.set_entries(app_state.get_auth_entries());
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_import_multiple({
        let ui = ui.clone_strong();
        move || {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();

            if let Err(e) = adapt_import_multiple(&mut app_state) {
                log::warn!("Import multiple failed: {}", e);
            }

            ui_state.set_entries(app_state.get_auth_entries());
            ui_state.set_pending_import_count(0);
            CallbackResult::success()
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_cancel_import_multiple({
        move || {
            let mut app_state = app_state.borrow_mut();
            app_state.pending_imports = None;
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_save({
        let ui = ui.clone_strong();
        move |label, account, issuer, color| {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();
            let ui_nav = ui.global::<Navigate>();

            if let Err(e) = adapt_save(label, account, issuer, color, &mut app_state) {
                return CallbackResult::from(e);
            };

            ui_state.set_entries(app_state.get_auth_entries());
            ui_nav.invoke_backward_animate(Animate::None);
            ui_nav.invoke_main(
                MainParams { version: CardPageVersion::Main },
                NavigateOptions { replace: true, animate: Animate::None },
            );
            CallbackResult::success()
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_validate_new_label({
        move |label| {
            let mut app_state = app_state.borrow_mut();

            if let Err(e) = adapt_validate_new_label(label, &mut app_state) {
                return e.to_validation_string();
            };

            SharedString::new()
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_validate_edit_label({
        move |index, label| {
            let app_state = app_state.borrow_mut();

            let Ok(index) = usize::try_from(index) else {
                return AuthError::IndexError.to_validation_string();
            };

            if let Err(e) = app_state
                .auth_table
                .validate_edit(index, move |a| a.edit(AuthEditField::Label(label.clone().into())))
            {
                return e.to_validation_string();
            };

            SharedString::new()
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_validate_edit_account({
        move |account| {
            if let Err(e) = AuthEditField::Account(account.into()).validate() {
                return e.to_validation_string();
            }

            SharedString::new()
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_validate_edit_issuer({
        move |issuer| {
            if let Err(e) = AuthEditField::Issuer(issuer.into()).validate() {
                return e.to_validation_string();
            }

            SharedString::new()
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_edit({
        let ui = ui.clone_strong();
        move |index, label, account, issuer, color| {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();

            let Ok(index) = usize::try_from(index) else {
                return CallbackResult::from(AuthError::IndexError);
            };

            if let Err(e) = app_state.auth_table.edit(index, move |a| {
                a.edit(AuthEditField::Label(label.clone().into()))?;
                a.edit(AuthEditField::Account(account.clone().into()))?;
                a.edit(AuthEditField::Issuer(issuer.clone().into()))?;
                a.color = color as u8;
                Ok(())
            }) {
                return CallbackResult::from(e);
            }

            ui_state.set_entries(app_state.get_auth_entries());
            CallbackResult::success()
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_set_archived({
        let ui = ui.clone_strong();
        move |index, archived| {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();

            if let Err(e) = adapt_set_archived(index, archived, &mut app_state) {
                log::warn!("{}", e);
                return;
            }

            ui_state.set_entries(app_state.get_auth_entries());
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_set_archive_mode({
        let ui = ui.clone_strong();
        move |archive_mode| {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();
            app_state.archive_mode = archive_mode;
            ui_state.set_entries(app_state.get_auth_entries());
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_delete_code({
        let ui = ui.clone_strong();
        move |index| {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();

            let Ok(index) = usize::try_from(index) else {
                log::warn!("{}", AuthError::IndexError.to_validation_string());
                return;
            };

            if let Err(e) = app_state.auth_table.remove(index) {
                log::warn!("{}", e);
                return;
            }

            ui_state.set_entries(app_state.get_auth_entries());
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_get_view_index({
        move |entries, source_index| match entries.iter().position(|entry| entry.index == source_index) {
            Some(i) => i as i32,
            None => {
                log::warn!("Could not find index of code that should exist");
                0
            }
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_move_position({
        let ui = ui.clone_strong();
        move |index, up| {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();

            if let Err(e) = adapt_move_position(index, up, &mut app_state) {
                log::warn!("{}", e);
                return;
            }

            ui_state.set_entries(app_state.get_auth_entries());
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_set_sort_mode({
        let ui = ui.clone_strong();
        move |sort_mode| {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();
            app_state.set_sort_mode(CardSortMode::from(sort_mode as usize));
            ui_state.set_entries(app_state.get_auth_entries());
        }
    });

    ui.run().expect("UI running");
}

// TODO: unit test these functions?
fn adapt_scan_qr(url: String, app_state: &mut AppState) -> Result<(AuthView, SharedString), AuthError> {
    let time = get_timestamp_in_seconds();
    let auth = Auth::new(url, time)?;

    let label_validation = match app_state.auth_table.validate_push(&auth) {
        Ok(_) => SharedString::new(),
        Err(OrderedTableError::PushDuplicateError((reason, _))) => match reason {
            AuthDuplicateReason::Totp(_) => return Err(AuthError::from(reason)),
            AuthDuplicateReason::Label(_) => reason.to_validation_string(),
        },
        Err(OrderedTableError::PushInvalidError(e)) => e.to_validation_string(),
        Err(e) => return Err(AuthError::OrderedTableError(e)),
    };

    let auth_view = AuthView::new(&auth, time);
    app_state.new_code = Some(auth);

    Ok((auth_view, label_validation))
}

fn adapt_save(
    label: SharedString,
    account: SharedString,
    issuer: SharedString,
    color: i32,
    app_state: &mut AppState,
) -> Result<(), AuthError> {
    let mut auth = adapt_validate_new_label(label, app_state)?;
    auth.edit(AuthEditField::Account(account.into()))?;
    auth.edit(AuthEditField::Issuer(issuer.into()))?;
    auth.color = color as u8;
    app_state.auth_table.separate_categories(|a| a.get_category());
    app_state.auth_table.push_categorized(|a| a.get_category(), auth)?;
    Ok(())
}

fn adapt_validate_new_label(label: SharedString, app_state: &mut AppState) -> Result<Auth, AuthError> {
    let label: String = label.into();
    let mut auth = app_state.new_code.clone().ok_or(AuthError::NoNewAuthError)?;
    auth.edit(AuthEditField::Label(label))?;
    let duplicate_opt = app_state.auth_table.find(&auth)?;

    if let Some((reason, _i)) = duplicate_opt {
        return Err(AuthError::from(reason));
    };

    Ok(auth)
}

fn adapt_set_archived(index: i32, archived: bool, app_state: &mut AppState) -> Result<(), AuthError> {
    let Ok(index) = usize::try_from(index) else {
        return Err(AuthError::IndexError);
    };

    if app_state.auth_table.get(index)?.archived == archived {
        return Err(AuthError::RedundantArchivalError(index));
    }

    app_state.auth_table.edit(index, |a| {
        a.archived = archived;
        Ok(())
    })?;

    app_state.auth_table.separate_categories(|a| a.get_category());
    Ok(())
}

fn adapt_move_position(index: i32, up: bool, app_state: &mut AppState) -> Result<(), AuthError> {
    let Ok(destination) = usize::try_from(index + if up { -1 } else { 1 }) else {
        return Err(AuthError::IndexError);
    };

    let Ok(index) = usize::try_from(index) else {
        return Err(AuthError::IndexError);
    };

    // OrderedTable returns errors safely for underflows
    app_state.auth_table.move_position_categorized(|a| a.get_category(), index, destination)?;
    Ok(())
}

fn adapt_import_multiple(app_state: &mut AppState) -> Result<(), AuthError> {
    let entries = app_state.pending_imports.take().ok_or(AuthError::NoPendingImportsError)?;

    let mut existing_label_counts: HashMap<String, usize> = HashMap::new();

    for auth in app_state.auth_table.iter() {
        let label = auth.get_label().to_string();
        *existing_label_counts.entry(label).or_insert(0) += 1;
    }

    for entry in &entries {
        *existing_label_counts.entry(entry.get_label().clone()).or_insert(0) += 1;
    }

    let mut import_label_counts: HashMap<String, usize> = HashMap::new();

    for entry in entries {
        let original_label = entry.get_label().to_string();

        let needs_import_prefix =
            existing_label_counts.get(&original_label).map(|count| *count > 1).unwrap_or_default();

        let label = if needs_import_prefix {
            make_import_label(&original_label, &mut import_label_counts)
        } else {
            original_label.clone()
        };

        log::info!("Importing label {}", label);

        let mut import_auth = entry;
        import_auth.edit(AuthEditField::Label(label))?;
        import_auth.color = 0;

        app_state.auth_table.separate_categories(|a| a.get_category());
        if let Err(e) = app_state.auth_table.push_categorized(|a| a.get_category(), import_auth) {
            log::warn!("Failed to import entry: {:?}", e);
        } else if needs_import_prefix {
            *import_label_counts.entry(original_label).or_insert(0) += 1;
        }
    }

    Ok(())
}

fn scan_qr_request() -> Result<String, AuthError> {
    log::debug!("Scanning a TOTP QR code");
    let opt = open_qr_scanner::<GuiPermissions>(ScanQrOptions::default())
        .map_err(|e| AuthError::NavigateToQrError(e))?;
    let nav_res = opt.ok_or(AuthError::ScanQrFailedError)?;

    let data = match nav_res {
        ScanQrResult::Qr(d) => d,
        ScanQrResult::LeftClicked => return Err(AuthError::ScanQrCanceledError),
        _ => return Err(AuthError::UnknownQrResultError),
    };

    let url = std::str::from_utf8(data.as_slice()).map_err(|e| AuthError::DecodeQrError(e))?;
    Ok(String::from(url))
}

fn format_totp_code(code: &str) -> String {
    code.chars().enumerate().fold(String::with_capacity(code.len() * 2), |mut result, (i, c)| {
        if i == code.len() / 2 {
            result.push_str("  "); // Add two spaces after the left block
            result.push(c);
        } else if i > 0 {
            result.push(' '); // Add one space after other digits
            result.push(c);
        } else {
            result.push(c); // First digit, no space
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL1: &str = "otpauth://totp/Example:alice@google.com?secret=JBSWY3DPEHPK3PXP&issuer=Example";
    const URL2: &str = "otpauth://totp/Example:alice@google.com?secret=ABSWY3DPEHPK3PXP&issuer=Example";
    const URL3: &str = "otpauth://totp/D:alice@google.com?secret=BBSWY3DPEHPK3PXP&issuer=D";
    const URL4: &str = "otpauth://totp/C:alice@google.com?secret=CBSWY3DPEHPK3PXP&issuer=C";
    const URL5: &str = "otpauth://totp/B:alice@google.com?secret=DBSWY3DPEHPK3PXP&issuer=B";
    const URL_INVALID: &str =
        "otpauth://totp/Exam:ple:alice@google.com?secret=ABSWY3DPEHPK3PXP&issuer=Exam:ple";

    fn app_state0() -> AppState {
        AppState {
            auth_table: OrderedTable::new(),
            search_text: String::new(),
            new_code: None,
            pending_imports: None,
            archive_mode: false,
            model: Rc::new(VecModel::default()),
            sort_mode: CardSortMode::Label,
            last_time: 0,
        }
    }

    fn app_state1() -> AppState {
        let mut app_state = app_state0();
        let auth = Auth::new(String::from(URL1), 0).unwrap();
        app_state.auth_table.push(auth).unwrap();
        app_state
    }

    fn app_state3() -> AppState {
        let mut app_state = app_state0();
        let auth = Auth::new(String::from(URL3), 0).unwrap();
        app_state.auth_table.push(auth).unwrap();
        let auth = Auth::new(String::from(URL4), 0).unwrap();
        app_state.auth_table.push(auth).unwrap();
        let auth = Auth::new(String::from(URL5), 0).unwrap();
        app_state.auth_table.push(auth).unwrap();
        app_state
    }

    #[test]
    fn test_adapt_save() {
        let mut app_state = app_state3();
        adapt_set_archived(1, true, &mut app_state).unwrap();
        let desired_auth = Auth::new(String::from(URL1), 0).unwrap();
        app_state.new_code = Some(desired_auth.clone());
        adapt_save(
            SharedString::from("Example"),
            SharedString::from("alice@google.com"),
            SharedString::from("Example"),
            0,
            &mut app_state,
        )
        .unwrap();
        assert_eq!(Auth::new(String::from(URL1), 0).unwrap(), app_state.auth_table.get(2).unwrap().clone());
    }

    #[test]
    fn test_adapt_scan_qr() {
        let mut app_state = app_state0();
        let url = String::from(URL1);
        let res = adapt_scan_qr(url.clone(), &mut app_state).unwrap();
        let desired_auth = Auth::new(url, 0).unwrap();
        let desired_auth_view = AuthView::new(&desired_auth, 0);
        assert_eq!(res, (desired_auth_view, SharedString::new()));
        assert_eq!(app_state.new_code.unwrap(), desired_auth);
    }

    #[test]
    fn test_adapt_scan_qr_duplicate_totp() {
        let mut app_state = app_state1();
        let url = String::from(URL1);
        let res = adapt_scan_qr(url.clone(), &mut app_state).unwrap_err();
        match res {
            AuthError::DuplicateError(AuthDuplicateReason::Totp(other))
                if other == String::from("Example") =>
            {
                ()
            }
            _ => panic!("Failed with wrong error: {}", res),
        }
        assert!(app_state.new_code.is_none());
    }

    #[test]
    fn test_adapt_scan_qr_duplicate_label() {
        let mut app_state = app_state1();
        let url = String::from(URL2);
        let res = adapt_scan_qr(url.clone(), &mut app_state).unwrap();
        let desired_auth = Auth::new(url, 0).unwrap();
        let desired_auth_view = AuthView::new(&desired_auth, 0);
        assert_eq!(
            res,
            (desired_auth_view, SharedString::from(tr::lookup_id(TrId::MainAddCodeLabelAlreadyInUse)))
        );
        assert_eq!(app_state.new_code.unwrap(), desired_auth);
    }

    #[test]
    fn test_adapt_scan_qr_invalid_totp() {
        let mut app_state = app_state0();
        let url = String::from(URL_INVALID);
        let res = adapt_scan_qr(url.clone(), &mut app_state).unwrap_err();
        match res {
            AuthError::ValidationError(AuthValidationError::InvalidTotpError(_)) => (),
            _ => panic!("Failed with wrong error: {}", res),
        }
        assert!(app_state.new_code.is_none());
    }

    #[test]
    fn test_adapt_validate_new_label() {
        let label = SharedString::from("Example");
        let mut app_state = app_state0();
        let desired_auth = Auth::new(String::from(URL1), 0).unwrap();
        app_state.new_code = Some(desired_auth.clone());
        let auth = adapt_validate_new_label(label, &mut app_state).unwrap();
        assert_eq!(auth, desired_auth);
    }

    #[test]
    fn test_adapt_validate_new_label_duplicate() {
        let label = SharedString::from("Example");
        let mut app_state = app_state1();
        let desired_auth = Auth::new(String::from(URL2), 0).unwrap();
        app_state.new_code = Some(desired_auth.clone());
        let res = adapt_validate_new_label(label, &mut app_state).unwrap_err();
        match res {
            AuthError::DuplicateError(AuthDuplicateReason::Label(other))
                if other == String::from("Example") =>
            {
                ()
            }
            _ => panic!("Failed with wrong error: {}", res),
        }
    }

    #[test]
    fn test_adapt_validate_new_label_empty() {
        let label = SharedString::from("");
        let mut app_state = app_state1();
        let desired_auth = Auth::new(String::from(URL2), 0).unwrap();
        app_state.new_code = Some(desired_auth.clone());
        let res = adapt_validate_new_label(label, &mut app_state).unwrap_err();
        match res {
            AuthError::ValidationError(AuthValidationError::InvalidLabelError) => (),
            _ => panic!("Failed with wrong error: {}", res),
        }
    }

    #[test]
    fn test_adapt_set_archived() {
        // Test that archived items are separated from active items
        let mut app_state = app_state3();
        adapt_set_archived(1, true, &mut app_state).unwrap();

        assert_eq!(Auth::new(String::from(URL3), 0).unwrap(), app_state.auth_table.get(0).unwrap().clone());
        assert_eq!(Auth::new(String::from(URL5), 0).unwrap(), app_state.auth_table.get(1).unwrap().clone());
        let mut archived_auth = Auth::new(String::from(URL4), 0).unwrap();
        archived_auth.archived = true;
        assert_eq!(archived_auth, app_state.auth_table.get(2).unwrap().clone());

        // Set up for last test
        adapt_set_archived(1, true, &mut app_state).unwrap();
        let mut archived_auth_2 = Auth::new(String::from(URL5), 0).unwrap();
        assert_eq!(Auth::new(String::from(URL3), 0).unwrap(), app_state.auth_table.get(0).unwrap().clone());
        archived_auth_2.archived = true;
        assert_eq!(archived_auth_2, app_state.auth_table.get(1).unwrap().clone());
        assert_eq!(archived_auth, app_state.auth_table.get(2).unwrap().clone());

        // Test that restored items go to end of active items
        adapt_set_archived(2, false, &mut app_state).unwrap();
        assert_eq!(Auth::new(String::from(URL3), 0).unwrap(), app_state.auth_table.get(0).unwrap().clone());
        assert_eq!(Auth::new(String::from(URL4), 0).unwrap(), app_state.auth_table.get(1).unwrap().clone());
        assert_eq!(archived_auth_2, app_state.auth_table.get(2).unwrap().clone());
    }

    #[test]
    fn test_adapt_set_archived_negative_index() {
        let mut app_state = app_state3();
        let err = adapt_set_archived(-1, true, &mut app_state).unwrap_err();
        match err {
            AuthError::IndexError => (),
            _ => panic!("Failed with wrong error: {}", err),
        }
    }

    #[test]
    fn test_adapt_set_archived_out_of_bounds() {
        let mut app_state = app_state3();
        let err = adapt_set_archived(3, true, &mut app_state).unwrap_err();
        match err {
            AuthError::OrderedTableError(OrderedTableError::OutOfBoundsError(i, l)) if i == l && l == 3 => (),
            _ => panic!("Failed with wrong error: {}", err),
        }
    }

    #[test]
    fn test_adapt_set_archived_redundant() {
        let mut app_state = app_state3();
        adapt_set_archived(2, true, &mut app_state).unwrap();
        let err = adapt_set_archived(2, true, &mut app_state).unwrap_err();
        match err {
            AuthError::RedundantArchivalError(i) if i == 2 => (),
            _ => panic!("Failed with wrong error: {}", err),
        }
    }

    #[test]
    fn test_adapt_move_position() {
        let mut app_state = app_state3();
        adapt_move_position(0, false, &mut app_state).unwrap();
        assert_eq!(Auth::new(String::from(URL4), 0).unwrap(), app_state.auth_table.get(0).unwrap().clone());
        assert_eq!(Auth::new(String::from(URL3), 0).unwrap(), app_state.auth_table.get(1).unwrap().clone());
        assert_eq!(Auth::new(String::from(URL5), 0).unwrap(), app_state.auth_table.get(2).unwrap().clone());

        adapt_move_position(1, true, &mut app_state).unwrap();
        assert_eq!(Auth::new(String::from(URL3), 0).unwrap(), app_state.auth_table.get(0).unwrap().clone());
        assert_eq!(Auth::new(String::from(URL4), 0).unwrap(), app_state.auth_table.get(1).unwrap().clone());
        assert_eq!(Auth::new(String::from(URL5), 0).unwrap(), app_state.auth_table.get(2).unwrap().clone());
    }

    #[test]
    fn test_adapt_move_position_with_archive() {
        let mut app_state = app_state3();
        adapt_set_archived(2, true, &mut app_state).unwrap();

        assert_eq!(Auth::new(String::from(URL3), 0).unwrap(), app_state.auth_table.get(0).unwrap().clone());
        assert_eq!(Auth::new(String::from(URL4), 0).unwrap(), app_state.auth_table.get(1).unwrap().clone());
        let mut archived_auth = Auth::new(String::from(URL5), 0).unwrap();
        archived_auth.archived = true;
        assert_eq!(archived_auth, app_state.auth_table.get(2).unwrap().clone());

        let err = adapt_move_position(1, false, &mut app_state).unwrap_err();
        match err {
            AuthError::OrderedTableError(OrderedTableError::CategoryOutOfBoundsError(..)) => (),
            _ => panic!("Failed with wrong error: {}", err),
        }
    }

    #[test]
    fn test_adapt_move_position_negative_destination() {
        let mut app_state = app_state3();
        let err = adapt_move_position(0, true, &mut app_state).unwrap_err();
        match err {
            AuthError::IndexError => (),
            _ => panic!("Failed with wrong error: {}", err),
        }
    }

    #[test]
    fn test_adapt_move_position_negative_index() {
        let mut app_state = app_state3();
        let err = adapt_move_position(-1, false, &mut app_state).unwrap_err();
        match err {
            AuthError::IndexError => (),
            _ => panic!("Failed with wrong error: {}", err),
        }
    }

    #[test]
    fn test_format_totp_code_6() {
        let res = format_totp_code("123456");
        assert_eq!(res, String::from("1 2 3  4 5 6"));
    }

    #[test]
    fn test_format_totp_code_8() {
        let res = format_totp_code("12345678");
        assert_eq!(res, String::from("1 2 3 4  5 6 7 8"));
    }

    #[test]
    fn test_get_auth_entries() {
        let app_state = app_state3();
        // Default sort mode should be Label
        assert_eq!(app_state.sort_mode, CardSortMode::Label);
        // Default archive mode should be false
        assert!(!app_state.archive_mode);
        let model = app_state.get_auth_entries();
        assert_eq!(
            model.row_data(0).unwrap(),
            AuthView::new(app_state.auth_table.get(2).unwrap(), 0).with_index(2)
        );
        assert_eq!(
            model.row_data(1).unwrap(),
            AuthView::new(app_state.auth_table.get(1).unwrap(), 0).with_index(1)
        );
        assert_eq!(
            model.row_data(2).unwrap(),
            AuthView::new(app_state.auth_table.get(0).unwrap(), 0).with_index(0)
        );
    }

    #[test]
    fn test_get_auth_entries_search() {
        let mut app_state = app_state3();
        app_state.search_text = String::from("c");
        let model = app_state.get_auth_entries();
        assert_eq!(
            model.row_data(0).unwrap(),
            AuthView::new(app_state.auth_table.get(1).unwrap(), 0).with_index(1)
        );
    }

    #[test]
    fn test_get_auth_entries_remove_archived() {
        let mut app_state = app_state3();
        adapt_set_archived(1, true, &mut app_state).unwrap();
        let model = app_state.get_auth_entries();
        assert_eq!(
            model.row_data(0).unwrap(),
            AuthView::new(app_state.auth_table.get(1).unwrap(), 0).with_index(1)
        );
        assert_eq!(
            model.row_data(1).unwrap(),
            AuthView::new(app_state.auth_table.get(0).unwrap(), 0).with_index(0)
        );
    }

    #[test]
    fn test_get_auth_entries_view_archive() {
        let mut app_state = app_state3();
        adapt_set_archived(1, true, &mut app_state).unwrap();
        app_state.archive_mode = true;
        let model = app_state.get_auth_entries();
        assert_eq!(
            model.row_data(0).unwrap(),
            AuthView::new(app_state.auth_table.get(2).unwrap(), 0).with_index(2)
        );
    }
}
