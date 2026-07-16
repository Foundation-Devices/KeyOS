// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use haptics::{messages::Vibrate, HapticPattern, HapticsApi};
use keycard::{
    error::{KeycardError, KeycardIdentifyError},
    messages::{KeycardId, ShamirScheme},
};
use slint_keyos_platform::{
    async_archive, async_scalar, futures_lite,
    server::{CheckedPermissions, MessageAllowed},
};
use whence::WhenceExt;

pub trait KeycardBackupState {
    fn clear_error(&mut self);
    fn set_saved_shard_index(&mut self, index: usize);
    fn set_saving_to_keycard(&mut self, saving: bool);

    fn request_overwrite_confirmation(
        &mut self,
        error: KeycardIdentifyError,
    ) -> futures_lite::future::BoxedLocal<bool>;

    fn notify_store_shard_error(&mut self, error: KeycardError) -> futures_lite::future::BoxedLocal<()>;

    fn notify_envoy_backup_error(&mut self, error: EnvoyError) -> futures_lite::future::BoxedLocal<()>;
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum KeycardBackupError {
    #[error(transparent)]
    Identify(#[from] KeycardIdentifyError),
    #[error(transparent)]
    Store(#[from] KeycardError),
    #[error(transparent)]
    Envoy(#[from] EnvoyError),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum EnvoyError {
    #[error("{0}")]
    Response(String),
    #[error(transparent)]
    Ql(#[from] quantum_link::SendMessageError),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum BackupKind {
    Manual,
    Magic,
}

fn should_request_overwrite_confirmation(error: KeycardIdentifyError) -> bool {
    match error {
        // These are unauthenticated or malformed states, usually safe to recover by rewriting.
        KeycardIdentifyError::HmacMismatch | KeycardIdentifyError::InvalidData => false,
        // These cases contain authenticated existing data and require explicit user confirmation.
        KeycardIdentifyError::ExistingShard
        | KeycardIdentifyError::DifferentSeedFingerprint
        | KeycardIdentifyError::DifferentDeviceId => true,
    }
}

/// Backup keycards using the default 2-of-3 scheme.
/// Delegates to [`backup_keycards_with_scheme`] with [`ShamirScheme::DEFAULT`].
pub async fn backup_keycards<S, PK, PQ, PH>(
    state: &mut S,
    kind: BackupKind,
) -> whence::Result<(), KeycardBackupError>
where
    S: KeycardBackupState,
    PK: CheckedPermissions
        + MessageAllowed<keycard::messages::GenerateShards>
        + MessageAllowed<keycard::messages::PopShard>
        + MessageAllowed<keycard::messages::IdentifyKeycard>
        + MessageAllowed<keycard::messages::StoreShardToKeycard>
        + MessageAllowed<keycard::messages::SetShamirScheme>,
    PQ: CheckedPermissions + MessageAllowed<quantum_link::messages::BackupShard>,
    PH: CheckedPermissions + MessageAllowed<Vibrate>,
{
    backup_keycards_with_scheme::<S, PK, PQ, PH>(state, kind, ShamirScheme::DEFAULT).await
}

/// Backup keycards using a custom Shamir scheme
pub async fn backup_keycards_with_scheme<S, PK, PQ, PH>(
    state: &mut S,
    kind: BackupKind,
    scheme: ShamirScheme,
) -> whence::Result<(), KeycardBackupError>
where
    S: KeycardBackupState,
    PK: CheckedPermissions
        + MessageAllowed<keycard::messages::GenerateShards>
        + MessageAllowed<keycard::messages::PopShard>
        + MessageAllowed<keycard::messages::IdentifyKeycard>
        + MessageAllowed<keycard::messages::StoreShardToKeycard>
        + MessageAllowed<keycard::messages::SetShamirScheme>,
    PQ: CheckedPermissions + MessageAllowed<quantum_link::messages::BackupShard>,
    PH: CheckedPermissions + MessageAllowed<Vibrate>,
{
    let with_magic_backup = kind == BackupKind::Magic;
    let haptic = HapticsApi::<PH>::default();
    let mut cards_stored = vec![];

    // reset UI state
    state.clear_error();
    state.set_saved_shard_index(0);
    state.set_saving_to_keycard(false);

    // Set the scheme before generating shards
    async_archive::<PK, _>(keycard::messages::SetShamirScheme(scheme)).await.whence()?;
    async_scalar::<PK, _>(keycard::messages::GenerateShards { with_magic_backup }).await.whence()?;

    if kind == BackupKind::Magic {
        let shard = async_archive::<PK, _>(keycard::messages::PopShard).await.whence()?;
        backup_shard_envoy::<S, PQ>(state, shard).await.whence()?;
        state.set_saved_shard_index(1);
    }

    // Calculate cards needed based on the scheme
    // For magic backup: one shard goes to Envoy, rest to keycards
    let cards_needed = if with_magic_backup { scheme.share_count - 1 } else { scheme.share_count };

    while cards_stored.len() < cards_needed {
        backup_card::<S, PK, PH>(state, &haptic, &mut cards_stored).await?;
        let new_index = cards_stored.len() + if with_magic_backup { 1 } else { 0 };
        state.set_saved_shard_index(new_index);
    }

    Ok(())
}

pub async fn backup_shard_envoy<S, P>(
    state: &mut S,
    shard: backup_shard::Shard,
) -> Result<(), KeycardBackupError>
where
    S: KeycardBackupState,
    P: CheckedPermissions + MessageAllowed<quantum_link::messages::BackupShard>,
{
    const MAX_ATTEMPTS_BEFORE_RETRY_PROMPT: u8 = 3;
    let mut attempts_since_prompt: u8 = 0;

    loop {
        log::info!("backing up shard to envoy");
        let request = async_archive::<P, _>(quantum_link::messages::BackupShard { shard: shard.clone() });
        match request.await {
            Ok(quantum_link::foundation_api::backup::BackupShardResponse::Success) => {
                log::info!("successfully backed up shard with envoy");
                return Ok(());
            }
            Ok(quantum_link::foundation_api::backup::BackupShardResponse::Error { error }) => {
                log::error!("envoy failed to backup shard {error}");
                return Err(KeycardBackupError::Envoy(EnvoyError::Response(error)));
            }
            Err(e @ quantum_link::SendMessageError::NoDevicePaired) => {
                log::error!("no device paired, fatal error");
                Err(EnvoyError::Ql(e))?;
            }
            Err(error) => {
                attempts_since_prompt = attempts_since_prompt.saturating_add(1);
                if attempts_since_prompt <= MAX_ATTEMPTS_BEFORE_RETRY_PROMPT {
                    log::warn!(
                        "failed to send shard to envoy {error}; attempt {attempts_since_prompt}/{MAX_ATTEMPTS_BEFORE_RETRY_PROMPT}, retrying automatically"
                    );
                    continue;
                }

                log::error!(
                    "failed to send shard to envoy {error}; attempt {attempts_since_prompt}/{MAX_ATTEMPTS_BEFORE_RETRY_PROMPT}, waiting for user retry confirmation"
                );
                attempts_since_prompt = 0;
                let error = EnvoyError::Ql(error);
                state.notify_envoy_backup_error(error).await;
                state.clear_error();
                continue;
            }
        }
    }
}

pub async fn backup_card<S, PK, PH>(
    state: &mut S,
    haptic: &HapticsApi<PH>,
    cards_stored: &mut Vec<keycard::messages::KeycardId>,
) -> Result<(), KeycardBackupError>
where
    S: KeycardBackupState,
    PK: CheckedPermissions
        + MessageAllowed<keycard::messages::IdentifyKeycard>
        + MessageAllowed<keycard::messages::StoreShardToKeycard>,

    PH: CheckedPermissions + MessageAllowed<Vibrate>,
{
    loop {
        state.set_saving_to_keycard(false);

        let (id, error) = identify_keycard::<PK>(cards_stored).await?;

        // A keycard was identified, give haptic confirmation
        haptic.click();

        if let Some(error) = error {
            if should_request_overwrite_confirmation(error) {
                let confirmed = state.request_overwrite_confirmation(error).await;
                state.clear_error();

                if !confirmed {
                    // User denied, retry
                    continue;
                }
            }
        }

        state.set_saving_to_keycard(true);
        let store_result = store_shard::<PK>(id).await;
        state.set_saving_to_keycard(false);

        match store_result {
            Ok(id) => {
                haptic.vibrate(HapticPattern::PulsingStrongOne100);
                cards_stored.push(id);
                return Ok(());
            }
            Err(e) => {
                state.notify_store_shard_error(e).await;
                state.clear_error();
            }
        }
    }
}

async fn identify_keycard<PK>(
    cards_stored: &[KeycardId],
) -> Result<(KeycardId, Option<KeycardIdentifyError>), KeycardError>
where
    PK: CheckedPermissions + MessageAllowed<keycard::messages::IdentifyKeycard>,
{
    loop {
        let response = async_archive::<PK, _>(keycard::messages::IdentifyKeycard).await;
        match response {
            Ok((id, e)) => {
                if !cards_stored.contains(&id) {
                    log::info!("handle_identify_result: {id} {e:?}");
                    return Ok((id, e));
                }
                log::info!("identify_keycard returned a duplicate card {id} {e:?}");
            }
            Err(keycard::error::KeycardError::Nfc(nfc::error::NfcError::Timeout)) => {
                log::debug!("DetectKeycard timeout");
            }
            Err(e @ KeycardError::Nfc(nfc::error::NfcError::Disabled)) => {
                log::error!("nfc is disabled, terminating backup flow");
                return Err(e.into());
            }
            Err(e) => {
                log::error!("identify_keycard failed: {e:#}");
            }
        }
    }
}

async fn store_shard<PK>(id: keycard::messages::KeycardId) -> Result<KeycardId, KeycardError>
where
    PK: CheckedPermissions + MessageAllowed<keycard::messages::StoreShardToKeycard>,
{
    let mut retries = 5;
    loop {
        let response = async_archive::<PK, _>(keycard::messages::StoreShardToKeycard(id.clone())).await;
        match response {
            Ok(()) => {
                return Ok(id);
            }
            Err(e @ keycard::error::KeycardError::Nfc(nfc::error::NfcError::Timeout)) => {
                log::debug!("store_shard timeout");
                retries -= 1;
                if retries < 1 {
                    return Err(e);
                }
            }
            Err(e) => {
                log::error!("store_shard failed: {e:?}");
                return Err(e);
            }
        }
    }
}

#[macro_export]
macro_rules! impl_keycard_backup_error_from {
    () => {
        impl From<$crate::backup::KeycardBackupError> for KeycardBackupError {
            fn from(error: $crate::backup::KeycardBackupError) -> Self {
                match error {
                    $crate::backup::KeycardBackupError::Identify(e) => e.into(),
                    $crate::backup::KeycardBackupError::Store(e) => e.into(),
                    $crate::backup::KeycardBackupError::Envoy(e) => e.into(),
                }
            }
        }

        impl From<keycard::error::KeycardIdentifyError> for KeycardBackupError {
            fn from(error: keycard::error::KeycardIdentifyError) -> Self {
                use keycard::error::KeycardIdentifyError;

                use crate::{tr, TrId};

                match error {
                    KeycardIdentifyError::InvalidData => Self {
                        error: true,
                        title: tr::lookup_id(TrId::KeycardBackupMagicModalInvalidDataDetectedHeader).into(),
                        message: tr::lookup_id(TrId::KeycardBackupMagicModalInvalidDataDetectedContent)
                            .into(),
                        ok_text: tr::lookup_id(TrId::KeycardBackupMagicModalInvalidDataDetectedOverwrite)
                            .into(),
                        cancel_text: tr::lookup_id(TrId::CommonButtonCancel).into(),
                    },
                    KeycardIdentifyError::DifferentDeviceId => Self {
                        error: true,
                        title: tr::lookup_id(TrId::KeycardBackupMagicModalDataDetectedHeader).into(),
                        message: tr::lookup_id(TrId::KeycardBackupMagicModalDataDetectedContentPrime).into(),
                        ok_text: tr::lookup_id(TrId::CommonButtonConfirm).into(),
                        cancel_text: tr::lookup_id(TrId::CommonButtonCancel).into(),
                    },
                    KeycardIdentifyError::DifferentSeedFingerprint => Self {
                        error: true,
                        title: tr::lookup_id(TrId::KeycardBackupMagicModalDataDetectedHeader).into(),
                        message: tr::lookup_id(TrId::KeycardBackupMagicModalDataDetectedContentMasterKey)
                            .into(),
                        ok_text: tr::lookup_id(TrId::CommonButtonConfirm).into(),
                        cancel_text: tr::lookup_id(TrId::CommonButtonCancel).into(),
                    },
                    KeycardIdentifyError::HmacMismatch => Self {
                        error: true,
                        title: tr::lookup_id(TrId::KeycardBackupMagicModalInvalidThirdPartyKeycardHeader)
                            .into(),
                        message: tr::lookup_id(TrId::KeycardBackupMagicModalInvalidThirdPartyKeycardContent)
                            .into(),
                        ok_text: tr::lookup_id(TrId::CommonButtonIUnderstand).into(),
                        cancel_text: tr::lookup_id(TrId::CommonButtonCancel).into(),
                    },
                    KeycardIdentifyError::ExistingShard => Self {
                        error: true,
                        title: tr::lookup_id(TrId::KeycardBackupMagicModalDataDetectedHeader).into(),
                        message: tr::lookup_id(TrId::KeycardBackupMagicModalDataDetectedContentThisPrime)
                            .into(),
                        ok_text: tr::lookup_id(TrId::CommonButtonConfirm).into(),
                        cancel_text: tr::lookup_id(TrId::CommonButtonCancel).into(),
                    },
                }
            }
        }

        impl From<keycard::error::KeycardError> for KeycardBackupError {
            fn from(_: keycard::error::KeycardError) -> Self {
                use crate::{tr, TrId};
                Self {
                    error: true,
                    title: tr::lookup_id(TrId::KeycardBackupMagicModalErrorHeader).into(),
                    message: tr::lookup_id(TrId::KeycardBackupMagicModalErrorContent).into(),
                    ok_text: tr::lookup_id(TrId::CommonButtonRetry).into(),
                    cancel_text: Default::default(),
                }
            }
        }

        impl From<$crate::backup::EnvoyError> for KeycardBackupError {
            fn from(_: $crate::backup::EnvoyError) -> Self {
                use crate::{tr, TrId};
                Self {
                    error: true,
                    title: tr::lookup_id(TrId::KeycardBackupMagicModalEnvoyErrorHeader).into(),
                    message: tr::lookup_id(TrId::KeycardBackupMagicModalEnvoyErrorContent).into(),
                    ok_text: tr::lookup_id(TrId::CommonButtonRetry).into(),
                    cancel_text: Default::default(),
                }
            }
        }
    };
}

#[macro_export]
macro_rules! impl_backup_state_adapter {
    () => {
        struct BackupStateAdapter<'a> {
            global: &'a KeycardBackupGlobal<'a>,
            state: std::rc::Rc<std::cell::RefCell<BackupState>>,

            kind: $crate::backup::BackupKind,
            saved_shard_index: usize,
            saving_to_keycard: bool,
        }

        impl BackupStateAdapter<'_> {
            fn set_slint_state(&self) {
                let steps = to_step_model(self.kind, self.saved_shard_index, self.saving_to_keycard);
                let steps = slint_keyos_platform::slint::ModelRc::new(
                    slint_keyos_platform::slint::VecModel::from(steps),
                );
                self.global.set_steps(steps);
                self.global.set_saving_to_keycard(self.saving_to_keycard);
            }
        }

        impl $crate::backup::KeycardBackupState for BackupStateAdapter<'_> {
            fn clear_error(&mut self) { self.global.set_error(KeycardBackupError::default()); }

            fn set_saved_shard_index(&mut self, index: usize) {
                self.saved_shard_index = index;
                self.set_slint_state();
            }

            fn set_saving_to_keycard(&mut self, saving: bool) {
                self.saving_to_keycard = saving;
                self.set_slint_state();
            }

            fn request_overwrite_confirmation(
                &mut self,
                error: keycard::error::KeycardIdentifyError,
            ) -> slint_keyos_platform::futures_lite::future::BoxedLocal<bool> {
                log::info!("waiting for overwrite confirmation");
                self.global.set_error(error.into());
                let (tx, rx) = oneshot::channel();
                *self.state.borrow_mut() = BackupState::NeedsConfirmation { confirmation: tx };
                Box::pin(async move { rx.await.unwrap_or(false) })
            }

            fn notify_store_shard_error(
                &mut self,
                error: keycard::error::KeycardError,
            ) -> slint_keyos_platform::futures_lite::future::BoxedLocal<()> {
                log::info!("waiting for store shard error confirmation");
                self.global.set_error(error.into());
                let (tx, rx) = oneshot::channel();
                *self.state.borrow_mut() = BackupState::NeedsConfirmation { confirmation: tx };
                Box::pin(async move {
                    let _ = rx.await;
                })
            }

            fn notify_envoy_backup_error(
                &mut self,
                error: $crate::backup::EnvoyError,
            ) -> slint_keyos_platform::futures_lite::future::BoxedLocal<()> {
                log::info!("waiting for envoy backup error confirmation");
                self.global.set_error(error.into());
                let (tx, rx) = oneshot::channel();
                *self.state.borrow_mut() = BackupState::NeedsConfirmation { confirmation: tx };
                Box::pin(async move {
                    let _ = rx.await;
                })
            }
        }
    };
}

#[macro_export]
macro_rules! backup_impl_to_step_model {
    () => {
        fn to_step_model(
            kind: $crate::backup::BackupKind,
            saved_shard_index: usize,
            saving_to_keycard: bool,
        ) -> Vec<crate::StepModel> {
            use crate::{tr::lookup_id, StepModel, TrId};
            match kind {
                $crate::backup::BackupKind::Magic => {
                    let loading_text = |idx: usize| -> slint_keyos_platform::slint::SharedString {
                        if saving_to_keycard && saved_shard_index == idx {
                            lookup_id(TrId::KeycardBackupMagicSavingToKeycard).into()
                        } else if idx <= 1 {
                            lookup_id(TrId::KeycardBackupMagicTapAKeycard).into()
                        } else {
                            lookup_id(TrId::KeycardBackupMagicTapAnotherKeycard).into()
                        }
                    };
                    vec![
                        StepModel {
                            label: if saved_shard_index >= 1 {
                                lookup_id(TrId::KeycardBackupMagicFirstPartStoredEnvoy).into()
                            } else {
                                lookup_id(TrId::KeycardBackupMagicSendingFirstPart).into()
                            },
                            icon: "arrow-right".into(),
                            completed: saved_shard_index >= 1,
                            in_progress: saved_shard_index == 0,
                            error: false,
                        },
                        StepModel {
                            label: if saved_shard_index >= 2 {
                                lookup_id(TrId::KeycardBackupMagicSecondPartStoredKeycard).into()
                            } else {
                                loading_text(1)
                            },
                            icon: "arrow-right".into(),
                            completed: saved_shard_index >= 2,
                            in_progress: saved_shard_index == 1,
                            error: false,
                        },
                        StepModel {
                            label: if saved_shard_index >= 3 {
                                lookup_id(TrId::KeycardBackupMagicThirdPartStoredKeycard).into()
                            } else {
                                loading_text(2)
                            },
                            icon: "arrow-right".into(),
                            completed: saved_shard_index >= 3,
                            in_progress: saved_shard_index == 2,
                            error: false,
                        },
                    ]
                }
                $crate::backup::BackupKind::Manual => {
                    let loading_text = |idx: usize| -> slint_keyos_platform::slint::SharedString {
                        if saving_to_keycard && saved_shard_index == idx {
                            lookup_id(TrId::KeycardBackupMagicSavingToKeycard).into()
                        } else if idx == 0 {
                            lookup_id(TrId::KeycardBackupMagicTapAKeycard).into()
                        } else {
                            lookup_id(TrId::KeycardBackupMagicTapAnotherKeycard).into()
                        }
                    };
                    vec![
                        StepModel {
                            label: if saved_shard_index >= 1 {
                                lookup_id(TrId::KeycardBackupManualFirstPartStoredKeycard).into()
                            } else {
                                loading_text(0)
                            },
                            icon: "arrow-right".into(),
                            completed: saved_shard_index >= 1,
                            in_progress: saved_shard_index == 0,
                            error: false,
                        },
                        StepModel {
                            label: if saved_shard_index >= 2 {
                                lookup_id(TrId::KeycardBackupMagicSecondPartStoredKeycard).into()
                            } else {
                                loading_text(1)
                            },
                            icon: "arrow-right".into(),
                            completed: saved_shard_index >= 2,
                            in_progress: saved_shard_index == 1,
                            error: false,
                        },
                        StepModel {
                            label: if saved_shard_index >= 3 {
                                lookup_id(TrId::KeycardBackupMagicThirdPartStoredKeycard).into()
                            } else {
                                loading_text(2)
                            },
                            icon: "arrow-right".into(),
                            completed: saved_shard_index >= 3,
                            in_progress: saved_shard_index == 2,
                            error: false,
                        },
                    ]
                }
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use keycard::error::KeycardIdentifyError;

    use super::should_request_overwrite_confirmation;

    #[test]
    fn invalid_data_and_hmac_mismatch_have_same_policy() {
        assert_eq!(
            should_request_overwrite_confirmation(KeycardIdentifyError::InvalidData),
            should_request_overwrite_confirmation(KeycardIdentifyError::HmacMismatch),
        );
        assert!(!should_request_overwrite_confirmation(KeycardIdentifyError::InvalidData));
    }

    #[test]
    fn authenticated_existing_data_requires_confirmation() {
        assert!(should_request_overwrite_confirmation(KeycardIdentifyError::ExistingShard));
        assert!(should_request_overwrite_confirmation(KeycardIdentifyError::DifferentSeedFingerprint));
        assert!(should_request_overwrite_confirmation(KeycardIdentifyError::DifferentDeviceId));
    }
}
