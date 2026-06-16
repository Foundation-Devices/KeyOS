// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Custom rkyv serialization helpers for KeyOS

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rkyv::{
    with::{ArchiveWith, DeserializeWith, SerializeWith},
    Archive, Archived, Deserialize, Place, Serialize,
};

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
