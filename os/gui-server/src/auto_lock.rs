// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use server::{ScalarEventHandler, ScalarHandler, ServerContext};
use xous_ticktimer::TicktimerCallback;

#[cfg(not(feature = "recovery-os"))]
use crate::handlers::AutoLockStep;
use crate::{animation::AnimationCompleteAction, handlers::AutoLockTimerCallback, Gui};

// This is only used when `settings` is not available, i.e. in recovery mode
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(600);

// TODO (SFT-5047): This should be configurable
#[cfg(not(feature = "recovery-os"))]
const DIM_TIMEOUT: Duration = Duration::from_secs(45);

// Poweroff timeout = lock timeout * POWEROFF_MULTIPLIER
// TODO (SFT-5047): This should be configurable
#[cfg(all(keyos, not(feature = "recovery-os")))]
const POWEROFF_MULTIPLIER: u32 = 2;

#[cfg(not(feature = "recovery-os"))]
const DIM_TIMEOUT_LOCKED: Duration = Duration::from_secs(10);
#[cfg(all(keyos, not(feature = "recovery-os")))]
const LCD_OFF_TIMEOUT_LOCKED: Duration = Duration::from_secs(15);

// Onboarding auto-shutdown timeout of inactivity.
#[cfg(all(keyos, not(feature = "recovery-os")))]
const ONBOARDING_AUTO_SHUTDOWN_TIMEOUT: Duration = Duration::from_hours(1);

// Timeout value that's considered "Never". The actual value in the duration is (-1 as u64), but that's
// way too big to represent as an u32 milliseconds, so we check for this threshold instead.
#[cfg(not(feature = "recovery-os"))]
const MAX_TIMEOUT: Duration = Duration::from_secs(24 * 3600);

pub struct AutoLockState {
    lock_timeout: Duration,
    callback: Option<TicktimerCallback>,
}

impl Default for AutoLockState {
    fn default() -> Self { Self { lock_timeout: DEFAULT_LOCK_TIMEOUT, callback: None } }
}

impl Gui {
    pub(crate) fn init_auto_lock(&mut self, context: &mut ServerContext<Gui>) {
        #[cfg(not(feature = "recovery-os"))]
        self.settings.server_subscribe_auto_lock(context);
        self.auto_lock.callback = Some(xous_ticktimer::TicktimerCallback::new(context.sid()).unwrap());
    }

    pub(crate) fn reset_auto_lock(&mut self) -> bool {
        let screen_was_dimmed = self.display.is_dimmed() && self.display.is_lcd_on();
        if screen_was_dimmed {
            self.animate_backlight_to(self.screen_brightness_setting(), AnimationCompleteAction::None);
            self.rgb_led.turn_on();
        }
        #[cfg(not(feature = "recovery-os"))]
        {
            // If the onboarding app is running, we don't want to dim the screen
            #[cfg(keyos)]
            if self.is_onboarding_running() {
                self.auto_lock.request_callback(ONBOARDING_AUTO_SHUTDOWN_TIMEOUT, AutoLockStep::PowerOff);
                return false;
            }

            let timeout = if self.is_locked() { DIM_TIMEOUT_LOCKED } else { DIM_TIMEOUT };
            self.auto_lock.request_callback(timeout, AutoLockStep::Dim);
        }
        screen_was_dimmed
    }
}

#[cfg(not(feature = "recovery-os"))]
impl AutoLockState {
    fn request_callback(&self, timeout: Duration, step: AutoLockStep) {
        use server::MessageId as _;

        if timeout > MAX_TIMEOUT {
            return;
        }
        self.callback.as_ref().unwrap().request(
            timeout.as_millis() as usize,
            AutoLockTimerCallback::ID,
            step as usize,
        );
    }
}

impl ScalarEventHandler<settings::global::AutoLock> for Gui {
    fn handle(
        &mut self,
        msg: settings::global::AutoLock,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        log::debug!("Changing auto-lock timeout to {:?}", msg.0);
        self.auto_lock.lock_timeout = msg.0;
    }
}

impl ScalarHandler<AutoLockTimerCallback> for Gui {
    fn handle(&mut self, msg: AutoLockTimerCallback, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        #[cfg(not(keyos))]
        log::debug!("Not reacting to {msg:?} in hosted mode");
        #[cfg(feature = "recovery-os")]
        {
            log::debug!("Not reacting to {msg:?} in recovery os mode");
            return;
        }

        #[cfg(all(keyos, not(feature = "recovery-os")))]
        match msg.0 {
            AutoLockStep::Dim => {
                if self.display.is_lcd_on() && !self.display.is_dimmed() {
                    log::debug!("Dimming LCD");
                    self.rgb_led.turn_off();
                    self.animate_backlight_to(
                        self.screen_brightness_setting_dimmed(),
                        crate::AnimationCompleteAction::LcdDim,
                    );
                }
                let timeout = if self.is_locked() {
                    LCD_OFF_TIMEOUT_LOCKED.saturating_sub(DIM_TIMEOUT_LOCKED)
                } else {
                    self.auto_lock.lock_timeout.saturating_sub(DIM_TIMEOUT)
                };
                self.auto_lock.request_callback(timeout, AutoLockStep::LcdOff);
            }
            AutoLockStep::LcdOff => {
                if !self.auto_lock_enabled() {
                    log::debug!("Auto-lock skipped (kiosk policy)");
                } else if self.display.is_lcd_on() {
                    log::info!("Turning LCD off (no activity)");
                    self.lock();
                    self.turn_off_lcd();
                }
                self.auto_lock.request_callback(
                    self.auto_lock.lock_timeout.saturating_mul(POWEROFF_MULTIPLIER - 1),
                    AutoLockStep::PowerOff,
                );
            }
            AutoLockStep::PowerOff => {
                if !self.auto_lock_enabled() {
                    log::debug!("Auto-shutdown skipped (kiosk policy)");
                    self.auto_lock.request_callback(self.auto_lock_timeout(), AutoLockStep::PowerOff);
                    return;
                }
                use power_manager::ChargeStatus;
                let power_manager = crate::PowerManagerExtApi::default();
                match power_manager.status().unwrap().charge_status {
                    ChargeStatus::Charging | ChargeStatus::ChargeDone => {
                        log::debug!("Not shutting down yet, we are on a charger");
                        self.auto_lock.request_callback(self.auto_lock_timeout(), AutoLockStep::PowerOff);
                    }
                    ChargeStatus::Idle | ChargeStatus::Boosting | ChargeStatus::Fault => {
                        log::info!("Shutting down (no activity)");
                        self.shutdown(false);
                    }
                }
            }
        }
    }
}

impl Gui {
    #[cfg(all(keyos, not(feature = "recovery-os")))]
    fn auto_lock_timeout(&self) -> Duration {
        if self.is_onboarding_running() {
            return ONBOARDING_AUTO_SHUTDOWN_TIMEOUT;
        }

        self.auto_lock.lock_timeout
    }
}
