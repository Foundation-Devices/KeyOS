// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::msg;
use server::{BlockingArchive, BlockingArchiveHandler, BlockingScalarHandler, ServerContext};
use xous::PID;

use crate::{registry::AppRole, Gui};

impl BlockingArchiveHandler<msg::RegisterAppMessage> for Gui {
    fn handle(
        &mut self,
        msg::RegisterAppMessage(reg): msg::RegisterAppMessage,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <msg::RegisterAppMessage as BlockingArchive>::Response {
        self.handle_register_app(sender, reg)
    }
}

impl BlockingArchiveHandler<msg::RegisterControlCenter> for Gui {
    fn handle(
        &mut self,
        reg: msg::RegisterControlCenter,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <msg::RegisterControlCenter as BlockingArchive>::Response {
        self.handle_register_control_center_app(sender, reg.cid, reg.height)
    }
}

impl BlockingArchiveHandler<msg::RegisterKeyboard> for Gui {
    fn handle(
        &mut self,
        reg: msg::RegisterKeyboard,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <msg::RegisterKeyboard as BlockingArchive>::Response {
        self.handle_register_keyboard_app(sender, reg.cid, reg.height)
    }
}

macro_rules! impl_claim_handler {
    ($msg:ident, $role:ident) => {
        impl BlockingScalarHandler<msg::$msg> for Gui {
            fn handle(&mut self, _msg: msg::$msg, sender: PID, _context: &mut ServerContext<Self>) {
                self.app_registry.claim(AppRole::$role, sender);
            }
        }
    };
}

impl_claim_handler!(ClaimLauncherRole, Launcher);
impl_claim_handler!(ClaimSettingsRole, Settings);
impl_claim_handler!(ClaimOnboardingRole, Onboarding);
impl_claim_handler!(ClaimSwitcherRole, Switcher);
impl_claim_handler!(ClaimLockScreenRole, LockScreen);
impl_claim_handler!(ClaimAlertsRole, Alerts);
