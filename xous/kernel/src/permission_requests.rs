// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

//! In-flight permission requests, keyed by their slot index in the `entries` table.
//!
//! When a send is blocked by the connection's permission mask, the kernel records the
//! request here (including the target server identity resolved *at that moment*), parks
//! the sender, and broadcasts only the request id to the permission broker. The broker
//! fetches the data by id and answers by id.
//!
//! The grant always lands on the exact connection and server the user was asked about
//! because the entry is invalidated the moment that identity could change: when the
//! connection is disconnected or re-pointed, the target server is destroyed, or either
//! process exits, the kernel tombstones the request and wakes its parked sender with an
//! error. The entry then lingers only to reserve its slot (so a prompt still on screen cannot
//! resolve a reused id); when the broker finally answers it, the slot is freed and nothing is
//! woken. A grantable (never-tombstoned) request therefore always still has its sender
//! parked, so the resolve path needs no re-validation of the connection or server. Because a
//! slot is freed only when the broker answers it, its index is a safe id: a new request can
//! reuse the slot only after the broker is done with the previous one.

use xous::{AppId, MessageId, CID, PID, SID, TID};

use crate::process::current_pid;

/// In-flight permission prompts are bounded by parked sender threads, and realistically by
/// how many prompts a user can have pending; a full table denies the send outright.
const MAX_PERMISSION_REQUESTS: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct PermissionRequest {
    pub sender_pid: PID,
    /// The sender's app id, captured at park time. The broker keys the persisted grant on this
    /// immutable identity rather than re-deriving it from `sender_pid`, whose process may have
    /// exited and had its pid recycled by the time the broker resolves the prompt.
    pub sender_app_id: AppId,
    pub sender_tid: TID,
    pub cid: CID,
    pub sidx: usize,
    pub server_sid: SID,
    pub message_id: MessageId,
    /// Whether the request may still be granted. Cleared when the request is invalidated (its
    /// connection was disconnected or re-pointed, its server destroyed, or a participating
    /// process exited): that already woke the sender with an error, so an invalid entry only
    /// lingers to reserve its slot and grants nothing when finally answered.
    pub valid: bool,
}

pub struct PermissionRequests {
    entries: [Option<PermissionRequest>; MAX_PERMISSION_REQUESTS],
}

impl PermissionRequests {
    pub const fn new() -> Self { Self { entries: [None; MAX_PERMISSION_REQUESTS] } }

    /// Record an in-flight request and return its id (its slot index), or `None` when the
    /// table is full.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        sender_app_id: AppId,
        sender_tid: TID,
        cid: CID,
        sidx: usize,
        server_sid: SID,
        message_id: MessageId,
    ) -> Option<u16> {
        let slot = self.entries.iter().position(|entry| entry.is_none())?;
        self.entries[slot] = Some(PermissionRequest {
            sender_pid: current_pid(),
            sender_app_id,
            sender_tid,
            cid,
            sidx,
            server_sid,
            message_id,
            valid: true,
        });
        Some(slot as u16)
    }

    pub fn get(&self, id: u16) -> Option<&PermissionRequest> { self.entries.get(id as usize)?.as_ref() }

    /// Remove and return the request, freeing its slot for reuse.
    pub fn take(&mut self, id: u16) -> Option<PermissionRequest> { self.entries.get_mut(id as usize)?.take() }

    /// Invalidate the next still-valid request matching `pred`, returning the parked sender to
    /// release (its pid, tid, and request id), or `None` once none remain. The caller wakes
    /// each returned sender with an error, so a request whose connection, server, or process
    /// went away is released promptly instead of blocking until a resolve that could only deny
    /// it. Loop over it to drain every match.
    pub fn tombstone_next(&mut self, pred: impl Fn(&PermissionRequest) -> bool) -> Option<(PID, TID, u16)> {
        let (slot, entry) = self.entries.iter_mut().enumerate().find_map(|(slot, entry)| {
            let entry = entry.as_mut()?;
            (entry.valid && pred(entry)).then_some((slot, entry))
        })?;
        entry.valid = false;
        Some((entry.sender_pid, entry.sender_tid, slot as u16))
    }
}
