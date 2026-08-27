// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        seed::{Seed, SeedDuplicateReason, SeedValidationError},
        tr, CallbackResult, ResultLevel, TrId,
    },
    ordered_table::OrderedTableError,
    slint_keyos_platform::slint::SharedString,
    std::num::TryFromIntError,
};

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("OrderedTableError: {0}")]
    OrderedTableError(#[from] OrderedTableError<Seed>),
    #[error("ValidationError: {0}")]
    ValidationError(#[from] SeedValidationError),
    #[error("DuplicateError: {0}")]
    DuplicateError(#[from] SeedDuplicateReason),
    #[error("Invalid index could not be parsed: {0:?}")]
    IndexError(#[from] TryFromIntError),
    #[error("Unknwon QR data payload type")]
    UnknownQrDataType,
    #[error(transparent)]
    GenericError(#[from] anyhow::Error),
}

pub trait ToValidationString {
    fn to_validation_string(&self) -> String;
}

impl ToValidationString for VaultError {
    fn to_validation_string(&self) -> String {
        match self {
            VaultError::OrderedTableError(e) => e.to_validation_string(),
            VaultError::ValidationError(e) => e.to_validation_string(),
            VaultError::DuplicateError(reason) => reason.to_validation_string(),
            ref other => other.to_string(),
        }
    }
}

impl ToValidationString for OrderedTableError<Seed> {
    fn to_validation_string(&self) -> String {
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

impl ToValidationString for SeedValidationError {
    fn to_validation_string(&self) -> String {
        match self {
            SeedValidationError::InvalidLabelError => {
                tr::lookup_id(TrId::CreateItemSeedErrorsMissingField).to_string()
            }
            SeedValidationError::CappedIndexError => {
                tr::lookup_id(TrId::CommonCreateAccountsanitizedCappedIndex).to_string()
            }
            // Note: empty passwords are caught in slint and don't currently have validation text
            ref other => other.to_string(),
        }
    }
}

impl ToValidationString for SeedDuplicateReason {
    fn to_validation_string(&self) -> String {
        match self {
            SeedDuplicateReason::Label(_other) => {
                tr::lookup_id(TrId::CreateItemSeedErrorsWalletAlreadyInUse).to_string()
            }
            SeedDuplicateReason::Bitcoin12(_other) => {
                tr::lookup_id(TrId::CreateItemSeedErrorsIndexAlreadyInUse).to_string()
            }
            SeedDuplicateReason::Bitcoin24(_other) => {
                tr::lookup_id(TrId::CreateItemSeedErrorsIndexAlreadyInUse).to_string()
            }
            SeedDuplicateReason::NostrKey(_other) => {
                tr::lookup_id(TrId::CreateItemSeedErrorsIndexAlreadyInUse).to_string()
            }
            SeedDuplicateReason::PasswordGeneratedIndex(_other) => {
                tr::lookup_id(TrId::CreateItemSeedErrorsIndexAlreadyInUse).to_string()
            } // ref other => other.to_string(),
        }
    }
}

impl From<VaultError> for CallbackResult {
    fn from(error: VaultError) -> Self {
        match error {
            VaultError::OrderedTableError(e) => Self::from(e),
            VaultError::ValidationError(e) => Self::from(e),
            VaultError::DuplicateError(reason) => Self::from(reason),
            ref other => {
                Self::failure(ResultLevel::Error, String::from("Error"), other.to_validation_string().into())
            }
        }
    }
}

impl From<OrderedTableError<Seed>> for CallbackResult {
    fn from(error: OrderedTableError<Seed>) -> Self {
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

impl From<SeedValidationError> for CallbackResult {
    fn from(error: SeedValidationError) -> Self {
        log::warn!("{}", error);
        // ValidationErrors should never be seen because save buttons
        // are disabled in case of these validation errors.
        Self::failure(ResultLevel::Error, String::from("Error"), error.to_validation_string().to_string())
    }
}

impl From<SeedDuplicateReason> for CallbackResult {
    fn from(reason: SeedDuplicateReason) -> Self {
        log::warn!("{}", reason);
        // Other DuplicateReasons should never be seen because save buttons
        // are disabled in case of these validation errors.
        Self::failure(ResultLevel::Error, String::from("Error"), reason.to_validation_string().to_string())
    }
}

impl CallbackResult {
    pub fn success() -> Self {
        Self {
            success: true,
            level: ResultLevel::Info,
            title: SharedString::new(),
            text: SharedString::new(),
        }
    }

    pub fn failure(level: ResultLevel, title: String, text: String) -> Self {
        Self { success: false, level, title: SharedString::from(title), text: SharedString::from(text) }
    }
}
