// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::msg::ShowControlCenter;
use server::{ScalarHandler, ServerContext};
use xous::PID;

use crate::Gui;

impl ScalarHandler<ShowControlCenter> for Gui {
    fn handle(&mut self, msg: ShowControlCenter, sender: PID, _context: &mut ServerContext<Self>) {
        if let Some(window) = self.windows.get_mut(&sender) {
            window.display_control_center = msg.0;
        }
    }
}
