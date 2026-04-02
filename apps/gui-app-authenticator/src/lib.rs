// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    ordered_table::{SortableCard, TableEntry},
    serde::{Deserialize, Serialize},
    std::collections::HashMap,
    std::time::Duration,
    totp_rs::TOTP,
};

pub mod google_migration;

pub const DATABASE_FILE: &str = "authenticator_database_v3.json";

#[derive(PartialEq, Debug, thiserror::Error)]
pub enum AuthDuplicateReason {
    #[error("Duplicate label: {0:?}")]
    Label(String),
    #[error("Duplicate TOTP with label {0:?}")]
    Totp(String),
}

#[derive(PartialEq, Debug, thiserror::Error)]
pub enum AuthValidationError {
    #[error("Invalid label, labels must not be empty")]
    InvalidLabelError,
    #[error("Account field must not be empty")]
    EmptyAccountError,
    #[error("Account field must not contain \":\"")]
    InvalidAccountError,
    #[error("Issuer field must not contain \":\"")]
    InvalidIssuerError,
    #[error("Time period must be 30 seconds: {0:?}")]
    InvalidTimestepError(u64),
    #[error("Invalid TOTP URL: {0:?}")]
    InvalidTotpError(totp_rs::TotpUrlError),
}

#[repr(u32)]
pub enum AuthCategories {
    Active = 0,
    Archived,
}

// Always provide defaults for new values
// Requires debug to debug associated types in OrderedTable
// Be careful not to debug log the whole thing with private TOTP keys
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Auth {
    totp: TOTP,
    label: String,
    #[serde(default)]
    pub color: u8,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    date: u64,
}

trait AuthValidation {
    fn validate(&self) -> Result<(), AuthValidationError>;
}

impl AuthValidation for TOTP {
    fn validate(&self) -> Result<(), AuthValidationError> {
        AuthEditField::Account(self.account_name.clone()).validate()?;
        AuthEditField::Issuer(self.issuer.clone().unwrap_or_default()).validate()?;

        if self.step != 30 {
            return Err(AuthValidationError::InvalidTimestepError(self.step));
        }

        Ok(())
    }
}

impl TableEntry for Auth {
    type DuplicateReason = AuthDuplicateReason;
    type ValidationError = AuthValidationError;

    fn validate(&self) -> Result<(), Self::ValidationError> {
        AuthEditField::Label(self.label.clone()).validate()?;
        self.totp.validate()?;
        Ok(())
    }

    fn is_duplicate(&self, other: &Self) -> Option<Self::DuplicateReason> {
        if self.totp == other.totp {
            return Some(AuthDuplicateReason::Totp(other.label.clone()));
        }

        if self.label == other.label {
            return Some(AuthDuplicateReason::Label(self.label.clone()));
        }

        None
    }
}

impl SortableCard for Auth {
    fn get_label(&self) -> &String { &self.label }

    fn get_date(&self) -> u64 { self.date }
}

impl Auth {
    pub fn new(totp_url: String, date: u64) -> Result<Self, AuthValidationError> {
        // Use unchecked, because github, and possibly others, may use short secrets
        let totp =
            TOTP::from_url_unchecked(&totp_url).map_err(|e| AuthValidationError::InvalidTotpError(e))?;
        totp.validate()?;

        // Don't validate default label, which can be empty initially before
        // pushing to a table
        let label = totp.issuer.clone().unwrap_or(String::new());
        let auth = Self { totp, label, color: 0, archived: false, date };
        Ok(auth)
    }

    pub fn get_code(&self, time: u64) -> String { self.totp.generate(time) }

    pub fn get_account(&self) -> &str { &self.totp.account_name }

    pub fn get_issuer(&self) -> &str { self.totp.issuer.as_deref().unwrap_or("") }

    pub fn edit(&mut self, field: AuthEditField) -> Result<(), AuthValidationError> {
        field.validate()?;
        match field {
            AuthEditField::Label(val) => self.label = val,
            AuthEditField::Account(val) => self.totp.account_name = val,
            AuthEditField::Issuer(val) => self.totp.issuer = if val.is_empty() { None } else { Some(val) },
        }

        Ok(())
    }

    pub fn get_category(&self) -> u32 {
        (if self.archived { AuthCategories::Archived } else { AuthCategories::Active }) as u32
    }
}

#[derive(Debug, thiserror::Error, Clone)]
pub enum AuthEditField {
    #[error("label: {0:?}")]
    Label(String),
    #[error("account: {0:?}")]
    Account(String),
    #[error("issuer: {0:?}")]
    Issuer(String),
}

impl AuthEditField {
    pub fn validate(&self) -> Result<(), AuthValidationError> {
        match self {
            AuthEditField::Label(val) => {
                if val.len() == 0 {
                    return Err(AuthValidationError::InvalidLabelError);
                }
            }
            AuthEditField::Account(val) => {
                if val.len() == 0 {
                    return Err(AuthValidationError::EmptyAccountError);
                }

                if val.contains(":") {
                    return Err(AuthValidationError::InvalidAccountError);
                }
            }
            AuthEditField::Issuer(val) => {
                if val.contains(":") {
                    return Err(AuthValidationError::InvalidIssuerError);
                }
            }
        }

        Ok(())
    }
}

pub fn get_timestamp_in_seconds() -> u64 {
    #[cfg(not(test))]
    return std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|e| {
            log::error!("Could not get time: {:?}", e);
            Duration::ZERO
        })
        .as_secs();
    #[cfg(test)]
    return 0;
}

pub fn make_import_label(label: &str, existing_labels: &mut HashMap<String, usize>) -> String {
    let count = *existing_labels.entry(label.to_string()).or_insert(0);

    if count > 0 {
        format!("[IMPORT {}] {}", count, label)
    } else {
        format!("[IMPORT] {}", label)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn auth1() -> Result<Auth, AuthValidationError> {
        let url = String::from("otpauth://totp/Test:testuser?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY&issuer=Test");
        Ok(Auth::new(url, 0)?)
    }

    fn auth2() -> Result<Auth, AuthValidationError> {
        let url = String::from(
            "otpauth://totp/Production:testuser?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY&issuer=Production",
        );
        Ok(Auth::new(url, 0)?)
    }

    fn auth3() -> Result<Auth, AuthValidationError> {
        let url = String::from(
            "otpauth://totp/Production:testuser?secret=GZ6FORKTNBVFGQTFJJGEIRDOKY&issuer=Production",
        );
        Ok(Auth::new(url, 0)?)
    }

    fn auth_no_issuer() -> Result<Auth, AuthValidationError> {
        let url = String::from("otpauth://totp/testuser?secret=GZ6FORKTNBVFGQTFJJGEIRDOKY");
        Ok(Auth::new(url, 0)?)
    }

    fn auth_short() -> Result<Auth, AuthValidationError> {
        let url = String::from("otpauth://totp/GitHub:my-username?secret=5DU3JDHQL4QFTOC4&issuer=GitHub");
        Ok(Auth::new(url, 0)?)
    }

    #[test]
    fn create_auth() {
        let auth = auth1().unwrap();
        auth.validate().unwrap();
        assert_eq!(auth.label, String::from("Test"));
    }

    #[test]
    fn create_short_auth() { auth_short().unwrap(); }

    #[test]
    fn create_auth_no_issuer() { auth_no_issuer().unwrap(); }

    #[test]
    fn validate_auth_no_label() {
        let auth = auth_no_issuer().unwrap();
        assert_eq!(auth.validate().unwrap_err(), AuthValidationError::InvalidLabelError);
    }

    #[test]
    fn not_equal() {
        let auth1 = auth1().unwrap();
        let auth3 = auth3().unwrap();
        assert!(auth1.is_duplicate(&auth3).is_none());
    }

    #[test]
    fn same_totp_priority() {
        let auth1 = auth1().unwrap();
        assert_eq!(auth1.is_duplicate(&auth1).unwrap(), AuthDuplicateReason::Totp(String::from("Test")));
    }

    #[test]
    fn same_totp() {
        let auth1 = auth1().unwrap();
        let auth2 = auth2().unwrap();
        assert_eq!(
            auth1.is_duplicate(&auth2).unwrap(),
            AuthDuplicateReason::Totp(String::from("Production"))
        );
    }

    #[test]
    fn same_label() {
        let auth2 = auth2().unwrap();
        let auth3 = auth3().unwrap();
        assert_eq!(
            auth2.is_duplicate(&auth3).unwrap(),
            AuthDuplicateReason::Label(String::from("Production"))
        );
    }

    #[test]
    fn validate_account_name() {
        let field = AuthEditField::Account(String::from("Customer"));
        field.validate().unwrap();
    }

    #[test]
    fn validate_issuer() {
        let field = AuthEditField::Issuer(String::from("Production"));
        field.validate().unwrap();
    }

    #[test]
    fn code_invalid_url() {
        let url = String::from("otpauth://totp/Te:st:testuser?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY&issuer=Test");
        match Auth::new(url, 0) {
            Ok(_) => panic!("This TOTP URL should not be valid."),
            Err(AuthValidationError::InvalidTotpError(_)) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn validate_invalid_account() {
        let field = AuthEditField::Account(String::from("Custo:mer"));
        match field.validate() {
            Ok(_) => panic!("Invalid account should fail."),
            Err(AuthValidationError::InvalidAccountError) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn validate_empty_account() {
        let field = AuthEditField::Account(String::new());
        match field.validate() {
            Ok(_) => panic!("Empty account should fail."),
            Err(AuthValidationError::EmptyAccountError) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn validate_invalid_issuer() {
        let field = AuthEditField::Issuer(String::from("Custo:mer"));
        match field.validate() {
            Ok(_) => panic!("Invalid issuer should fail."),
            Err(AuthValidationError::InvalidIssuerError) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn validate_allow_empty_issuer() {
        let field = AuthEditField::Issuer(String::new());
        field.validate().unwrap();
    }

    #[test]
    fn edit_label() {
        let mut auth1 = auth1().unwrap();
        let field = AuthEditField::Label(String::from("Customer"));
        auth1.edit(field).unwrap();
        assert_eq!(auth1.label, String::from("Customer"));
    }

    #[test]
    fn edit_account() {
        let mut auth1 = auth1().unwrap();
        let field = AuthEditField::Account(String::from("Customer"));
        auth1.edit(field).unwrap();
        assert_eq!(auth1.totp.account_name, String::from("Customer"));
    }

    #[test]
    fn edit_issuer() {
        let mut auth1 = auth1().unwrap();
        let field = AuthEditField::Issuer(String::from("Customer"));
        auth1.edit(field).unwrap();
        assert_eq!(auth1.totp.issuer, Some(String::from("Customer")));
    }

    #[test]
    fn edit_issuer_none() {
        let mut auth1 = auth1().unwrap();
        let field = AuthEditField::Issuer(String::new());
        auth1.edit(field).unwrap();
        assert_eq!(auth1.totp.issuer, None);
    }

    #[test]
    fn edit_invalid_account() {
        let mut auth1 = auth1().unwrap();
        let field = AuthEditField::Account(String::from("Custo:mer"));
        match auth1.edit(field) {
            Ok(_) => panic!("Invalid account should fail."),
            Err(AuthValidationError::InvalidAccountError) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn edit_empty_account() {
        let mut auth1 = auth1().unwrap();
        let field = AuthEditField::Account(String::new());
        match auth1.edit(field) {
            Ok(_) => panic!("Empty account should fail."),
            Err(AuthValidationError::EmptyAccountError) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn edit_invalid_issuer() {
        let mut auth1 = auth1().unwrap();
        let field = AuthEditField::Issuer(String::from("Custo:mer"));
        match auth1.edit(field) {
            Ok(_) => panic!("Invalid issuer should fail."),
            Err(AuthValidationError::InvalidIssuerError) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn table_validate_account() {
        let mut auth1 = auth1().unwrap();
        auth1.totp.account_name = String::from("");
        match auth1.validate() {
            Ok(_) => panic!("This TOTP should not be valid."),
            Err(AuthValidationError::EmptyAccountError) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn table_validate_issuer() {
        let mut auth1 = auth1().unwrap();
        auth1.totp.issuer = Some(String::from(":"));
        match auth1.validate() {
            Ok(_) => panic!("This TOTP should not be valid."),
            Err(AuthValidationError::InvalidIssuerError) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn get_code() {
        let auth1 = auth1().unwrap();
        let code = auth1.get_code(0);
        assert_eq!(code, "775288");
    }

    #[test]
    fn invalid_timestep() {
        let url = String::from("otpauth://totp/ACME%20Co:john.doe@email.com?secret=HXDMVJECJJWSRB3HWIZR4IFUGFTMXBOZ&issuer=ACME%20Co&algorithm=SHA1&digits=6&period=40");
        match Auth::new(url, 0).unwrap_err() {
            AuthValidationError::InvalidTimestepError(t) if t == 40 => (),
            other => panic!("Failed with the wrong error: {}", other),
        }
    }
}
