// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Casa pairing exports compatible with Passport Core.

use {
    crate::{
        account_id::AccountId,
        export_account::{ImportedAccountDefaults, UrExport, WalletConnector},
        sensitive_xpriv::SensitiveXpriv,
        state::AccountColor,
        AppState, ExportCapabilities, ExportFormats, VisualFormat,
    },
    anyhow::{bail, Context},
    foundation_urtypes::registry::{CoinInfo, CoinType, DerivedKeyRef, KeypathRef},
    minicbor::{data::Tag, Encode, Encoder},
    ngwallet::{
        bdk_wallet::bitcoin::{
            bip32::{ChildNumber, DerivationPath, Fingerprint, Xpriv, Xpub},
            Network,
        },
        config::NgAccountConfig,
    },
    slint_keyos_platform::slint::SharedString,
    std::num::NonZeroU32,
};

pub struct Connector;
pub static CONNECTOR: Connector = Connector;

const PAIRING_UR_TYPE: &str = "crypto-account";
const CASA_PURPOSE: u32 = 45;
const CASA_UR_PATH: [u32; 1] = [CASA_PURPOSE | (1 << 31)];
const TAG_CRYPTO_OUTPUT: Tag = Tag::new(308);
const TAG_SCRIPT_HASH: Tag = Tag::new(400);
const TAG_WITNESS_PUBLIC_KEY_HASH: Tag = Tag::new(404);
const TAG_HDKEY_LEGACY: Tag = Tag::new(303);
const TAG_KEYPATH_LEGACY: Tag = Tag::new(304);
const TAG_COIN_INFO_LEGACY: Tag = Tag::new(305);

struct PairingKeys {
    fingerprint: Fingerprint,
    root: Xpub,
    casa: Xpub,
}

fn casa_origin(fingerprint: NonZeroU32) -> KeypathRef<'static> {
    KeypathRef { components: (&CASA_UR_PATH).into(), source_fingerprint: Some(fingerprint), depth: Some(1) }
}

impl Connector {
    fn pairing_keys(state: &AppState, id: &AccountId, cfg: &NgAccountConfig) -> anyhow::Result<PairingKeys> {
        let master_key = state.store.load_master_key(cfg.network).context("load active Master Key")?;
        if id.fingerprint().copied() != Some(master_key.fingerprint) {
            bail!("active Master Key does not match the account selected for Casa pairing");
        }
        let root = SensitiveXpriv(
            Xpriv::new_master(cfg.network, &master_key.key.0).context("derive Casa root key")?,
        );
        let casa_path = DerivationPath::from(vec![ChildNumber::Hardened { index: CASA_PURPOSE }]);
        let casa = SensitiveXpriv(
            root.0.derive_priv(&state.store.secp, &casa_path).context("derive Casa m/45' key")?,
        );
        Ok(PairingKeys {
            fingerprint: master_key.fingerprint,
            root: Xpub::from_priv(&state.store.secp, &root.0),
            casa: Xpub::from_priv(&state.store.secp, &casa.0),
        })
    }

    fn summary(state: &AppState, id: &AccountId, cfg: &NgAccountConfig) -> anyhow::Result<String> {
        let keys = Self::pairing_keys(state, id, cfg)?;
        Ok(format!(
            concat!(
                "    # Passport Summary File\n",
                "    # For wallet with master key fingerprint: {}\n",
                "\n",
                "    # Top-level, 'master' extended public key ('m/'):\n",
                "\n",
                "    {}\n",
                "\n",
                "    # Casa extended public key (\"m/45'\"):\n",
                "\n",
                "    {}\n",
                "    "
            ),
            keys.fingerprint, keys.root, keys.casa,
        ))
    }
}

fn encode_legacy_derived_key(
    encoder: &mut Encoder<Vec<u8>>,
    key: &DerivedKeyRef<'_>,
) -> Result<(), minicbor::encode::Error<std::convert::Infallible>> {
    let DerivedKeyRef {
        is_private,
        key_data,
        chain_code,
        use_info,
        origin,
        children,
        parent_fingerprint,
        name,
        note,
    } = key;
    let len = u64::from(*is_private)
        + 1
        + u64::from(chain_code.is_some())
        + u64::from(use_info.is_some())
        + u64::from(origin.is_some())
        + u64::from(children.is_some())
        + u64::from(parent_fingerprint.is_some())
        + u64::from(name.is_some())
        + u64::from(note.is_some());
    encoder.map(len)?;

    if *is_private {
        encoder.u8(2)?.bool(true)?;
    }
    encoder.u8(3)?.bytes(key_data)?;
    if let Some(chain_code) = chain_code {
        encoder.u8(4)?.bytes(chain_code)?;
    }
    if let Some(use_info) = use_info {
        encoder.u8(5)?.tag(TAG_COIN_INFO_LEGACY)?;
        use_info.encode(encoder, &mut ())?;
    }
    if let Some(origin) = origin {
        encoder.u8(6)?.tag(TAG_KEYPATH_LEGACY)?;
        origin.encode(encoder, &mut ())?;
    }
    if let Some(children) = children {
        encoder.u8(7)?.tag(TAG_KEYPATH_LEGACY)?;
        children.encode(encoder, &mut ())?;
    }
    if let Some(parent_fingerprint) = parent_fingerprint {
        encoder.u8(8)?.u32(parent_fingerprint.get())?;
    }
    if let Some(name) = name {
        encoder.u8(9)?.str(name)?;
    }
    if let Some(note) = note {
        encoder.u8(10)?.str(note)?;
    }

    Ok(())
}

fn encode_crypto_account(master_fingerprint: u32, keys: &[DerivedKeyRef<'_>]) -> anyhow::Result<Vec<u8>> {
    let mut encoder = Encoder::new(Vec::new());
    encoder.map(2)?.u8(1)?.u32(master_fingerprint)?.u8(2)?.array(keys.len() as u64)?;
    for key in keys {
        encoder
            .tag(TAG_CRYPTO_OUTPUT)?
            .tag(TAG_SCRIPT_HASH)?
            .tag(TAG_WITNESS_PUBLIC_KEY_HASH)?
            .tag(TAG_HDKEY_LEGACY)?;
        encode_legacy_derived_key(&mut encoder, key)?;
    }
    Ok(encoder.into_writer())
}

impl WalletConnector for Connector {
    fn capabilities(&self) -> ExportCapabilities { ExportCapabilities { single: false, join_multisig: true } }

    fn formats(&self) -> ExportFormats { ExportFormats { visual: VisualFormat::UR2, file: true } }

    fn file_extension(&self, _as_multi: bool) -> String { "txt".to_owned() }

    fn display_name(&self) -> SharedString { "Casa".into() }

    fn imported_account_defaults(&self) -> Option<ImportedAccountDefaults> {
        Some(ImportedAccountDefaults { label: self.display_name(), color: AccountColor::Purple })
    }

    fn connect(
        &self,
        state: &AppState,
        id: &AccountId,
        cfg: &NgAccountConfig,
        _as_multi: bool,
    ) -> Result<String, anyhow::Error> {
        Self::summary(state, id, cfg)
    }

    fn connect_ur(
        &self,
        state: &AppState,
        id: &AccountId,
        cfg: &NgAccountConfig,
        _as_multi: bool,
    ) -> Result<Option<UrExport>, anyhow::Error> {
        let keys = Self::pairing_keys(state, id, cfg)?;
        let fingerprint = NonZeroU32::new(u32::from_be_bytes(keys.fingerprint.to_bytes()))
            .context("Casa pairing fingerprint must not be zero")?;
        let use_info = match cfg.network {
            Network::Bitcoin => CoinInfo::BTC_MAINNET,
            _ => CoinInfo::new(CoinType::BTC, CoinInfo::NETWORK_BTC_TESTNET),
        };
        let root_key = DerivedKeyRef {
            is_private: false,
            key_data: keys.root.public_key.serialize(),
            chain_code: Some(keys.root.chain_code.to_bytes()),
            use_info: Some(use_info.clone()),
            origin: Some(KeypathRef::new_master(fingerprint)),
            children: None,
            parent_fingerprint: None,
            name: None,
            note: None,
        };
        let casa_key = DerivedKeyRef {
            is_private: false,
            key_data: keys.casa.public_key.serialize(),
            chain_code: Some(keys.casa.chain_code.to_bytes()),
            use_info: Some(use_info),
            origin: Some(casa_origin(fingerprint)),
            children: None,
            parent_fingerprint: Some(fingerprint),
            name: None,
            note: None,
        };
        let cbor = encode_crypto_account(fingerprint.get(), &[root_key, casa_key])
            .context("encode Casa pairing crypto-account")?;
        Ok(Some(UrExport { ur_type: PAIRING_UR_TYPE, cbor }))
    }

    fn export_filename(&self, id: &AccountId, _as_multi: bool) -> String {
        let fingerprint = id.fingerprint().map(|value| value.to_string().to_lowercase()).unwrap_or_default();
        format!("{fingerprint}-casa-multisig.txt")
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        foundation_urtypes::registry::{ChildNumber as UrChildNumber, HDKeyRef},
        minicbor::{Decode, Decoder},
    };

    #[test]
    fn crypto_account_wire_format_is_pinned() {
        let fingerprint = NonZeroU32::new(0x1234_5678).unwrap();
        let keys = [
            DerivedKeyRef {
                is_private: false,
                key_data: [2; 33],
                chain_code: Some([3; 32]),
                use_info: Some(CoinInfo::BTC_MAINNET),
                origin: Some(KeypathRef::new_master(fingerprint)),
                children: None,
                parent_fingerprint: None,
                name: None,
                note: None,
            },
            DerivedKeyRef {
                is_private: false,
                key_data: [4; 33],
                chain_code: Some([5; 32]),
                use_info: Some(CoinInfo::BTC_MAINNET),
                origin: Some(casa_origin(fingerprint)),
                children: None,
                parent_fingerprint: Some(fingerprint),
                name: None,
                note: None,
            },
        ];

        let cbor = encode_crypto_account(fingerprint.get(), &keys).unwrap();
        assert_eq!(
            hex::encode(&cbor),
            concat!(
                "a2011a123456780282d90134d90190d90194d9012fa40358210202020202020202020202020202020202020202",
                "020202020202020202020202020458200303030303030303030303030303030303030303030303030303030303",
                "03030305d90131a006d90130a30180021a123456780300d90134d90190d90194d9012fa5035821040404040404",
                "040404040404040404040404040404040404040404040404040404045820050505050505050505050505050505",
                "050505050505050505050505050505050505d90131a006d90130a30182182df5021a123456780301081a123456",
                "78",
            )
        );
        let mut decoder = Decoder::new(&cbor);
        assert_eq!(decoder.map().unwrap(), Some(2));
        assert_eq!(decoder.u8().unwrap(), 1);
        assert_eq!(decoder.u32().unwrap(), fingerprint.get());
        assert_eq!(decoder.u8().unwrap(), 2);
        assert_eq!(decoder.array().unwrap(), Some(2));

        for expected_depth in 0..=1 {
            assert_eq!(decoder.tag().unwrap(), Tag::new(308));
            assert_eq!(decoder.tag().unwrap(), Tag::new(400));
            assert_eq!(decoder.tag().unwrap(), Tag::new(404));
            assert_eq!(decoder.tag().unwrap(), Tag::new(303));
            let HDKeyRef::DerivedKey(key) = HDKeyRef::decode(&mut decoder, &mut ()).unwrap() else {
                panic!("Casa account entries must be derived public keys");
            };
            let origin = key.origin.unwrap();
            assert_eq!(origin.source_fingerprint, Some(fingerprint));
            assert_eq!(origin.depth, Some(expected_depth));
            if expected_depth == 0 {
                assert!(origin.components.is_empty());
            } else {
                let component = origin.components.iter().next().unwrap();
                assert_eq!(component.number, UrChildNumber::Number(45));
                assert!(component.is_hardened);
            }
        }
    }
}
