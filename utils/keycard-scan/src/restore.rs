// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use haptics::{messages::Vibrate, HapticPattern, HapticsApi};
use slint_keyos_platform::{
    async_archive,
    server::{CheckedPermissions, MessageAllowed},
};

#[derive(Debug, thiserror::Error)]
pub enum GetRemoteShardError {
    #[error(transparent)]
    SendMessage(#[from] quantum_link::SendMessageError),
    #[error("envoy reported error: {0}")]
    Envoy(String),
    #[error("invalid shard")]
    InvalidShard,
}

pub async fn get_remote_shard<P>(
    seed_fingerprint: [u8; 32],
    timestamp: u32,
    mut predicate: impl FnMut(&quantum_link::SendMessageError) -> bool,
) -> Result<Option<backup_shard::Shard>, GetRemoteShardError>
where
    P: CheckedPermissions + MessageAllowed<quantum_link::messages::RestoreShard>,
{
    let shard = loop {
        let response = async_archive::<P, _>(quantum_link::messages::RestoreShard {
            seed_fingerprint: quantum_link::foundation_api::backup::SeedFingerprint(seed_fingerprint),
            timestamp,
        })
        .await;
        match response {
            Ok(quantum_link::foundation_api::backup::RestoreShardResponse::Success { shard }) => break shard,
            Ok(quantum_link::foundation_api::backup::RestoreShardResponse::NotFound) => {
                return Ok(None);
            }
            Ok(quantum_link::foundation_api::backup::RestoreShardResponse::Error { error }) => {
                return Err(GetRemoteShardError::Envoy(error));
            }
            Err(e) => {
                if predicate(&e) {
                    log::info!("failed to restore quantum link shard, retrying... {e:?}");
                } else {
                    Err(e)?;
                }
            }
        }
    };

    backup_shard::Shard::decode(&shard.0)
        .inspect_err(|e| log::error!("failed to decode shard {e:?}"))
        .map(Some)
        .map_err(|_| GetRemoteShardError::InvalidShard)
}

pub enum ScanEvent {
    WaitingForKeycard,
    ReadingFromKeycard,
    ScanComplete { cards_loaded: usize },
}

pub async fn scan_keycard<PK, PH>(
    haptic: &HapticsApi<PH>,
    cards_loaded: &mut Vec<keycard::messages::KeycardId>,
    mut handler: impl FnMut(ScanEvent),
) -> Result<keycard::messages::LoadedShard, keycard::error::KeycardError>
where
    PK: CheckedPermissions
        + MessageAllowed<keycard::messages::DetectKeycard>
        + MessageAllowed<keycard::messages::LoadShardFromKeycard>,
    PH: CheckedPermissions + MessageAllowed<Vibrate>,
{
    loop {
        handler(ScanEvent::WaitingForKeycard);

        loop {
            let response = async_archive::<PK, _>(keycard::messages::DetectKeycard {
                timeout: std::time::Duration::from_millis(1000),
            })
            .await;
            match response {
                Ok(id) => {
                    if !cards_loaded.contains(&id) {
                        log::info!("detected keycard: {id}");
                        break;
                    }
                    log::info!("skipping duplicate keycard: {id}");
                }
                Err(keycard::error::KeycardError::Nfc(nfc::error::NfcError::Timeout)) => {
                    log::debug!("DetectKeycard timeout");
                }
                Err(e) => {
                    log::info!("DetectKeycard failed: {e:?}");
                    return Err(e.into());
                }
            }
        }

        // A keycard was identified, give haptic confirmation
        haptic.click();

        handler(ScanEvent::ReadingFromKeycard);

        let response = async_archive::<PK, _>(keycard::messages::LoadShardFromKeycard).await;
        match response {
            Ok(shard) => {
                if !cards_loaded.contains(&shard.id) {
                    log::info!(
                        "loaded shard from keycard: {}, has_magic_backup: {:?}",
                        shard.id,
                        shard.has_magic_backup
                    );
                    cards_loaded.push(shard.id.clone());
                    handler(ScanEvent::ScanComplete { cards_loaded: cards_loaded.len() });
                    haptic.vibrate(HapticPattern::PulsingStrongOne100);
                    return Ok(shard);
                }
                log::info!("skipping duplicate keycard: {}", shard.id);
            }
            Err(keycard::error::KeycardError::Nfc(nfc::error::NfcError::Timeout)) => {
                log::debug!("LoadShardFromKeycard timeout");
            }
            Err(keycard::error::KeycardError::BlankTag) => {
                log::info!("LoadShardFromKeycard: blank tag, retrying");
            }
            Err(e) => {
                log::info!("LoadShardFromKeycard failed: {e:?}");
                return Err(e.into());
            }
        }
    }
}
