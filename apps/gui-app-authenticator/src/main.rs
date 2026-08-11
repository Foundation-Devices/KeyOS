// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
//
// SPDX-FileCopyrightText: 2026 immz https://github.com/immz4
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(test))]
use std::{io::Read, path::Path};

#[cfg(not(test))]
use anyhow::Context;
#[cfg(not(test))]
use slint_keyos_platform::file_backed::JsonBacked;
#[cfg(not(test))]
use slint_keyos_platform::{
    gui_server_api::navigation::filepicker::{self, SelectFileOptions},
    navigation::select_file,
};
use {
    crate::aegis::{AegisError, ParsedAegisExport},
    crate::gui_permissions::GuiPermissions,
    crate::twofas::{ParsedTwoFasExport, TwoFasError},
    auth::{
        get_timestamp_in_seconds, make_import_label, Auth, AuthDuplicateReason, AuthEditField,
        AuthValidationError, DATABASE_FILE,
    },
    fuzzy_filter::FuzzyFilter,
    google_migration::MigrationError,
    i18n::replace_placeholders,
    ordered_table::{CardSortMode, FilePersistence, OrderedTable, OrderedTableError, SortableCard},
    slint_keyos_platform::{
        app,
        gui_server_api::{
            navigation::qrscanner::{MatchedQrResult, ScanQrOptions, ScanQrResult},
            GuiServerError, InputMessage,
        },
        navigation::open_qr_scanner,
        sleep,
        slint::{Model, ModelRc, SharedString, Timer, TimerMode, VecModel},
        spawn_local, spawn_worker, StoredValue,
    },
    std::{collections::HashSet, rc::Rc, time::Duration},
};

use crate::fs_permissions::FileSystemPermissions;
pub mod aegis;
mod auth;
pub mod google_migration;
mod import_crypto;
pub mod kdf;
pub mod proton;
pub mod twofas;

crypto::use_api!();

const UPDATE_INTERVAL_MS: u64 = 1000;
const TOTP_TIMESTEP: i32 = 30;
#[cfg(not(test))]
const MAX_IMPORT_FILE_SIZE_BYTES: u64 = 4 * 1024 * 1024;

enum ScanImportInput {
    Url(String),
    File { bytes: Vec<u8>, display_name: String },
}

fn reset_decrypt_ui_state(ui: &AppWindow) {
    let ui_state = ui.global::<AuthenticatorCallbacks>();
    ui_state.set_show_decrypt_cancel(false);
    ui_state.set_decrypt_password(String::new().into());
    ui_state.set_decrypt_password_failed(false);
    ui_state.set_decrypt_generic_failure(false);
}

fn reset_decrypt_status(ui: &AppWindow) {
    let ui_state = ui.global::<AuthenticatorCallbacks>();
    ui_state.set_show_decrypt_cancel(false);
    ui_state.set_decrypt_password_failed(false);
    ui_state.set_decrypt_generic_failure(false);
}

fn clear_decrypt_job(
    ui: &AppWindow,
    app_state: &mut AppState,
    drop_pending_import: bool,
    clear_password: bool,
) {
    let ui_state = ui.global::<AuthenticatorCallbacks>();
    if drop_pending_import {
        app_state.pending_decrypt_import = None;
    }
    if drop_pending_import {
        ui_state.set_decrypt_password(String::new().into());
    }
    app_state.invalidate_decrypt_job();
    app_state.decrypt_cancel_timer.stop();
    ui_state.set_main_result(CallbackResult::success());
    ui_state.set_decrypting(false);
    if clear_password {
        reset_decrypt_ui_state(ui);
    } else {
        reset_decrypt_status(ui);
    }
}

#[derive(Debug, Clone, Copy)]
enum ImportFileFormat {
    AegisJson,
    AegisUri,
    ProtonCsv,
    ProtonAuthenticatorJson,
    ProtonZipJson,
    ProtonZipPgp,
    TwoFasJson,
}

#[derive(Debug, Clone, Copy)]
enum DecryptType {
    Aegis,
    OpenPgp,
    ProtonAuthenticator,
    TwoFas,
}

#[derive(Clone)]
struct DetectedImportFile {
    // Leaving format in for future debug needs
    #[allow(unused)]
    format: ImportFileFormat,
    decrypt_type: Option<DecryptType>,
    payload: ImportPayload,
}

#[derive(Clone)]
struct PendingDecryptImport {
    display_name: String,
    detected: DetectedImportFile,
}

#[derive(Clone)]
enum ImportPayload {
    AegisJsonPlain(Vec<u8>),
    AegisJsonEncrypted(aegis::EncryptedExport),
    AegisUri(Vec<u8>),
    ProtonCsv(Vec<u8>),
    ProtonAuthenticatorJsonPlain(Vec<u8>),
    ProtonAuthenticatorJsonEncrypted(proton::ProtonAuthenticatorEncryptedExport),
    ProtonZipJson(Vec<u8>),
    ProtonZipPgp(Vec<u8>),
    TwoFasJsonPlain(Vec<u8>),
    TwoFasJsonEncrypted(twofas::EncryptedExport),
}

impl DetectedImportFile {
    fn into_pending_decrypt(self, display_name: String) -> Result<PendingDecryptImport, AuthError> {
        if self.decrypt_type.is_none() {
            return Err(AuthError::UnsupportedImportFile(anyhow::anyhow!(
                "Import file does not require decryption"
            )));
        }

        Ok(PendingDecryptImport { display_name, detected: self })
    }
}

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
    #[error("Unsupported import file: {0}")]
    UnsupportedImportFile(anyhow::Error),
    #[error("Import decryption password did not match")]
    DecryptPasswordMismatch,
    #[error("Aegis import error: {0}")]
    AegisImportError(#[from] AegisError),
    #[error("No pending imports")]
    NoPendingImportsError,
    #[error("No pending encrypted import")]
    NoPendingEncryptedImportError,
    #[error("{0:?}")]
    GenericError(#[from] anyhow::Error),
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

fn log_import_error_redacted(prefix: &str, error: &AuthError) {
    let category = match error {
        AuthError::UnsupportedImportFile(_) => "unsupported import file",
        AuthError::DecryptPasswordMismatch => "decrypt password mismatch",
        AuthError::AegisImportError(_) => "Aegis import error",
        AuthError::GenericError(_) => "generic import error",
        AuthError::ValidationError(_) => "validation error",
        AuthError::DuplicateError(_) => "duplicate entry",
        AuthError::MigrationParseError(_) => "migration parse error",
        _ => "internal import error",
    };
    log::warn!("{prefix}: {category}");
}

#[cfg_attr(test, allow(dead_code))]
#[derive(serde::Serialize, serde::Deserialize)]
struct AuthSettings {
    sort_mode: CardSortMode,
}

impl Default for AuthSettings {
    fn default() -> Self { Self { sort_mode: CardSortMode::Label } }
}

struct AppState {
    auth_table: OrderedTable<Auth, FilePersistence<FileSystemPermissions>>,
    crypto: CryptoApi,
    search_text: String,
    new_code: Option<Auth>,
    pending_imports: Option<Vec<Auth>>,
    pending_decrypt_import: Option<PendingDecryptImport>,
    decrypt_pending: bool,
    decrypt_cancel_timer: Timer,
    current_decrypt_job_id: u64,
    archive_mode: bool,
    model: Rc<VecModel<AuthView>>,
    #[cfg(not(test))]
    settings: JsonBacked<AuthSettings, FileSystemPermissions>,
    #[cfg(test)]
    sort_mode: CardSortMode,
    last_time: u64,
    #[cfg(not(test))]
    fs: FileSystem,
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
    fn invalidate_decrypt_job(&mut self) {
        if self.decrypt_pending {
            let previous_job_id = self.current_decrypt_job_id;
            self.current_decrypt_job_id = self.current_decrypt_job_id.wrapping_add(1);
            self.decrypt_pending = false;
            log::info!(
                "Decrypt job advanced from {} to {} during cancellation/invalidation",
                previous_job_id,
                self.current_decrypt_job_id
            );
        }
    }

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
            AuthError::UnsupportedImportFile(_) => {
                SharedString::from(tr::lookup_id(TrId::ImportFileModalUnsupportedContent))
            }
            AuthError::DecryptPasswordMismatch => {
                SharedString::from(tr::lookup_id(TrId::DecryptFileFailedToUnlockFile))
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

    fn unsupported_import_file_error() -> Self {
        Self::failure(
            ResultLevel::Info,
            tr::lookup_id(TrId::ImportFileModalUnsupportedHeader).to_string(),
            tr::lookup_id(TrId::ImportFileModalUnsupportedContent).to_string(),
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
        crypto: CryptoApi::default(),
        search_text: String::new(),
        new_code: None,
        pending_imports: None,
        pending_decrypt_import: None,
        decrypt_pending: false,
        decrypt_cancel_timer: Timer::default(),
        current_decrypt_job_id: 0,
        archive_mode: false,
        model: Rc::new(VecModel::default()),
        #[cfg(not(test))]
        settings: JsonBacked::new("settings.json", fs::Location::AppData).0,
        #[cfg(test)]
        sort_mode: CardSortMode::Label,
        last_time: 0,
        #[cfg(not(test))]
        fs: Default::default(),
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
            let from_edit = caller == ScanQrCaller::Edit;

            let import_input = match scan_qr_request(app_state) {
                Ok(input) => input,
                Err(e) => {
                    if matches!(e, AuthError::UnsupportedImportFile(_)) {
                        ui.global::<AuthenticatorCallbacks>()
                            .set_main_result(CallbackResult::unsupported_import_file_error());
                    } else {
                        let ui_nav = ui.global::<Navigate>();
                        CallbackResult::from(e).navigate_from_scan_qr(from_edit, ui_nav);
                    }
                    return;
                }
            };

            let mut app_state = app_state.borrow_mut();
            match import_input {
                ScanImportInput::Url(url) => app_state.handle_scanned_url(url, from_edit, &ui),
                ScanImportInput::File { bytes, display_name } => {
                    app_state.handle_import_file(bytes, display_name, from_edit, &ui)
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
            ui_state.set_main_result(CallbackResult::success());
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
            app_state.discard_pending_imports();
            ui_state.set_pending_import_count(0);
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_cancel_import_multiple({
        let ui = ui.clone_strong();
        move || {
            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();
            app_state.discard_pending_imports();
            app_state.pending_decrypt_import = None;
            ui_state.set_pending_import_count(0);
            ui_state.set_main_result(CallbackResult::success());
            ui_state.set_decrypting(false);
            reset_decrypt_ui_state(&ui);
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_decrypt_file({
        let ui = ui.clone_strong();
        move |password| {
            let password = password.to_string();
            let (crypto, pending_import, decrypt_job_id) = {
                let mut app_state = app_state.borrow_mut();
                let ui_state = ui.global::<AuthenticatorCallbacks>();

                if app_state.decrypt_pending || ui_state.get_decrypting() {
                    return;
                }

                ui_state.set_decrypt_password_failed(false);
                ui_state.set_decrypt_generic_failure(false);

                let Some(pending_import) = app_state.pending_decrypt_import.clone() else {
                    ui_state.set_decrypt_generic_failure(true);
                    return;
                };

                app_state.decrypt_pending = true;
                app_state.current_decrypt_job_id = app_state.current_decrypt_job_id.wrapping_add(1);
                let decrypt_job_id = app_state.current_decrypt_job_id;
                ui_state.set_decrypting(true);
                ui_state.set_show_decrypt_cancel(false);
                app_state.decrypt_cancel_timer.stop();
                app_state.decrypt_cancel_timer.start(TimerMode::SingleShot, Duration::from_secs(5), {
                    let ui = ui.clone_strong();
                    move || {
                        let ui_state = ui.global::<AuthenticatorCallbacks>();
                        if ui_state.get_decrypting() {
                            ui_state.set_show_decrypt_cancel(true);
                        }
                    }
                });
                (app_state.crypto.clone(), pending_import, decrypt_job_id)
            };

            let ui = ui.clone_strong();
            spawn_local(async move {
                // Yield once so the decrypt page can update before starting work
                sleep(Duration::from_millis(1)).await;

                let result =
                    spawn_worker(async move { adapt_decrypt_import(crypto, pending_import, password).await })
                        .await;
                let ui_state = ui.global::<AuthenticatorCallbacks>();
                let mut app_state = app_state.borrow_mut();

                if decrypt_job_id != app_state.current_decrypt_job_id {
                    log::info!("Ignoring stale decrypt result for superseded import job {}", decrypt_job_id);
                    return;
                }
                app_state.decrypt_pending = false;
                app_state.decrypt_cancel_timer.stop();

                ui_state.set_decrypting(false);
                reset_decrypt_ui_state(&ui);

                match result {
                    Ok(entries) => {
                        app_state.pending_decrypt_import = None;
                        app_state.stage_pending_imports(entries, &ui);
                    }
                    Err(AuthError::DecryptPasswordMismatch) => {
                        ui_state.set_decrypt_password_failed(true);
                    }
                    Err(e) => {
                        log_import_error_redacted("Import decrypt failed", &e);
                        ui_state.set_decrypt_generic_failure(true);
                    }
                }
            })
            .detach();
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_cancel_decrypt_file({
        let ui = ui.clone_strong();
        move || {
            let mut app_state = app_state.borrow_mut();
            clear_decrypt_job(&ui, &mut app_state, false, false);
        }
    });

    ui.global::<AuthenticatorCallbacks>().on_abandon_decrypt_file({
        let ui = ui.clone_strong();
        move || {
            let mut app_state = app_state.borrow_mut();
            clear_decrypt_job(&ui, &mut app_state, true, true);
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

    cx.set_input_handler({
        let gui_api = cx.gui.clone();
        let ui = ui.clone_strong();
        move |input| {
            if input.msg != InputMessage::NavigationFocused {
                return;
            }
            let Ok(Some(nav_bytes)) = gui_api.navigate_pending() else {
                log::error!("Navigation focused but no pending nav request");
                return;
            };
            let Some(MatchedQrResult { scan_result, matched_rules }) =
                MatchedQrResult::from_slice(&nav_bytes)
            else {
                return;
            };
            let ScanQrResult::Qr { data, .. } = scan_result else {
                log::warn!(
                    "Unexpected scan result type for universal QR: matched_rules={:?}",
                    matched_rules.iter().map(|r| r.rule_id.as_str()).collect::<Vec<_>>()
                );
                return;
            };
            let url = match std::str::from_utf8(data.as_slice()) {
                Ok(s) => String::from(s),
                Err(e) => {
                    log::error!("Failed to decode universal QR data: {}", e);
                    return;
                }
            };
            ui.global::<Navigate>().invoke_return_home_animate(Animate::None);

            let mut app_state = app_state.borrow_mut();
            let ui_state = ui.global::<AuthenticatorCallbacks>();
            app_state.search_text = String::new();
            app_state.new_code = None;
            app_state.discard_pending_imports();
            app_state.pending_decrypt_import = None;
            app_state.invalidate_decrypt_job();
            app_state.decrypt_cancel_timer.stop();
            ui_state.set_pending_import_count(0);
            ui_state.set_decrypting(false);
            reset_decrypt_ui_state(&ui);
            app_state.handle_scanned_url(url, false, &ui);
        }
    });

    ui.run().expect("UI running");
}

impl AppState {
    fn stage_pending_imports(&mut self, entries: Vec<Auth>, ui: &AppWindow) {
        self.discard_pending_imports();
        let count = entries.len();
        let ui_nav = ui.global::<Navigate>();
        let ui_callbacks = ui.global::<AuthenticatorCallbacks>();
        self.pending_imports = Some(entries);
        ui_callbacks.set_pending_import_count(count as i32);
        ui_nav.invoke_main(
            MainParams { version: CardPageVersion::Main },
            NavigateOptions { replace: true, animate: Animate::None },
        );
    }

    fn discard_pending_imports(&mut self) { self.pending_imports = None; }

    fn handle_scanned_url(&mut self, url: String, from_edit: bool, ui: &AppWindow) {
        let ui_nav = ui.global::<Navigate>();

        if google_migration::is_migration_uri(&url) {
            match google_migration::parse_migration_uri(&url) {
                Ok(entries) => self.stage_pending_imports(entries, ui),
                Err(e) => {
                    log::warn!("Failed to parse migration URI: {}", e);
                    CallbackResult::from(AuthError::from(e)).navigate_from_scan_qr(from_edit, ui_nav);
                }
            }
            return;
        }

        match adapt_scan_qr(url, self) {
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
            }
        }
    }

    fn handle_import_file(&mut self, bytes: Vec<u8>, display_name: String, from_edit: bool, ui: &AppWindow) {
        let ui_nav = ui.global::<Navigate>();
        let ui_state = ui.global::<AuthenticatorCallbacks>();

        if self.decrypt_pending {
            log::info!("Ignoring file import while a decrypt is still pending");
            return;
        }

        let detected = match detect_import_file(bytes) {
            Ok(detected) => detected,
            Err(e) => {
                log::warn!("Import file rejected before dispatch: {}", e);
                ui_state.set_main_result(CallbackResult::unsupported_import_file_error());
                return;
            }
        };

        match dispatch_import_file(detected, display_name, self) {
            Ok(Some(entries)) => self.stage_pending_imports(entries, ui),
            Ok(None) => {
                ui_state.set_show_decrypt_cancel(false);
                ui_nav.invoke_decrypt_file(
                    DecryptFileParams { header: self.pending_import_header() },
                    NavigateOptions { replace: from_edit, animate: Animate::None },
                )
            }
            Err(e) => {
                log_import_error_redacted("Import file rejected before decrypt", &e);
                ui_state.set_main_result(CallbackResult::unsupported_import_file_error());
            }
        }
    }

    fn pending_import_header(&self) -> SharedString {
        self.pending_decrypt_import
            .as_ref()
            .map(|pending| SharedString::from(pending.display_name.as_str()))
            .unwrap_or_default()
    }
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

fn pick_next_import_label(label: &str, occupied_labels: &HashSet<String>) -> String {
    if !occupied_labels.contains(label) {
        return label.to_string();
    }

    let mut count = 0usize;
    loop {
        let import_label = make_import_label(label, count);
        if !occupied_labels.contains(&import_label) {
            return import_label;
        }
        count += 1;
    }
}

fn adapt_import_multiple(app_state: &mut AppState) -> Result<(), AuthError> {
    let entries = app_state.pending_imports.take().ok_or(AuthError::NoPendingImportsError)?;
    let mut occupied_labels: HashSet<String> =
        app_state.auth_table.iter().map(|auth| auth.get_label().to_string()).collect();

    let mut entries = entries.into_iter();
    while let Some(entry) = entries.next() {
        let label = pick_next_import_label(entry.get_label(), &occupied_labels);

        let mut import_auth = entry;
        if let Err(error) = import_auth.edit(AuthEditField::Label(label.clone())) {
            return Err(error.into());
        }
        import_auth.color = 0;

        app_state.auth_table.separate_categories(|a| a.get_category());
        if let Err(e) = app_state.auth_table.push_categorized(|a| a.get_category(), import_auth) {
            log::warn!("Failed to import entry: {:?}", e);
        } else {
            occupied_labels.insert(label);
        }
    }

    Ok(())
}

fn detect_import_file(bytes: Vec<u8>) -> Result<DetectedImportFile, AuthError> {
    let proton_zip_json = proton::extract_zip_entry(bytes.as_slice(), proton::ZIP_JSON_ENTRY);
    match proton_zip_json {
        Ok(data_json) => {
            return Ok(DetectedImportFile {
                format: ImportFileFormat::ProtonZipJson,
                decrypt_type: None,
                payload: ImportPayload::ProtonZipJson(data_json),
            });
        }
        Err(error) => {
            log::debug!("Proton ZIP JSON probe failed: {error}");
        }
    }

    let proton_zip_pgp = proton::extract_zip_entry(bytes.as_slice(), proton::ZIP_PGP_ENTRY);
    match proton_zip_pgp {
        Ok(data_pgp) => {
            return Ok(DetectedImportFile {
                format: ImportFileFormat::ProtonZipPgp,
                decrypt_type: Some(DecryptType::OpenPgp),
                payload: ImportPayload::ProtonZipPgp(data_pgp),
            });
        }
        Err(error) => {
            log::debug!("Proton ZIP PGP probe failed: {error}");
        }
    }

    match proton::probe_csv_export(bytes.as_slice()) {
        Ok(()) => {
            return Ok(DetectedImportFile {
                format: ImportFileFormat::ProtonCsv,
                decrypt_type: None,
                payload: ImportPayload::ProtonCsv(bytes),
            });
        }
        Err(error) => {
            let _ = error;
            log::debug!("Proton CSV probe failed for {}-byte input", bytes.len());
        }
    }

    match proton::parse_authenticator_export(bytes.as_slice()) {
        Ok(parsed) => {
            return Ok(match parsed {
                proton::ParsedAuthenticatorExport::Plain => DetectedImportFile {
                    format: ImportFileFormat::ProtonAuthenticatorJson,
                    decrypt_type: None,
                    payload: ImportPayload::ProtonAuthenticatorJsonPlain(bytes),
                },
                proton::ParsedAuthenticatorExport::Encrypted(export) => DetectedImportFile {
                    format: ImportFileFormat::ProtonAuthenticatorJson,
                    decrypt_type: Some(DecryptType::ProtonAuthenticator),
                    payload: ImportPayload::ProtonAuthenticatorJsonEncrypted(export),
                },
            });
        }
        Err(error) => {
            log::debug!("Proton Authenticator JSON probe failed: {error}");
        }
    }

    match twofas::parse_export(bytes.as_slice()) {
        Ok(parsed) => {
            return Ok(match parsed {
                ParsedTwoFasExport::Plain => DetectedImportFile {
                    format: ImportFileFormat::TwoFasJson,
                    decrypt_type: None,
                    payload: ImportPayload::TwoFasJsonPlain(bytes),
                },
                ParsedTwoFasExport::Encrypted(export) => DetectedImportFile {
                    format: ImportFileFormat::TwoFasJson,
                    decrypt_type: Some(DecryptType::TwoFas),
                    payload: ImportPayload::TwoFasJsonEncrypted(export),
                },
            });
        }
        Err(error) => {
            log::debug!("2FAS JSON probe failed: {error}");
        }
    }

    match aegis::parse_export(bytes.as_slice()) {
        Ok(parsed) => {
            return Ok(match parsed {
                ParsedAegisExport::Plain(db) => DetectedImportFile {
                    format: ImportFileFormat::AegisJson,
                    decrypt_type: None,
                    payload: ImportPayload::AegisJsonPlain(db),
                },
                ParsedAegisExport::Encrypted(export) => DetectedImportFile {
                    format: ImportFileFormat::AegisJson,
                    decrypt_type: Some(DecryptType::Aegis),
                    payload: ImportPayload::AegisJsonEncrypted(export),
                },
            });
        }
        Err(error) => {
            log::debug!("Aegis JSON probe failed: {error}");
        }
    }

    match aegis::probe_uri_export(bytes.as_slice()) {
        Ok(()) => {
            return Ok(DetectedImportFile {
                format: ImportFileFormat::AegisUri,
                decrypt_type: None,
                payload: ImportPayload::AegisUri(bytes),
            });
        }
        Err(error) => {
            let _ = error;
            log::debug!("Aegis URI probe failed for {}-byte input", bytes.len());
        }
    }

    Err(AuthError::UnsupportedImportFile(anyhow::anyhow!("Unknown import file format")))
}

fn dispatch_import_file(
    detected: DetectedImportFile,
    display_name: String,
    app_state: &mut AppState,
) -> Result<Option<Vec<Auth>>, AuthError> {
    match detected {
        DetectedImportFile { payload: ImportPayload::AegisJsonPlain(db), .. } => Ok(Some(
            aegis::ingest_plaintext_db(db.as_slice())
                .map_err(|e| AuthError::UnsupportedImportFile(e.into()))?,
        )),
        detected @ DetectedImportFile { payload: ImportPayload::AegisJsonEncrypted(_), .. } => {
            app_state.pending_decrypt_import = Some(detected.into_pending_decrypt(display_name)?);
            Ok(None)
        }
        DetectedImportFile { payload: ImportPayload::AegisUri(bytes), .. } => Ok(Some(
            aegis::ingest_uri_export(bytes.as_slice())
                .map_err(|e| AuthError::UnsupportedImportFile(e.into()))?,
        )),
        DetectedImportFile { payload: ImportPayload::ProtonZipJson(bytes), .. } => {
            Ok(Some(proton::ingest_json_export(bytes.as_slice()).map_err(AuthError::UnsupportedImportFile)?))
        }
        DetectedImportFile { payload: ImportPayload::ProtonCsv(bytes), .. } => {
            Ok(Some(proton::ingest_csv_export(bytes.as_slice()).map_err(AuthError::UnsupportedImportFile)?))
        }
        DetectedImportFile { payload: ImportPayload::ProtonAuthenticatorJsonPlain(bytes), .. } => Ok(Some(
            proton::ingest_authenticator_plain_export(bytes.as_slice())
                .map_err(AuthError::UnsupportedImportFile)?,
        )),
        detected @ DetectedImportFile {
            payload: ImportPayload::ProtonAuthenticatorJsonEncrypted(_), ..
        } => {
            app_state.pending_decrypt_import = Some(detected.into_pending_decrypt(display_name)?);
            Ok(None)
        }
        detected @ DetectedImportFile { payload: ImportPayload::ProtonZipPgp(_), .. } => {
            app_state.pending_decrypt_import = Some(detected.into_pending_decrypt(display_name)?);
            Ok(None)
        }
        DetectedImportFile { payload: ImportPayload::TwoFasJsonPlain(bytes), .. } => Ok(Some(
            twofas::ingest_plaintext_export(bytes.as_slice())
                .map_err(|error| AuthError::UnsupportedImportFile(anyhow::Error::new(error)))?,
        )),
        detected @ DetectedImportFile { payload: ImportPayload::TwoFasJsonEncrypted(_), .. } => {
            app_state.pending_decrypt_import = Some(detected.into_pending_decrypt(display_name)?);
            Ok(None)
        }
    }
}

async fn adapt_decrypt_import(
    crypto: CryptoApi,
    pending_import: PendingDecryptImport,
    password: String,
) -> Result<Vec<Auth>, AuthError> {
    match &pending_import.detected.payload {
        ImportPayload::AegisJsonEncrypted(export) => {
            let plaintext =
                aegis::decrypt_export(&crypto, export, password.as_str()).await.map_err(|error| {
                    if matches!(error, AegisError::PasswordMismatch) {
                        AuthError::DecryptPasswordMismatch
                    } else {
                        AuthError::AegisImportError(error)
                    }
                })?;
            aegis::ingest_plaintext_db(plaintext.as_slice()).map_err(AuthError::AegisImportError)
        }
        ImportPayload::TwoFasJsonEncrypted(export) => {
            let plaintext = twofas::decrypt_export(&crypto, export, password.as_str()).await.map_err(
                |error| match error {
                    TwoFasError::PasswordMismatch => AuthError::DecryptPasswordMismatch,
                    TwoFasError::Generic(error) => AuthError::GenericError(error),
                },
            )?;
            twofas::ingest_decrypted_services(plaintext.as_slice()).map_err(|error| match error {
                TwoFasError::PasswordMismatch => AuthError::DecryptPasswordMismatch,
                TwoFasError::Generic(error) => AuthError::GenericError(error),
            })
        }
        ImportPayload::ProtonZipPgp(bytes) => {
            let plaintext = proton::decrypt_pgp_export(bytes.as_slice(), password.as_str()).map_err(
                |error| match error {
                    proton::ProtonError::PasswordMismatch => AuthError::DecryptPasswordMismatch,
                    proton::ProtonError::Generic(error) => AuthError::GenericError(error),
                },
            )?;
            proton::ingest_json_export(plaintext.as_slice()).map_err(AuthError::GenericError)
        }
        ImportPayload::ProtonAuthenticatorJsonEncrypted(export) => {
            let plaintext = proton::decrypt_authenticator_export(&crypto, export, password.as_str())
                .await
                .map_err(|error| match error {
                    proton::ProtonError::PasswordMismatch => AuthError::DecryptPasswordMismatch,
                    proton::ProtonError::Generic(error) => AuthError::GenericError(error),
                })?;
            Ok(proton::ingest_authenticator_plain_export(plaintext.as_slice())
                .map_err(AuthError::GenericError)?)
        }
        _ => Err(AuthError::UnsupportedImportFile(anyhow::anyhow!(
            "Pending decrypt import did not contain decryptable payload"
        ))),
    }
}

#[cfg(not(test))]
fn execute_file_picker(state: StoredValue<AppState>) -> Result<Option<(Vec<u8>, String)>, AuthError> {
    let options = SelectFileOptions::default().with_dirs_allowed(true);
    let files = match select_file::<GuiPermissions>(options) {
        Ok(Some(f)) => f,
        Ok(None) => {
            log::info!("Nothing returned from file picker");
            return Ok(None);
        }
        Err(e) => {
            log::info!("Error while picking file: {:?}", e);
            return Ok(None);
        }
    };

    let (path, location) = match files.files().len() {
        0 => {
            log::error!("No files selected");
            return Ok(None);
        }
        1 => files.files()[0].clone(),
        _ => {
            log::info!("Multiple files selected, using first only");
            files.files()[0].clone()
        }
    };

    let location = match location {
        filepicker::Location::Internal => fs::Location::User,
        filepicker::Location::Airlock => fs::Location::Airlock,
        filepicker::Location::External => fs::Location::Usb,
    };

    let opened = state
        .borrow()
        .fs
        .open_file(&path, location, fs::OpenFlags { read: true, write: false, create: false })
        .context(format!("Failed to open selected file {}", path))
        .map_err(AuthError::UnsupportedImportFile)?;

    let metadata = opened
        .metadata()
        .context(format!("Failed to read metadata for selected file {}", path))
        .map_err(AuthError::UnsupportedImportFile)?;
    if metadata.size > MAX_IMPORT_FILE_SIZE_BYTES {
        return Err(AuthError::UnsupportedImportFile(anyhow::anyhow!(
            "Selected import file is too large: {} bytes (maximum {} bytes)",
            metadata.size,
            MAX_IMPORT_FILE_SIZE_BYTES
        )));
    }

    let mut bytes = Vec::new();
    let _ = opened
        .take(MAX_IMPORT_FILE_SIZE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context(format!("Failed to read selected file {}", path))
        .map_err(AuthError::UnsupportedImportFile)?;
    if bytes.len() as u64 > MAX_IMPORT_FILE_SIZE_BYTES {
        return Err(AuthError::UnsupportedImportFile(anyhow::anyhow!(
            "Selected import file is too large: more than {} bytes",
            MAX_IMPORT_FILE_SIZE_BYTES
        )));
    }

    let display_name =
        Path::new(&path).file_name().and_then(|value| value.to_str()).map(String::from).unwrap_or(path);

    Ok(Some((bytes, display_name)))
}

#[cfg(test)]
fn execute_file_picker(_state: StoredValue<AppState>) -> Result<Option<(Vec<u8>, String)>, AuthError> {
    Ok(None)
}

fn scan_import_input_from_file_picker_result(
    result: Result<Option<(Vec<u8>, String)>, AuthError>,
) -> Result<ScanImportInput, AuthError> {
    let (bytes, display_name) = result?.ok_or(AuthError::ScanQrCanceledError)?;
    Ok(ScanImportInput::File { bytes, display_name })
}

fn scan_qr_request(state: StoredValue<AppState>) -> Result<ScanImportInput, AuthError> {
    let opts = ScanQrOptions {
        header_title: tr::lookup_id(TrId::ScanTitle).into(),
        header_left_icon: String::from("chevron-left"),
        button_icon: String::from("file"),
        button_text: tr::lookup_id(TrId::CameraImportFromFile).into(),
        ..ScanQrOptions::default()
    };

    let opt = open_qr_scanner::<GuiPermissions>(opts).map_err(|e| AuthError::NavigateToQrError(e))?;
    let nav_res = opt.ok_or(AuthError::ScanQrFailedError)?;

    match nav_res {
        ScanQrResult::Qr { data, .. } => {
            let url = std::str::from_utf8(data.as_slice()).map_err(|e| AuthError::DecodeQrError(e))?;
            Ok(ScanImportInput::Url(String::from(url)))
        }
        ScanQrResult::LeftClicked => return Err(AuthError::ScanQrCanceledError),
        ScanQrResult::ButtonClicked => {
            // Sleep to avoid the OOM killer closing the authenticator app while the
            // file picker process starts.
            std::thread::sleep(Duration::from_millis(500));
            scan_import_input_from_file_picker_result(execute_file_picker(state))
        }
        _ => return Err(AuthError::UnknownQrResultError),
    }
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

    #[test]
    fn test_format_totp_code_6() {
        let res = format_totp_code("123456");
        assert_eq!(res, String::from("1 2 3  4 5 6"));
    }

    #[test]
    fn test_file_picker_preserves_unsupported_import_file_errors() {
        let result = scan_import_input_from_file_picker_result(Err(AuthError::UnsupportedImportFile(
            anyhow::anyhow!("Selected import file is too large"),
        )));

        assert!(matches!(result, Err(AuthError::UnsupportedImportFile(_))));
    }

    #[test]
    fn test_format_totp_code_8() {
        let res = format_totp_code("12345678");
        assert_eq!(res, String::from("1 2 3 4  5 6 7 8"));
    }
}
