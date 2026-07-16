// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use tiny_skia::{Color, ColorU8};

use crate::IS_DARK;

pub const SHADOW_RADIUS: u32 = 3;
pub const SHADOW_OFFSET_Y: usize = 2;
pub const SHADOW_OPACITY_DARK: u16 = 204;
pub const SHADOW_OPACITY_LIGHT: u16 = 128;

#[cfg(keyos)]
macro_rules! color {
    ($r:literal, $g:literal, $b:literal) => {
        ColorU8::from_rgba($b, $g, $r, 255)
    };
}

#[cfg(not(keyos))]
macro_rules! color {
    ($r:literal, $g:literal, $b:literal) => {
        ColorU8::from_rgba($r, $g, $b, 255)
    };
}

pub struct KeyColors {
    pub text: ColorU8,
    pub background: ColorU8,
    pub gradient_top: ColorU8,
    pub gradient_bottom: ColorU8,
}

pub struct ColorScheme {
    pub normal: KeyColors,
    pub accent: KeyColors,
    pub cta: KeyColors,
    pub disabled: KeyColors,
}

const COLORS_DARK: ColorScheme = ColorScheme {
    normal: KeyColors {
        text: color!(0xFF, 0xFF, 0xFF),
        background: color!(0x23, 0x1F, 0x20),
        gradient_top: color!(0x44, 0x44, 0x44),
        gradient_bottom: color!(0x23, 0x1F, 0x20),
    },
    accent: KeyColors {
        text: color!(0xFF, 0xFF, 0xFF),
        background: color!(0x44, 0x44, 0x44),
        gradient_top: color!(0x5A, 0x5A, 0x5A),
        gradient_bottom: color!(0x23, 0x1F, 0x20),
    },
    cta: KeyColors {
        text: color!(0xFF, 0xFF, 0xFF),
        background: color!(0x00, 0x9D, 0xB9),
        gradient_top: color!(0x33, 0xB7, 0xC1),
        gradient_bottom: color!(0x00, 0x9D, 0xB9),
    },
    disabled: KeyColors {
        text: color!(0x94, 0x94, 0x94),
        background: color!(0x44, 0x44, 0x44),
        gradient_top: color!(0x5A, 0x5A, 0x5A),
        gradient_bottom: color!(0x23, 0x1F, 0x20),
    },
};

const COLORS_LIGHT: ColorScheme = ColorScheme {
    normal: KeyColors {
        text: color!(0x23, 0x1F, 0x20),
        background: color!(0xFF, 0xFF, 0xFF),
        gradient_top: color!(0x94, 0x94, 0x94),
        gradient_bottom: color!(0x23, 0x1F, 0x20),
    },
    accent: KeyColors {
        text: color!(0x23, 0x1F, 0x20),
        background: color!(0xD5, 0xD5, 0xD5),
        gradient_top: color!(0x94, 0x94, 0x94),
        gradient_bottom: color!(0x23, 0x1F, 0x20),
    },
    cta: KeyColors {
        text: color!(0xFF, 0xFF, 0xFF),
        background: color!(0x00, 0x9D, 0xB9),
        gradient_top: color!(0x33, 0xB7, 0xC1),
        gradient_bottom: color!(0x00, 0x9D, 0xB9),
    },
    disabled: KeyColors {
        text: color!(0xC1, 0xC1, 0xC1),
        background: color!(0xD5, 0xD5, 0xD5),
        gradient_top: color!(0x94, 0x94, 0x94),
        gradient_bottom: color!(0x23, 0x1F, 0x20),
    },
};

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStyle {
    #[default]
    Normal,
    Accent,
    Cta, // Call To Action
    Disabled,
}

impl KeyStyle {
    pub fn colors(&self) -> &KeyColors {
        let colors =
            if IS_DARK.load(std::sync::atomic::Ordering::SeqCst) { &COLORS_DARK } else { &COLORS_LIGHT };
        match self {
            KeyStyle::Normal => &colors.normal,
            KeyStyle::Accent => &colors.accent,
            KeyStyle::Cta => &colors.cta,
            KeyStyle::Disabled => &colors.disabled,
        }
    }
}

pub fn to_color(c: ColorU8) -> Color { Color::from_rgba8(c.red(), c.green(), c.blue(), c.alpha()) }
