// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{
    btree_map::{Entry, VacantEntry},
    BTreeMap,
};

use server::xous;

use super::{CancelToken, CONTROL_START};

const CANCEL_BITS: u32 = 1;
const CANCEL_MASK: u32 = (1 << CANCEL_BITS) - 1;
const MAX_EPOCH: u32 = ((CONTROL_START - 1) >> CANCEL_BITS) as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotKey(u32);

impl SlotKey {
    pub fn new(epoch: u32) -> Self {
        debug_assert!(epoch <= MAX_EPOCH);
        Self(epoch << CANCEL_BITS)
    }

    pub fn from_msg_id(msg_id: xous::MessageId) -> Option<Self> {
        if msg_id >= CONTROL_START {
            return None;
        }
        Some(Self(u32::try_from(msg_id).ok()?))
    }

    pub fn msg_id(self) -> xous::MessageId { self.0 as usize }

    pub fn cancel_msg_id(self) -> xous::MessageId { (self.0 | CANCEL_MASK) as usize }

    pub fn epoch(self) -> u32 { self.0 >> CANCEL_BITS }

    pub fn is_cancel(self) -> bool { self.0 & CANCEL_MASK != 0 }
}

#[derive(Debug)]
pub struct SlotMap {
    entries: BTreeMap<u32, Slot>,
    next_epoch: u32,
}

#[derive(Debug)]
pub enum Slot {
    Request(RequestSlot),
    Subscription(SubscriptionSlot),
}

impl Slot {
    pub fn pid(&self) -> xous::PID {
        match self {
            Slot::Request(req) => req.pid,
            Slot::Subscription(sub) => sub.pid,
        }
    }
}

#[derive(Debug)]
pub struct SubscriptionSlot {
    pub tx: async_channel::Sender<xous::MessageEnvelope>,
    pub pid: xous::PID,
    pub cancel_token: CancelToken,
}

#[derive(Debug)]
pub struct RequestSlot {
    pub tx: Option<oneshot::Sender<Result<xous::MessageEnvelope, xous::Error>>>,
    pub pid: xous::PID,
    pub cancel_token: CancelToken,
}

impl SlotMap {
    pub fn new() -> Self { Self { entries: BTreeMap::new(), next_epoch: 1 } }

    pub fn reserve(&mut self) -> ReservedSlot<'_> {
        let entries = &mut self.entries as *mut BTreeMap<u32, Slot>;
        let start = self.next_epoch;
        for epoch in (start..=MAX_EPOCH).chain(1..start) {
            self.next_epoch = if epoch == MAX_EPOCH { 1 } else { epoch + 1 };
            // SAFETY: loop iterations are disjoint
            // works around rust-lang/rust#70255
            if let Entry::Vacant(entry) = unsafe { (&mut *entries).entry(epoch) } {
                return ReservedSlot { key: SlotKey::new(*entry.key()), entry };
            }
        }
        panic!("exhausted slot epochs");
    }

    pub fn disconnect_pid(&mut self, pid: xous::PID) {
        self.entries.retain(|_, entry| match entry {
            Slot::Request(req) if req.pid == pid => {
                if let Some(tx) = req.tx.take() {
                    let _ = tx.send(Err(xous::Error::ProcessTerminated));
                }
                false
            }
            Slot::Subscription(sub) if sub.pid == pid => false,
            _ => true,
        });
    }

    pub fn remove_cancel_token(&mut self, token: CancelToken) -> bool {
        self.entries
            .iter()
            .find(|(_, slot)| match slot {
                Slot::Request(req) => req.cancel_token == token,
                Slot::Subscription(sub) => sub.cancel_token == token,
            })
            .map(|(epoch, _)| *epoch)
            .inspect(|epoch| {
                self.entries.remove(epoch);
            })
            .is_some()
    }

    pub fn get_slot(&mut self, slot_key: SlotKey) -> Option<&mut Slot> {
        self.entries.get_mut(&slot_key.epoch())
    }

    pub fn free_key(&mut self, slot_key: SlotKey) { self.entries.remove(&slot_key.epoch()); }
}

pub struct ReservedSlot<'a> {
    pub key: SlotKey,
    entry: VacantEntry<'a, u32, Slot>,
}

impl ReservedSlot<'_> {
    pub fn insert(self, slot: Slot) { self.entry.insert(slot); }
}
