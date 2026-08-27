// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
//
// SPDX-FileCopyrightText: 2026 immz https://github.com/immz4
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(test))]
use std::time::Duration;

use {
    crate::{tr, TrId},
    anyhow::{anyhow, Context},
    ordered_table::{SortableCard, TableEntry},
    serde::{Deserialize, Serialize},
    std::fmt,
    totp_rs::{Algorithm, TotpUrlError, TOTP},
    url::{form_urlencoded, Url},
    urlencoding::{decode, encode},
};

pub const DATABASE_FILE: &str = "authenticator_database_v3.json";

#[derive(PartialEq, Debug, thiserror::Error)]
pub enum AuthDuplicateReason {
    #[error("Duplicate label: {0:?}")]
    Label(String),
    #[error("Duplicate TOTP with label {0:?}")]
    Totp(String),
}

/// Category of a TOTP URL parse failure, with no part of the offending URL in it.
///
/// Every TotpUrlError variant keeps the offending part of the URL, and for
/// TotpUrlError::Secret that is the raw shared secret, so the error itself must not
/// be stored or rendered.
pub fn totp_url_error_category(error: &TotpUrlError) -> &'static str {
    match error {
        TotpUrlError::Url(_) => "unparseable URL",
        TotpUrlError::Scheme(_) => "invalid scheme",
        TotpUrlError::Host(_) => "invalid host",
        TotpUrlError::Secret(_) => "invalid secret encoding",
        TotpUrlError::SecretSize(_) => "secret too short",
        TotpUrlError::Algorithm(_) => "unknown algorithm",
        TotpUrlError::Digits(_) => "unparseable digit count",
        TotpUrlError::DigitsNumber(_) => "digit count out of range",
        TotpUrlError::Step(_) => "unparseable time step",
        TotpUrlError::Issuer(_) => "issuer contains a colon",
        TotpUrlError::IssuerDecoding(_) => "undecodable issuer",
        TotpUrlError::IssuerMistmatch(_, _) => "issuer mismatch",
        TotpUrlError::AccountName(_) => "account name contains a colon",
        TotpUrlError::AccountNameDecoding(_) => "undecodable account name",
    }
}

#[derive(PartialEq, Debug, thiserror::Error)]
pub enum AuthValidationError {
    #[error("Invalid label, labels must not be empty")]
    InvalidLabelError,
    #[error("Account field must not be empty")]
    EmptyAccountError,
    #[error("Time period must be 30 seconds: {0:?}")]
    InvalidTimestepError(u64),
    #[error("Invalid TOTP URL: {0}")]
    InvalidTotpError(&'static str),
}

#[repr(u32)]
pub enum AuthCategories {
    Active = 0,
    Archived,
}

// Always provide defaults for new values.
// `OrderedTable` requires its associated types to implement `Debug`, so this
// type has a custom implementation below that deliberately omits the TOTP
// key material.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
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

impl fmt::Debug for Auth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Auth")
            .field("label", &self.label)
            .field("account", &self.totp.account_name)
            .field("issuer", &self.totp.issuer)
            .field("color", &self.color)
            .field("archived", &self.archived)
            .field("date", &self.date)
            .finish_non_exhaustive()
    }
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

fn sanitize_issuer_and_account(totp_url: &str) -> Option<(String, String, String)> {
    let mut parsed_url = Url::parse(totp_url).ok()?;
    let decoded_path = decode(parsed_url.path().trim_start_matches('/')).ok()?.to_string();

    let (issuer, account) = match parsed_url
        .query_pairs()
        .find(|(key, _)| key == "issuer")
        .map(|(_, value)| value.into_owned())
    {
        // A non-empty query issuer is authoritative. Only a matching label
        // prefix is structural; otherwise preserve the complete label as the
        // account name.
        Some(query_issuer) if !query_issuer.is_empty() => {
            let account = decoded_path
                .strip_prefix(&query_issuer)
                .and_then(|path| path.strip_prefix(':'))
                .unwrap_or(&decoded_path)
                .to_string();
            (query_issuer, account)
        }
        // Without a usable query issuer, the label is the only issuer and
        // account source, so its final colon is structural.
        Some(_) | None => {
            let (issuer, account) = decoded_path.rsplit_once(':')?;
            (issuer.to_string(), account.to_string())
        }
    };

    // Parse through a colon-free placeholder, then restore the selected account
    // after totp-rs has read the remaining URI parameters.
    let encoded_issuer = encode(&issuer);
    parsed_url.set_path("/account");

    let mut query_serializer = form_urlencoded::Serializer::new(String::new());
    let mut has_issuer = false;
    for (key, value) in parsed_url.query_pairs() {
        if key == "issuer" {
            query_serializer.append_pair("issuer", &encoded_issuer);
            has_issuer = true;
        } else {
            query_serializer.append_pair(&key, &value);
        }
    }
    if !has_issuer {
        query_serializer.append_pair("issuer", &encoded_issuer);
    }
    parsed_url.set_query(Some(&query_serializer.finish()));

    Some((parsed_url.to_string(), issuer, account))
}

impl Auth {
    pub fn new(totp_url: String, date: u64) -> Result<Self, AuthValidationError> {
        // Use unchecked, because github, and possibly others, may use short secrets
        let mut totp = match TOTP::from_url_unchecked(&totp_url) {
            Ok(t) => t,
            Err(
                error @ (TotpUrlError::IssuerMistmatch(_, _)
                | TotpUrlError::Issuer(_)
                | TotpUrlError::AccountName(_)),
            ) => {
                let (sanitized_url, issuer, account_name) = sanitize_issuer_and_account(&totp_url)
                    .ok_or_else(|| AuthValidationError::InvalidTotpError(totp_url_error_category(&error)))?;
                let mut sanitized_totp = TOTP::from_url_unchecked(&sanitized_url)
                    .map_err(|e| AuthValidationError::InvalidTotpError(totp_url_error_category(&e)))?;
                sanitized_totp.issuer = Some(issuer);
                sanitized_totp.account_name = account_name;
                sanitized_totp
            }
            Err(e) => return Err(AuthValidationError::InvalidTotpError(totp_url_error_category(&e))),
        };

        // Account names are a mandatory field, so this is extremely rare,
        // but it should not prevent importing a TOTP.
        if totp.account_name.is_empty() {
            totp.account_name = tr::lookup_id(TrId::MainImportNoName).to_string();
        }

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

pub fn make_totp_auth(totp_url: &str, label: Option<&str>) -> anyhow::Result<Auth> {
    let mut auth = Auth::new(totp_url.to_owned(), get_timestamp_in_seconds()).map_err(anyhow::Error::new)?;
    if let Some(label) = label.filter(|label| !label.is_empty()) {
        auth.edit(AuthEditField::Label(label.to_string())).map_err(anyhow::Error::new)?;
    } else if auth.get_issuer().is_empty() {
        auth.edit(AuthEditField::Label(auth.get_account().to_string())).map_err(anyhow::Error::new)?;
    }
    Ok(auth)
}

pub fn build_totp_url(
    secret: &str,
    account: &str,
    issuer: Option<&str>,
    algorithm: &str,
    digits: u32,
    period: u64,
) -> anyhow::Result<String> {
    let algorithm = match algorithm {
        "SHA1" => Algorithm::SHA1,
        "SHA256" => Algorithm::SHA256,
        "SHA512" => Algorithm::SHA512,
        _ => return Err(anyhow!("Invalid TOTP algorithm {algorithm}")),
    };
    let secret = base32::decode(base32::Alphabet::RFC4648 { padding: false }, secret)
        .context("Invalid TOTP secret encoding")?;
    let digits = usize::try_from(digits).context("Invalid TOTP digit count")?;
    let totp = TOTP::new_unchecked(
        algorithm,
        digits,
        1,
        period,
        secret,
        issuer.map(str::to_string),
        account.to_string(),
    );
    Ok(totp.get_url())
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
            }
            AuthEditField::Issuer(_val) => (),
        }

        Ok(())
    }
}

#[cfg(not(test))]
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

#[cfg(test)]
pub fn get_timestamp_in_seconds() -> u64 { 0 }

pub fn make_import_label(label: &str, count: usize) -> String {
    let import_prefix = tr::lookup_id(TrId::MainImport);
    if count > 0 {
        format!("[{} {}] {}", import_prefix, count, label)
    } else {
        format!("[{}] {}", import_prefix, label)
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
    fn debug_output_omits_totp_secret() {
        let auth = auth1().unwrap();
        let debug = format!("{auth:?}");

        assert!(!debug.contains("GZ4FORKTNBVFGQTFJJGEIRDOKY"));
        assert!(!debug.contains("secret"));
    }

    fn auth_colon_issuer_unescaped() -> Result<Auth, AuthValidationError> {
        let url =
            String::from("otpauth://totp/Te:st:testuser?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY&issuer=Te:st");
        Ok(Auth::new(url, 0)?)
    }

    fn auth_colon_issuer_escaped() -> Result<Auth, AuthValidationError> {
        let url =
            String::from("otpauth://totp/Te%3Ast:testuser?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY&issuer=Te:st");
        Ok(Auth::new(url, 0)?)
    }

    fn auth_colon_issuer_query_escaped() -> Result<Auth, AuthValidationError> {
        let url =
            String::from("otpauth://totp/Te:st:testuser?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY&issuer=Te%3Ast");
        Ok(Auth::new(url, 0)?)
    }

    fn auth_mismatched_label_prefix() -> Result<Auth, AuthValidationError> {
        // Slack-style: label prefix is a friendly name ("Slack (Foundation)")
        // but the canonical issuer is just "Slack".
        let url = String::from(
            "otpauth://totp/Slack%20(Foundation):test@foundation.xyz?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP&issuer=Slack",
        );
        Ok(Auth::new(url, 0)?)
    }

    fn auth_mismatched_label_prefix_with_colon() -> Result<Auth, AuthValidationError> {
        // The friendly prefix itself contains a colon (e.g. "Foundation: Team").
        // The complete label should remain the account when it does not begin
        // with the authoritative query issuer plus a colon.
        let url = String::from(
            "otpauth://totp/Slack%20(Foundation%3A%20Team):test@foundation.xyz?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP&issuer=Slack",
        );
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
    fn create_auth_colon_issuer_unescaped() {
        let auth = auth_colon_issuer_unescaped().unwrap();
        assert_eq!(auth.get_issuer(), "Te:st");
        assert_eq!(auth.get_account(), "testuser");
    }

    #[test]
    fn create_auth_colon_issuer_escaped() {
        let auth = auth_colon_issuer_escaped().unwrap();
        assert_eq!(auth.get_issuer(), "Te:st");
        assert_eq!(auth.get_account(), "testuser");
    }

    #[test]
    fn create_auth_colon_issuer_query_escaped() {
        let auth = auth_colon_issuer_query_escaped().unwrap();
        assert_eq!(auth.get_issuer(), "Te:st");
        assert_eq!(auth.get_account(), "testuser");
    }

    #[test]
    fn sanitize_issuer_preserves_percent_escape() {
        let auth = Auth::new(
            String::from("otpauth://totp/A%253AB:alice?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY&issuer=A%253AB"),
            0,
        )
        .unwrap();

        assert_eq!(auth.get_issuer(), "A%3AB");
        assert_eq!(auth.get_account(), "alice");
    }

    #[test]
    fn sanitize_empty_query_issuer_uses_label_parts() {
        let auth =
            Auth::new(String::from("otpauth://totp/ACME:alice?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY&issuer="), 0)
                .unwrap();

        assert_eq!(auth.get_issuer(), "ACME");
        assert_eq!(auth.get_account(), "alice");
    }

    #[test]
    fn sanitize_missing_query_issuer_uses_label_parts() {
        let (_, issuer, account) =
            sanitize_issuer_and_account("otpauth://totp/ACME:alice?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY")
                .unwrap();

        assert_eq!(issuer, "ACME");
        assert_eq!(account, "alice");
    }

    #[test]
    fn create_auth_mismatched_label_prefix() {
        let auth = auth_mismatched_label_prefix().unwrap();
        assert_eq!(auth.get_issuer(), "Slack");
        assert_eq!(auth.get_account(), "Slack (Foundation):test@foundation.xyz");
    }

    #[test]
    fn create_auth_mismatched_label_prefix_with_colon() {
        let auth = auth_mismatched_label_prefix_with_colon().unwrap();
        assert_eq!(auth.get_issuer(), "Slack");
        assert_eq!(auth.get_account(), "Slack (Foundation: Team):test@foundation.xyz");
    }

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
    fn mismatched_issuer_uses_query_parameter() {
        let url = String::from("otpauth://totp/Te:st:testuser?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY&issuer=Test");
        let auth = Auth::new(url, 0).unwrap();
        assert_eq!(auth.get_issuer(), "Test");
        assert_eq!(auth.get_account(), "Te:st:testuser");
    }

    #[test]
    fn matching_issuer_splits_account_on_first_colon() {
        let url = String::from("otpauth://totp/Test:te:stuser?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY&issuer=Test");
        let auth = Auth::new(url, 0).unwrap();
        assert_eq!(auth.get_issuer(), "Test");
        assert_eq!(auth.get_account(), "te:stuser");
    }

    #[test]
    fn mismatched_issuer_with_colon_uses_query_parameter() {
        let url = String::from(
            "otpauth://totp/GitHub:alice?secret=GZ4FORKTNBVFGQTFJJGEIRDOKY&issuer=Foundation%3ASSO",
        );
        let auth = Auth::new(url, 0).unwrap();
        assert_eq!(auth.get_issuer(), "Foundation:SSO");
        assert_eq!(auth.get_account(), "GitHub:alice");
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
