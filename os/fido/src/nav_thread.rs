// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Long-lived navigation thread.
//!
//! Owns the only blocking GUI call FIDO makes: `gui.check_user_presence(...)`. FIDO writes
//! a `PresenceState::Pending { fingerprint, options }` and notifies the shared `Condvar`;
//! Nav wakes, transitions the slot to `InProgress`, runs the blocking GUI call, then commits
//! `Completed` (gated on the fingerprint still matching). One thread, fixed count, spawned
//! once at server init.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use gui_server_api::navigation::securitykeys::UserPresenceOptions;

use crate::implementation::{PresencePoll, PresenceState, SELECTION_TIMEOUT};

gui_server_api::use_api!();

/// Shared rendezvous between FIDO and Nav: the state mutex carries the request payload in the
/// `Pending` variant; the Condvar wakes Nav when FIDO writes one.
type PresenceSlot = Arc<(Mutex<PresenceState>, Condvar)>;

/// FIDO-side handle to the Nav worker. Owns the rendezvous slot shared with the worker plus
/// the FIDO-thread-only activity timestamp used for inactivity-based eviction. All
/// presence-poll logic on the FIDO side flows through [`NavThread::poll`].
pub(crate) struct NavThread {
    slot: PresenceSlot,
    /// Refreshed on every matching-fingerprint poll. Inactivity-based eviction in [`poll`]
    /// uses `now - last_polled_at` rather than absolute slot age, so a slow user with a
    /// patient RP doesn't get the modal yanked out from under them.
    last_polled_at: Option<Instant>,
}

impl NavThread {
    /// Create the rendezvous slot, spawn the Nav worker (fixed count, lives for the entire
    /// process), and return the FIDO-side handle.
    pub(crate) fn start() -> Self {
        let slot: PresenceSlot = Arc::new((Mutex::new(PresenceState::Idle), Condvar::new()));
        let worker_slot = Arc::clone(&slot);
        std::thread::spawn(move || nav_loop(worker_slot));
        Self { slot, last_polled_at: None }
    }

    /// Inspects the shared `PresenceState` against a new request's fingerprint and returns
    /// the verdict the caller should hand back to the RP.
    ///
    /// When the slot is Idle, the prior owner has gone silent past `SELECTION_TIMEOUT`, or a
    /// Completed result for a different fingerprint is sitting unclaimed, this evicts and
    /// publishes a fresh `Pending` for the Nav worker.
    ///
    /// Mismatching fingerprints while a prompt is live also get `Pending` — we don't start a
    /// second modal or cancel the live one. A pending slot is only evicted when the RP that
    /// owns it has gone silent for longer than `SELECTION_TIMEOUT` (poll inactivity, not
    /// absolute age) and a different RP is now asking; the GUI-side keep-alive timeout
    /// handles closing an abandoned modal.
    pub(crate) fn poll(&mut self, fingerprint: [u8; 32], options: UserPresenceOptions) -> PresencePoll {
        let now = Instant::now();
        let (mutex, cvar) = &*self.slot;
        let mut guard = mutex.lock().unwrap_or_else(|p| p.into_inner());
        match &*guard {
            PresenceState::Idle => {
                *guard = PresenceState::Pending { fingerprint, options };
                cvar.notify_one();
                self.last_polled_at = Some(now);
                PresencePoll::Pending
            }
            PresenceState::Pending { fingerprint: fp, .. }
            | PresenceState::InProgress { fingerprint: fp }
                if *fp == fingerprint =>
            {
                // Owner still polling — refresh activity so the inactivity-based eviction
                // below treats this as a live flow.
                self.last_polled_at = Some(now);
                PresencePoll::Pending
            }
            PresenceState::Pending { .. } | PresenceState::InProgress { .. } => {
                // A different RP is asking. If the owner has been silent for longer than
                // SELECTION_TIMEOUT, the prompt is presumed wedged (the GUI keep-alive timer
                // would normally have auto-dismissed by now); drop the slot and let this new
                // request start fresh. Any in-flight Nav call will discard its result via
                // the fingerprint guard when it eventually completes.
                let last_polled_at = self.last_polled_at.unwrap_or(now);
                if now.saturating_duration_since(last_polled_at) > SELECTION_TIMEOUT {
                    log::warn!("pending presence not polled for >{}s, evicting", SELECTION_TIMEOUT.as_secs());
                    *guard = PresenceState::Pending { fingerprint, options };
                    cvar.notify_one();
                    self.last_polled_at = Some(now);
                    PresencePoll::Pending
                } else {
                    log::debug!("presence busy for another fingerprint, returning Pending");
                    PresencePoll::Pending
                }
            }
            PresenceState::Completed { fingerprint: fp, present, selected_key_index }
                if *fp == fingerprint =>
            {
                let present = *present;
                let selected_key_index = *selected_key_index;
                *guard = PresenceState::Idle;
                self.last_polled_at = None;
                if present {
                    PresencePoll::Confirmed { selected_key_index }
                } else {
                    PresencePoll::Dismissed
                }
            }
            PresenceState::Completed { .. } => {
                // Result for a previous fingerprint but nobody consumed it.
                log::debug!("pending presence has orphan completion for another fingerprint; evicting");
                *guard = PresenceState::Pending { fingerprint, options };
                cvar.notify_one();
                self.last_polled_at = Some(now);
                PresencePoll::Pending
            }
        }
    }
}

fn nav_loop(slot: PresenceSlot) {
    xous::set_thread_priority(xous::ThreadPriority::AppBackground0).ok();
    let gui_api = GuiApiLight::default();
    let (mutex, cvar) = &*slot;

    loop {
        // Wait for FIDO to publish a Pending request, then take its options and flip the
        // state to InProgress under the same lock so a concurrent FIDO eviction observes a
        // consistent slot.
        let (fingerprint, options) = {
            let mut guard = mutex.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if let PresenceState::Pending { fingerprint, options } = &*guard {
                    let fingerprint = *fingerprint;
                    let options = options.clone();
                    *guard = PresenceState::InProgress { fingerprint };
                    break (fingerprint, options);
                }
                guard = cvar.wait(guard).unwrap_or_else(|p| p.into_inner());
            }
        };

        let outcome = match gui_api.check_user_presence(options) {
            Ok(Some(r)) => (r.present(), r.selected_key_index()),
            Ok(None) => {
                log::warn!("nav: gui returned no result, treating as dismissed");
                (false, None)
            }
            Err(e) => {
                log::warn!("nav: gui IPC failed: {e:?}, treating as dismissed");
                (false, None)
            }
        };

        // Stale-result guard: only commit Completed if the slot is still InProgress for this
        // exact fingerprint. If FIDO has evicted or overwritten with a newer Pending, drop
        // the result on the floor — the next loop iteration will pick up whatever is there.
        let mut guard = mutex.lock().unwrap_or_else(|p| p.into_inner());
        match &*guard {
            PresenceState::InProgress { fingerprint: fp } if *fp == fingerprint => {
                *guard = PresenceState::Completed {
                    fingerprint,
                    present: outcome.0,
                    selected_key_index: outcome.1,
                };
            }
            _ => {
                log::debug!("nav: discarding stale presence result (state moved on)");
            }
        }
    }
}
