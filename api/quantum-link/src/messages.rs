// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use foundation_api::api::onboarding::OnboardingState;
use foundation_api::backup::{
    BackupShardResponse, CreateMagicBackupResult, PrimeMagicBackupStatusResponse, RestoreMagicBackupResult,
    RestoreShardResponse, SeedFingerprint,
};
use foundation_api::bitcoin::{AccountUpdate, SignPsbt};
use foundation_api::firmware::{FirmwareFetchEvent, FirmwareInstallEvent, FirmwareUpdateAvailable};
use foundation_api::fx::{ExchangeRate, ExchangeRateHistory};
use foundation_api::status::{EnvoyStatus, TimezoneResponse};

use crate::{PairingEvent, SecurityCheckState, SendMessageError};

#[derive(Debug, server::Message)]
#[response(())]
pub struct StartWithoutFilesystem;

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct PublishPsbt {
    pub transaction: foundation_api::bitcoin::BroadcastTransaction,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct SendAccountUpdate {
    pub account_id: String,
    pub update: Vec<u8>,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Vec<u8>)]
pub struct GetXidDocument;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Option<FirmwareUpdateAvailable>, SendMessageError>)]
pub struct CheckFirmwareUpdate;

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct StartFirmwareUpdate {
    pub chunk_offset: Option<u64>,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<BackupShardResponse, SendMessageError>)]
pub struct BackupShard {
    pub shard: backup_shard::Shard,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<RestoreShardResponse, SendMessageError>)]
pub struct RestoreShard {
    pub seed_fingerprint: SeedFingerprint,
    pub timestamp: u32,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct NotifyOnboardingState {
    pub state: OnboardingState,
}
#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct NotifyFirmwareInstall {
    pub event: FirmwareInstallEvent,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<bool, SendMessageError>)]
pub struct EnvoyMagicBackupEnabled;

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct SendMagicBackupEvent {
    pub event: foundation_api::backup::CreateMagicBackupEvent,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct SendRestoreMagicBackupResult {
    pub result: RestoreMagicBackupResult,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(CreateMagicBackupResult)]
pub struct AwaitCreateMagicBackupResult;

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct StartRestoreMagicBackup;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<PrimeMagicBackupStatusResponse, SendMessageError>)]
pub struct MagicBackupStatus;

/// Send `UnpairingRequest` to Envoy as a courtesy "goodbye", flush over BLE,
/// then tear down local pairing state. Use this from any happy-path unpair flow
/// (settings disconnect, factory reset) so Envoy can transition to a clean "unpaired" state
#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct UnpairFromEnvoy;

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct SendApplyPassphrase {
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct SendPrimeMagicBackupEnabled {
    pub enabled: bool,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), SendMessageError>)]
pub struct SendPrimeFiatPreference {
    pub currency_code: String,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<TimezoneResponse, SendMessageError>)]
pub struct EnvoyTimezone;

//
// Subscriptions
//

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(ExchangeRate)]
pub struct SubscribeExchangeRate;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(FirmwareFetchEvent)]
pub struct SubscribeFirmwareFetch;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(EnvoyStatus)]
pub struct SubscribeEnvoyStatus;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(SignPsbt)]
pub struct SubscribeSignPsbt;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(AccountUpdate)]
pub struct SubscribeAccountUpdate;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(PairingEvent)]
pub struct SubscribePairingEvent;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(OnboardingState)]
pub struct SubscribeOnboardingState;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(SecurityCheckState)]
pub struct SubscribeSecurityCheckState;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(SendAccountUpdate)]
pub struct SubscribePublishedAccountUpdate;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(ExchangeRateHistory)]
pub struct SubscribeExchangeRateHistory;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(foundation_api::backup::RestoreMagicBackupEvent)]
pub struct SubscribeRestoreMagicBackup;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(crate::ConnectionStatus)]
pub struct SubscribeConnectionStatus;
