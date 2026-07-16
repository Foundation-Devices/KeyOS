// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{marker::PhantomData, ops::Deref};

use rkyv::{bytecheck::CheckBytes, rancor};
use xous_ipc::{XousDeserializer, XousValidator};

use crate::WrongMessageTypeError;

pub struct Owned<T> {
    envelope: xous::MessageEnvelope,
    _marker: PhantomData<T>,
}

impl<T> std::fmt::Debug for Owned<T>
where
    T: rkyv::Archive,
    T::Archived: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Owned").field(&*self).finish()
    }
}

impl<T> Owned<T>
where
    T: rkyv::Archive,
    T::Archived: rkyv::Portable,
{
    pub fn new(envelope: xous::MessageEnvelope) -> Result<Self, rancor::Error>
    where
        T::Archived: for<'a> CheckBytes<XousValidator<'a>>,
    {
        match envelope.body.memory_message() {
            Some(mem) => rkyv::api::low::access::<T::Archived, rancor::Error>(as_slice(mem))?,
            None => rancor::fail!(WrongMessageTypeError),
        };

        Ok(Self { envelope, _marker: Default::default() })
    }

    pub fn new_move(envelope: xous::MessageEnvelope) -> Result<Self, rancor::Error>
    where
        T::Archived: for<'a> CheckBytes<XousValidator<'a>>,
    {
        match &envelope.body {
            xous::Message::Move(mem) => rkyv::api::low::access::<T::Archived, rancor::Error>(as_slice(mem))?,
            _ => rancor::fail!(WrongMessageTypeError),
        };

        Ok(Self { envelope, _marker: Default::default() })
    }

    #[inline]
    pub fn deserialize(&self) -> Result<T, rancor::Error>
    where
        T::Archived: rkyv::Deserialize<T, XousDeserializer>,
    {
        rkyv::api::low::deserialize(self.access())
    }

    #[inline]
    pub fn access(&self) -> &T::Archived { unsafe { rkyv::access_unchecked(self.as_slice()) } }
}

impl<T> Owned<T>
where
    T: rkyv::Archive,
    T::Archived: rkyv::Portable,
{
    #[inline]
    fn as_memory_message(&self) -> &xous::MemoryMessage {
        match self.envelope.body.memory_message() {
            Some(mem) => mem,
            None => unreachable!("message was already checked in Owned::new"),
        }
    }

    #[inline]
    pub fn as_slice(&self) -> &[u8] { as_slice(self.as_memory_message()) }
}

impl<T> Deref for Owned<T>
where
    T: rkyv::Archive,
    T::Archived: rkyv::Portable,
{
    type Target = T::Archived;

    #[inline]
    fn deref(&self) -> &Self::Target { self.access() }
}

#[inline]
fn as_slice(mem: &xous::MemoryMessage) -> &[u8] {
    let slice = mem.buf.as_slice();
    let used = mem.offset.map_or(0, |v| v.get()).min(slice.len());
    &slice[..used]
}
