// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::{msg, AppKind};
use server::{BlockingArchive, BlockingArchiveHandler, ServerContext};
use xous::PID;

use crate::Gui;

impl BlockingArchiveHandler<msg::RegisterAppMessage> for Gui {
    fn handle(
        &mut self,
        msg::RegisterAppMessage(reg): msg::RegisterAppMessage,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <msg::RegisterAppMessage as BlockingArchive>::Response {
        match reg.app_kind {
            AppKind::App => self.handle_register_app(sender, reg),
            AppKind::ControlCenter => self.handle_register_control_center_app(sender, reg),
            AppKind::Keyboard => self.handle_register_keyboard_app(sender, reg),
            AppKind::Launcher => self.handle_register_launcher_app(sender, reg),
            AppKind::Settings => self.handle_register_settings_app(sender, reg),
            AppKind::Onboarding => self.handle_register_onboarding_app(sender, reg),
            AppKind::Switcher => self.handle_register_switcher_app(sender, reg),
            AppKind::LockScreen => self.handle_register_lock_screen_app(sender, reg),
            AppKind::Alerts => self.handle_register_alerts_app(sender, reg),
        }
    }
}
