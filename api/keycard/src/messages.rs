// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use backup_shard::Shard;

use crate::error::{KeycardError, KeycardIdentifyError};

// === External messages ===

/// Shamir secret sharing scheme configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ShamirScheme {
    /// Number of shards required to reconstruct the secret
    pub threshold: usize,
    /// Total number of shards to generate
    pub share_count: usize,
}

impl ShamirScheme {
    pub const DEFAULT: Self = Self { threshold: 2, share_count: 3 };
}

impl Default for ShamirScheme {
    fn default() -> Self { Self::DEFAULT }
}

#[derive(Debug, Clone, Hash, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct KeycardId(pub Vec<u8>);

impl std::fmt::Display for KeycardId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{:02x?}", self.0) }
}

#[derive(Debug, server::Message)]
#[response(Result<(), KeycardError>)]
pub struct ResetShards;

/// Set the active Shamir scheme for subsequent GenerateShards calls.
/// Must be called before GenerateShards if you want a non-default scheme.
/// The scheme is cleared on ResetShards.
#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), KeycardError>)]
pub struct SetShamirScheme(pub ShamirScheme);

#[derive(Debug, server::Message)]
#[response(Result<(), KeycardError>)]
pub struct GenerateShards {
    pub with_magic_backup: bool,
}

impl server::FromScalar<1> for GenerateShards {
    fn from_scalar(value: [u32; 1]) -> Self {
        let with_magic_backup = bool::from_scalar(value);
        Self { with_magic_backup }
    }
}

impl server::AsScalar<1> for GenerateShards {
    fn as_scalar(&self) -> [u32; 1] { self.with_magic_backup.as_scalar() }
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Shard, KeycardError>)]
pub struct PopShard;

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), KeycardError>)]
pub struct PushShard {
    pub shard: Shard,
    pub accept_different_device_id: bool,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(KeycardId, Option<KeycardIdentifyError>), KeycardError>)]
pub struct IdentifyKeycard;

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<KeycardId, KeycardError>)]
pub struct DetectKeycard {
    pub timeout: std::time::Duration,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), KeycardError>)]
pub struct StoreShardToKeycard(pub KeycardId);

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), KeycardError>)]
pub struct FormatKeycard(pub KeycardId);

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<LoadedShard, KeycardError>)]
pub struct LoadShardFromKeycard;

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct LoadedShard {
    pub id: KeycardId,
    pub has_magic_backup: bool,
    pub seed_fingerprint: [u8; 32],
    /// Scheme info (from V1 shards, or default 2-of-3 for V0)
    pub scheme: ShamirScheme,
    /// Timestamp when the backup was created (from V1 shards, or 0 for V0)
    pub timestamp: u32,
}

#[derive(Debug, server::Message)]
#[response(Result<(), KeycardError>)]
pub struct CheckBackup;

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<MasterSeedRestored, KeycardError>)]
pub struct RestoreMasterSeed;

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct MasterSeedRestored {
    pub seed: security::Seed,
    pub different_device_id: bool,
}
