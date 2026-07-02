// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use app_manager::ThirdPartyCertificateInfo;
use const_oid::db::{
    rfc3280::EMAIL_ADDRESS,
    rfc4519::{COMMON_NAME, ORGANIZATION_NAME},
    rfc5280::ID_KP_CODE_SIGNING,
};
use const_oid::ObjectIdentifier;
use file_backed::JsonBacked;
use fs::Location;
use serde::{Deserialize, Serialize};
use x509_cert::{
    attr::AttributeValue,
    der::{
        asn1::{Ia5StringRef, PrintableStringRef, TeletexStringRef, Utf8StringRef},
        Decode, DecodePem, Tag, Tagged,
    },
    ext::pkix::{name::GeneralName, BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAltName},
    time::Validity,
    Certificate,
};

const THIRD_PARTY_CERTS_PATH: &str = "third_party_certs.json";
const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const SECP256K1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.10");
/// Bounds the imported PEM, which in turn bounds every parsed field, so the display strings
/// need no per-field caps.
const MAX_CERTIFICATE_PEM_BYTES: usize = 16 * 1024;

type ThirdPartyCertificatesFile =
    JsonBacked<StoredThirdPartyCertificates, crate::fs_permissions::FileSystemPermissions>;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct StoredThirdPartyCertificates {
    certificates: Vec<ThirdPartyCertificateInfo>,
}

#[derive(Debug)]
pub(crate) struct ThirdPartyCertificateStore {
    certs: ThirdPartyCertificatesFile,
}

impl Default for ThirdPartyCertificateStore {
    fn default() -> Self {
        let (mut certs, _restored) =
            ThirdPartyCertificatesFile::new(THIRD_PARTY_CERTS_PATH, Location::SystemAppData);
        certs.set_auto_save(false);

        Self { certs }
    }
}

impl ThirdPartyCertificateStore {
    pub(crate) fn list(&self) -> Vec<ThirdPartyCertificateInfo> {
        let mut certs = self.certs.certificates.clone();

        sort_certificates(&mut certs);
        certs
    }

    pub(crate) fn trusted_publishers(&self) -> Vec<ThirdPartyCertificateInfo> {
        let mut certs = self
            .certs
            .certificates
            .iter()
            .filter(|cert| cert.is_currently_valid())
            .cloned()
            .collect::<Vec<_>>();

        sort_certificates(&mut certs);
        certs
    }

    /// Whether `public_key` currently belongs to a *trusted* (unexpired) certificate.
    /// An expired cert can no longer launch the app that was signed with it, so the
    /// removal guard only applies while the cert is still trusted.
    pub(crate) fn is_trusted(&self, public_key: &str) -> bool {
        let public_key = normalized_public_key(public_key);
        self.certs
            .certificates
            .iter()
            .any(|cert| cert.public_key == public_key.as_str() && cert.is_currently_valid())
    }

    pub(crate) fn import(&mut self, certificate_pem: &[u8]) -> Result<ThirdPartyCertificateInfo, ()> {
        if certificate_pem.len() > MAX_CERTIFICATE_PEM_BYTES {
            log::warn!(
                "third-party certificate exceeds the {MAX_CERTIFICATE_PEM_BYTES}-byte cap: {} bytes",
                certificate_pem.len()
            );
            return Err(());
        }
        let cert = parse_third_party_certificate(certificate_pem).map_err(|e| {
            log::warn!("invalid third-party certificate: {e}");
        })?;

        // Snapshot the previous list so we can roll back if persistence fails.
        // Without this, a failing flush() (e.g. I/O / full volume) would leave
        // `self.certs` mutated while the API returns Invalid, so an imported but
        // unpersisted cert would silently keep trusting its apps until restart.
        let previous = self.certs.certificates.clone();
        {
            let mut certs = self.certs.guard();
            certs.certificates.retain(|existing| existing.public_key != cert.public_key);
            certs.certificates.push(cert.clone());
        }
        if let Err(e) = self.flush() {
            log::error!("failed to save third-party certificates: {e:?}");
            self.restore(previous);
            return Err(());
        }

        Ok(cert)
    }

    pub(crate) fn remove(&mut self, public_key: &str) -> Result<bool, fs::Error> {
        let public_key = normalized_public_key(public_key);
        if !self.certs.certificates.iter().any(|cert| cert.public_key == public_key.as_str()) {
            return Ok(false);
        }

        // Same rationale as `import`: keep in-memory trust state in lockstep
        // with what's persisted. A failed flush() must not leave the cert
        // "removed in memory but still on disk" (or vice versa on the next
        // restart).
        let previous = self.certs.certificates.clone();
        {
            let mut certs = self.certs.guard();
            certs.certificates.retain(|cert| cert.public_key != public_key.as_str());
        }

        if let Err(e) = self.flush() {
            self.restore(previous);
            return Err(e);
        }
        Ok(true)
    }

    fn flush(&mut self) -> Result<(), fs::Error> { self.certs.try_save() }

    fn restore(&mut self, certificates: Vec<ThirdPartyCertificateInfo>) {
        let mut certs = self.certs.guard();
        certs.0 = StoredThirdPartyCertificates { certificates };
    }
}

fn parse_third_party_certificate(certificate_bytes: &[u8]) -> anyhow::Result<ThirdPartyCertificateInfo> {
    let certificate =
        Certificate::from_pem(certificate_bytes).or_else(|_| Certificate::from_der(certificate_bytes))?;
    let tbs = &certificate.tbs_certificate;
    // Store the window but don't reject on it here: validity is checked at use (launch, listing),
    // so a not-yet-valid cert can be imported ahead of time and an expired one stays visible.
    let (not_before_unix_seconds, not_after_unix_seconds) = certificate_validity_unix_seconds(&tbs.validity);

    let name = subject_attribute(&tbs.subject, COMMON_NAME).ok_or_else(|| anyhow::anyhow!("missing CN"))?;
    let company =
        subject_attribute(&tbs.subject, ORGANIZATION_NAME).ok_or_else(|| anyhow::anyhow!("missing O"))?;
    let mut contact_email = subject_attribute(&tbs.subject, EMAIL_ADDRESS).unwrap_or_default();
    let mut support_url = String::new();

    if let Some((_, subject_alt_name)) = tbs.get::<SubjectAltName>()? {
        for name in subject_alt_name.0 {
            match name {
                GeneralName::Rfc822Name(email) if contact_email.is_empty() => {
                    contact_email = email.as_str().to_string();
                }
                GeneralName::UniformResourceIdentifier(uri) if support_url.is_empty() => {
                    support_url = uri.as_str().to_string();
                }
                _ => {}
            }
        }
    }

    if tbs.subject_public_key_info.algorithm.oid != ID_EC_PUBLIC_KEY {
        anyhow::bail!("third-party certificate is not an EC public key certificate");
    }
    let Some(curve_oid) = tbs
        .subject_public_key_info
        .algorithm
        .parameters
        .as_ref()
        .and_then(|params| ObjectIdentifier::try_from(x509_cert::der::asn1::AnyRef::from(params)).ok())
    else {
        anyhow::bail!("third-party certificate is missing an EC curve");
    };
    if curve_oid != SECP256K1 {
        anyhow::bail!("third-party certificate is not a secp256k1 certificate");
    }

    let public_key = compressed_public_key_hex(
        tbs.subject_public_key_info
            .subject_public_key
            .as_bytes()
            .ok_or_else(|| anyhow::anyhow!("invalid public key bit string"))?,
    )
    .ok_or_else(|| anyhow::anyhow!("invalid secp256k1 public key"))?;

    let (_, basic_constraints) =
        tbs.get::<BasicConstraints>()?.ok_or_else(|| anyhow::anyhow!("missing basic constraints"))?;
    if basic_constraints.ca {
        anyhow::bail!("third-party certificate must not be a CA");
    }

    let (_, key_usage) = tbs.get::<KeyUsage>()?.ok_or_else(|| anyhow::anyhow!("missing key usage"))?;
    if !key_usage.digital_signature() {
        anyhow::bail!("third-party certificate is missing digitalSignature key usage");
    }

    let (_, extended_key_usage) =
        tbs.get::<ExtendedKeyUsage>()?.ok_or_else(|| anyhow::anyhow!("missing extended key usage"))?;
    if !extended_key_usage.0.iter().any(|oid| *oid == ID_KP_CODE_SIGNING) {
        anyhow::bail!("third-party certificate is missing codeSigning extended key usage");
    }

    Ok(ThirdPartyCertificateInfo {
        name,
        company,
        contact_email,
        support_url,
        public_key,
        not_before_unix_seconds: Some(not_before_unix_seconds),
        not_after_unix_seconds: Some(not_after_unix_seconds),
        serial_number: tbs.serial_number.to_string(),
        issuer: tbs.issuer.to_string(),
        subject: tbs.subject.to_string(),
        basic_constraints: basic_constraints_text(&basic_constraints),
        key_usage: key_usage_text(&key_usage),
        extended_key_usage: extended_key_usage_text(&extended_key_usage),
    })
}

fn sort_certificates(certs: &mut [ThirdPartyCertificateInfo]) {
    certs.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.company.to_lowercase().cmp(&b.company.to_lowercase()))
    });
}

fn certificate_validity_unix_seconds(validity: &Validity) -> (u64, u64) {
    (validity.not_before.to_unix_duration().as_secs(), validity.not_after.to_unix_duration().as_secs())
}

fn subject_attribute(
    subject: &x509_cert::name::Name,
    oid: x509_cert::der::asn1::ObjectIdentifier,
) -> Option<String> {
    subject
        .0
        .iter()
        .flat_map(|rdn| rdn.0.iter())
        .find(|attribute| attribute.oid == oid)
        .and_then(|attribute| attribute_value_to_string(&attribute.value))
}

fn attribute_value_to_string(value: &AttributeValue) -> Option<String> {
    match value.tag() {
        Tag::PrintableString => PrintableStringRef::try_from(value).ok().map(|s| s.as_str().to_string()),
        Tag::Utf8String => Utf8StringRef::try_from(value).ok().map(|s| s.as_str().to_string()),
        Tag::Ia5String => Ia5StringRef::try_from(value).ok().map(|s| s.as_str().to_string()),
        Tag::TeletexString => TeletexStringRef::try_from(value).ok().map(|s| s.as_str().to_string()),
        _ => None,
    }
}

fn compressed_public_key_hex(public_key: &[u8]) -> Option<String> {
    let compressed = match public_key {
        [prefix @ (0x02 | 0x03), x @ ..] if x.len() == 32 => {
            let mut compressed = [0u8; 33];
            compressed[0] = *prefix;
            compressed[1..].copy_from_slice(x);
            compressed
        }
        [0x04, coordinates @ ..] if coordinates.len() == 64 => {
            let mut compressed = [0u8; 33];
            compressed[0] = if coordinates[63] & 1 == 0 { 0x02 } else { 0x03 };
            compressed[1..].copy_from_slice(&coordinates[..32]);
            compressed
        }
        _ => return None,
    };

    Some(hex::encode(compressed))
}

pub(crate) fn decode_public_key_hex(public_key: &str) -> Option<[u8; 33]> {
    let public_key = normalized_public_key(public_key);
    let mut decoded = [0u8; 33];
    hex::decode_to_slice(public_key, &mut decoded).ok()?;
    if matches!(decoded[0], 0x02 | 0x03) {
        Some(decoded)
    } else {
        None
    }
}

fn normalized_public_key(public_key: &str) -> String {
    public_key.chars().filter(|ch| !ch.is_ascii_whitespace()).collect()
}

fn basic_constraints_text(basic_constraints: &BasicConstraints) -> String {
    if basic_constraints.ca {
        "CA:TRUE".to_string()
    } else {
        "CA:FALSE".to_string()
    }
}

fn key_usage_text(key_usage: &KeyUsage) -> String {
    let mut usages = Vec::new();
    if key_usage.digital_signature() {
        usages.push("Digital Signature");
    }
    if key_usage.non_repudiation() {
        usages.push("Non Repudiation");
    }
    if key_usage.key_encipherment() {
        usages.push("Key Encipherment");
    }
    if key_usage.data_encipherment() {
        usages.push("Data Encipherment");
    }
    if key_usage.key_agreement() {
        usages.push("Key Agreement");
    }
    if key_usage.key_cert_sign() {
        usages.push("Key Cert Sign");
    }
    if key_usage.crl_sign() {
        usages.push("CRL Sign");
    }
    if key_usage.encipher_only() {
        usages.push("Encipher Only");
    }
    if key_usage.decipher_only() {
        usages.push("Decipher Only");
    }
    usages.join(", ")
}

fn extended_key_usage_text(extended_key_usage: &ExtendedKeyUsage) -> String {
    extended_key_usage
        .0
        .iter()
        .map(|oid| if *oid == ID_KP_CODE_SIGNING { "Code Signing".to_string() } else { oid.to_string() })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cert(not_before: Option<u64>, not_after: Option<u64>) -> ThirdPartyCertificateInfo {
        ThirdPartyCertificateInfo {
            name: "Example".to_string(),
            company: "Example Company".to_string(),
            contact_email: "hello@example.com".to_string(),
            support_url: "https://example.com".to_string(),
            public_key: String::new(),
            not_before_unix_seconds: not_before,
            not_after_unix_seconds: not_after,
            serial_number: "1".to_string(),
            issuer: String::new(),
            subject: String::new(),
            basic_constraints: String::new(),
            key_usage: String::new(),
            extended_key_usage: String::new(),
        }
    }

    #[test]
    fn currently_valid_needs_both_bounds_with_now_in_window() {
        // `now` is the real wall clock, so use windows whose result doesn't depend on its value.
        assert!(cert(Some(0), Some(u64::MAX)).is_currently_valid());
        assert!(!cert(Some(0), Some(0)).is_currently_valid());
        assert!(!cert(None, Some(u64::MAX)).is_currently_valid());
        assert!(!cert(Some(0), None).is_currently_valid());
    }
}
