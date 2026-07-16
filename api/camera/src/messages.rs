// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::{Frame, SubscriptionError};

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(Frame)]
#[error(SubscriptionError)]
pub struct Subscribe;

#[derive(Debug, server::Message)]
pub struct SetEnabled(pub bool);

#[derive(Debug, server::Message)]
pub struct NotifyVisible(pub bool);

#[derive(Debug, server::Message)]
#[response(bool)]
pub struct IsEnabled;

#[derive(Debug, server::Message)]
#[response(bool)]
pub struct IsInUse;

/// Get current camera parameters
#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(CameraParams)]
pub struct GetParams;

/// Set camera parameters (wraps [`CameraParams`])
#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct SetParams(pub CameraParams);

/// Camera parameters struct
#[derive(Debug, Clone, Copy, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct CameraParams {
    /// Auto control flags: bit0=AEC, bit1=AWB, bit2=AGC
    pub auto_controls: u8,
    /// AGC ceiling: 0=2x, 1=4x, 2=8x, 3=16x
    pub agc_ceiling: u8,
    /// Brightness: 0-255, default: 0
    pub brightness: u8,
    /// Contrast: 0-255
    pub contrast: u8,
    /// Saturation: 0-255
    pub saturation: u8,
    /// Sharpness: 0-31 (manual mode only)
    pub sharpness: u8,
    /// Denoise: 0-255 (manual mode only)
    pub denoise: u8,
    /// Auto sharpness enabled
    pub auto_sharpness: bool,
    /// Auto denoise enabled
    pub auto_denoise: bool,
}

impl Default for CameraParams {
    fn default() -> Self { Self::DEFAULT }
}

impl CameraParams {
    pub const DEFAULT: Self = Self {
        auto_controls: 0x07,   // AEC+AWB+AGC all enabled
        agc_ceiling: 1,        // 4x max gain
        brightness: 0,         // Default brightness
        contrast: 0x28,        // Default contrast (40)
        saturation: 0x50,      // Default saturation (80)
        sharpness: 0x04,       // Default sharpness (4) per datasheet
        denoise: 0x08,         // Default denoise (8) per datasheet
        auto_sharpness: false, // Manual mode by default
        auto_denoise: false,   // Manual mode by default
    };
}
