// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use rgb_led::RgbColor;
use server::{ScalarEventSubscriber, ScalarSubList};

pub struct Implementation {
    subscribers: ScalarSubList<RgbColor>,
}

impl Implementation {
    pub fn init() -> Self { Self { subscribers: ScalarSubList::default() } }

    pub fn subscribe(&mut self, subscriber: ScalarEventSubscriber<RgbColor>) {
        self.subscribers.push(subscriber);
    }

    pub fn set_all(&mut self, color: RgbColor) { self.subscribers.send_nowait(&color); }

    pub fn set(&mut self, _led: u8, color: RgbColor) { self.set_all(color); }
}
