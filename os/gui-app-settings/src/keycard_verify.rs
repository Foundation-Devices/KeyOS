// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use haptics::HapticPattern;
use keycard::error::KeycardError;
use keycard::messages::KeycardId;
use keycard_scan::backup::BackupKind;
use keycard_scan::restore::{get_remote_shard, scan_keycard, GetRemoteShardError, ScanEvent};
use slint_keyos_platform::slint::SharedString;
use slint_keyos_platform::{
    async_archive, async_scalar,
    slint::{ComponentHandle, ModelRc, VecModel},
    spawn_local, TaskHandle,
};
use whence::WhenceExt as _;

use crate::{
    haptics_permissions::HapticsPermissions, keycard_permissions::KeycardPermissions,
    quantum_link_permissions::QuantumLinkPermissions, security_permissions::SecurityPermissions,
    state::AppState, TrId, VerifyKeycardBackupGlobal,
};
use crate::{tr, StepModel};

pub struct KeycardVerifyFlow {
    _task: TaskHandle<()>,
}

impl KeycardVerifyFlow {
    pub fn start(state: crate::StoredValue<AppState>) {
        let task = spawn_local(async move {
            let ui = state.borrow().ui();
            let global = ui.global::<VerifyKeycardBackupGlobal>();
            if let Err(e) = verify_backup(&global).await {
                log::error!("Backup verification failed: {e:?}");
                global.set_error(e.slint_error());
            }
        });
        state.borrow_mut().backup_verification = Some(Self { _task: task });
    }
}

#[derive(Debug, thiserror::Error)]
enum VerifyKeycardError {
    #[error(transparent)]
    Keycard(#[from] keycard::error::KeycardError),
    #[error("failed to send quantum link message")]
    SendMessage(#[from] quantum_link::SendMessageError),
    #[error("failed to retrieve seed fingerprint")]
    Security(#[from] security::AccessDenied),
    #[error("envoy reported error: {0}")]
    Envoy(String),
    #[error("no remote shard found")]
    NoRemoteShardFound,
    #[error("remote shard is invalid")]
    InvalidShard,
    #[error("shard not from active seed")]
    ShardFromDifferentSeed,
}

impl From<GetRemoteShardError> for VerifyKeycardError {
    fn from(value: GetRemoteShardError) -> Self {
        match value {
            GetRemoteShardError::SendMessage(e) => Self::SendMessage(e),
            GetRemoteShardError::Envoy(e) => Self::Envoy(e),
            GetRemoteShardError::InvalidShard => Self::InvalidShard,
        }
    }
}

impl VerifyKeycardError {
    fn slint_error(&self) -> SharedString {
        match self {
            VerifyKeycardError::Keycard(KeycardError::DifferentSeedFingerprint)
            | VerifyKeycardError::ShardFromDifferentSeed => {
                tr::lookup_id(TrId::MagicVerifyBackupErrorKeycardDifferentBackup).into()
            }
            VerifyKeycardError::Keycard(KeycardError::BlankShard)
            | VerifyKeycardError::Keycard(KeycardError::BlankTag) => {
                tr::lookup_id(TrId::MagicVerifyBackupErrorKeycardEmpty).into()
            }
            VerifyKeycardError::Keycard(KeycardError::Nfc(_)) => {
                tr::lookup_id(TrId::MagicVerifyBackupErrorReadingKeycardFailed).into()
            }
            VerifyKeycardError::SendMessage(_) => {
                tr::lookup_id(TrId::MagicVerifyBackupErrorNoConnectionToEnvoy).into()
            }
            VerifyKeycardError::NoRemoteShardFound | VerifyKeycardError::InvalidShard => {
                tr::lookup_id(TrId::MagicVerifyBackupErrorNoEnvoyBackupFound).into()
            }
            e => slint_keyos_platform::slint::format!("{e}"),
        }
    }
}

#[derive(Default)]
struct VerifyState {
    shard_count: usize,
    reading_keycard: bool,
    kind: Option<BackupKind>,
    scheme: Option<keycard::messages::ShamirScheme>,
}

async fn verify_backup(global: &VerifyKeycardBackupGlobal<'_>) -> whence::Result<(), VerifyKeycardError> {
    log::info!("starting backup verification flow");

    let haptics = crate::HapticsApi::default();
    let mut state = VerifyState::default();
    let mut cards_loaded = vec![];

    update_slint_state(global, &state);
    async_scalar::<KeycardPermissions, _>(keycard::messages::ResetShards).await.whence()?;

    let seed_fingerprint =
        async_archive::<SecurityPermissions, _>(security::messages::GetSeedFingerprint).await.whence()?;
    let first_card = scan_card(global, &haptics, &mut state, &mut cards_loaded).await.whence()?;
    ensure_same_backup(&seed_fingerprint, &first_card.seed_fingerprint).whence()?;

    state.kind = Some(if first_card.has_magic_backup { BackupKind::Magic } else { BackupKind::Manual });
    state.scheme = Some(first_card.scheme);
    update_slint_state(global, &state);

    if first_card.has_magic_backup {
        let shard =
            get_remote_shard::<QuantumLinkPermissions>(first_card.seed_fingerprint, first_card.timestamp, {
                let mut retries_remaining: u8 = 2;
                move |e| match e {
                    quantum_link::SendMessageError::NoDevicePaired
                    | quantum_link::SendMessageError::Cancelled => false,
                    quantum_link::SendMessageError::Bluetooth(_)
                    | quantum_link::SendMessageError::Timeout => {
                        let should_retry = retries_remaining > 0;
                        retries_remaining = retries_remaining.saturating_sub(1);
                        should_retry
                    }
                }
            })
            .await
            .whence()?
            .ok_or(VerifyKeycardError::NoRemoteShardFound)
            .whence()?;

        log::info!("found remote shard");

        ensure_same_backup(&seed_fingerprint, shard.seed_fingerprint()).whence()?;
        async_archive::<KeycardPermissions, _>(keycard::messages::PushShard {
            shard,
            accept_different_device_id: true,
        })
        .await
        .whence()?;

        state.shard_count += 1;
        update_slint_state(global, &state);
    }

    let scheme = first_card.scheme;
    // For magic backup: one shard is from Envoy, rest from keycards
    let cards_needed = if first_card.has_magic_backup { scheme.share_count - 1 } else { scheme.share_count };
    while cards_loaded.len() < cards_needed {
        let shard = scan_card(global, &haptics, &mut state, &mut cards_loaded).await.whence()?;
        ensure_same_backup(&seed_fingerprint, &shard.seed_fingerprint).whence()?;
    }

    haptics.vibrate(HapticPattern::PulsingStrongOne100);

    async_scalar::<KeycardPermissions, _>(keycard::messages::CheckBackup).await.whence()?;

    Ok(())
}

async fn scan_card(
    global: &VerifyKeycardBackupGlobal<'_>,
    haptics: &crate::HapticsApi,
    state: &mut VerifyState,
    cards_loaded: &mut Vec<KeycardId>,
) -> Result<keycard::messages::LoadedShard, VerifyKeycardError> {
    let handler = |event: ScanEvent| {
        match event {
            ScanEvent::WaitingForKeycard => {
                state.reading_keycard = false;
            }
            ScanEvent::ReadingFromKeycard => {
                state.reading_keycard = true;
            }
            ScanEvent::ScanComplete { .. } => {
                state.reading_keycard = false;
                state.shard_count += 1;
            }
        };
        update_slint_state(global, state);
    };

    Ok(scan_keycard::<KeycardPermissions, HapticsPermissions>(haptics, cards_loaded, handler).await?)
}

fn ensure_same_backup(
    seed_fingerprint_a: &[u8],
    seed_fingerprint_b: &[u8],
) -> Result<(), VerifyKeycardError> {
    if seed_fingerprint_a != seed_fingerprint_b {
        Err(VerifyKeycardError::ShardFromDifferentSeed)
    } else {
        Ok(())
    }
}

fn update_slint_state(global: &VerifyKeycardBackupGlobal<'_>, state: &VerifyState) {
    let steps = to_step_model(state.kind, state.shard_count, state.reading_keycard);
    global.set_steps(ModelRc::new(VecModel::from(steps)));
    global.set_reading_from_keycard(state.reading_keycard);
    global.set_error(Default::default());
}

fn to_step_model(kind: Option<BackupKind>, shard_count: usize, reading_keycard: bool) -> Vec<StepModel> {
    let Some(kind) = kind else {
        return vec![StepModel {
            label: if reading_keycard {
                tr::lookup_id(TrId::CommonRecoverRestoreCardKeycardReadingKeycard).into()
            } else {
                tr::lookup_id(TrId::RecoverRestoreCardKeycardTapAKeycard).into()
            },
            icon: "arrow-right".into(),
            completed: false,
            in_progress: true,
            error: false,
        }];
    };

    let loading_text = |idx: usize| -> slint_keyos_platform::slint::SharedString {
        if reading_keycard && shard_count == idx {
            tr::lookup_id(TrId::CommonRecoverRestoreCardKeycardReadingKeycard).into()
        } else if idx == 1 {
            tr::lookup_id(TrId::RecoverRestoreCardKeycardTapAKeycard).into()
        } else {
            tr::lookup_id(TrId::RecoverRestoreCardKeycardTapAnotherKeycard).into()
        }
    };

    let first_card = StepModel {
        label: if shard_count >= 1 {
            tr::lookup_id(TrId::MagicVerifyBackupFirstPartVerifiedKeycard).into()
        } else {
            loading_text(0)
        },
        icon: "arrow-right".into(),
        completed: shard_count >= 1,
        in_progress: shard_count == 0,
        error: false,
    };

    match kind {
        BackupKind::Magic => {
            vec![
                first_card,
                StepModel {
                    label: if shard_count >= 2 {
                        tr::lookup_id(TrId::MagicVerifyBackupSecondPartVerifiedEnvoy).into()
                    } else if shard_count == 1 {
                        tr::lookup_id(TrId::MagicVerifyBackupSecondPartVerifiyngEnvoy).into()
                    } else {
                        loading_text(1)
                    },
                    icon: "arrow-right".into(),
                    completed: shard_count >= 2,
                    in_progress: shard_count == 1,
                    error: false,
                },
                StepModel {
                    label: if shard_count >= 3 {
                        tr::lookup_id(TrId::MagicVerifyBackupThirdPartVerifiedKeycard).into()
                    } else {
                        loading_text(2)
                    },
                    icon: "arrow-right".into(),
                    completed: shard_count >= 3,
                    in_progress: shard_count == 2,
                    error: false,
                },
            ]
        }
        BackupKind::Manual => {
            vec![
                first_card,
                StepModel {
                    label: if shard_count >= 2 {
                        tr::lookup_id(TrId::MagicVerifyBackupSecondPartVerifiedKeycard).into()
                    } else {
                        loading_text(1)
                    },
                    icon: "arrow-right".into(),
                    completed: shard_count >= 2,
                    in_progress: shard_count == 1,
                    error: false,
                },
                StepModel {
                    label: if shard_count >= 3 {
                        tr::lookup_id(TrId::MagicVerifyBackupThirdPartVerifiedKeycard).into()
                    } else {
                        loading_text(2)
                    },
                    icon: "arrow-right".into(),
                    completed: shard_count >= 3,
                    in_progress: shard_count == 2,
                    error: false,
                },
            ]
        }
    }
}
