// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{ArchiveEventHandler, Owned, ScalarEventHandler, ServerContext};

use crate::registry::AppRole;
use crate::{Gui, StartupState};

const BRIGHTNESS_LEVEL_PERCENT_MIN: u8 = 5;
#[cfg(keyos)]
const BRIGHTNESS_LEVEL_PERCENT_MAX: u8 = 95; // SFT-5361 workaround
#[cfg(not(keyos))]
const BRIGHTNESS_LEVEL_PERCENT_MAX: u8 = 100;

impl ScalarEventHandler<settings::global::ScreenBrightness> for Gui {
    fn handle(
        &mut self,
        msg: settings::global::ScreenBrightness,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        if self.display.is_lcd_on() {
            let brightness = msg.0.clamp(BRIGHTNESS_LEVEL_PERCENT_MIN, BRIGHTNESS_LEVEL_PERCENT_MAX);
            self.display.set_backlight_level_pct(brightness);
            self.rgb_led.set_brightness_pct(brightness);
        }
    }
}

impl ScalarEventHandler<settings::global::TouchOffset> for Gui {
    fn handle(
        &mut self,
        msg: settings::global::TouchOffset,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.touch_state.offset = msg.0;
    }
}

#[cfg(all(keyos, not(feature = "recovery-os")))]
impl ScalarEventHandler<settings::global::DeveloperMode> for Gui {
    fn handle(
        &mut self,
        msg: settings::global::DeveloperMode,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.developer_mode_enabled = msg.0;
    }
}

impl ArchiveEventHandler<settings::global::OnboardingStatus> for Gui {
    fn handle(
        &mut self,
        msg: Owned<settings::global::OnboardingStatus>,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        let Ok(msg) = msg.deserialize() else { return };

        log::info!("Got onboarding state {:?} with startup state {:?}", msg, self.startup_state);
        if self.startup_state == StartupState::InitialLockScreen {
            if msg.is_complete() {
                if let Some(pid) = self.app_registry.pid(AppRole::Launcher) {
                    self.switch_to_window(pid);
                    self.reset_auto_lock();
                    self.notify_lockscreen_unlocked();
                    self.startup_state = StartupState::Started;
                } else {
                    self.startup_state = StartupState::WaitingForLauncherPID;
                }
            } else if let Some(pid) = self.app_registry.pid(AppRole::Onboarding) {
                self.switch_to_window(pid);
                self.startup_state = StartupState::Started;
            } else {
                Self::launch_onboarding();
                self.startup_state = StartupState::WaitingForOnboardingPID;
            }
        }
    }
}

impl ScalarEventHandler<fs::FileSystemEvent> for Gui {
    fn handle(&mut self, msg: fs::FileSystemEvent, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        // There was trouble mounting the User FS, so we won't get the settings::OnboardingStatus notif
        // Assume this is a corrupted filesystem _after_ onboarding, and launch the Launcher, which will
        // handle formatting the partition.
        log::info!("Got invalid FS event: {msg:?}");
        if self.startup_state == StartupState::InitialLockScreen
            && msg.location == fs::Location::AppData
            && msg.event_type == fs::FileSystemEventType::Error
        {
            if let Some(pid) = self.app_registry.pid(AppRole::Launcher) {
                self.switch_to_window(pid);
                self.startup_state = StartupState::Started;
            } else {
                self.startup_state = StartupState::WaitingForLauncherPID;
            }
        }
    }
}
