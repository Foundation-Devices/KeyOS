// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Casa Health Check protocol, compatible with Passport Core.

use {
    crate::sensitive_xpriv::SensitiveXpriv,
    foundation_urtypes::value::Value as UrValue,
    ngwallet::{
        bdk_wallet::bitcoin::{
            base64::{prelude::BASE64_STANDARD, Engine},
            bip32::{ChildNumber, DerivationPath, Xpriv},
            key::TapTweak,
            secp256k1::{All, Message, Secp256k1, XOnlyPublicKey},
            sign_message::{signed_msg_hash, MessageSignature},
            Address, CompressedPublicKey, Network, PublicKey,
        },
        bip39::MasterKey,
    },
    std::{fmt, str::FromStr},
    zeroize::Zeroize,
};

pub const UR_TYPE: &str = "bytes";
const MAX_PATH_DEPTH: usize = 12;
const MAX_MESSAGE_LENGTH: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressType {
    Classic,
    P2sh,
    P2wpkh,
    P2wpkhP2sh,
    P2wsh,
    P2wshP2sh,
    P2tr,
}

impl AddressType {
    fn from_core(value: &str) -> Result<Self, Error> {
        let mut normalized = value.to_uppercase().replace(' ', "_");
        if value.is_empty() {
            return Ok(Self::Classic);
        }
        if !normalized.starts_with("AF_") {
            normalized.insert_str(0, "AF_");
        }
        Ok(match normalized.as_str() {
            "AF_CLASSIC" => Self::Classic,
            "AF_P2SH" => Self::P2sh,
            "AF_P2WPKH" => Self::P2wpkh,
            "AF_P2WPKH_P2SH" => Self::P2wpkhP2sh,
            "AF_P2WSH" => Self::P2wsh,
            "AF_P2WSH_P2SH" => Self::P2wshP2sh,
            "AF_P2TR" => Self::P2tr,
            _ => return Err(Error::UnsupportedAddressType),
        })
    }
}

struct Challenge {
    message: String,
    path: DerivationPath,
    normalized_path: String,
    address_type: AddressType,
}

impl Drop for Challenge {
    fn drop(&mut self) {
        self.message.zeroize();
        self.normalized_path.zeroize();
    }
}

pub struct SignedResponse(String);

impl SignedResponse {
    pub fn as_bytes(&self) -> &[u8] { self.0.as_bytes() }

    pub fn into_bytes(mut self) -> Vec<u8> { std::mem::take(&mut self.0).into_bytes() }
}

impl Drop for SignedResponse {
    fn drop(&mut self) { self.0.zeroize(); }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidUtf8,
    InvalidLineCount,
    InvalidMessage,
    InvalidPath,
    PathNotAllowed,
    PathTooDeep,
    UnsupportedAddressType,
    KeyDerivation,
    AddressDerivation,
    MessageDigest,
    InvalidUrType,
    InvalidUrCbor,
    UrEncode,
}

impl Error {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidUtf8 => "CASA-HC-INVALID-UTF8",
            Self::InvalidLineCount => "CASA-HC-LINE-COUNT",
            Self::InvalidMessage => "CASA-HC-MESSAGE",
            Self::InvalidPath => "CASA-HC-PATH-INVALID",
            Self::PathNotAllowed => "CASA-HC-PATH-NOT-ALLOWED",
            Self::PathTooDeep => "CASA-HC-PATH-DEPTH",
            Self::UnsupportedAddressType => "CASA-HC-ADDRESS-TYPE",
            Self::KeyDerivation => "CASA-HC-KEY-DERIVE",
            Self::AddressDerivation => "CASA-HC-ADDRESS-DERIVE",
            Self::MessageDigest => "CASA-HC-MESSAGE-DIGEST",
            Self::InvalidUrType => "CASA-HC-UR-TYPE",
            Self::InvalidUrCbor => "CASA-HC-UR-DECODE",
            Self::UrEncode => "CASA-HC-UR-ENCODE",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.code()) }
}

impl std::error::Error for Error {}

fn parse(input: &[u8]) -> Result<Challenge, Error> {
    let text = std::str::from_utf8(input).map_err(|_| Error::InvalidUtf8)?;
    // Preserve a trailing empty path, which is Core's master-key challenge.
    let mut lines: Vec<_> = text.split('\n').map(|line| line.trim_end_matches('\r')).collect();
    if lines.len() == 4 && lines.last() == Some(&"") {
        lines.pop();
    }
    if !matches!(lines.len(), 2 | 3) {
        return Err(Error::InvalidLineCount);
    }
    validate_message(lines[0])?;
    let normalized_path = normalize_path(lines[1])?;
    let path = DerivationPath::from_str(&normalized_path).map_err(|_| Error::InvalidPath)?;
    let address_type =
        lines.get(2).map_or(Ok(AddressType::Classic), |value| AddressType::from_core(value))?;
    Ok(Challenge { message: lines[0].to_owned(), path, normalized_path, address_type })
}

fn validate_message(message: &str) -> Result<(), Error> {
    if message.is_empty()
        || message.len() > MAX_MESSAGE_LENGTH
        || message.starts_with(' ')
        || message.ends_with(' ')
    {
        return Err(Error::InvalidMessage);
    }
    let mut spaces = 0;
    for byte in message.bytes() {
        if !(32..=126).contains(&byte) {
            return Err(Error::InvalidMessage);
        }
        spaces = if byte == b' ' { spaces + 1 } else { 0 };
        if spaces >= 4 {
            return Err(Error::InvalidMessage);
        }
    }
    Ok(())
}

fn normalize_path(path: &str) -> Result<String, Error> {
    let normalized = path.to_ascii_lowercase().replace('h', "'").replace('p', "'");
    if normalized.is_empty() || normalized == "m/" {
        return Ok("m".to_owned());
    }
    if !normalized.starts_with('m') {
        return Err(Error::InvalidPath);
    }
    if normalized == "m" {
        return Ok(normalized);
    }
    let rest = normalized.strip_prefix("m/").ok_or(Error::InvalidPath)?;
    let components: Vec<_> = rest.split('/').collect();
    if components.len() > MAX_PATH_DEPTH {
        return Err(Error::PathTooDeep);
    }
    for component in components {
        let number = component.strip_suffix('\'').unwrap_or(component);
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::InvalidPath);
        }
        let index = number.parse::<u32>().map_err(|_| Error::InvalidPath)?;
        if index >= (1 << 31) || number != index.to_string() {
            return Err(Error::InvalidPath);
        }
    }
    Ok(normalized)
}

fn is_allowed_path(path: &DerivationPath, allowed_origins: &[DerivationPath]) -> bool {
    let path = path.as_ref();
    if path.is_empty() {
        return true;
    }

    allowed_origins.iter().any(|origin| {
        let origin = origin.as_ref();
        if !path.starts_with(origin) {
            return false;
        }
        let suffix = &path[origin.len()..];
        suffix.len() <= 2 && suffix.iter().all(|child| matches!(child, ChildNumber::Normal { .. }))
    })
}

pub fn decode_ur(ur_type: &str, cbor: &[u8]) -> Result<Vec<u8>, Error> {
    match UrValue::from_ur(&ur_type.to_ascii_lowercase(), cbor) {
        Ok(UrValue::Bytes(bytes)) => Ok(bytes.to_vec()),
        Ok(_) => Err(Error::InvalidUrType),
        Err(_) if !ur_type.eq_ignore_ascii_case(UR_TYPE) => Err(Error::InvalidUrType),
        Err(_) => Err(Error::InvalidUrCbor),
    }
}

pub fn sign(
    secp: &Secp256k1<All>,
    master_key: &MasterKey,
    network: Network,
    input: &[u8],
    allowed_origins: &[DerivationPath],
) -> Result<SignedResponse, Error> {
    let challenge = parse(input)?;
    if !is_allowed_path(&challenge.path, allowed_origins) {
        return Err(Error::PathNotAllowed);
    }
    let root_xpriv =
        SensitiveXpriv(Xpriv::new_master(network, &master_key.key.0).map_err(|_| Error::KeyDerivation)?);
    let xpriv =
        SensitiveXpriv(root_xpriv.0.derive_priv(secp, &challenge.path).map_err(|_| Error::KeyDerivation)?);
    let public_key = PublicKey::new(xpriv.0.private_key.public_key(secp));
    let compressed = CompressedPublicKey::try_from(public_key).map_err(|_| Error::AddressDerivation)?;
    let address = match challenge.address_type {
        AddressType::Classic => Address::p2pkh(&public_key, network),
        AddressType::P2wpkh => Address::p2wpkh(&compressed, network),
        AddressType::P2wpkhP2sh => Address::p2shwpkh(&compressed, network),
        AddressType::P2tr => {
            let (tweaked, _) = XOnlyPublicKey::from(public_key.inner).tap_tweak(secp, None);
            Address::p2tr_tweaked(tweaked, network)
        }
        AddressType::P2sh | AddressType::P2wsh | AddressType::P2wshP2sh => {
            return Err(Error::UnsupportedAddressType);
        }
    };
    let digest = signed_msg_hash(&challenge.message);
    let message = Message::from_digest_slice(digest.as_ref()).map_err(|_| Error::MessageDigest)?;
    let signature = MessageSignature {
        signature: secp.sign_ecdsa_recoverable(&message, &xpriv.0.private_key),
        compressed: true,
    };
    let envelope = format!(
        "-----BEGIN BITCOIN SIGNED MESSAGE-----\n{}\n-----BEGIN SIGNATURE-----\n{}\n{}\n-----END BITCOIN SIGNED MESSAGE-----\n",
        challenge.message,
        address,
        BASE64_STANDARD.encode(signature.serialize()),
    );
    Ok(SignedResponse(envelope))
}

pub fn encode_ur(signed: &SignedResponse) -> Result<Vec<u8>, Error> {
    minicbor::to_vec(minicbor::bytes::ByteVec::from(signed.as_bytes().to_vec())).map_err(|_| Error::UrEncode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_core_path_spellings_and_master_challenge() {
        assert_eq!(parse(b"Health check\nm/44h/0H/0p/7").unwrap().normalized_path, "m/44'/0'/0'/7");
        assert_eq!(parse(b"Health check\nM/44h/0H/0p/7").unwrap().normalized_path, "m/44'/0'/0'/7");
        assert_eq!(parse(b"Health check\n").unwrap().normalized_path, "m");
        assert!(parse(b"Health check\r\nm/84'/0'/0'/0/0\r\n").is_ok());
        assert!(parse(b"Health check\nm/84'/0'/0'/0/0\nAF_P2WPKH\n").is_ok());
    }

    #[test]
    fn rejects_unknown_address_types_and_oversized_messages() {
        assert!(matches!(parse(b"Health check\nm\nAF_UNKNOWN"), Err(Error::UnsupportedAddressType)));
        assert!(matches!(parse(b"Health check\nm\nwsh"), Err(Error::UnsupportedAddressType)));
        let message = "a".repeat(MAX_MESSAGE_LENGTH + 1);
        let input = format!("{message}\nm");
        assert!(matches!(parse(input.as_bytes()), Err(Error::InvalidMessage)));
    }

    #[test]
    fn restricts_signing_to_master_and_casa_origin_paths() {
        let allowed = [DerivationPath::from_str("m/49/1/0").unwrap()];
        assert!(is_allowed_path(&DerivationPath::master(), &allowed));
        assert!(is_allowed_path(&DerivationPath::from_str("m/49/1/0").unwrap(), &allowed));
        assert!(is_allowed_path(&DerivationPath::from_str("m/49/1/0/1/7").unwrap(), &allowed));
        assert!(!is_allowed_path(&DerivationPath::from_str("m/84'/0'/0'/0/0").unwrap(), &allowed));
        assert!(!is_allowed_path(&DerivationPath::from_str("m/49/1/0/1/7/2").unwrap(), &allowed));
        assert!(!is_allowed_path(&DerivationPath::from_str("m/49/1/0/1'").unwrap(), &allowed));
        let allowed = [DerivationPath::master()];
        assert!(is_allowed_path(&DerivationPath::from_str("m/0/7").unwrap(), &allowed));
        assert!(!is_allowed_path(&DerivationPath::from_str("m/0/7/2").unwrap(), &allowed));
        assert!(!is_allowed_path(&DerivationPath::from_str("m/0'").unwrap(), &allowed));
    }

    #[test]
    fn rejects_signing_outside_casa_paths() {
        let secp = Secp256k1::new();
        let master = MasterKey::from_entropy(&secp, Network::Bitcoin, &[0x66; 16], "", None).unwrap();
        let allowed = [DerivationPath::from_str("m/49/1/0").unwrap()];
        assert!(matches!(
            sign(&secp, &master, Network::Bitcoin, b"Health check\nm/84'/0'/0'/0/0", &allowed,),
            Err(Error::PathNotAllowed)
        ));
    }

    #[test]
    fn supports_core_wrapped_segwit_address_type() {
        let secp = Secp256k1::new();
        let master = MasterKey::from_entropy(&secp, Network::Bitcoin, &[0x66; 16], "", None).unwrap();
        let allowed = [DerivationPath::from_str("m/49'/0'/0'").unwrap()];
        let signed = sign(
            &secp,
            &master,
            Network::Bitcoin,
            b"Health check\nm/49'/0'/0'/0/0\nAF_P2WPKH_P2SH\n",
            &allowed,
        )
        .unwrap();
        let text = std::str::from_utf8(signed.as_bytes()).unwrap();
        assert!(text.lines().nth(3).unwrap().starts_with('3'));
    }

    #[test]
    fn emits_core_envelope() {
        let secp = Secp256k1::new();
        let master = MasterKey::from_entropy(&secp, Network::Bitcoin, &[0x66; 16], "", None).unwrap();
        let allowed = [DerivationPath::from_str("m/44'/0'/0'").unwrap()];
        let signed =
            sign(&secp, &master, Network::Bitcoin, b"Health check\nm/44'/0'/0'/0/0", &allowed).unwrap();
        let text = std::str::from_utf8(signed.as_bytes()).unwrap();
        assert!(text.starts_with("-----BEGIN BITCOIN SIGNED MESSAGE-----\n"));
        assert!(text.contains("\n-----BEGIN SIGNATURE-----\n"));
        assert!(text.ends_with("\n-----END BITCOIN SIGNED MESSAGE-----\n"));
    }
}
