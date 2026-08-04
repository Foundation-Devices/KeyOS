// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use app_manager::ThirdPartyCertificateInfo;
use const_oid::db::{
    rfc3280::EMAIL_ADDRESS,
    rfc4519::{COMMON_NAME, ORGANIZATION_NAME},
    rfc5280::ID_KP_CODE_SIGNING,
};
use const_oid::{AssociatedOid, ObjectIdentifier};
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
    Certificate, TbsCertificate, Version,
};

const THIRD_PARTY_CERTS_PATH: &str = "third_party_certs.json";
const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const SECP256K1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.10");
/// Bounds the imported PEM or DER, which in turn bounds every parsed field, so the display strings
/// need no per-field caps.
const MAX_CERTIFICATE_BYTES: usize = 16 * 1024;

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

pub(crate) enum ImportThirdPartyCertificateError {
    Invalid,
    Storage,
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
        prepare_certificates(self.certs.certificates.clone())
    }

    pub(crate) fn allowed_publishers(&self) -> Vec<ThirdPartyCertificateInfo> {
        let certs = self
            .certs
            .certificates
            .iter()
            .filter(|cert| cert.is_currently_valid())
            .cloned()
            .collect::<Vec<_>>();

        prepare_certificates(certs)
    }

    /// Whether `public_key` currently belongs to an allowed, unexpired certificate.
    /// An expired cert can no longer launch the app that was signed with it, so the
    /// removal guard only applies while the cert is still allowed.
    pub(crate) fn is_allowed(&self, public_key: &str) -> bool {
        let public_key = normalized_public_key(public_key);
        self.certs
            .certificates
            .iter()
            .any(|cert| cert.public_key == public_key.as_str() && cert.is_currently_valid())
    }

    /// Parse and validate a certificate without changing the allow list.
    pub(crate) fn preview(&self, certificate_bytes: &[u8]) -> Result<ThirdPartyCertificateInfo, ()> {
        parse_third_party_certificate(certificate_bytes)
            .and_then(|cert| {
                ensure_current_certificate_validity(&cert)?;
                Ok(cert)
            })
            .map_err(|e| {
                log::warn!("invalid third-party certificate: {e}");
            })
    }

    pub(crate) fn import(
        &mut self,
        certificate_bytes: &[u8],
    ) -> Result<ThirdPartyCertificateInfo, ImportThirdPartyCertificateError> {
        let mut cert =
            self.preview(certificate_bytes).map_err(|()| ImportThirdPartyCertificateError::Invalid)?;
        // Preserve the original date when a user imports the same key again. Legacy entries have
        // no date, so their first post-upgrade import records one without making them unreadable.
        cert.added_unix_seconds =
            added_time_for_import(&self.certs.certificates, &cert.public_key, current_unix_seconds());

        // Snapshot the previous list so we can roll back if persistence fails.
        // Without this, a failing flush() (e.g. I/O / full volume) would leave
        // `self.certs` mutated while the API returns InternalError, so an imported but
        // unpersisted cert would silently keep allowing its apps until restart.
        let previous = self.certs.certificates.clone();
        {
            let mut certs = self.certs.guard();
            certs.certificates.retain(|existing| existing.public_key != cert.public_key);
            certs.certificates.push(cert.clone());
        }
        if let Err(e) = self.flush() {
            log::error!("failed to save third-party certificates: {e:?}");
            self.restore(previous);
            return Err(ImportThirdPartyCertificateError::Storage);
        }

        Ok(cert)
    }

    pub(crate) fn remove(&mut self, public_key: &str) -> Result<bool, fs::Error> {
        let public_key = normalized_public_key(public_key);
        if !self.certs.certificates.iter().any(|cert| cert.public_key == public_key.as_str()) {
            return Ok(false);
        }

        // Same rationale as `import`: keep in-memory allow-list state in lockstep
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
    if certificate_bytes.len() > MAX_CERTIFICATE_BYTES {
        anyhow::bail!(
            "third-party certificate exceeds the {MAX_CERTIFICATE_BYTES}-byte cap: {} bytes",
            certificate_bytes.len()
        );
    }

    let certificate =
        Certificate::from_pem(certificate_bytes).or_else(|_| Certificate::from_der(certificate_bytes))?;
    let tbs = &certificate.tbs_certificate;
    if tbs.version != Version::V3 {
        anyhow::bail!("third-party certificate must use X.509 v3");
    }
    reject_unknown_critical_extensions(tbs)?;

    // Keep the validity window in persisted metadata. Admission checks it after parsing, while
    // list() intentionally retains legacy stale entries so the user can inspect and remove them.
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

    let compressed_public_key = compressed_public_key(
        tbs.subject_public_key_info
            .subject_public_key
            .as_bytes()
            .ok_or_else(|| anyhow::anyhow!("invalid public key bit string"))?,
    )
    .ok_or_else(|| anyhow::anyhow!("invalid secp256k1 public key"))?;
    let public_key = hex::encode(compressed_public_key);
    let (fingerprint, short_fingerprint) = canonical_publisher_fingerprints(&compressed_public_key)?;

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
        fingerprint,
        short_fingerprint,
        added_unix_seconds: None,
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

fn ensure_current_certificate_validity(cert: &ThirdPartyCertificateInfo) -> anyhow::Result<()> {
    if !cert.is_currently_valid() {
        anyhow::bail!("third-party certificate is not currently valid");
    }
    Ok(())
}

/// Unknown non-critical X.509 v3 extensions are intentionally left uninterpreted. This is the
/// forward-compatible extension point for future certificate metadata. RFC 5280 requires an
/// implementation to reject a critical extension that it does not understand.
fn reject_unknown_critical_extensions(tbs: &TbsCertificate) -> anyhow::Result<()> {
    let understood = [BasicConstraints::OID, KeyUsage::OID, ExtendedKeyUsage::OID, SubjectAltName::OID];
    if let Some(extension) = tbs
        .extensions
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|extension| extension.critical && !understood.contains(&extension.extn_id))
    {
        anyhow::bail!("unsupported critical certificate extension {}", extension.extn_id);
    }
    Ok(())
}

fn populate_derived_metadata(certs: &mut [ThirdPartyCertificateInfo]) {
    for cert in certs {
        let Some(public_key) = decode_public_key_hex(&cert.public_key) else {
            log::warn!("stored third-party certificate has an invalid public key");
            continue;
        };
        match canonical_publisher_fingerprints(&public_key) {
            Ok((fingerprint, short_fingerprint)) => {
                cert.fingerprint = fingerprint;
                cert.short_fingerprint = short_fingerprint;
            }
            Err(error) => {
                log::error!("failed to derive stored third-party certificate fingerprint: {error}");
            }
        }
    }
}

fn prepare_certificates(mut certs: Vec<ThirdPartyCertificateInfo>) -> Vec<ThirdPartyCertificateInfo> {
    populate_derived_metadata(&mut certs);
    sort_certificates(&mut certs);
    certs
}

fn canonical_publisher_fingerprints(public_key: &[u8; 33]) -> anyhow::Result<(String, String)> {
    let fingerprint = publisher_fingerprint::PublisherFingerprint::from_compressed_public_key(public_key)?;
    Ok((fingerprint.full, fingerprint.short))
}

fn current_unix_seconds() -> Option<u64> {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|time| time.as_secs())
}

fn added_time_for_import(
    existing: &[ThirdPartyCertificateInfo],
    public_key: &str,
    now: Option<u64>,
) -> Option<u64> {
    existing
        .iter()
        .find(|certificate| certificate.public_key == public_key)
        .and_then(|certificate| certificate.added_unix_seconds)
        .or(now)
}

fn sort_certificates(certs: &mut [ThirdPartyCertificateInfo]) {
    certs.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint).then_with(|| a.public_key.cmp(&b.public_key)));
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

fn compressed_public_key(public_key: &[u8]) -> Option<[u8; 33]> {
    secp256k1::PublicKey::from_slice(public_key).ok().map(|key| key.serialize())
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
            fingerprint: String::new(),
            short_fingerprint: String::new(),
            added_unix_seconds: None,
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

    #[test]
    fn import_admission_rejects_expired_not_yet_valid_and_unknown_windows() {
        assert!(ensure_current_certificate_validity(&cert(Some(0), Some(u64::MAX))).is_ok());
        assert!(ensure_current_certificate_validity(&cert(Some(0), Some(0))).is_err());
        assert!(ensure_current_certificate_validity(&cert(Some(u64::MAX), Some(u64::MAX))).is_err());
        assert!(ensure_current_certificate_validity(&cert(None, Some(u64::MAX))).is_err());
    }

    #[test]
    fn canonical_fingerprint_matches_sdk_test_vector() {
        let public_key =
            decode_public_key_hex("0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798")
                .unwrap();

        let (fingerprint, short_fingerprint) = canonical_publisher_fingerprints(&public_key).unwrap();

        assert_eq!(fingerprint, "0f715baf5d4c2ed329785cef29e562f73488c8a2bb9dbc5700b361d54b9b0554");
        assert_eq!(short_fingerprint, "0f715baf…4b9b0554");
    }

    #[test]
    fn compressed_public_key_rejects_points_outside_secp256k1() {
        let mut invalid_compressed = [0xff; 33];
        invalid_compressed[0] = 0x02;
        assert!(compressed_public_key(&invalid_compressed).is_none());

        let mut invalid_uncompressed = [0xff; 65];
        invalid_uncompressed[0] = 0x04;
        assert!(compressed_public_key(&invalid_uncompressed).is_none());
    }

    #[test]
    fn parse_accepts_unknown_non_critical_extension() {
        const UNKNOWN_EXTENSION_OID: ObjectIdentifier =
            ObjectIdentifier::new_unwrap("1.3.6.1.4.1.55555.1.99");
        let pem = include_bytes!("../testdata/third-party-cert-with-unknown-extension.pem");
        let encoded = Certificate::from_pem(pem).unwrap();
        assert_eq!(encoded.tbs_certificate.version, Version::V3);
        assert!(encoded
            .tbs_certificate
            .extensions
            .as_deref()
            .unwrap()
            .iter()
            .any(|extension| extension.extn_id == UNKNOWN_EXTENSION_OID && !extension.critical));

        let parsed = parse_third_party_certificate(pem).unwrap();

        assert_eq!(parsed.name, "Extension Test Publisher");
        assert_eq!(parsed.public_key, "03c213976a975d508fd4d1e5e04de72c334b966dbb9db5adc3e8d76ca720d9d572");
        assert_eq!(parsed.fingerprint, "e71fa12f4331c92985e92e7e55b85dd55e75ba22bc192db4e91f202a3f3b9452");
        assert_eq!(parsed.short_fingerprint, "e71fa12f…3f3b9452");
        assert_eq!(parsed.added_unix_seconds, None);
    }

    #[test]
    fn parse_rejects_truncated_oversized_and_bad_length_inputs() {
        let pem = include_bytes!("../testdata/third-party-cert-with-unknown-extension.pem");
        assert!(parse_third_party_certificate(&pem[..pem.len() / 2]).is_err());

        let oversized = vec![0u8; MAX_CERTIFICATE_BYTES + 1];
        assert!(parse_third_party_certificate(&oversized).is_err());

        // DER SEQUENCE claiming a 65,535-byte body but containing one byte.
        assert!(parse_third_party_certificate(&[0x30, 0x82, 0xff, 0xff, 0x00]).is_err());
    }

    #[test]
    fn legacy_stored_certificate_gets_derived_fingerprints_when_listed() {
        let legacy_json = r#"{
            "certificates": [{
                "name": "Legacy Publisher",
                "company": "Claimed Company",
                "contact_email": "",
                "support_url": "",
                "public_key": "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
                "not_before_unix_seconds": 0,
                "not_after_unix_seconds": 18446744073709551615,
                "serial_number": "1",
                "issuer": "",
                "subject": "",
                "basic_constraints": "CA:FALSE",
                "key_usage": "Digital Signature",
                "extended_key_usage": "Code Signing"
            }]
        }"#;
        let stored: StoredThirdPartyCertificates = serde_json::from_str(legacy_json).unwrap();
        assert!(stored.certificates[0].fingerprint.is_empty());
        assert!(stored.certificates[0].short_fingerprint.is_empty());
        assert_eq!(stored.certificates[0].added_unix_seconds, None);

        let listed = prepare_certificates(stored.certificates);

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].fingerprint, "0f715baf5d4c2ed329785cef29e562f73488c8a2bb9dbc5700b361d54b9b0554");
        assert_eq!(listed[0].short_fingerprint, "0f715baf…4b9b0554");
        assert_eq!(listed[0].added_unix_seconds, None);
        assert!(decode_public_key_hex(&listed[0].public_key).is_some());
    }

    #[test]
    fn certificate_sort_uses_fingerprint_not_claimed_name() {
        let mut high_fingerprint = cert(Some(0), Some(u64::MAX));
        high_fingerprint.name = "A claimed name".to_string();
        high_fingerprint.fingerprint = "ff".repeat(32);
        high_fingerprint.public_key = "03".to_string();
        let mut low_fingerprint = cert(Some(0), Some(u64::MAX));
        low_fingerprint.name = "Z claimed name".to_string();
        low_fingerprint.fingerprint = "00".repeat(32);
        low_fingerprint.public_key = "02".to_string();
        let mut certs = vec![high_fingerprint, low_fingerprint];

        sort_certificates(&mut certs);

        assert_eq!(certs[0].name, "Z claimed name");
        assert_eq!(certs[1].name, "A claimed name");
    }

    #[test]
    fn reimport_preserves_original_date_added() {
        let public_key = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
        let mut existing = cert(Some(0), Some(u64::MAX));
        existing.public_key = public_key.to_string();
        existing.added_unix_seconds = Some(42);

        assert_eq!(added_time_for_import(&[existing], public_key, Some(99)), Some(42));
        assert_eq!(added_time_for_import(&[], public_key, Some(99)), Some(99));
    }
}
