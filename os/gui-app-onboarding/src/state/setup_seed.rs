// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::future::Future;

use anyhow::Context;
use fs::messages::FormatEncryptedVolume;
use quantum_link::foundation_api::onboarding::OnboardingState;
use security::{
    messages::{ComputeSeedFingerprint, GetRandom, GetSeedFingerprint, SetSeedAndPin},
    Seed,
};
use sha2::{Digest, Sha256};
use slint_keyos_platform::{
    async_archive, async_scalar,
    gui_server_api::navigation::qrscanner::{ScanQrOptions, ScanQrResult},
    navigation::open_qr_scanner,
    slint::{ComponentHandle, Model, ModelRc, SharedString},
    StoredValue,
};

use crate::{
    fs_permissions::FileSystemPermissions,
    gui_permissions::GuiPermissions,
    notify_onboarding_state,
    security_permissions::SecurityPermissions,
    state::{AppState, PendingPin},
    tr, Animate, MasterSeedState, Navigate, NavigateOptions, QlStatus, SeedGlobal, TrId,
};

/// Domain separator for folding secure element randomness into a new master
/// seed; see [`create_new_master_seed`].
const SEED_DOMAIN: &[u8] = b"keyos-onboarding-master-seed-v1";

pub async fn create_new_master_seed(state: StoredValue<AppState>) {
    wrap_set_seed(state, true, async move {
        log::info!("Starting master seed creation process");

        let PendingPin { pin, pin_entry } = state.borrow().get_pending_pin()?;

        let mut seed_bytes = [0u8; 16];
        getrandom::getrandom(&mut seed_bytes).context("Failed to generate random seed")?;

        // Fold in the secure element's own RNG (independent of the trng
        // service getrandom draws from) via a hash rather than XOR, so
        // unpredictability doesn't rest on an assumption that the two
        // sources can't observe or predict each other.
        let se_random = async_archive::<SecurityPermissions, _>(GetRandom).await.context("GetRandom")?;
        let mut hasher = Sha256::new();
        hasher.update(SEED_DOMAIN);
        hasher.update(seed_bytes);
        hasher.update(se_random);
        seed_bytes.copy_from_slice(&hasher.finalize()[..16]);

        let seed = Seed::Twelve(seed_bytes);

        async_archive::<SecurityPermissions, _>(SetSeedAndPin { seed, pin, pin_entry })
            .await
            .context("SetSeedAndPin")?;

        log::info!("Master seed created successfully");

        Ok(())
    })
    .await
}

pub async fn restore_from_seed_words(state: StoredValue<AppState>, words: ModelRc<SharedString>) {
    let ui = state.borrow().ui();
    let nav = ui.global::<Navigate>();
    let seed_global = ui.global::<SeedGlobal>();

    let mnemonic_str = words.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(" ");
    let Ok(mnemonic) = bip39::Mnemonic::parse_normalized(&mnemonic_str).context("invalid mnemonic") else {
        seed_global.set_master_seed_state(MasterSeedState::Failed);
        return;
    };
    let seed = Seed::from_mnemonic(&mnemonic);

    // In case we're recovering a previously erased master key,
    // compare the fingerprints to ensure they're matching
    if seed_global.get_is_master_key_recovery() && !compare_with_current_seed(&seed).await.unwrap_or_default()
    {
        seed_global.set_fingerprint_mismatch(true);
        nav.invoke_master_key_deleted_restore(NavigateOptions { replace: true, ..Default::default() });
        return;
    }

    wrap_set_seed(state, false, async move {
        log::info!("Starting seed words restore process");

        let PendingPin { pin, pin_entry } = state.borrow().get_pending_pin()?;

        async_archive::<SecurityPermissions, _>(SetSeedAndPin { seed, pin, pin_entry })
            .await
            .context("SetSeedAndPin")?;

        log::info!("Seed words restore successful");

        Ok(())
    })
    .await
}

pub async fn restore_from_seed_qr(state: StoredValue<AppState>) -> anyhow::Result<()> {
    let PendingPin { pin, pin_entry } = state.borrow().get_pending_pin()?;
    let ui = state.borrow().ui();
    let nav = ui.global::<Navigate>();
    let seed_global = ui.global::<SeedGlobal>();

    seed_global.set_seed_qr_error(false);

    log::info!("Found pending PIN, opening QR scanner");

    let scan_result = open_qr_scanner::<GuiPermissions>(ScanQrOptions {
        header_title: tr::lookup_id(TrId::SeedQrTitle).to_string(),
        ..ScanQrOptions::default()
    })
    .context("qr scan failed")?;

    let qr_data = match scan_result {
        Some(ScanQrResult::Qr { data, .. }) => data,
        Some(ScanQrResult::LeftClicked) => {
            log::info!("QR scan cancelled by user");
            return Ok(());
        }
        _ => {
            anyhow::bail!("Unexpected QR scan result type");
        }
    };

    nav.invoke_restore_seed_qr(NavigateOptions { animate: Animate::None, replace: false });

    let words = match security::parse_seedqr(&qr_data).context("Failed to parse SeedQR") {
        Ok(words) => words,
        Err(e) => {
            log::error!("failed to parse SeedQR: {e:?}");
            seed_global.set_seed_qr_error(true);
            return Ok(());
        }
    };

    let seed = Seed::from_mnemonic(&words);

    // In case we're recovering a previously erased master key,
    // compare the fingerprints to ensure they're matching
    if seed_global.get_is_master_key_recovery() && !compare_with_current_seed(&seed).await? {
        seed_global.set_fingerprint_mismatch(true);
        nav.invoke_master_key_deleted_restore(NavigateOptions { replace: true, ..Default::default() });
        return Ok(());
    }

    wrap_set_seed(state, false, async move {
        async_archive::<SecurityPermissions, _>(SetSeedAndPin { seed, pin, pin_entry })
            .await
            .context("SetSeedAndPin")?;

        log::info!("SeedQR restore successful");

        Ok(())
    })
    .await;

    Ok(())
}

pub async fn wrap_set_seed(
    state: StoredValue<AppState>,
    new_seed: bool,
    f: impl Future<Output = anyhow::Result<()>>,
) {
    let ui = state.borrow().ui();
    let global = ui.global::<SeedGlobal>();

    global.set_master_seed_state(MasterSeedState::SavingToSE);
    match f.await {
        Ok(_) => {
            state.borrow_mut().clear_pending_set_pin();

            // If we're recovering a master key, we're done
            if global.get_is_master_key_recovery() {
                global.set_master_seed_state(MasterSeedState::Success);
                return;
            }

            notify_onboarding_state(state, OnboardingState::WalletCreated);

            global.set_master_seed_state(MasterSeedState::EncryptingFs);

            reset_fs_encrypted_if_needed().await;

            global.set_master_seed_state(MasterSeedState::Success);

            if new_seed {
                log::info!("new seed. will not restore magic backup");
            } else {
                log::info!("seed restore. restoring magic backup");
                let bt_state = state.borrow().ql_status.clone();
                restore_magic_backup(bt_state).await;
            }
            log::info!("setup_seed complete");
        }
        Err(e) => {
            log::error!("failed to setup seed: {e:?}");
            global.set_master_seed_state(MasterSeedState::Failed);
        }
    }
}

pub async fn restore_magic_backup(ql: QlStatus) {
    ql.send_ql_archive_retry(quantum_link::messages::StartRestoreMagicBackup, |e| {
        log::error!("failed to restore magic backup {e:?}, retrying...")
    })
    .await;

    log::info!("started magic backup restore");
}

async fn reset_fs_encrypted_if_needed() {
    let fs = crate::FileSystem::default();
    if let Err(fs::Error::NoMedia) = fs.metadata("/", fs::Location::AppData) {
        log::info!("seed has changed. auto-formatting encrypted volume");
        async_scalar::<FileSystemPermissions, _>(FormatEncryptedVolume).await;
        log::info!("format completed");
    } else {
        log::info!("no format needed");
    }
}

pub(crate) async fn compare_with_current_seed(seed: &Seed) -> anyhow::Result<bool> {
    let old_fingerprint = async_archive::<SecurityPermissions, _>(GetSeedFingerprint)
        .await
        .context("failed to get old seed fingerprint")?;

    let new_seed_fingerprint = async_archive::<SecurityPermissions, _>(ComputeSeedFingerprint(seed.clone()))
        .await
        .context("failed to get new seed fingerprint")?;

    Ok(old_fingerprint == new_seed_fingerprint)
}
