// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! rkyv serialization plumbing and `with` helpers shared across the crate.

use core::cell::RefCell;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rkyv::{
    ser::{
        allocator::{Arena, ArenaHandle},
        writer::Buffer as RkyvBuffer,
        Positional, Serializer, Writer,
    },
    with::{ArchiveWith, DeserializeWith, SerializeWith},
    Archive, Archived, Deserialize, Place, Portable, Serialize,
};

pub type XousDeserializer = rkyv::rancor::Strategy<(), rkyv::rancor::Error>;
pub type XousValidator<'a> = rkyv::api::low::LowValidator<'a, rkyv::rancor::Error>;
pub type XousSerializer<'a, 'b> = rkyv::rancor::Strategy<
    rkyv::ser::Serializer<rkyv::ser::writer::Buffer<'b>, ArenaHandle<'a>, ()>,
    rkyv::rancor::Error,
>;
pub(crate) type SizeOfSerializer<'a> =
    rkyv::rancor::Strategy<rkyv::ser::Serializer<SizeOfWriter, ArenaHandle<'a>, ()>, rkyv::rancor::Error>;

thread_local! {
    static ALLOC_ARENA: RefCell<Arena> = RefCell::new(Arena::new());
}

fn with_arena<R>(f: impl FnOnce(ArenaHandle<'_>) -> R) -> R {
    ALLOC_ARENA.with_borrow_mut(|alloc| f(alloc.acquire()))
}

/// Serialized byte length of `src`, computed without writing it.
pub(crate) fn serialized_size<T>(src: &T) -> core::result::Result<usize, rkyv::rancor::Error>
where
    T: for<'a> rkyv::Serialize<SizeOfSerializer<'a>>,
{
    with_arena(|arena| {
        let mut serializer = Serializer::new(SizeOfWriter::new(), arena, ());
        rkyv::api::serialize_using(src, &mut serializer)?;
        Ok(serializer.pos())
    })
}

/// Serialize `src` into `dst`, returning the number of bytes written.
pub(crate) fn serialize_into<T>(dst: &mut [u8], src: &T) -> core::result::Result<usize, rkyv::rancor::Error>
where
    T: for<'a, 'b> rkyv::Serialize<XousSerializer<'a, 'b>>,
{
    with_arena(|arena| {
        let mut serializer = Serializer::new(RkyvBuffer::from(dst), arena, ());
        rkyv::api::serialize_using(src, &mut serializer)?;
        Ok(serializer.pos())
    })
}

/// Validate and deserialize archived `bytes` into an owned value.
pub(crate) fn decode<T>(bytes: &[u8]) -> core::result::Result<T, rkyv::rancor::Error>
where
    T: rkyv::Archive,
    T::Archived:
        Portable + for<'a> rkyv::bytecheck::CheckBytes<XousValidator<'a>> + Deserialize<T, XousDeserializer>,
{
    let archived = rkyv::api::low::access::<T::Archived, rkyv::rancor::Error>(bytes)?;
    rkyv::api::low::deserialize::<T, rkyv::rancor::Error>(archived)
}

/// A writer that only counts the size of the serialized data without actually writing it.
/// Used to determine buffer size requirements before allocating memory.
#[derive(Debug, Default)]
pub struct SizeOfWriter {
    pos: usize,
}

impl SizeOfWriter {
    pub(crate) fn new() -> Self { Self { pos: 0 } }
}

impl Positional for SizeOfWriter {
    fn pos(&self) -> usize { self.pos }
}

impl rkyv::rancor::Fallible for SizeOfWriter {
    type Error = rkyv::rancor::Error;
}

impl Writer<rkyv::rancor::Error> for SizeOfWriter {
    fn write(&mut self, bytes: &[u8]) -> core::result::Result<(), rkyv::rancor::Error> {
        self.pos += bytes.len();
        Ok(())
    }
}

/// A custom UnixTimestamp implementation that doesn't have an error.
/// making it compatible with infallible error types
pub struct WithUnixTimestamp;

impl ArchiveWith<SystemTime> for WithUnixTimestamp {
    type Archived = Archived<Duration>;
    type Resolver = <Duration as Archive>::Resolver;

    #[inline]
    fn resolve_with(field: &SystemTime, resolver: Self::Resolver, out: Place<Self::Archived>) {
        let duration = field.duration_since(UNIX_EPOCH).unwrap_or_default();
        Archive::resolve(&duration, resolver, out);
    }
}

impl<S> SerializeWith<SystemTime, S> for WithUnixTimestamp
where
    S: rkyv::rancor::Fallible + ?Sized,
{
    fn serialize_with(field: &SystemTime, s: &mut S) -> Result<Self::Resolver, S::Error> {
        let duration = field.duration_since(UNIX_EPOCH).unwrap_or_default();
        duration.serialize(s)
    }
}

impl<D> DeserializeWith<Archived<Duration>, SystemTime, D> for WithUnixTimestamp
where
    D: rkyv::rancor::Fallible + ?Sized,
{
    fn deserialize_with(field: &Archived<Duration>, _: &mut D) -> Result<SystemTime, D::Error> {
        Ok(UNIX_EPOCH + Duration::from(*field))
    }
}

pub struct WithAppId;

impl ArchiveWith<xous::AppId> for WithAppId {
    type Archived = Archived<[u32; 4]>;
    type Resolver = <[u32; 4] as Archive>::Resolver;

    #[inline]
    fn resolve_with(field: &xous::AppId, resolver: Self::Resolver, out: Place<Self::Archived>) {
        let words: [u32; 4] = field.into();
        Archive::resolve(&words, resolver, out);
    }
}

impl<S> SerializeWith<xous::AppId, S> for WithAppId
where
    S: rkyv::rancor::Fallible + ?Sized,
{
    fn serialize_with(field: &xous::AppId, s: &mut S) -> Result<Self::Resolver, S::Error> {
        let words: [u32; 4] = field.into();
        words.serialize(s)
    }
}

impl<D> DeserializeWith<Archived<[u32; 4]>, xous::AppId, D> for WithAppId
where
    D: rkyv::rancor::Fallible + ?Sized,
{
    fn deserialize_with(field: &Archived<[u32; 4]>, d: &mut D) -> Result<xous::AppId, D::Error> {
        let words = field.deserialize(d)?;
        Ok(words.into())
    }
}
