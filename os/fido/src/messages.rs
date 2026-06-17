// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use iso7816::command::{CommandView, FromSliceError};
use server::{AsScalar, FromScalar};

use crate::error::FidoError;
use crate::SecurityKeyView;

#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub enum Transport {
    Usb,
    Nfc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum U2fApduParseError {
    WrongLength,
    ClassNotSupported,
}

impl U2fApduParseError {
    pub fn to_u2f_response(self) -> Vec<u8> {
        match self {
            Self::WrongLength => vec![0x67, 0x00],
            Self::ClassNotSupported => vec![0x6e, 0x00],
        }
    }
}

impl From<FromSliceError> for U2fApduParseError {
    fn from(error: FromSliceError) -> Self {
        match error {
            FromSliceError::InvalidClass => Self::ClassNotSupported,
            FromSliceError::TooShort
            | FromSliceError::TooLong
            | FromSliceError::InvalidFirstBodyByteForExtended
            | FromSliceError::InvalidSliceLength => Self::WrongLength,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct U2fApduCommand {
    pub class: u8,
    pub instruction: u8,
    pub p1: u8,
    pub p2: u8,
    pub data: Vec<u8>,
    pub expected: u32,
    pub extended: bool,
}

impl U2fApduCommand {
    pub fn parse(apdu: &[u8]) -> Result<Self, U2fApduParseError> {
        let command = CommandView::try_from(apdu)?;
        Ok(Self::from_command_view(command))
    }

    pub fn from_command_view(command: CommandView<'_>) -> Self {
        Self {
            class: command.class().into_inner(),
            instruction: command.instruction().into(),
            p1: command.p1,
            p2: command.p2,
            data: command.data().to_vec(),
            expected: command.expected() as u32,
            extended: command.extended,
        }
    }

    pub fn data(&self) -> &[u8] { &self.data }
}

// === Key change event ===

/// Wrapper for the key list published via events.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct KeysChangedEvent {
    pub keys: Vec<SecurityKeyView>,
}

// === Key management messages ===

/// Subscribe to key changes. Returns current key list immediately,
/// then pushes updates whenever keys are modified.
#[derive(server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(KeysChangedEvent)]
pub struct SubscribeKeyChanges;

// === Presence keep-alive event ===

/// Heartbeat published by the FIDO server every time it replies to an RP with a "retry me"
/// status (`ConditionNotSatisfied` for U2F, `UserActionPending` for CTAP2). Subscribers — the
/// Security Keys app while its user-presence modal is up — use these to distinguish an active
/// RP from an abandoned one and auto-dismiss the modal when the heartbeat stops.
///
/// The fingerprint is the SHA-256 of the current in-flight request, exposed so the UI can
/// ignore heartbeats for a different fingerprint if a newer request has taken over.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct PresenceKeepAliveEvent {
    pub fingerprint: [u8; 32],
}

/// Subscribe to presence keep-alive events.
#[derive(server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(PresenceKeepAliveEvent)]
pub struct SubscribePresenceKeepAlive;

// === Operation outcome event ===

/// Whether a completed FIDO operation was a registration or an authentication.
#[derive(Debug, Clone, Copy, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum OperationType {
    Registration,
    Authentication,
}

/// Event published by the FIDO server after a U2F/CTAP operation completes (success or
/// failure). The Security Keys app is the only subscriber: it shows the success/failure
/// modal in response. At outcome time the app is guaranteed running (it just handled the
/// presence prompt), so a subscription is sufficient.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct OperationOutcomeEvent {
    pub security_key_index: usize,
    pub operation: OperationType,
    pub success: bool,
}

/// Subscribe to operation outcome events.
#[derive(server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(OperationOutcomeEvent)]
pub struct SubscribeOperationOutcomes;

/// Create a new security key with UI metadata. Returns the new key's index, or an error if
/// creation failed before any state was mutated. Note: a `save_and_notify` failure after a
/// successful in-memory create is intentionally still returned as `Ok(index)` and only logged
/// — the new key is usable in this session and worst case is lost on reboot, which is a softer
/// failure mode than refusing the create over a transient FS or subscriber hiccup.
#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<usize, FidoError>)]
pub struct CreateSecurityKey {
    pub label: String,
    pub color: u8,
    pub icon: String,
}

/// Edit metadata of an existing security key. Returns the validation outcome so the GUI
/// can surface `EmptyLabel`/`DuplicateLabel` without a separate `validate_label` round-trip.
#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), FidoError>)]
pub struct EditSecurityKey {
    pub index: usize,
    pub label: String,
    pub color: u8,
    pub icon: String,
    pub date: u64,
}

/// Set the archived state of a security key.
/// Archived keys are automatically set to live=false.
/// Restoring from archive sets live=true.
#[derive(Debug, server::Message)]
#[response(Result<(), FidoError>)]
pub struct SetArchived {
    pub index: usize,
    pub archived: bool,
}

impl AsScalar<2> for SetArchived {
    fn as_scalar(&self) -> [u32; 2] { [self.index as u32, self.archived as u32] }
}
impl FromScalar<2> for SetArchived {
    fn from_scalar([a, b]: [u32; 2]) -> Self { Self { index: a as usize, archived: b != 0 } }
}

/// Blocking synchronous snapshot of all security keys. Used at app startup to populate
/// local state before the async `SubscribeKeyChanges` stream has had a chance to run —
/// avoids a race where the app (launched by a presence check) concludes "no keys" because
/// the initial subscribed event hasn't been drained yet.
#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Vec<SecurityKeyView>)]
pub struct ListSecurityKeys;

// === Selection messages ===

#[derive(Debug, server::Message)]
#[response(Option<usize>)]
pub struct GetSelectedSecurityKey;

/// Fire-and-forget message for selecting a security key.
#[derive(Debug, server::Message)]
pub struct SelectSecurityKey(pub Option<usize>);

// === Protocol messages ===

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Vec<u8>)]
pub struct U2fProcessApdu {
    pub command: U2fApduCommand,
    pub transport: Transport,
}

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Vec<u8>)]
pub struct CtapProcessCbor {
    pub cmd: u8,
    pub raw: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u2f_apdu_command_rejects_short_buffers() {
        for len in 0..4 {
            let data = vec![0; len];
            assert_eq!(U2fApduCommand::parse(&data), Err(U2fApduParseError::WrongLength));
        }
    }

    #[test]
    fn u2f_apdu_command_parses_short_apdu() {
        let command = U2fApduCommand::parse(&[0x00, 0x01, 0x02, 0x00, 0x02, 0xaa, 0xbb, 0x00]).unwrap();

        assert_eq!(command.class, 0x00);
        assert_eq!(command.instruction, 0x01);
        assert_eq!(command.p1, 0x02);
        assert_eq!(command.p2, 0x00);
        assert_eq!(command.data(), &[0xaa, 0xbb]);
        assert_eq!(command.expected, 256);
        assert!(!command.extended);
    }

    #[test]
    fn u2f_apdu_command_parses_extended_apdu() {
        let command =
            U2fApduCommand::parse(&[0x00, 0x01, 0x02, 0x00, 0x00, 0x00, 0x02, 0xaa, 0xbb, 0x00, 0x00])
                .unwrap();

        assert_eq!(command.data(), &[0xaa, 0xbb]);
        assert_eq!(command.expected, 65_536);
        assert!(command.extended);
    }
}

// === Test messages ===

#[cfg(feature = "test-app")]
#[derive(Debug, server::Message)]
#[response(Result<(), FidoError>)]
pub struct ResetState;
