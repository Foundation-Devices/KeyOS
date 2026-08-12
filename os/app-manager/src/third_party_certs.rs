// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;
use std::io::{Read, Write};

use app_manager::{ThirdPartyCertificateError, ThirdPartyCertificateInfo};
use const_oid::db::{
    rfc3280::EMAIL_ADDRESS,
    rfc4519::{COMMON_NAME, ORGANIZATION_NAME},
    rfc5280::ID_KP_CODE_SIGNING,
};
use const_oid::{AssociatedOid, ObjectIdentifier};
use fs::Location;
use x509_cert::{
    attr::AttributeValue,
    der::{
        asn1::{Ia5StringRef, PrintableStringRef, TeletexStringRef, Utf8StringRef},
        Decode, DecodePem, Encode, Tag, Tagged,
    },
    ext::pkix::{name::GeneralName, BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAltName},
    time::Validity,
    Certificate, TbsCertificate, Version,
};

use crate::FileSystem;

const THIRD_PARTY_CERTS_DIR: &str = "third_party_certs";
const CERTIFICATE_SUFFIX: &str = ".der";
const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const SECP256K1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.10");
/// Bounds the imported PEM or DER, which in turn bounds every parsed field, so the display strings
/// need no per-field caps.
const MAX_CERTIFICATE_BYTES: usize = 16 * 1024;

/// The certificate files are the allow list. Every field callers see is derived from them at load,
/// so a field added later also applies to certificates that are already allowed.
///
/// Keyed by fingerprint, which is both the file name and the identity a user checks, so listings
/// come out in an order no claimed name can influence.
#[derive(Debug)]
pub(crate) struct ThirdPartyCertificateStore {
    fs: FileSystem,
    certs: BTreeMap<String, ThirdPartyCertificateInfo>,
}

impl ThirdPartyCertificateStore {
    pub(crate) fn new(fs: FileSystem) -> Self {
        match fs.create_dir(THIRD_PARTY_CERTS_DIR, Location::SystemAppData) {
            Ok(_) | Err(fs::Error::FileAlreadyExists) => {}
            Err(e) => log::error!("failed to create the third-party certificate directory: {e:?}"),
        }

        let certs = load_certificates(&fs);

        Self { fs, certs }
    }

    pub(crate) fn list(&self) -> Vec<ThirdPartyCertificateInfo> { self.certs.values().cloned().collect() }

    /// The certificate for `fingerprint` while it is still allowed to launch apps. An expired one
    /// can no longer launch what it signed, so the removal guard only applies before it expires.
    pub(crate) fn allowed(&self, fingerprint: &str) -> Option<&ThirdPartyCertificateInfo> {
        self.certs.get(fingerprint).filter(|cert| cert.is_usable())
    }

    /// Parse and validate a certificate without changing the allow list.
    pub(crate) fn preview(
        &self,
        certificate_bytes: &[u8],
    ) -> Result<ThirdPartyCertificateInfo, ThirdPartyCertificateError> {
        let cert = parse_third_party_certificate(certificate_bytes).map_err(|e| {
            log::warn!("invalid third-party certificate: {e}");
            ThirdPartyCertificateError::Invalid
        })?;

        if cert.has_expired() {
            log::warn!("third-party certificate expired at {}", cert.not_after_unix_seconds);
            return Err(ThirdPartyCertificateError::Expired {
                not_after_unix_seconds: cert.not_after_unix_seconds,
            });
        }
        if cert.is_not_yet_valid() {
            log::warn!("third-party certificate starts at {}", cert.not_before_unix_seconds);
            return Err(ThirdPartyCertificateError::NotYetValid {
                not_before_unix_seconds: cert.not_before_unix_seconds,
            });
        }

        Ok(cert)
    }

    /// Add a certificate to the allow list under `expected_fingerprint`, which the caller must
    /// already have shown the user. A publisher is only ever allowed under a confirmed identity.
    pub(crate) fn import(
        &mut self,
        certificate_bytes: &[u8],
        expected_fingerprint: &str,
    ) -> Result<ThirdPartyCertificateInfo, ThirdPartyCertificateError> {
        let mut cert = self.preview(certificate_bytes)?;
        if cert.fingerprint != expected_fingerprint {
            log::warn!("third-party certificate holds {}, not the confirmed key", cert.fingerprint);
            return Err(ThirdPartyCertificateError::FingerprintMismatch);
        }

        cert.added_unix_seconds =
            store_certificate(&self.fs, &cert.fingerprint, certificate_bytes).map_err(|e| {
                log::error!("failed to store third-party certificate: {e:#}");
                ThirdPartyCertificateError::Internal
            })?;

        // Same fingerprint means the same key, so this renews a publisher rather than adding one.
        self.certs.insert(cert.fingerprint.clone(), cert.clone());

        Ok(cert)
    }

    pub(crate) fn remove(&mut self, fingerprint: &str) -> Result<bool, fs::Error> {
        if !self.certs.contains_key(fingerprint) {
            return Ok(false);
        }

        self.fs.remove(certificate_path(fingerprint), Location::SystemAppData)?;
        self.certs.remove(fingerprint);

        Ok(true)
    }
}

fn load_certificates(fs: &FileSystem) -> BTreeMap<String, ThirdPartyCertificateInfo> {
    let dir = match fs.open_dir(THIRD_PARTY_CERTS_DIR, Location::SystemAppData) {
        Ok(dir) => dir,
        Err(e) => {
            log::error!("failed to open the third-party certificate directory: {e:?}");
            return BTreeMap::new();
        }
    };

    let mut certs = BTreeMap::new();
    while let Ok(Some(entry)) = dir.next_entry() {
        if !entry.is_file || !entry.name.ends_with(CERTIFICATE_SUFFIX) {
            continue;
        }
        match load_certificate(fs, &entry) {
            Ok(cert) => {
                certs.insert(cert.fingerprint.clone(), cert);
            }
            // Leave the file alone: silently deleting something the user allowed is worse than
            // listing nothing for it until the log says why.
            Err(e) => log::warn!("ignoring third-party certificate {}: {e:#}", entry.name),
        }
    }

    certs
}

fn load_certificate(fs: &FileSystem, entry: &fs::DirEntry) -> anyhow::Result<ThirdPartyCertificateInfo> {
    if entry.len > MAX_CERTIFICATE_BYTES as u64 {
        anyhow::bail!("exceeds the {MAX_CERTIFICATE_BYTES}-byte cap: {} bytes", entry.len);
    }

    let path = format!("{THIRD_PARTY_CERTS_DIR}/{}", entry.name);
    let mut bytes = Vec::with_capacity(entry.len as usize);
    fs.open_file(&path, Location::SystemAppData, fs::OpenFlags::READ_ONLY)?.read_to_end(&mut bytes)?;

    let mut cert = parse_third_party_certificate(&bytes)?;
    // remove() addresses the file by fingerprint, so a name disagreeing with the key inside would
    // leave a publisher the user cannot delete.
    if entry.name != certificate_file_name(&cert.fingerprint) {
        anyhow::bail!("name does not match the key it holds, fingerprinted {}", cert.fingerprint);
    }
    cert.added_unix_seconds = created_unix_seconds(fs, &path);

    Ok(cert)
}

/// Write the certificate to its fingerprint-named file and return the time that file was created.
/// An interrupted write leaves one that no longer parses, so the publisher goes missing until it is
/// imported again rather than turning into another one.
fn store_certificate(
    fs: &FileSystem,
    fingerprint: &str,
    certificate_bytes: &[u8],
) -> anyhow::Result<Option<u64>> {
    let path = certificate_path(fingerprint);

    let mut file = fs.open_file(&path, Location::SystemAppData, fs::OpenFlags::CREATE)?;
    file.write_all(&certificate_der(certificate_bytes)?)?;
    // Creating does not truncate, so a renewal shorter than the certificate it replaces would keep
    // the old tail.
    file.truncate()?;
    // The directory entry only catches up once the file is closed.
    drop(file);

    Ok(created_unix_seconds(fs, &path))
}

/// When the file was first written. Renewing a publisher rewrites its file in place, which keeps the
/// entry the creation time lives in, so this stays the time the user first allowed that key.
fn created_unix_seconds(fs: &FileSystem, path: &str) -> Option<u64> {
    match fs.metadata(path, Location::SystemAppData) {
        Ok(metadata) => unix_seconds(&metadata.created),
        Err(e) => {
            log::warn!("no creation time for {path}: {e:?}");
            None
        }
    }
}

/// Re-encode an accepted certificate as DER, so the stored file matches the name it is given.
fn certificate_der(certificate_bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let certificate =
        Certificate::from_pem(certificate_bytes).or_else(|_| Certificate::from_der(certificate_bytes))?;

    Ok(certificate.to_der()?)
}

fn certificate_file_name(fingerprint: &str) -> String { format!("{fingerprint}{CERTIFICATE_SUFFIX}") }

fn certificate_path(fingerprint: &str) -> String {
    format!("{THIRD_PARTY_CERTS_DIR}/{}", certificate_file_name(fingerprint))
}

fn unix_seconds(datetime: &fs::DateTime) -> Option<u64> {
    let civil = jiff::civil::DateTime::new(
        datetime.date.year.try_into().ok()?,
        datetime.date.month.try_into().ok()?,
        datetime.date.day.try_into().ok()?,
        datetime.time.hour.try_into().ok()?,
        datetime.time.min.try_into().ok()?,
        datetime.time.sec.try_into().ok()?,
        0,
    )
    .ok()?;

    u64::try_from(civil.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp().as_second()).ok()
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

    // Admission rejects a certificate outside its window, but list() keeps an expired one visible
    // so the user can still inspect and remove it.
    let (not_before_unix_seconds, not_after_unix_seconds) = certificate_validity_unix_seconds(&tbs.validity);
    if not_before_unix_seconds > not_after_unix_seconds {
        anyhow::bail!("third-party certificate expires before it starts");
    }

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
        not_before_unix_seconds,
        not_after_unix_seconds,
        serial_number: tbs.serial_number.to_string(),
        issuer: tbs.issuer.to_string(),
        subject: tbs.subject.to_string(),
        basic_constraints: basic_constraints_text(&basic_constraints),
        key_usage: key_usage_text(&key_usage),
        extended_key_usage: extended_key_usage_text(&extended_key_usage),
    })
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

fn canonical_publisher_fingerprints(public_key: &[u8; 33]) -> anyhow::Result<(String, String)> {
    let fingerprint = publisher_fingerprint::PublisherFingerprint::from_compressed_public_key(public_key)?;
    Ok((fingerprint.full, fingerprint.short))
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
    fn file_timestamp_converts_to_unix_seconds() {
        let datetime = |year, month, day, hour, min, sec| fs::DateTime {
            date: fs::Date { year, month, day },
            time: fs::Time { hour, min, sec, millis: 0 },
        };

        assert_eq!(unix_seconds(&datetime(2026, 8, 5, 12, 30, 15)), Some(1_785_933_015));
        assert_eq!(unix_seconds(&datetime(2026, 13, 1, 0, 0, 0)), None);
        assert_eq!(unix_seconds(&datetime(2026, 2, 30, 0, 0, 0)), None);
    }
}
