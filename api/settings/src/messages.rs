// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
pub use crate::types::messages::*;

#[derive(Debug, server::Message)]
#[response(global::BoardRevision)]
pub struct GetBoardRevision;

#[derive(Debug, server::Message)]
#[response(global::SystemTheme)]
pub struct GetPrimeColor;

#[derive(Debug, server::Message)]
pub struct FlushAll {
    /// if true, flushes all dirty files regardless of age.
    /// should be called on shutdown.
    pub force: bool,
}

impl server::FromScalar<1> for FlushAll {
    fn from_scalar([force]: [u32; 1]) -> Self { Self { force: force != 0 } }
}

impl server::AsScalar<1> for FlushAll {
    fn as_scalar(&self) -> [u32; 1] { [self.force as u32] }
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(global::TimeZone)]
pub struct LookupTimeZone {
    pub name: String,
    pub offset_minutes: i32,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Vec<global::TimeZone>)]
pub struct ListTimeZone {
    pub offset: Option<u32>,
    pub count: Option<u32>,
}

#[derive(Debug, server::Message)]
#[response(())]
pub struct ResetSettings;
