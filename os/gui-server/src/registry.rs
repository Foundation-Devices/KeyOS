// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;

use gui_server_api::AppKind;
use xous::PID;

/// Keeps track of PIDs for special apps.
#[derive(Debug, Default)]
pub(crate) struct AppRegistry {
    map: HashMap<AppKind, PID>,
    pre_lock_app: Option<PID>,
}

impl AppRegistry {
    pub fn lock_screen_pid(&self) -> Option<PID> { self.map.get(&AppKind::LockScreen).copied() }

    pub fn launcher_app_pid(&self) -> Option<PID> { self.map.get(&AppKind::Launcher).copied() }

    pub fn settings_app_pid(&self) -> Option<PID> { self.map.get(&AppKind::Settings).copied() }

    pub fn onboarding_app_pid(&self) -> Option<PID> { self.map.get(&AppKind::Onboarding).copied() }

    pub fn switcher_app_pid(&self) -> Option<PID> { self.map.get(&AppKind::Switcher).copied() }

    pub fn pre_lock_app_id(&self) -> Option<PID> { self.pre_lock_app }

    pub fn alerts_app_pid(&self) -> Option<PID> { self.map.get(&AppKind::Alerts).copied() }

    pub fn set_lock_screen_pid(&mut self, pid: PID) { self.set_app(pid, AppKind::LockScreen); }

    pub fn set_launcher_app_pid(&mut self, pid: PID) { self.set_app(pid, AppKind::Launcher); }

    pub fn set_settings_app_pid(&mut self, pid: PID) { self.set_app(pid, AppKind::Settings); }

    pub fn set_onboarding_app_pid(&mut self, pid: PID) { self.set_app(pid, AppKind::Onboarding); }

    pub fn set_switcher_app_pid(&mut self, pid: PID) { self.set_app(pid, AppKind::Switcher); }

    pub fn set_pre_lock_app_pid(&mut self, pid: Option<PID>) { self.pre_lock_app = pid; }

    pub fn set_alerts_app_pid(&mut self, pid: Option<PID>) {
        if let Some(pid) = pid {
            self.set_app(pid, AppKind::Alerts);
        } else {
            self.map.remove(&AppKind::Alerts);
        }
    }

    pub(crate) fn is_lock_screen_app(&self, pid: PID) -> bool {
        self.app_by_pid(pid) == Some(AppKind::LockScreen)
    }

    pub(crate) fn is_switcher_app(&self, pid: PID) -> bool { self.app_by_pid(pid) == Some(AppKind::Switcher) }

    pub(crate) fn is_launcher_app(&self, pid: PID) -> bool { self.app_by_pid(pid) == Some(AppKind::Launcher) }

    pub(crate) fn close_app(&mut self, pid: PID) {
        if let Some(app) = self.app_by_pid(pid) {
            self.map.remove(&app);
        } else if self.pre_lock_app == Some(pid) {
            self.pre_lock_app = None;
        }
    }

    pub(crate) fn is_essential_app(&self, pid: PID) -> bool {
        matches!(
            self.app_by_pid(pid),
            Some(
                AppKind::Launcher
                    | AppKind::LockScreen
                    | AppKind::Switcher
                    | AppKind::Onboarding
                    | AppKind::Alerts
            )
        )
    }

    fn set_app(&mut self, pid: PID, app: AppKind) { self.map.insert(app, pid); }

    fn app_by_pid(&self, pid: PID) -> Option<AppKind> {
        self.map.iter().find_map(|(&k, &v)| if v == pid { Some(k) } else { None })
    }
}
