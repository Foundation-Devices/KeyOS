// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cmp::Ordering;
use std::time::Duration;

use anyhow::{bail, Context};
use ngwallet::{
    bdk_wallet::{
        bitcoin::{
            amount::Amount, bip32::Xpriv, psbt::Error as BdkPsbtError, Network as NgNetwork,
            NetworkKind as NgNetworkKind, Psbt,
        },
        signer::SignerError,
        SignOptions,
    },
    config::NgAccountConfig,
    psbt::{OutputKind, TransactionDetails},
};
use quantum_link::{
    foundation_api::bitcoin::BroadcastTransaction,
    messages::{PublishPsbt, SubscribeSignPsbt},
};
use slint_keyos_platform::{
    async_archive,
    slint::{ComponentHandle, ModelRc, SharedString, ToSharedString, VecModel},
    spawn_local, spawn_worker, subscribe_archive, timeout, StoredValue,
};

use crate::{
    account_id::AccountId,
    bitcoin_settings::ExchangeRate,
    quantum_link_permissions::QuantumLinkPermissions,
    state::{AccountColor, AppState, PendingMultiSig, PendingSingleSig},
    store::AccountSource,
    CreateAccount, CreateAccountState, DisplayAmount, FileSaveState, MultiSigView, Navigate, NavigateOptions,
    PsbtOutputKind, PsbtOutputView, PsbtView, ShowFiatValue, SignPsbt, SignPsbtState,
};

const FEE_WARNING_THRESHOLD: i32 = 25;
const MAX_DISPLAY_DIGITS: usize = 9;

#[derive(Default)]
pub enum PendingPsbt {
    #[default]
    None,
    Unsigned {
        account_id: AccountId,
        psbt: Psbt,
        details: TransactionDetails,
        origin: PsbtOrigin,
    },
    Signed {
        account_id: AccountId,
        psbt: Psbt,
        origin: PsbtOrigin,
    },
    NotSaved {
        psbt: Psbt,
        origin: PsbtOrigin,
    },
}

#[derive(Clone)]
pub enum PsbtOrigin {
    Qr { ur_type: String },
    QuantumLink,
    File,
}

impl From<&PsbtOrigin> for crate::PsbtOriginView {
    fn from(origin: &PsbtOrigin) -> Self {
        match origin {
            PsbtOrigin::Qr { .. } => crate::PsbtOriginView::Qr,
            PsbtOrigin::QuantumLink => crate::PsbtOriginView::Quantum,
            PsbtOrigin::File => crate::PsbtOriginView::File,
        }
    }
}

impl PendingPsbt {
    pub fn take_unsigned(&mut self) -> Option<(AccountId, Psbt, TransactionDetails, PsbtOrigin)> {
        match std::mem::take(self) {
            PendingPsbt::Unsigned { account_id, psbt, details, origin } => {
                Some((account_id, psbt, details, origin))
            }
            state => {
                *self = state;
                None
            }
        }
    }
}

pub fn init(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let global = ui.global::<SignPsbt>();

    spawn_local(async move {
        let mut events = subscribe_archive::<QuantumLinkPermissions, _>(SubscribeSignPsbt);
        while let Some(msg) = events.next().await {
            verify::verify_psbt(state, msg.psbt, PsbtOrigin::QuantumLink, false).await
        }
    })
    .detach();

    global.on_cancel_signing(move || {
        let _ = std::mem::take(&mut state.borrow_mut().pending_psbt);
        let ui = state.borrow().ui();
        let global = ui.global::<SignPsbt>();
        global.set_state(SignPsbtState::Idle);
    });

    global.on_sign_psbt(move || {
        spawn_local(async move {
            match sign_psbt(state).await {
                Ok(_) => {
                    log::info!("successfully signed psbt");
                }
                Err(e) => {
                    log::error!("failed to sign psbt {e:?}");
                    let ui = state.borrow().ui();
                    let global = ui.global::<SignPsbt>();
                    global.set_state(SignPsbtState::Error);
                }
            }
        })
        .detach()
    });

    global.on_get_signed_ur(move |density| {
        let pending = state.borrow().map(|s| &s.pending_psbt);
        let (_account_id, signed, origin) = match &*pending {
            PendingPsbt::Signed { account_id, psbt, origin } => (account_id, psbt, origin),
            _ => {
                log::error!("tried getting signed UR with no signed PSBT");
                return Default::default();
            }
        };

        let ur_type = match origin {
            PsbtOrigin::Qr { ur_type } => ur_type.as_str(),
            _ => "psbt",
        };
        let bytes = minicbor::bytes::ByteVec::from(signed.serialize());
        let ur_bytes = minicbor::to_vec(bytes).unwrap();
        slint_keyos_platform::qrcode::encode_qr_parts(ur_type, ur_bytes, density)
    });

    global.on_save_signed_psbt_to_file(move || {
        let ui = state.borrow().ui();
        let global = ui.global::<SignPsbt>();

        match save_psbt_to_file(state) {
            Ok(path) => {
                global.set_saved_file_path(path.into());
                global.set_file_save_state(FileSaveState::Saved);
            }
            Err(e) => {
                log::error!("failed to save psbt {e:?}");
                global.set_file_save_state(FileSaveState::Error);
            }
        }
    });
    global.on_confirm_create_account(move || {
        let ui = state.borrow().ui();
        let sign_psbt_global = ui.global::<SignPsbt>();
        let create_account_global = ui.global::<CreateAccount>();

        sign_psbt_global.set_show_account_not_found_modal(false);
        create_account_global.set_state(CreateAccountState::Idle);

        if sign_psbt_global.get_is_multisig_account() {
            create_account_global.set_prefilled_mode(false);

            let nav = ui.global::<Navigate>();
            nav.invoke_import_multi_sig(NavigateOptions { replace: true, ..Default::default() });
        } else {
            let pending_singlesig = state
                .borrow()
                .pending_singlesig
                .unwrap_or(PendingSingleSig { index: 0, network: NgNetwork::Testnet4 });

            create_account_global.set_prefilled_mode(true);
            create_account_global.set_prefilled_index(pending_singlesig.index.to_string().into());
            create_account_global.set_prefilled_network(pending_singlesig.network.into());

            let nav = ui.global::<Navigate>();
            nav.invoke_create_account(NavigateOptions { replace: true, ..Default::default() });
        }
    });

    global.on_confirm_restore_account(move || {
        let ui = state.borrow().ui();
        let global = ui.global::<SignPsbt>();

        global.set_show_account_archived_modal(false);

        let account_id = state.borrow_mut().pending_archived_account_id.take();
        let pending_psbt = std::mem::take(&mut state.borrow_mut().pending_psbt);

        if let Some(account_id) = account_id {
            AppState::update_account_config(state, account_id, |config| {
                config.archived = false;
            });

            if let PendingPsbt::NotSaved { psbt, origin } = pending_psbt {
                spawn_local(async move {
                    verify::verify_psbt(state, psbt.serialize(), origin, true).await;
                })
                .detach();
            }
        }
    });
}

fn save_psbt_to_file(state: StoredValue<AppState>) -> anyhow::Result<String> {
    let pending = state.borrow().map(|s| &s.pending_psbt);

    let signed = match &*pending {
        PendingPsbt::Signed { psbt, .. } => psbt,
        _ => {
            bail!("tried saving unsigned psbt")
        }
    };
    let bytes = signed.serialize();
    let fs = crate::FileSystem::default();

    // TODO: use file browser ui for selecting a dir
    // once it is working
    let path = "signed.psbt";
    let mut file = fs
        .open_file(path, fs::Location::Airlock, fs::OpenFlags { read: true, write: true, create: true })
        .context("open file")?;

    file.overwrite(&bytes).context("writing file")?;

    Ok(path.to_string())
}

#[derive(Debug, thiserror::Error)]
pub enum SignPsbtError {
    #[error("publish failed")]
    PublishFailed,
    #[error("no pending psbt")]
    NoPendingPsbt,
    #[error(transparent)]
    Sign(#[from] SignerError),
    #[error(transparent)]
    Unknown(#[from] anyhow::Error),
}

async fn sign_psbt(state: StoredValue<AppState>) -> Result<(), SignPsbtError> {
    let Some((account_id, psbt, _details, origin)) = state.borrow_mut().pending_psbt.take_unsigned() else {
        return Err(SignPsbtError::NoPendingPsbt);
    };

    let ui = state.borrow().ui();
    let global = ui.global::<crate::SignPsbt>();
    global.set_state(crate::SignPsbtState::Signing);

    let load_account = state.borrow().store.load_account(account_id);

    let (account_id, account, signed) = spawn_worker(async move {
        let (id, account) = load_account.await.context("load account")?;
        let mut signed_psbt = psbt;
        let options = SignOptions { trust_witness_utxo: true, ..SignOptions::default() };
        for wallet in account.wallets.read().unwrap().iter() {
            let bdk_wallet = wallet.bdk_wallet.lock().unwrap();
            bdk_wallet.sign(&mut signed_psbt, options.clone())?;
        }

        Ok::<_, SignPsbtError>((id, account, signed_psbt))
    })
    .await?;

    global.set_state(crate::SignPsbtState::Success);
    global.set_origin((&origin).into());

    if let PsbtOrigin::QuantumLink = &origin {
        let result = timeout(broadcast_signed_psbt(&account_id, &signed), Duration::from_secs(10)).await;
        if let Err(_) = result {
            return Err(SignPsbtError::PublishFailed);
        }
    }

    {
        let mut state = state.borrow_mut();
        // insert acct (in case we just loaded it)
        state.store.insert_account(account_id.clone(), account);
        state.pending_psbt = PendingPsbt::Signed { account_id, psbt: signed, origin: origin.clone() }
    }

    if let PsbtOrigin::File = &origin {
        match save_psbt_to_file(state) {
            Ok(path) => {
                global.set_saved_file_path(path.into());
            }
            Err(e) => {
                log::error!("Failed to auto-save signed PSBT: {:?}", e);
            }
        }
    }

    Ok(())
}

pub async fn broadcast_signed_psbt(account_id: &AccountId, psbt: &Psbt) {
    let message = PublishPsbt {
        transaction: BroadcastTransaction { account_id: account_id.to_string(), psbt: psbt.serialize() },
    };
    log::info!("broadcasting signed psbt");
    while let Err(e) = async_archive::<QuantumLinkPermissions, _>(message.clone()).await {
        log::error!("failed to broadcast psbt {e:?}, retrying...");
    }
    log::info!("successfully broadcasted signed psbt");
}

pub mod verify {
    use {
        super::*,
        crate::{RouteOption, RouteState},
        ngwallet::{
            bdk_wallet::{
                descriptor::Descriptor as BdkDescriptor, keys::DescriptorPublicKey, miniscript::ForEachKey,
            },
            bip32::NgAccountPath,
            config::MultiSigDetails,
        },
        std::str::FromStr,
    };

    #[derive(Debug, Clone)]
    pub enum InferredAccountDetails {
        MultiSig(MultiSigDetails),
        SingleSig { account_index: u32, network: NgNetwork },
    }

    // Infer account details from a set of descriptors found in a PSBT.
    // Returns None if the descriptors don't match a consistent account pattern.
    fn infer_account_from_descriptors(tx_descriptors: &Vec<String>) -> Option<InferredAccountDetails> {
        if tx_descriptors.is_empty() {
            return None;
        }

        let multisig_results: Vec<_> =
            tx_descriptors.iter().filter_map(|desc| MultiSigDetails::from_descriptor(desc).ok()).collect();

        if multisig_results.len() == tx_descriptors.len() && !multisig_results.is_empty() {
            let first = &multisig_results[0].0;
            if multisig_results.iter().all(|(ms, _)| ms == first) {
                return Some(InferredAccountDetails::MultiSig(first.clone()));
            }
        }

        let key_sources: Vec<_> = tx_descriptors
            .iter()
            .filter_map(|desc_str| {
                let descriptor = BdkDescriptor::<DescriptorPublicKey>::from_str(desc_str).ok()?;
                let mut sources = Vec::new();

                descriptor.for_each_key(|key| {
                    if let DescriptorPublicKey::XPub(xpub) = key {
                        if let Some((fingerprint, path)) = &xpub.origin {
                            sources.push((*fingerprint, path.clone()));
                        }
                    }
                    true
                });

                Some(sources)
            })
            .flatten()
            .collect();

        if key_sources.is_empty() {
            return None;
        }

        let account_infos: Vec<_> =
            key_sources.iter().filter_map(|(_, path)| NgAccountPath::parse(path).ok().flatten()).collect();

        if account_infos.is_empty() {
            return None;
        }

        let first_account = &account_infos[0];
        let all_match = account_infos.iter().all(|info| info.account == first_account.account);

        if !all_match {
            return None;
        }

        let network = match first_account.to_network_kind() {
            Some(NgNetworkKind::Main) => NgNetwork::Bitcoin,
            _ => NgNetwork::Testnet4,
        };

        Some(InferredAccountDetails::SingleSig { account_index: first_account.account, network })
    }

    pub async fn verify_psbt(
        state: StoredValue<AppState>,
        bytes: Vec<u8>,
        origin: PsbtOrigin,
        nav_replace: bool,
    ) {
        let ui = state.borrow().ui();
        let nav = ui.global::<Navigate>();
        let route_state = ui.global::<RouteState>();

        if route_state.get_active() != RouteOption::SignPsbt {
            nav.invoke_sign_psbt(NavigateOptions { replace: nav_replace, ..Default::default() });
        }

        match verify_inner(state, bytes, origin.clone()).await {
            Ok(_) => (),
            Err(VerifyPsbtError::AccountArchived { account_id, verified: psbt }) => {
                let ui = state.borrow().ui();
                let global = ui.global::<SignPsbt>();

                state.borrow_mut().pending_psbt = PendingPsbt::NotSaved { psbt, origin };

                let is_multisig = account_id.is_multi();

                let account_index = if is_multisig {
                    String::new()
                } else {
                    account_id.index().map(|i| i.to_string()).unwrap_or_default()
                };

                global.set_show_account_archived_modal(true);
                global.set_is_multisig_account(is_multisig);
                global.set_account_index(account_index.into());

                state.borrow_mut().pending_archived_account_id = Some(account_id);
            }
            Err(VerifyPsbtError::AccountNotFound { verified: psbt, details }) => {
                let tx_descriptors: Vec<String> = details
                    .descriptors
                    .iter()
                    .map(|d| d.to_string())
                    .map(|d| normalize_descriptor(&d).to_string())
                    .collect();

                if let Some(inferred) = infer_account_from_descriptors(&tx_descriptors) {
                    let ui = state.borrow().ui();
                    let global = ui.global::<SignPsbt>();

                    match inferred {
                        InferredAccountDetails::MultiSig(multisig_details) => {
                            state.borrow_mut().pending_multisig = Some(PendingMultiSig {
                                details: multisig_details.clone(),
                                source: AccountSource::Generic,
                            });
                            state.borrow_mut().pending_psbt = PendingPsbt::NotSaved { psbt, origin };

                            let multisig_view = MultiSigView::from(&multisig_details);
                            let create_account_global = ui.global::<CreateAccount>();
                            create_account_global.set_pending_multisig_account(multisig_view);

                            global.set_show_account_not_found_modal(true);
                            global.set_is_multisig_account(true);
                            global.set_account_index(String::new().into());
                        }
                        InferredAccountDetails::SingleSig { account_index, network } => {
                            state.borrow_mut().pending_singlesig =
                                Some(PendingSingleSig { index: account_index, network });
                            state.borrow_mut().pending_psbt = PendingPsbt::NotSaved { psbt, origin };

                            global.set_show_account_not_found_modal(true);
                            global.set_is_multisig_account(false);
                            global.set_account_index(account_index.to_string().into());
                        }
                    }
                } else {
                    log::error!("Failed to infer account details from PSBT descriptors");
                    let ui = state.borrow().ui();
                    let global = ui.global::<SignPsbt>();
                    global.set_origin((&origin).into());
                    global.set_state(SignPsbtState::Error);
                }
            }
            Err(VerifyPsbtError::Validate(ngwallet::psbt::Error::CantSign(fingerprints))) => {
                let ui = state.borrow().ui();
                let global = ui.global::<SignPsbt>();

                let fingerprint_list = fingerprints
                    .iter()
                    .map(|f| f.to_string().to_uppercase())
                    .collect::<Vec<String>>()
                    .join(", ");
                log::info!("Found fingerprints: {}", fingerprint_list);

                let needed_fingerprint = state.borrow().store.fingerprint.to_string().to_uppercase();

                global.set_found_fingerprints(fingerprint_list.into());
                global.set_needed_fingerprint(needed_fingerprint.into());
                global.set_show_cant_sign_modal(true);
            }
            Err(e) => {
                log::error!("failed to verify psbt {e:?}");
                let ui = state.borrow().ui();
                let global = ui.global::<SignPsbt>();
                global.set_origin((&origin).into());
                global.set_state(SignPsbtState::Error);
            }
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum VerifyPsbtError {
        #[error(transparent)]
        Deserialize(BdkPsbtError),
        #[error(transparent)]
        Validate(#[from] ngwallet::psbt::Error),
        #[error("account not found")]
        AccountNotFound { verified: Psbt, details: TransactionDetails },
        #[error("account archived")]
        AccountArchived { account_id: AccountId, verified: Psbt },
        #[error(transparent)]
        Unknown(#[from] anyhow::Error),
    }

    async fn verify_inner(
        state: StoredValue<AppState>,
        bytes: Vec<u8>,
        origin: PsbtOrigin,
    ) -> Result<(), VerifyPsbtError> {
        let ui = state.borrow().ui();
        let global = ui.global::<crate::SignPsbt>();

        global.set_state(SignPsbtState::Verifying);

        let (psbt, network_kind) = spawn_worker(async move {
            let psbt = Psbt::deserialize(&bytes).map_err(VerifyPsbtError::Deserialize)?;
            let network_kind = ngwallet::psbt::validate_network(&psbt)?;
            Ok::<_, VerifyPsbtError>((psbt, network_kind))
        })
        .await?;

        let network = match network_kind {
            Some(NgNetworkKind::Main) => NgNetwork::Bitcoin,
            Some(NgNetworkKind::Test) => NgNetwork::Testnet4,
            None => NgNetwork::Bitcoin,
        };
        let master_key = state.borrow().store.load_master_key(network)?;
        let xpriv = Xpriv::new_master(network, &master_key.key.0).context("get xpriv from master key")?;
        let secp = state.borrow().store.secp.clone();

        // PSBT descriptors are untrusted but needed to find the account
        // The stored account is the trust anchor for multisig inputs and change
        // Revalidate multisig transactions with its registered configuration
        let (psbt, discovery) = spawn_worker({
            let secp = secp.clone();
            let xpriv = xpriv.clone();
            async move {
                let res = ngwallet::psbt::validate(&secp, &xpriv, &psbt, network, None)?;
                Ok::<_, VerifyPsbtError>((psbt, res))
            }
        })
        .await?;

        let (account_id, acct) = {
            let tx_descriptors: Vec<String> = discovery
                .descriptors
                .iter()
                .map(|d| d.to_string())
                .map(|d| normalize_descriptor(&d).to_string())
                .collect();

            match state
                .borrow()
                .store
                .active_accounts()
                .find(|(_id, config)| can_sign(&discovery, &tx_descriptors, &*config))
                .map(|(id, config)| (id.clone(), config.clone()))
            {
                Some(res) => res,
                None => return Err(VerifyPsbtError::AccountNotFound { verified: psbt, details: discovery }),
            }
        };

        if acct.archived {
            return Err(VerifyPsbtError::AccountArchived { account_id, verified: psbt });
        }

        let (psbt, details) = if let Some(multisig) = acct.multisig.clone() {
            spawn_worker(async move {
                let res = ngwallet::psbt::validate(&secp, &xpriv, &psbt, network, Some(&multisig))?;
                Ok::<_, VerifyPsbtError>((psbt, res))
            })
            .await?
        } else {
            (psbt, discovery)
        };

        let psbt_view = {
            let state = state.borrow();
            let display_amount = state.settings.display_amount.clone();
            let show_fiat_value = state.settings.show_fiat_value;
            let exchange_rate = state.settings.exchange_rate.clone();
            let locale = state.system_settings.get_locale().lang().to_string();

            PsbtView::from_details(&acct, &details, display_amount, show_fiat_value, exchange_rate, &locale)
        };

        global.set_pending_psbt(psbt_view);
        global.set_state(crate::SignPsbtState::Sign);
        state.borrow_mut().pending_psbt = PendingPsbt::Unsigned { account_id, details, psbt, origin };

        Ok(())
    }

    fn normalize_descriptor(desc: &str) -> &str { desc.split('#').next().unwrap_or(desc) }

    fn can_sign(details: &TransactionDetails, tx_descriptors: &Vec<String>, acct: &NgAccountConfig) -> bool {
        if details.descriptors.is_empty() || acct.descriptors.is_empty() {
            return false;
        }

        // TODO: we probably want to check if it can sign all parts, not just one
        acct.descriptors.iter().any(|cfg| {
            let internal = normalize_descriptor(cfg.internal.as_str());
            let external = cfg.external.as_deref().map(normalize_descriptor);
            tx_descriptors.contains(&String::from(internal))
                || external.is_some_and(|ext| tx_descriptors.contains(&String::from(ext)))
        })
    }
}

impl PsbtView {
    fn from_details(
        acct: &NgAccountConfig,
        details: &TransactionDetails,
        display_amount: DisplayAmount,
        show_fiat_value: ShowFiatValue,
        exchange_rate: ExchangeRate,
        locale: &str,
    ) -> Self {
        let mut outputs = details.outputs.iter().collect::<Vec<_>>();

        outputs.sort_by(|a, b| match (&a.kind, &b.kind) {
            (OutputKind::External(_), OutputKind::External(_)) => Ordering::Equal,
            (OutputKind::External(_), _) => Ordering::Less,
            (_, OutputKind::External(_)) => Ordering::Greater,
            (_, _) => Ordering::Equal,
        });

        let outputs = outputs
            .iter()
            .map(|out| {
                let amount_btc: SharedString = format_btc(out.amount, display_amount, locale);
                let amount_currency =
                    format_currency(out.amount, &exchange_rate, show_fiat_value, acct.network, locale);

                let (kind, address, message, transfer_index) = match &out.kind {
                    OutputKind::Change(address) => (
                        PsbtOutputKind::Change,
                        address.to_shared_string(),
                        Default::default(),
                        Default::default(),
                    ),
                    OutputKind::Transfer { address, account } => (
                        PsbtOutputKind::Transfer,
                        address.to_shared_string(),
                        Default::default(),
                        account.to_string(),
                    ),
                    OutputKind::External(address) => (
                        PsbtOutputKind::External,
                        address.to_shared_string(),
                        Default::default(),
                        Default::default(),
                    ),
                    OutputKind::Suspicious(address) => (
                        PsbtOutputKind::Suspicious,
                        address.to_shared_string(),
                        Default::default(),
                        Default::default(),
                    ),
                    OutputKind::OpReturn(_parts) => {
                        // TODO: handle opreturn parts properly
                        (PsbtOutputKind::OpReturn, Default::default(), Default::default(), Default::default())
                    }
                };

                PsbtOutputView {
                    kind,
                    amount_btc,
                    amount_currency,
                    address,
                    message,
                    account_index: transfer_index.into(),
                }
            })
            .collect::<Vec<PsbtOutputView>>();

        let total = details.display_total() + details.fee;

        let fee_btc = format_btc(details.fee, display_amount, locale);
        let total_btc = format_btc(total, display_amount, locale);

        let fee_currency =
            format_currency(details.fee, &exchange_rate, show_fiat_value, acct.network, locale);
        let total_currency = format_currency(total, &exchange_rate, show_fiat_value, acct.network, locale);

        let crypto_icon = match display_amount {
            DisplayAmount::Btc => "bitcoin-b",
            DisplayAmount::Auto | DisplayAmount::Sats => "sats",
        }
        .to_shared_string();

        let fee_percent: i32 = ((details.fee.to_sat() * 100) as f64 / total.to_sat() as f64).round() as i32;

        Self {
            account_name: acct.name.to_shared_string(),
            is_multisig: acct.multisig.is_some(),
            account_index: acct.index.to_shared_string(),
            card_color: if acct.multisig.is_some() {
                AccountColor::from_hex(&acct.color).into()
            } else {
                AccountColor::for_account_index(acct.index).into()
            },
            outputs: ModelRc::new(VecModel::from(outputs)),
            fee_btc,
            fee_currency,
            total_btc,
            total_currency,
            crypto_icon,
            // TODO: there is no icon for currencies
            fiat_icon: SharedString::default(),
            fee_percent,
            fee_warning_threshold: FEE_WARNING_THRESHOLD,
        }
    }
}

// TODO: move formatting functions to slint_keyos_platform later
fn get_locale_separators(locale: &str) -> (&'static str, &'static str) {
    if locale.starts_with("en") {
        (",", ".")
    } else {
        (".", ",")
    }
}

fn format_currency(
    amount: Amount,
    exchange_rate: &ExchangeRate,
    show_fiat_value: ShowFiatValue,
    network: NgNetwork,
    locale: &str,
) -> SharedString {
    if show_fiat_value == ShowFiatValue::Disabled {
        return SharedString::new();
    }

    // Suppress fiat when Envoy's rate currency doesn't match the user's pick;
    // showing a non-USD symbol with a USD-derived value would be wrong.
    if exchange_rate.currency_code != show_fiat_value.code() {
        return SharedString::new();
    }

    match network {
        NgNetwork::Bitcoin => {
            let (thousands_sep, decimal_sep) = get_locale_separators(locale);
            let total_value = amount.to_btc() * exchange_rate.rate as f64;
            let total_cents = (total_value * 100.0).round() as i64;
            let whole_part = total_cents / 100;
            let fractional_part = (total_cents % 100) as i32;

            let whole_str = whole_part.to_string();
            // Symbols containing any letter (e.g. `CHF`, `Kč`, `Bs.`, `zł`, `ден`, `C$`) get a
            // separating space so they don't read as `Kč45,000.00`. Pure glyphs (`€`, `$`, ...) stay glued.
            let symbol = show_fiat_value.symbol();
            let mut result = String::from(symbol);
            if symbol.chars().any(|c| c.is_alphabetic()) {
                result.push(' ');
            }

            for (i, ch) in whole_str.chars().enumerate() {
                result.push(ch);
                let remaining = whole_str.len() - i - 1;
                if remaining > 0 && remaining % 3 == 0 {
                    result.push_str(thousands_sep);
                }
            }

            result.push_str(decimal_sep);
            result.push_str(&format!("{:02}", fractional_part));

            result.to_shared_string()
        }
        _ => SharedString::new(),
    }
}

fn format_sats_with_separators(sats: u64, locale: &str) -> String {
    let (thousands_sep, _) = get_locale_separators(locale);

    let sats_str = sats.to_string();
    let mut result = String::new();

    for (i, ch) in sats_str.chars().enumerate() {
        result.push(ch);
        let remaining = sats_str.len() - i - 1;
        if remaining > 0 && remaining % 3 == 0 {
            result.push_str(thousands_sep);
        }
    }

    result
}

fn format_btc_amount(sats: u64, locale: &str) -> String {
    let (thousands_sep, decimal_sep) = get_locale_separators(locale);

    let btc_sats = Amount::ONE_BTC.to_sat();
    let btc_part = sats / btc_sats;
    let sat_part = sats % btc_sats;

    let mut result = String::new();
    let mut digit_count = 0;

    if btc_part > 0 {
        let btc_str = btc_part.to_string();
        let btc_len = btc_str.len();

        for (i, ch) in btc_str.chars().enumerate() {
            if digit_count >= MAX_DISPLAY_DIGITS {
                break;
            }

            result.push(ch);
            digit_count += 1;

            let remaining_digits = btc_len - i - 1;
            if remaining_digits > 0 && remaining_digits % 3 == 0 && digit_count < MAX_DISPLAY_DIGITS {
                result.push_str(thousands_sep);
            }
        }
    } else {
        result.push('0');
        digit_count = 1;
    }

    if digit_count < MAX_DISPLAY_DIGITS && sat_part > 0 {
        result.push_str(decimal_sep);

        let sat_str = format!("{:0>8}", sat_part);
        for ch in sat_str.chars() {
            if digit_count >= MAX_DISPLAY_DIGITS {
                break;
            }
            result.push(ch);
            digit_count += 1;
        }
    }

    result
}

fn format_btc(amount: Amount, display_amount: DisplayAmount, locale: &str) -> SharedString {
    let sats = amount.to_sat();
    match display_amount {
        DisplayAmount::Auto => {
            if amount > Amount::ONE_BTC {
                format_btc_amount(sats, locale)
            } else {
                format_sats_with_separators(sats, locale)
            }
        }
        DisplayAmount::Btc => format_btc_amount(sats, locale),
        DisplayAmount::Sats => format_sats_with_separators(sats, locale),
    }
    .to_shared_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_sats_with_separators_en() {
        assert_eq!(format_sats_with_separators(1000, "en"), "1,000");
        assert_eq!(format_sats_with_separators(1234567, "en"), "1,234,567");
        assert_eq!(format_sats_with_separators(2100000000000000, "en"), "2,100,000,000,000,000");
        assert_eq!(format_sats_with_separators(100, "en"), "100");
    }

    #[test]
    fn test_format_sats_with_separators_es() {
        assert_eq!(format_sats_with_separators(1000, "es"), "1.000");
        assert_eq!(format_sats_with_separators(1234567, "es"), "1.234.567");
        assert_eq!(format_sats_with_separators(100, "es"), "100");
    }

    #[test]
    fn test_format_btc_amount_en() {
        // 1.5 BTC = 150,000,000 sats
        assert_eq!(format_btc_amount(150000000, "en"), "1.50000000");
        // 0.00012345 BTC = 12345 sats
        assert_eq!(format_btc_amount(12345, "en"), "0.00012345");
        // Large amount: 1,234.56789012 BTC, truncated at 9 digits (1,234.56789)
        assert_eq!(format_btc_amount(123456789012, "en"), "1,234.56789");
        // Zero
        assert_eq!(format_btc_amount(0, "en"), "0");
        // Exactly 1 BTC
        assert_eq!(format_btc_amount(100000000, "en"), "1");
        // 0.1 BTC
        assert_eq!(format_btc_amount(10000000, "en"), "0.10000000");
        // Preserve trailing zeroes for sub-BTC values
        assert_eq!(format_btc_amount(110, "en"), "0.00000110");
    }

    #[test]
    fn test_format_btc_amount_es() {
        // 1.5 BTC with European formatting (comma as decimal separator)
        assert_eq!(format_btc_amount(150000000, "es"), "1,50000000");
        // Large amount with European thousands separator (period for thousands, comma for decimal)
        assert_eq!(format_btc_amount(123456789012, "es"), "1.234,56789");
        // Preserve trailing zeroes for sub-BTC values
        assert_eq!(format_btc_amount(110, "es"), "0,00000110");
    }

    #[test]
    fn test_format_currency_en() {
        let exchange_rate = ExchangeRate { currency_code: "USD".into(), rate: 50000.0 };

        // 1 BTC at $50,000
        let amount = Amount::from_sat(100_000_000);
        assert_eq!(
            format_currency(amount, &exchange_rate, ShowFiatValue::USD, NgNetwork::Bitcoin, "en"),
            "$50,000.00"
        );

        // 0.5 BTC at $50,000 = $25,000
        let amount = Amount::from_sat(50_000_000);
        assert_eq!(
            format_currency(amount, &exchange_rate, ShowFiatValue::USD, NgNetwork::Bitcoin, "en"),
            "$25,000.00"
        );

        // Large amount: 100 BTC = $5,000,000
        let amount = Amount::from_sat(10_000_000_000);
        assert_eq!(
            format_currency(amount, &exchange_rate, ShowFiatValue::USD, NgNetwork::Bitcoin, "en"),
            "$5,000,000.00"
        );

        // Small amount with cents
        let amount = Amount::from_sat(12_345);
        assert_eq!(
            format_currency(amount, &exchange_rate, ShowFiatValue::USD, NgNetwork::Bitcoin, "en"),
            "$6.17"
        );

        // Testnet should return empty string
        let amount = Amount::from_sat(100_000_000);
        assert_eq!(
            format_currency(amount, &exchange_rate, ShowFiatValue::USD, NgNetwork::Testnet4, "en"),
            ""
        );

        // Disabled fiat display should return empty string even for mainnet.
        assert_eq!(
            format_currency(amount, &exchange_rate, ShowFiatValue::Disabled, NgNetwork::Bitcoin, "en"),
            ""
        );
    }

    #[test]
    fn test_format_currency_es() {
        let exchange_rate = ExchangeRate { currency_code: "USD".into(), rate: 50000.0 };

        // 1 BTC at $50,000 with European formatting
        let amount = Amount::from_sat(100_000_000);
        assert_eq!(
            format_currency(amount, &exchange_rate, ShowFiatValue::USD, NgNetwork::Bitcoin, "es"),
            "$50.000,00"
        );

        // Large amount: 100 BTC = $5,000,000 with European formatting
        let amount = Amount::from_sat(10_000_000_000);
        assert_eq!(
            format_currency(amount, &exchange_rate, ShowFiatValue::USD, NgNetwork::Bitcoin, "es"),
            "$5.000.000,00"
        );

        // Small amount with cents
        let amount = Amount::from_sat(12_345);
        assert_eq!(
            format_currency(amount, &exchange_rate, ShowFiatValue::USD, NgNetwork::Bitcoin, "es"),
            "$6,17"
        );
    }

    #[test]
    fn test_format_currency_uses_selected_symbol() {
        let amount = Amount::from_sat(100_000_000);

        // Matching code → uses the selected currency's symbol.
        let eur_rate = ExchangeRate { currency_code: "EUR".into(), rate: 45000.0 };
        assert_eq!(
            format_currency(amount, &eur_rate, ShowFiatValue::EUR, NgNetwork::Bitcoin, "en"),
            "€45,000.00"
        );

        // Pure-letter symbol → space inserted so it doesn't read glued.
        let chf_rate = ExchangeRate { currency_code: "CHF".into(), rate: 40000.0 };
        assert_eq!(
            format_currency(amount, &chf_rate, ShowFiatValue::CHF, NgNetwork::Bitcoin, "en"),
            "CHF 40,000.00"
        );

        // Non-ASCII letter symbol → also gets a space.
        let czk_rate = ExchangeRate { currency_code: "CZK".into(), rate: 1_000_000.0 };
        assert_eq!(
            format_currency(amount, &czk_rate, ShowFiatValue::CZK, NgNetwork::Bitcoin, "en"),
            "Kč 1,000,000.00"
        );

        // Mixed letter+glyph symbol (CAD's `C$`) — has a letter, so gets the same space treatment.
        let cad_rate = ExchangeRate { currency_code: "CAD".into(), rate: 60000.0 };
        assert_eq!(
            format_currency(amount, &cad_rate, ShowFiatValue::CAD, NgNetwork::Bitcoin, "en"),
            "C$ 60,000.00"
        );

        // Pure-glyph symbol stays glued.
        let jpy_rate = ExchangeRate { currency_code: "JPY".into(), rate: 7_500_000.0 };
        assert_eq!(
            format_currency(amount, &jpy_rate, ShowFiatValue::JPY, NgNetwork::Bitcoin, "en"),
            "¥7,500,000.00"
        );

        // PAB's symbol ends in a period (no embedded whitespace anymore); single space inserted.
        let pab_rate = ExchangeRate { currency_code: "PAB".into(), rate: 50000.0 };
        assert_eq!(
            format_currency(amount, &pab_rate, ShowFiatValue::PAB, NgNetwork::Bitcoin, "en"),
            "B/. 50,000.00"
        );

        // Mismatched code → suppress fiat rather than show wrong-units value.
        let usd_rate = ExchangeRate { currency_code: "USD".into(), rate: 50000.0 };
        assert_eq!(format_currency(amount, &usd_rate, ShowFiatValue::EUR, NgNetwork::Bitcoin, "en"), "");
    }
}
