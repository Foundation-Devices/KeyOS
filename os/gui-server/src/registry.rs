// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;

use xous::PID;

/// A privileged role an app claims. Each variant has a matching claim message in
/// the gui-server API; the keyboard and control center are not roles, they
/// register through their own messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AppRole {
    Launcher,
    Settings,
    Onboarding,
    Switcher,
    LockScreen,
    Alerts,
}

/// Keeps track of PIDs for special apps.
#[derive(Debug, Default)]
pub(crate) struct AppRegistry {
    map: HashMap<AppRole, PID>,
    pre_lock_app: Option<PID>,
}

impl AppRegistry {
    pub(crate) fn claim(&mut self, role: AppRole, pid: PID) { self.map.insert(role, pid); }

    pub(crate) fn pid(&self, role: AppRole) -> Option<PID> { self.map.get(&role).copied() }

    pub(crate) fn role(&self, pid: PID) -> Option<AppRole> {
        self.map.iter().find_map(|(&k, &v)| if v == pid { Some(k) } else { None })
    }

    pub(crate) fn pre_lock_app_id(&self) -> Option<PID> { self.pre_lock_app }

    pub(crate) fn set_pre_lock_app_pid(&mut self, pid: Option<PID>) { self.pre_lock_app = pid; }

    pub(crate) fn close_app(&mut self, pid: PID) {
        if let Some(role) = self.role(pid) {
            self.map.remove(&role);
        } else if self.pre_lock_app == Some(pid) {
            self.pre_lock_app = None;
        }
    }

    pub(crate) fn is_essential_app(&self, pid: PID) -> bool {
        matches!(
            self.role(pid),
            Some(
                AppRole::Launcher
                    | AppRole::LockScreen
                    | AppRole::Switcher
                    | AppRole::Onboarding
                    | AppRole::Alerts
            )
        )
    }
}
