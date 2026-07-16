// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use anyhow::Context;
use haptics::HapticPattern;
use keycard::messages::MasterSeedRestored;
use keycard_scan::restore::{get_remote_shard, scan_keycard, ScanEvent};
use security::messages::SetSeedAndPin;
use slint_keyos_platform::{
    async_archive, async_scalar,
    slint::{ComponentHandle, ModelRc, VecModel},
    spawn_local, StoredValue, TaskHandle,
};

use crate::{
    haptics_permissions::HapticsPermissions,
    keycard_permissions::KeycardPermissions,
    quantum_link_permissions::QuantumLinkPermissions,
    security_permissions::SecurityPermissions,
    state::{
        setup_seed::{compare_with_current_seed, wrap_set_seed},
        AppState, PendingPin,
    },
    tr, KeycardRestoreGlobal, KeycardRestoreKind, MasterSeedState, SeedGlobal, StepModel, TrId,
};

pub struct KeycardRestoreFlow {
    _task: TaskHandle<()>,
}

impl KeycardRestoreFlow {
    pub fn start(state: StoredValue<AppState>) {
        let task = spawn_local(async move {
            if let Err(e) = restore(state).await {
                log::error!("Restore flow failed: {e:?}");
                let ui = state.borrow().ui();
                let global = ui.global::<SeedGlobal>();
                global.set_master_seed_state(MasterSeedState::Failed);
            }
        });

        state.borrow_mut().keycard_restore = Some(Self { _task: task });
    }
}

async fn restore(state: StoredValue<AppState>) -> anyhow::Result<()> {
    let ui = state.borrow().ui();
    let seed_global = ui.global::<SeedGlobal>();
    seed_global.set_master_seed_state(MasterSeedState::Idle);

    let (seed, kind) = get_seed_from_cards(state).await?;

    // we need to detach from the cancellation context
    spawn_local(restore_seed(state, seed, kind)).detach();

    Ok(())
}

async fn restore_seed(
    state: StoredValue<AppState>,
    MasterSeedRestored { seed, different_device_id }: MasterSeedRestored,
    kind: KeycardRestoreKind,
) {
    let ui = state.borrow().ui();
    let nav = ui.global::<crate::Navigate>();
    let seed_global = ui.global::<SeedGlobal>();
    let global = ui.global::<KeycardRestoreGlobal>();

    global.set_restore_kind(kind);
    global.set_different_device_id(different_device_id);
    seed_global.set_master_seed_state(MasterSeedState::Idle);

    // In case we're recovering a previously erased master key,
    // compare the fingerprints to ensure they're matching
    if seed_global.get_is_master_key_recovery()
        && !compare_with_current_seed(&seed)
            .await
            .inspect_err(|e| log::error!("failed to compare seed {e:?}"))
            .unwrap_or(false)
    {
        seed_global.set_fingerprint_mismatch(true);
        nav.invoke_master_key_deleted_restore(crate::NavigateOptions { replace: true, ..Default::default() });
        return;
    }

    wrap_set_seed(state, false, async move {
        let PendingPin { pin, pin_entry } = state.borrow().get_pending_pin()?;

        async_archive::<SecurityPermissions, _>(SetSeedAndPin { seed, pin, pin_entry })
            .await
            .context("failed to set pin and seed")?;

        Ok(())
    })
    .await;
}

async fn scan_card(
    global: &KeycardRestoreGlobal<'_>,
    haptics: &crate::HapticsApi,
    restore_state: &mut RestoreState,
    cards_loaded: &mut Vec<keycard::messages::KeycardId>,
) -> anyhow::Result<keycard::messages::LoadedShard> {
    let handler = |event: ScanEvent| {
        match event {
            ScanEvent::WaitingForKeycard => {
                restore_state.reading_keycard = false;
            }
            ScanEvent::ReadingFromKeycard => {
                restore_state.reading_keycard = true;
            }
            ScanEvent::ScanComplete { cards_loaded } => {
                restore_state.cards_scanned = cards_loaded;
                restore_state.reading_keycard = false;
            }
        }
        update_slint_state(global, restore_state);
    };

    Ok(scan_keycard::<KeycardPermissions, HapticsPermissions>(haptics, cards_loaded, handler).await?)
}

async fn get_seed_from_cards(
    state: StoredValue<AppState>,
) -> anyhow::Result<(MasterSeedRestored, KeycardRestoreKind)> {
    log::info!("starting keycard restore flow");

    let ui = state.borrow().ui();
    let global = ui.global::<KeycardRestoreGlobal>();
    let haptics = crate::HapticsApi::default();

    let mut cards_loaded = Vec::new();
    let mut restore_state = RestoreState::default();

    update_slint_state(&global, &restore_state);

    async_scalar::<KeycardPermissions, _>(keycard::messages::ResetShards).await?;

    let first_card = scan_card(&global, &haptics, &mut restore_state, &mut cards_loaded).await?;
    let scheme = first_card.scheme;
    restore_state.scheme = Some(scheme);

    if first_card.has_magic_backup {
        restore_state.remote_shard_state = RemoteShardState::Loading;
        restore_state.kind = Some(KeycardRestoreKind::Magic);
        update_slint_state(&global, &restore_state);

        match restore_remote_shard(first_card.seed_fingerprint, first_card.timestamp).await {
            Ok(Some(seed)) => {
                restore_state.remote_shard_state = RemoteShardState::Restored;
                restore_state.cards_scanned = 2;
                update_slint_state(&global, &restore_state);
                return Ok((seed, KeycardRestoreKind::Magic));
            }
            Ok(None) => {
                restore_state.remote_shard_state = RemoteShardState::NotFound;
                restore_state.kind = Some(KeycardRestoreKind::TwoCards);
                update_slint_state(&global, &restore_state);
                log::info!("no remote shard found");
            }
            Err(e) => {
                restore_state.remote_shard_state = RemoteShardState::NotFound;
                restore_state.kind = Some(KeycardRestoreKind::TwoCards);
                update_slint_state(&global, &restore_state);
                log::warn!("failed to restore remote shard {e:?}")
            }
        }
    } else {
        restore_state.kind = Some(KeycardRestoreKind::Manual);
        update_slint_state(&global, &restore_state);
    }

    // Scan additional cards until we have enough shards to restore (threshold)
    // We already have 1 shard from the first card
    while cards_loaded.len() < scheme.threshold {
        let _ = scan_card(&global, &haptics, &mut restore_state, &mut cards_loaded).await?;
    }

    let seed = async_archive::<KeycardPermissions, _>(keycard::messages::RestoreMasterSeed).await?;
    haptics.vibrate(HapticPattern::PulsingStrongOne100);
    let kind = restore_state.kind.unwrap_or(KeycardRestoreKind::Manual);

    Ok((seed, kind))
}

pub async fn restore_remote_shard(
    seed_fingerprint: [u8; 32],
    timestamp: u32,
) -> anyhow::Result<Option<keycard::messages::MasterSeedRestored>> {
    let Some(shard) = get_remote_shard::<QuantumLinkPermissions>(seed_fingerprint, timestamp, |e| match e {
        quantum_link::SendMessageError::NoDevicePaired => false,
        _ => true,
    })
    .await?
    else {
        return Ok(None);
    };
    async_archive::<KeycardPermissions, _>(keycard::messages::PushShard {
        shard,
        accept_different_device_id: true,
    })
    .await?;
    let seed = async_archive::<KeycardPermissions, _>(keycard::messages::RestoreMasterSeed).await?;
    Ok(Some(seed))
}

#[derive(Default)]
struct RestoreState {
    cards_scanned: usize,
    reading_keycard: bool,
    remote_shard_state: RemoteShardState,
    kind: Option<KeycardRestoreKind>,
    scheme: Option<keycard::messages::ShamirScheme>,
}

#[derive(Default)]
enum RemoteShardState {
    #[default]
    Idle,
    Loading,
    Restored,
    NotFound,
}

fn update_slint_state(global: &KeycardRestoreGlobal<'_>, state: &RestoreState) {
    let steps = to_step_model(state);
    global.set_steps(ModelRc::new(VecModel::from(steps)));
    global.set_reading_from_keycard(state.reading_keycard);
}

fn to_step_model(state: &RestoreState) -> Vec<StepModel> {
    use slint_keyos_platform::slint::SharedString;

    let Some(kind) = state.kind else {
        let label = if state.reading_keycard {
            tr::lookup_id(TrId::CommonRecoverRestoreCardKeycardReadingKeycard).into()
        } else {
            tr::lookup_id(TrId::RecoverRestoreCardKeycardTapAKeycard).into()
        };
        return vec![StepModel {
            label,
            icon: "arrow-right".into(),
            completed: false,
            in_progress: true,
            error: false,
        }];
    };

    let second_card_text = || -> SharedString {
        if state.reading_keycard && state.cards_scanned == 1 {
            tr::lookup_id(TrId::CommonRecoverRestoreCardKeycardReadingKeycard).into()
        } else {
            tr::lookup_id(TrId::RecoverRestoreCardKeycardTapAnotherKeycard).into()
        }
    };

    let first_card = StepModel {
        label: tr::lookup_id(TrId::RecoverRestoreCardEnvoyFirstPartRestoredKeycard).into(),
        icon: "arrow-right".into(),
        completed: true,
        in_progress: false,
        error: false,
    };

    match kind {
        KeycardRestoreKind::Magic => vec![
            first_card,
            StepModel {
                label: if matches!(state.remote_shard_state, RemoteShardState::Restored) {
                    tr::lookup_id(TrId::RecoverSuccessEnvoySecondRestoredEnvoy).into()
                } else {
                    tr::lookup_id(TrId::RecoverRestoreCardEnvoyReadingFromEnvoy).into()
                },
                icon: "arrow-right".into(),
                completed: matches!(state.remote_shard_state, RemoteShardState::Restored),
                in_progress: matches!(state.remote_shard_state, RemoteShardState::Loading),
                error: false,
            },
        ],
        KeycardRestoreKind::TwoCards => vec![
            first_card,
            StepModel {
                label: tr::lookup_id(TrId::RecoverRestoreCardKeycardNoEnvoyBackup).into(),
                icon: "close".into(),
                completed: false,
                in_progress: false,
                error: false,
            },
            StepModel {
                label: if state.cards_scanned >= 2 {
                    tr::lookup_id(TrId::RecoverRestoreCardEnvoySecondPartRestoredKeycard).into()
                } else {
                    second_card_text()
                },
                icon: "arrow-right".into(),
                completed: state.cards_scanned >= 2,
                in_progress: state.cards_scanned == 1,
                error: false,
            },
        ],
        KeycardRestoreKind::Manual => vec![
            first_card,
            StepModel {
                label: if state.cards_scanned >= 2 {
                    tr::lookup_id(TrId::RecoverRestoreCardEnvoySecondPartRestoredKeycard).into()
                } else {
                    second_card_text()
                },
                icon: "arrow-right".into(),
                completed: state.cards_scanned >= 2,
                in_progress: state.cards_scanned == 1,
                error: false,
            },
        ],
    }
}
