// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::thread;
use std::time::Duration;

#[cfg(keyos)]
mod atsama5d2;
#[cfg(keyos)]
use atsama5d2::Implementation;

#[cfg(not(keyos))]
mod hosted;
#[cfg(not(keyos))]
use hosted::Implementation;
#[cfg(not(keyos))]
use rgb_led::messages::SubscribeColorUpdates;
use rgb_led::{messages::*, RgbColor};
#[cfg(not(keyos))]
use server::{ScalarEventSubscriber, ScalarEventSubscriptionHandler};
use server::{ScalarHandler, Server, ServerContext};
use xous::PID;

const ANIMATION_DELAY_MS: usize = 50;

#[derive(server::Server)]
#[name = "os/rgb-server"]
pub struct RgbServer {
    implementation: Implementation,
    current_color: RgbColor,
}

impl RgbServer {
    pub fn new() -> Self { Self { implementation: Implementation::init(), current_color: RgbColor::BLACK } }
}

impl Server for RgbServer {}

impl ScalarHandler<SetAllTo> for RgbServer {
    fn handle(&mut self, SetAllTo(color): SetAllTo, _sender: PID, _context: &mut ServerContext<Self>) {
        self.implementation.set_all(color);
        self.current_color = color;
    }
}

impl ScalarHandler<SetTo> for RgbServer {
    fn handle(&mut self, SetTo(index, color): SetTo, _sender: PID, _context: &mut ServerContext<Self>) {
        self.implementation.set(index as u8, color)
    }
}

#[cfg(not(keyos))]
impl ScalarEventSubscriptionHandler<SubscribeColorUpdates> for RgbServer {
    fn handle(
        &mut self,
        _msg: SubscribeColorUpdates,
        subscriber: ScalarEventSubscriber<RgbColor>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.implementation.subscribe(subscriber);
        Ok(())
    }
}

impl ScalarHandler<AnimateAllTo> for RgbServer {
    fn handle(
        &mut self,
        AnimateAllTo(animation): AnimateAllTo,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        log::trace!("Animating {animation:?}");

        for i in (0..animation.duration_ms).step_by(ANIMATION_DELAY_MS) {
            let color = animation.from.lerp(animation.to, i as f32 / animation.duration_ms as f32);
            self.implementation.set_all(color);
            thread::sleep(Duration::from_millis(ANIMATION_DELAY_MS as u64));
        }
        self.implementation.set_all(animation.to);
        thread::sleep(Duration::from_millis(ANIMATION_DELAY_MS as u64));
        if animation.reset {
            self.implementation.set_all(self.current_color);
        } else {
            self.current_color = animation.to;
        }
    }
}

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::System3).unwrap();

    server::listen(RgbServer::new())
}
