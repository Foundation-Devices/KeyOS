// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod get_assertion;
mod get_info;
mod make_credential;

pub use get_assertion::{GetAssertionRequest, GetAssertionResponse};
pub use get_info::{GetInfoResponse, Options as GetInfoResponseOptions};
pub use make_credential::{MakeCredentialRequest, MakeCredentialResponse};

use super::error::Error;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    MakeCredential,
    GetAssertion,
    GetNextAssertion,
    GetInfo,
    ClientPIN,
    Reset,
    BioEnrollment,
    CredentialManagement,
    Selection,
    LargeBlobs,
    Config,
}
impl TryFrom<u8> for Command {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0x01 => Ok(Command::MakeCredential),
            0x02 => Ok(Command::GetAssertion),
            0x04 => Ok(Command::GetInfo),
            0x06 => Ok(Command::ClientPIN),
            0x07 => Ok(Command::Reset),
            0x08 => Ok(Command::GetNextAssertion),
            0x09 => Ok(Command::BioEnrollment),
            0x0a => Ok(Command::CredentialManagement),
            0x0b => Ok(Command::Selection),
            0x0c => Ok(Command::LargeBlobs),
            0x0d => Ok(Command::Config),
            _ => Err(Error::InvalidCommand),
        }
    }
}
impl From<Command> for u8 {
    fn from(value: Command) -> u8 {
        match value {
            Command::MakeCredential => 0x01,
            Command::GetAssertion => 0x02,
            Command::GetInfo => 0x04,
            Command::ClientPIN => 0x06,
            Command::Reset => 0x07,
            Command::GetNextAssertion => 0x08,
            Command::BioEnrollment => 0x09,
            Command::CredentialManagement => 0x0a,
            Command::Selection => 0x0b,
            Command::LargeBlobs => 0x0c,
            Command::Config => 0x0d,
        }
    }
}

#[derive(Debug)]
pub struct ClientDataHash(pub [u8; 32]);
impl<'b, C> minicbor::Decode<'b, C> for ClientDataHash {
    fn decode(d: &mut minicbor::Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let bytes = d.bytes()?;
        Ok(ClientDataHash(bytes.try_into().unwrap()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(i32)]
pub enum PublicKeyCredential {
    Es256 = -7,
    Es384 = -35,
    Es512 = -36,
    EdDsa = -8,
    // Rs256 = -257,
    Unknown = 0,
}
impl From<i32> for PublicKeyCredential {
    fn from(value: i32) -> Self {
        match value {
            -7 => PublicKeyCredential::Es256,
            -35 => PublicKeyCredential::Es384,
            -36 => PublicKeyCredential::Es512,
            -8 => PublicKeyCredential::EdDsa,
            // -257 => PublicKeyCredential::Rs256,
            _ => PublicKeyCredential::Unknown,
        }
    }
}

#[derive(Debug)]
pub struct PublicKeyCredentialParameters {
    _type: String,
    pub alg: PublicKeyCredential,
}
impl PublicKeyCredentialParameters {
    /// Keys with algorithm ES256 (-7) MUST specify P-256 (1) as the crv parameter and MUST NOT use the
    /// compressed point form.
    #[cfg(test)]
    pub fn es256() -> Self { Self { _type: "public-key".to_string(), alg: PublicKeyCredential::Es256 } }

    /// Keys with algorithm ES384 (-35) MUST specify P-384 (2) as the crv parameter and MUST NOT use the
    /// compressed point form.
    #[cfg(test)]
    pub fn es384() -> Self { Self { _type: "public-key".to_string(), alg: PublicKeyCredential::Es384 } }

    // /// Keys with algorithm ES512 (-36) MUST specify P-521 (3) as the crv parameter and MUST NOT use the
    // /// compressed point form.
    // pub fn es512() -> Self { Self { _type: "public-key".to_string(), alg: PublicKeyCredential::Es512 } }

    /// Keys with algorithm EdDSA (-8) MUST specify Ed25519 (6) as the crv parameter. (These always use a
    /// compressed form in COSE.)
    #[cfg(test)]
    pub fn ed_dsa() -> Self { Self { _type: "public-key".to_string(), alg: PublicKeyCredential::EdDsa } }
}
impl<C> minicbor::Encode<C> for PublicKeyCredentialParameters {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.map(2)?.str("alg")?.i32(self.alg as i32)?.str("type")?.str(&self._type)?.ok()
    }
}
impl<'b, C> minicbor::Decode<'b, C> for PublicKeyCredentialParameters {
    fn decode(d: &mut minicbor::Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let mut len = d.map()?;
        let mut _type = String::new();
        let mut alg = 0;
        while len.map(|l| l > 0).unwrap_or(false) {
            len = len.map(|l| l - 1);
            match d.str()? {
                "type" => _type = d.str()?.to_string(),
                "alg" => alg = d.i32()?,
                _ => d.skip()?,
            }
        }
        Ok(PublicKeyCredentialParameters { _type, alg: PublicKeyCredential::from(alg) })
    }
}

#[derive(Debug)]
pub struct PublicKeyCredentialDescriptor {
    pub id: Vec<u8>,
    _type: String,
}
impl PublicKeyCredentialDescriptor {
    pub fn with_public_key(data: &[u8]) -> Self {
        Self { id: data.to_vec(), _type: "public-key".to_string() }
    }
}
impl<C> minicbor::Encode<C> for PublicKeyCredentialDescriptor {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.map(2)?.str("id")?.bytes(&self.id)?.str("type")?.str(&self._type)?.ok()
    }
}
impl<'b, C> minicbor::Decode<'b, C> for PublicKeyCredentialDescriptor {
    fn decode(d: &mut minicbor::Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let mut len = d.map()?;
        let mut id = Vec::new();
        let mut _type = String::new();
        while len.map(|l| l > 0).unwrap_or(false) {
            len = len.map(|l| l - 1);
            match d.str()? {
                "id" => id = d.bytes()?.to_vec(),
                "type" => _type = d.str()?.to_string(),
                _ => d.skip()?,
            }
        }
        Ok(PublicKeyCredentialDescriptor { id, _type })
    }
}

#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct PublicKeyCredentialRpEntity {
    pub id: String,
    pub name: Option<String>,
}
impl<'b, C> minicbor::Decode<'b, C> for PublicKeyCredentialRpEntity {
    fn decode(d: &mut minicbor::Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let mut len = d.map()?;
        let mut id = String::new();
        let mut name = None;
        while len.map(|l| l > 0).unwrap_or(false) {
            len = len.map(|l| l - 1);
            match d.str()? {
                "id" => id = d.str()?.to_string(),
                "name" => name = Some(d.str()?.to_string()),
                _ => d.skip()?,
            }
        }
        Ok(PublicKeyCredentialRpEntity { id, name })
    }
}

#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct PublicKeyCredentialUserEntity {
    pub id: Vec<u8>,
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub icon: Option<String>,
}
impl<C> minicbor::Encode<C> for PublicKeyCredentialUserEntity {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.map(2)?.str("id")?.bytes(&self.id)?;
        if let Some(icon) = &self.icon {
            e.str("icon")?.str(&icon)?;
        }
        if let Some(name) = &self.name {
            e.str("name")?.str(&name)?;
        }
        if let Some(display_name) = &self.display_name {
            e.str("displayName")?.str(&display_name)?;
        }
        e.ok()
    }
}
impl<'b, C> minicbor::Decode<'b, C> for PublicKeyCredentialUserEntity {
    fn decode(d: &mut minicbor::Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let mut len = d.map()?;
        let mut id = Vec::new();
        let mut name = None;
        let mut display_name = None;
        let mut icon = None;
        while len.map(|l| l > 0).unwrap_or(false) {
            len = len.map(|l| l - 1);
            match d.str()? {
                "id" => id = d.bytes()?.to_vec(),
                "icon" => icon = Some(d.str()?.to_string()),
                "name" => name = Some(d.str()?.to_string()),
                "displayName" => display_name = Some(d.str()?.to_string()),
                _ => d.skip()?,
            }
        }
        Ok(PublicKeyCredentialUserEntity { id, name, display_name, icon })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AuthDataFlags {
    pub up: bool,
    pub uv: bool,
    pub at: bool,
    pub ed: bool,
}
impl From<AuthDataFlags> for u8 {
    fn from(f: AuthDataFlags) -> Self {
        (f.up as u8) | ((f.uv as u8) << 2) | ((f.at as u8) << 6) | ((f.ed as u8) << 7)
    }
}

#[derive(Debug)]
pub struct AttestedCredentialData {
    pub aaguid: [u8; 16],
    pub credential_id_length: u16,
    pub credential_id: Vec<u8>,
    pub credential_public_key: Vec<u8>,
}
impl AttestedCredentialData {
    #[cfg(not(test))]
    pub fn prime(aaguid: [u8; 16], public_key: &[u8], attestation_pubkey: &[u8]) -> Self {
        AttestedCredentialData {
            aaguid,
            credential_id_length: public_key.len() as u16,
            credential_id: public_key.to_vec(),
            credential_public_key: attestation_pubkey.to_vec(),
        }
    }

    #[cfg(test)]
    pub fn prime(aaguid: [u8; 16], public_key: &[u8], _attestation_pubkey: &[u8]) -> Self {
        AttestedCredentialData {
            aaguid,
            credential_id_length: public_key.len() as u16,
            credential_id: public_key.to_vec(),
            credential_public_key: vec![
                0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21, 0x58, 0x20, 0x4e, 0xbb, 0x5f, 0xff, 0x3e,
                0x89, 0x21, 0xd9, 0xb1, 0x19, 0x9b, 0x41, 0xef, 0x6c, 0xeb, 0x4f, 0xe6, 0xb6, 0x00, 0x0a,
                0xe9, 0xb3, 0x1a, 0x84, 0x01, 0xdd, 0xaf, 0xfd, 0xd1, 0x14, 0x70, 0x1c, 0x22, 0x58, 0x20,
                0x3c, 0x5d, 0x48, 0xcc, 0x62, 0xb7, 0xe2, 0x24, 0xef, 0x9a, 0xe4, 0x31, 0x84, 0xee, 0xdc,
                0x86, 0x42, 0x4b, 0xf3, 0xe2, 0xb6, 0x54, 0xc5, 0xee, 0xf6, 0xbc, 0xe8, 0x5f, 0x29, 0x6d,
                0x8e, 0xce,
            ],
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&self.aaguid);
        data.extend_from_slice(&self.credential_id_length.to_be_bytes());
        data.extend_from_slice(&self.credential_id);
        data.extend_from_slice(&self.credential_public_key);
        data
    }
}
impl<C> minicbor::Encode<C> for AttestedCredentialData {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.bytes(&self.to_vec())?.ok()
    }
}

#[derive(Debug, Clone)]
pub enum Extension {
    /// This authentication extension allows WebAuthn Relying Parties that have previously registered a
    /// credential using the legacy FIDO JavaScript APIs to request an assertion.
    AppId,
    /// This registration extension and authentication extension allows for a simple form of transaction
    /// authorization. A WebAuthn Relying Party can specify a prompt string, intended for display on a
    /// trusted device on the authenticator
    TxAuthSimple,
    /// This registration extension and authentication extension allows images to be used as transaction
    /// authorization prompts as well. This allows authenticators without a font rendering engine to be used
    /// and also supports a richer visual appearance than accomplished with the webauthn.txauth.simple
    /// extension.
    TxAuthGeneric,
    /// This registration extension allows relying parties to specify a credential protection policy when
    /// creating a credential. Additionally, authenticators may choose to establish a default credential
    /// protection policy greater than userVerificationOptional (the lowest level) and unilaterally enforce
    /// such policy.
    CredProtect,
    /// This registration extension and authentication extension enables the platform to retrieve a symmetric
    /// secret scoped to the credential from the authenticator.
    HmacSecret,
    /// This client platform-only extension provides for storage and retrieval of a per-credential key that
    /// is used by the client platform when writing and reading elements in the large-blob array.
    LargeBlobKey,
    /// This registration extension and authentication extension enables RPs to provide a small amount of
    /// extra credential configuration information (the credBlob value) to the authenticator when a
    /// credential is made.
    CredBlob,
    /// This registration extension returns the current minimum PIN length value to the Relying Party.
    MinPinLength,
    // TODO: continue from https://www.iana.org/assignments/webauthn/webauthn.xhtml
    Unknown(String),
}
impl ToString for Extension {
    fn to_string(&self) -> String {
        match self {
            Extension::AppId => "appid".to_string(),
            Extension::TxAuthSimple => "txAuthSimple".to_string(),
            Extension::TxAuthGeneric => "txAuthGeneric".to_string(),
            Extension::CredProtect => "credProtect".to_string(),
            Extension::HmacSecret => "hmac-secret".to_string(),
            Extension::LargeBlobKey => "largeBlobKey".to_string(),
            Extension::CredBlob => "credBlob".to_string(),
            Extension::MinPinLength => "minPinLength".to_string(),
            Extension::Unknown(s) => s.to_string(),
        }
    }
}
impl From<&str> for Extension {
    fn from(s: &str) -> Self {
        match s {
            "appid" => Extension::AppId,
            "txAuthSimple" => Extension::TxAuthSimple,
            "txAuthGeneric" => Extension::TxAuthGeneric,
            "credProtect" => Extension::CredProtect,
            "hmac-secret" => Extension::HmacSecret,
            "largeBlobKey" => Extension::LargeBlobKey,
            "credBlob" => Extension::CredBlob,
            "minPinLength" => Extension::MinPinLength,
            s => Extension::Unknown(s.to_string()),
        }
    }
}
impl<C> minicbor::Encode<C> for Extension {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.str(&self.to_string())?.ok()
    }
}
impl<'b, C> minicbor::Decode<'b, C> for Extension {
    fn decode(d: &mut minicbor::Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        Ok(Extension::from(d.str()?))
    }
}

#[derive(Debug, Clone)]
pub struct ExtensionOutput {
    pub ext: Extension,
    pub data: Vec<u8>,
}
impl<C> minicbor::Encode<C> for ExtensionOutput {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.encode(&self.ext)?.bytes(&self.data)?.ok()
    }
}

#[derive(Debug)]
pub struct ExtensionOutputs(Vec<ExtensionOutput>);
impl ExtensionOutputs {
    pub fn new() -> Self { ExtensionOutputs(Vec::new()) }
}
impl<C> minicbor::Encode<C> for ExtensionOutputs {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        let ext_cnt = self.0.len();
        if ext_cnt > 0 {
            e.map(ext_cnt as u64)?;
            for ext_output in &self.0 {
                e.encode(&ext_output)?;
            }
        }
        e.ok()
    }
}

#[derive(Debug)]
pub struct AuthData {
    pub rp_id_hash: [u8; 32],
    pub flags: AuthDataFlags,
    pub sign_count: u32,
    pub attest_credential_data: Option<AttestedCredentialData>,
    pub extentions: ExtensionOutputs,
}
impl AuthData {
    pub fn new(_rp_id: &str) -> Self {
        #[cfg(not(test))]
        let rp_id_hash = crate::CryptoApi::default().sha256(_rp_id.as_bytes()).expect("sha256");
        #[cfg(test)]
        let rp_id_hash = [
            0x20, 0xa8, 0x3b, 0x42, 0xcd, 0x54, 0x0c, 0x2f, 0xcd, 0xee, 0x61, 0xe4, 0xf1, 0x47, 0x93, 0xed,
            0xe7, 0x74, 0xfa, 0x82, 0x58, 0x83, 0x56, 0x83, 0xa7, 0xc8, 0xe8, 0x85, 0xa8, 0xc2, 0x70, 0xc2,
        ];
        Self {
            rp_id_hash,
            flags: AuthDataFlags { up: false, uv: false, at: false, ed: false },
            sign_count: 0,
            attest_credential_data: None,
            extentions: ExtensionOutputs::new(),
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        let mut auth_data = self.rp_id_hash.to_vec();
        auth_data.push(u8::from(self.flags));
        auth_data.extend_from_slice(&self.sign_count.to_be_bytes());
        if let Some(attest_credential_data) = &self.attest_credential_data {
            auth_data.extend_from_slice(&attest_credential_data.to_vec());
        }
        // TODO: extensions ?
        auth_data
    }

    pub fn add_attest_credential_data(&mut self, data: AttestedCredentialData) {
        self.attest_credential_data = Some(data);
        self.flags.at = true;
    }

    pub fn add_extention_output(&mut self, ext_output: ExtensionOutput) {
        self.extentions.0.push(ext_output);
        self.flags.ed = true;
    }
}
impl<C> minicbor::Encode<C> for AuthData {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.bytes(&self.to_vec())?.encode(&self.extentions)?.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_client_data_hash() {
        let data_client_data_hash = [
            0x58, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0,
        ];
        let client_data_hash = minicbor::decode::<ClientDataHash>(&data_client_data_hash).unwrap();
        assert_eq!(client_data_hash.0, [0u8; 32]);
    }

    #[test]
    fn public_key_credential_parameters() {
        let params = PublicKeyCredentialParameters::es256();
        let encoded = minicbor::to_vec(&params).unwrap();
        let decoded = minicbor::decode::<PublicKeyCredentialParameters>(&encoded).unwrap();
        assert_eq!(params.alg, decoded.alg);
        assert_eq!(params._type, decoded._type);
    }

    #[test]
    fn decode_public_key_credential_descriptor() {
        let data_desc = [
            0xa2, 0x62, 0x69, 0x64, 0x58, 0x40, 0x92, 0x5c, 0x1a, 0xfe, 0x50, 0x36, 0xc0, 0x75, 0xe3, 0xf3,
            0x10, 0xd8, 0x6b, 0x53, 0xe9, 0x11, 0x7a, 0x8b, 0xac, 0xe9, 0xf1, 0xa8, 0x03, 0x3e, 0xc9, 0x2f,
            0xcf, 0xd4, 0x67, 0x33, 0x4c, 0xb7, 0xdb, 0x74, 0x0c, 0x6e, 0x07, 0x54, 0xd0, 0xbf, 0xad, 0x76,
            0xe8, 0x80, 0xa9, 0x28, 0x67, 0x79, 0x92, 0x45, 0xe9, 0xdb, 0xa6, 0x97, 0x78, 0x4a, 0xbf, 0x5d,
            0x06, 0xa5, 0x2d, 0x61, 0x1a, 0xf9, 0x64, 0x74, 0x79, 0x70, 0x65, 0x6a, 0x70, 0x75, 0x62, 0x6c,
            0x69, 0x63, 0x2d, 0x6b, 0x65, 0x79,
        ];
        let desc = minicbor::decode::<PublicKeyCredentialDescriptor>(&data_desc).unwrap();
        println!("{:02x?}", desc);
        assert_eq!(
            desc.id,
            vec![
                0x92, 0x5c, 0x1a, 0xfe, 0x50, 0x36, 0xc0, 0x75, 0xe3, 0xf3, 0x10, 0xd8, 0x6b, 0x53, 0xe9,
                0x11, 0x7a, 0x8b, 0xac, 0xe9, 0xf1, 0xa8, 0x03, 0x3e, 0xc9, 0x2f, 0xcf, 0xd4, 0x67, 0x33,
                0x4c, 0xb7, 0xdb, 0x74, 0x0c, 0x6e, 0x07, 0x54, 0xd0, 0xbf, 0xad, 0x76, 0xe8, 0x80, 0xa9,
                0x28, 0x67, 0x79, 0x92, 0x45, 0xe9, 0xdb, 0xa6, 0x97, 0x78, 0x4a, 0xbf, 0x5d, 0x06, 0xa5,
                0x2d, 0x61, 0x1a, 0xf9
            ]
        );
        assert_eq!(desc._type, "public-key".to_string());
    }

    #[test]
    fn decode_public_key_credential_rp_entity() {
        let data_rp = [
            0xa2, 0x62, 0x69, 0x64, 0x6a, 0x67, 0x69, 0x74, 0x6c, 0x61, 0x62, 0x2e, 0x63, 0x6f, 0x6d, 0x64,
            0x6e, 0x61, 0x6d, 0x65, 0x66, 0x47, 0x69, 0x74, 0x4c, 0x61, 0x62,
        ];
        let rp = minicbor::decode::<PublicKeyCredentialRpEntity>(&data_rp).unwrap();
        assert_eq!(rp.id, "gitlab.com".to_string());
        assert_eq!(rp.name, Some("GitLab".to_string()));
    }

    #[test]
    fn decode_public_key_credential_user_entity() {
        let data_user = [
            0xa3, 0x62, 0x69, 0x64, 0x58, 0x40, 0xac, 0x5b, 0xda, 0x27, 0xc2, 0x1a, 0x67, 0x30, 0xb2, 0xbf,
            0xef, 0x25, 0x13, 0xe7, 0xe4, 0x72, 0x67, 0x14, 0x31, 0xff, 0xfa, 0x8a, 0xcf, 0x1c, 0xae, 0x07,
            0xec, 0xa1, 0x9f, 0x16, 0x12, 0xd5, 0x05, 0xbc, 0x4f, 0x33, 0x2b, 0x9f, 0xfb, 0x74, 0xd5, 0x25,
            0xaa, 0x99, 0x5c, 0x6e, 0x75, 0x73, 0xae, 0x97, 0x03, 0xa2, 0xfa, 0x7b, 0xca, 0xd2, 0x47, 0x5a,
            0xd1, 0x8b, 0xff, 0xed, 0xe9, 0xc7, 0x64, 0x6e, 0x61, 0x6d, 0x65, 0x6c, 0x66, 0x69, 0x73, 0x63,
            0x61, 0x5f, 0x66, 0x61, 0x63, 0x69, 0x6c, 0x65, 0x6b, 0x64, 0x69, 0x73, 0x70, 0x6c, 0x61, 0x79,
            0x4e, 0x61, 0x6d, 0x65, 0x6c, 0x66, 0x69, 0x73, 0x63, 0x61, 0x5f, 0x66, 0x61, 0x63, 0x69, 0x6c,
            0x65,
        ];
        let user = minicbor::decode::<PublicKeyCredentialUserEntity>(&data_user).unwrap();
        println!("{:02x?}", user);
        assert_eq!(
            user.id,
            vec![
                0xac, 0x5b, 0xda, 0x27, 0xc2, 0x1a, 0x67, 0x30, 0xb2, 0xbf, 0xef, 0x25, 0x13, 0xe7, 0xe4,
                0x72, 0x67, 0x14, 0x31, 0xff, 0xfa, 0x8a, 0xcf, 0x1c, 0xae, 0x07, 0xec, 0xa1, 0x9f, 0x16,
                0x12, 0xd5, 0x05, 0xbc, 0x4f, 0x33, 0x2b, 0x9f, 0xfb, 0x74, 0xd5, 0x25, 0xaa, 0x99, 0x5c,
                0x6e, 0x75, 0x73, 0xae, 0x97, 0x03, 0xa2, 0xfa, 0x7b, 0xca, 0xd2, 0x47, 0x5a, 0xd1, 0x8b,
                0xff, 0xed, 0xe9, 0xc7
            ]
        );
        assert_eq!(user.name, Some("fisca_facile".to_string()));
        assert_eq!(user.display_name, Some("fisca_facile".to_string()));
    }
}
