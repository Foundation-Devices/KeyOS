// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod command;
mod error;

use command::{AuthenticateRequest, AuthenticateResponse, Command, RegisterRequest, VersionResponse};
pub(crate) use command::{KeyHandle, RegisterResponse};
pub(crate) use error::Error;
use error::Status;
use gui_server_api::navigation::securitykeys::UserPresenceOptions;

use crate::{
    implementation::{presence_fingerprint, FidoServer, PresencePoll, SELECTION_TIMEOUT},
    messages::{OperationOutcomeEvent, OperationType, Transport, U2fApduCommand},
    RegisteredKey,
};

/// Info about a completed FIDO operation, carried out of `process_apdu` so that only
/// operations which actually mutated state notify subscribers.
#[derive(Debug)]
struct OperationOutcome {
    security_key_index: usize,
    is_registration: bool,
}

impl FidoServer {
    fn process_apdu(
        &mut self,
        command: &U2fApduCommand,
        transport: Transport,
        is_repeat: bool,
    ) -> Result<(Vec<u8>, Option<OperationOutcome>), Error> {
        log::debug!("process_apdu({:02x?},{:?})", command, transport);
        let instruction = command.instruction;
        let p1 = command.p1;
        let p2 = command.p2;
        if p2 != 0 {
            return Err(Error::WrongParameter);
        }
        let body = command.data();
        // Fingerprint used to match this request against any pending presence prompt on retry.
        let fingerprint = presence_fingerprint(body);
        match Command::try_from(instruction)? {
            Command::Register => {
                if !is_repeat {
                    log::info!("Register");
                }
                // gitlab.com/github.com/google.com send a 3 here !!!
                // if p1 != 0 {
                //     return Err(Error::WrongParameter);
                // }

                // Note: no short-circuit on empty state.security_keys — the Security Keys
                // app's presence dropdown now offers a "Create a new key" entry even when
                // the user has zero keys, so a fresh user can register their first key
                // directly from a RP-initiated Register request.

                let req = RegisterRequest::from_apdu_data(body)?;
                log::debug!("{req:02x?}");

                // NFC can't stay in card emulation across the CNS retry loop, so we can't
                // drive a presence prompt there. Require a pre-selected key and treat the
                // physical tap as consent. USB goes through the non-blocking prompt below.
                let security_key_index = if transport == Transport::Nfc {
                    let (index, _timestamp) = self
                        .selected
                        .ok_or(Error::Other)
                        .inspect_err(|_| log::error!("No key pre-selected for NFC!"))?;
                    index
                } else {
                    // Fast-path: a key selected by the user within the timeout window avoids a
                    // re-prompt on back-to-back operations.
                    let pre_selected = self.selected.and_then(|(idx, ts)| {
                        if ts.elapsed() < SELECTION_TIMEOUT {
                            Some(idx)
                        } else {
                            None
                        }
                    });

                    match self
                        .poll_or_start_presence(fingerprint, UserPresenceOptions::registration(pre_selected))
                    {
                        PresencePoll::Pending => {
                            // RP should retry; U2F spec says so on SW_CONDITIONS_NOT_SATISFIED.
                            return Err(Error::ConditionNotSatisfied);
                        }
                        PresencePoll::Dismissed => {
                            log::info!("User dismissed Register prompt");
                            return Err(Error::Other);
                        }
                        PresencePoll::Confirmed { selected_key_index } => {
                            selected_key_index.or(pre_selected).ok_or_else(|| {
                                log::error!("No key selected after user presence check!");
                                Error::Other
                            })?
                        }
                    }
                };
                let response_bytes = self.attest_and_register_u2f(
                    security_key_index,
                    req.application_parameter,
                    req.challenge_parameter,
                )?;

                Ok((response_bytes, Some(OperationOutcome { security_key_index, is_registration: true })))
            }
            Command::Authenticate => {
                if !is_repeat {
                    log::info!("Authenticate");
                }
                let req = AuthenticateRequest::from_apdu_data(body)?;
                log::debug!("{req:02x?}");
                let security_key_index = req.key_handle.security_key_index;
                let registered_key_index = req.key_handle.registered_key_index;

                // P1=0x07 "check-only" must return immediately without any presence prompt.
                if p1 == 0x07 {
                    log::debug!("check-only");
                    let security_key = self.security_key(security_key_index)?;
                    let registered_key_indexes =
                        security_key.registered_key_indexes(true, &req.application_parameter);
                    if registered_key_indexes.contains(&registered_key_index) {
                        // note that despite the name this signals a success condition
                        return Err(Error::ConditionNotSatisfied);
                    } else {
                        log::error!("U2F asked for an unknown key handle");
                        return Err(Error::WrongData);
                    }
                }

                let enforce_user_presence = match p1 {
                    0x03 => {
                        log::debug!("enforce-user-presence-and-sign");
                        true
                    }
                    0x08 => {
                        log::debug!("dont-enforce-user-presence-and-sign");
                        false
                    }
                    _ => return Err(Error::WrongParameter),
                };

                // Liveness check: a key must be live, or currently selected (within timeout).
                // Selection via the presence prompt below will also unlock the key for this
                // request — we re-check after the poll.
                let currently_selected = self
                    .selected
                    .is_some_and(|(idx, ts)| idx == security_key_index && ts.elapsed() < SELECTION_TIMEOUT);
                let security_key = self.security_key(security_key_index)?;
                if !security_key.live && !currently_selected {
                    log::error!("U2F trying to use a not-live and not-selected Key");
                    return Err(Error::WrongData);
                }

                let user_present = if currently_selected {
                    true
                } else if transport == Transport::Nfc {
                    // Same reason as Register: NFC card-emulation can't carry a CNS retry
                    // loop, so we take the physical tap as consent.
                    true
                } else {
                    match self.poll_or_start_presence(
                        fingerprint,
                        UserPresenceOptions::authentication(Some(security_key_index)),
                    ) {
                        PresencePoll::Pending => return Err(Error::ConditionNotSatisfied),
                        PresencePoll::Dismissed => {
                            log::info!("User dismissed Authenticate prompt");
                            return Err(Error::Other);
                        }
                        PresencePoll::Confirmed { .. } => true,
                    }
                };
                if enforce_user_presence && !user_present {
                    log::error!("User Presence canceled !");
                    return Err(Error::ConditionNotSatisfied);
                }
                let mut resp = AuthenticateResponse::new(user_present);
                // Re-fetch `security_key` here: the earlier immutable borrow was invalidated by
                // the mutable `self.poll_or_start_presence(...)` call above.
                let registered_key =
                    self.security_key(security_key_index)?.registered_key(registered_key_index)?;
                // FIDO U2F: the authenticator MUST verify that the key handle was issued for
                // the RP that's now requesting a signature. Without this, a malicious RP could
                // guess the small-integer (security_key_index, registered_key_index) tuple and
                // request a signature bound to a different application. (SFT-6916)
                match registered_key {
                    RegisteredKey::U2f(u2f) if u2f.application_parameter == req.application_parameter => {}
                    _ => {
                        log::error!("U2F Authenticate: key handle does not match application_parameter");
                        return Err(Error::WrongData);
                    }
                }
                resp.counter = registered_key.signature_counter();
                let mut signature_base = Vec::new();
                signature_base.extend_from_slice(&req.application_parameter);
                signature_base.push(resp.user_presence);
                signature_base.extend_from_slice(&resp.counter.to_be_bytes());
                signature_base.extend_from_slice(&req.challenge_parameter);
                (resp.signature, _) =
                    self.sign_der(security_key_index, registered_key_index, &signature_base)?;
                log::debug!("{resp:02x?}");

                Ok((resp.to_vec(), Some(OperationOutcome { security_key_index, is_registration: false })))
            }
            Command::Version => {
                log::info!("Version");
                if p1 != 0 {
                    return Err(Error::WrongParameter);
                }
                let resp = VersionResponse::prime();
                Ok((resp.to_vec(), None))
            }
        }
    }

    pub fn u2f_process_apdu(&mut self, command: &U2fApduCommand, transport: Transport) -> Vec<u8> {
        // Detect repeats of the same APDU (same fingerprint) arriving while we wait on the
        // user — an RP polling during a user-presence window. We gate the
        // "Register"/"Authenticate" info log on `!is_repeat` so the retry loop doesn't spam.
        let fingerprint = presence_fingerprint(command.data());
        let is_repeat = self.last_u2f_fingerprint == Some(fingerprint);
        self.last_u2f_fingerprint = Some(fingerprint);

        let res = self.process_apdu(command, transport, is_repeat);
        if let Err(ref e) = res {
            match e {
                // Not a real error — it's the "retry me" signal while we wait for user
                // presence. Keep it at debug so production INFO-level logs stay clean.
                Error::ConditionNotSatisfied => log::debug!("{res:?}"),
                _ => log::error!("{res:?}"),
            }
        }

        let status = Status::from(&res);
        let response = status.to_vec(res.as_ref().map(|(data, _)| data.as_slice()).unwrap_or_default());

        // Notify subscribers only when an operation actually completed and mutated state.
        // Pending retries (ConditionNotSatisfied), check-only queries, and Version replies all
        // leave state untouched, and notifying on those spams subscribers once per retry.
        if let Ok((_, Some(outcome))) = &res {
            let security_key_index = outcome.security_key_index;
            let is_registration = outcome.is_registration;
            if let Err(e) = self.refresh_and_notify() {
                log::error!("Failed to refresh FIDO keys: {:?}", e);
            }
            let op = if is_registration { "Register" } else { "Authenticate" };
            log::info!("{op} success (security_key_index={security_key_index})");
            self.operation_outcome_subscribers.send(&OperationOutcomeEvent {
                security_key_index,
                operation: if is_registration {
                    OperationType::Registration
                } else {
                    OperationType::Authentication
                },
                success: true,
            });
        }

        response
    }
}
