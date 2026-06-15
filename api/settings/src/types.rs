// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//!
//! Global settings data types
//!
//! Each global setting has a Get/Set/Subscribe method on [`crate::SettingsApi`].

use std::time::Duration;

use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use server::{AsScalar, FromScalar};

use crate::macros::*;

create_modules! {
    SystemTheme,
    ScreenBrightness,
    OnboardingStatus,
    Locale,
    DeviceName,
    ShowSecurityWords,
    AutoLock,
    EnvoyTimeSync,
    UseStandardTimeFormat,
    TimeZone,
    DebugTouch,
    AirlockMode,
    TouchOffset,
    MagicBackupEnabled,
    NfcEnabled,
    BluetoothEnabled,
    CameraEnabled,
    UsbEnabled,
    DeveloperMode,
}

global_scalar! {
    /// System-wide theme.
    ///
    /// This affects the appearance of all system apps on KeyOS
    system,
    #[derive(FromPrimitive, ToPrimitive)]
    pub enum SystemTheme {
        Dark,
        Light,
    }
}

impl FromScalar<1> for SystemTheme {
    fn from_scalar(value: [u32; 1]) -> Self { Self::from_u32(value[0]).unwrap() }
}

impl AsScalar<1> for SystemTheme {
    fn as_scalar(&self) -> [u32; 1] { [self.to_u32().unwrap()] }
}

global_scalar! {
    /// Screen brightness level (0-100)
    system,
    pub struct ScreenBrightness(pub u8);
}

impl FromScalar<1> for ScreenBrightness {
    fn from_scalar(value: [u32; 1]) -> Self { Self(value[0] as u8) }
}

impl AsScalar<1> for ScreenBrightness {
    fn as_scalar(&self) -> [u32; 1] { [self.0 as u32] }
}

global_archive! {
    /// Status of the user's onboarding.
    encrypted,
    #[derive(Default)]
    pub enum OnboardingStatus {
        #[default]
        NotComplete,
        Complete,
    }
}

impl OnboardingStatus {
    pub fn is_complete(&self) -> bool { matches!(self, Self::Complete) }
}

impl ArchivedOnboardingStatus {
    pub fn is_complete(&self) -> bool { matches!(self, Self::Complete) }
}

global_scalar! {
    /// The user's preferred language.
    system,
    #[derive(Default, FromPrimitive, ToPrimitive)]
    pub enum Locale {
        #[default]
        EnglishUS,
    }
}

impl FromScalar<1> for Locale {
    fn from_scalar(value: [u32; 1]) -> Self { Self::from_u32(value[0]).unwrap() }
}

impl AsScalar<1> for Locale {
    fn as_scalar(&self) -> [u32; 1] { [self.to_u32().unwrap()] }
}

impl Locale {
    pub fn lang(&self) -> &str {
        match self {
            Self::EnglishUS => "en",
        }
    }
}

global_archive! {
    /// The user's device name.
    system,
    pub struct DeviceName(pub String);
}

impl From<&str> for DeviceName {
    fn from(value: &str) -> Self { Self(String::from(value)) }
}

impl DeviceName {
    pub const DEFAULT: &str = "Passport Prime";
}

global_scalar! {
    /// Whether to show the security words on the login screen.
    system,
    pub struct ShowSecurityWords(pub bool);
}

impl FromScalar<1> for ShowSecurityWords {
    fn from_scalar(value: [u32; 1]) -> Self { Self(bool::from_scalar(value)) }
}

impl AsScalar<1> for ShowSecurityWords {
    fn as_scalar(&self) -> [u32; 1] { self.0.as_scalar() }
}

global_scalar! {
    /// Time to lock the device after inactivity.
    system,
    pub struct AutoLock(pub Duration);
}

impl FromScalar<2> for AutoLock {
    fn from_scalar(value: [u32; 2]) -> Self {
        Self(Duration::from_secs(value[0] as u64) + Duration::from_millis(value[1] as u64))
    }
}

impl AsScalar<2> for AutoLock {
    fn as_scalar(&self) -> [u32; 2] { [self.0.as_secs() as u32, self.0.subsec_millis()] }
}

global_scalar! {
    /// Whether to set time from envoy via QuantumLink
    system,
    pub struct EnvoyTimeSync(pub bool);
}

impl FromScalar<1> for EnvoyTimeSync {
    fn from_scalar(value: [u32; 1]) -> Self { Self(bool::from_scalar(value)) }
}

impl AsScalar<1> for EnvoyTimeSync {
    fn as_scalar(&self) -> [u32; 1] { self.0.as_scalar() }
}

global_scalar! {
    /// Whether to use the standard time format (24-hour).
    system,
    pub struct UseStandardTimeFormat(pub bool);
}

impl FromScalar<1> for UseStandardTimeFormat {
    fn from_scalar(value: [u32; 1]) -> Self { Self(bool::from_scalar(value)) }
}

impl AsScalar<1> for UseStandardTimeFormat {
    fn as_scalar(&self) -> [u32; 1] { self.0.as_scalar() }
}

global_archive! {
    /// The user's preferred timezone.
    system,
    pub struct TimeZone {
         pub name: String,
         pub data: Vec<u8>
    }
}

impl Default for TimeZone {
    fn default() -> Self {
        Self {
            name: String::from("America/New_York"),
            data: include_bytes!("../assets/america_new_york.tzif").to_vec(),
        }
    }
}

impl TimeZone {
    pub fn now(&self) -> jiff::Zoned { jiff::Timestamp::now().to_zoned(self.timezone()) }

    pub fn timezone(&self) -> jiff::tz::TimeZone {
        jiff::tz::TimeZone::tzif(&self.name, &self.data).unwrap_or_else(|_| TimeZone::default().timezone())
    }

    pub fn name(&self) -> &str { &self.name }

    pub fn abbreviation(&self, timestamp: jiff::Timestamp) -> String {
        let tz = self.timezone();
        let info = tz.to_offset_info(timestamp);
        info.abbreviation().into()
    }
}

global_scalar! {
    system,
    pub struct DebugTouch(pub bool);
}

impl FromScalar<1> for DebugTouch {
    fn from_scalar(value: [u32; 1]) -> Self { Self(bool::from_scalar(value)) }
}

impl AsScalar<1> for DebugTouch {
    fn as_scalar(&self) -> [u32; 1] { self.0.as_scalar() }
}

global_scalar! {
    system,
    #[derive(FromPrimitive, ToPrimitive)]
    pub enum AirlockMode {
        Disabled,
        ReadOnly,
        ReadWrite,
    }
}

impl FromScalar<1> for AirlockMode {
    fn from_scalar(value: [u32; 1]) -> Self { Self::from_u32(value[0]).unwrap() }
}

impl AsScalar<1> for AirlockMode {
    fn as_scalar(&self) -> [u32; 1] { [self.to_u32().unwrap()] }
}

global_scalar! {
    system,
    pub struct TouchOffset(pub i32);
}

impl FromScalar<1> for TouchOffset {
    fn from_scalar(value: [u32; 1]) -> Self { Self(i32::from_scalar(value)) }
}

impl AsScalar<1> for TouchOffset {
    fn as_scalar(&self) -> [u32; 1] { self.0.as_scalar() }
}

global_scalar! {
    system,
    pub struct MagicBackupEnabled(pub bool);
}

global_scalar! {
    system,
    pub struct NfcEnabled(pub bool);
}

global_scalar! {
    system,
    pub struct BluetoothEnabled(pub bool);
}

global_scalar! {
    system,
    pub struct CameraEnabled(pub bool);
}

global_scalar! {
    system,
    pub struct UsbEnabled(pub bool);
}

global_scalar! {
    encrypted,
    pub struct DeveloperMode(pub bool);
}

impl FromScalar<1> for MagicBackupEnabled {
    fn from_scalar(value: [u32; 1]) -> Self { Self(bool::from_scalar(value)) }
}

impl AsScalar<1> for MagicBackupEnabled {
    fn as_scalar(&self) -> [u32; 1] { self.0.as_scalar() }
}

impl FromScalar<1> for NfcEnabled {
    fn from_scalar(value: [u32; 1]) -> Self { Self(bool::from_scalar(value)) }
}

impl AsScalar<1> for NfcEnabled {
    fn as_scalar(&self) -> [u32; 1] { self.0.as_scalar() }
}

impl FromScalar<1> for BluetoothEnabled {
    fn from_scalar(value: [u32; 1]) -> Self { Self(bool::from_scalar(value)) }
}

impl AsScalar<1> for BluetoothEnabled {
    fn as_scalar(&self) -> [u32; 1] { self.0.as_scalar() }
}

impl FromScalar<1> for CameraEnabled {
    fn from_scalar(value: [u32; 1]) -> Self { Self(bool::from_scalar(value)) }
}

impl AsScalar<1> for CameraEnabled {
    fn as_scalar(&self) -> [u32; 1] { self.0.as_scalar() }
}

impl FromScalar<1> for UsbEnabled {
    fn from_scalar(value: [u32; 1]) -> Self { Self(bool::from_scalar(value)) }
}

impl AsScalar<1> for UsbEnabled {
    fn as_scalar(&self) -> [u32; 1] { self.0.as_scalar() }
}

impl FromScalar<1> for DeveloperMode {
    fn from_scalar(value: [u32; 1]) -> Self { Self(bool::from_scalar(value)) }
}

impl AsScalar<1> for DeveloperMode {
    fn as_scalar(&self) -> [u32; 1] { self.0.as_scalar() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timezone() {
        let tz = TimeZone::default();
        assert_eq!(tz.name(), "America/New_York");
        let timezone = tz.timezone();
        assert_eq!(timezone.iana_name(), Some("America/New_York"));
    }
}
