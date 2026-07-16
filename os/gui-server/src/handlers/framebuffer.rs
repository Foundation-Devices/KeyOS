// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::msg::SubmitFrame;
use server::{MoveHandler, ServerContext};

use crate::Gui;

impl MoveHandler<SubmitFrame> for Gui {
    const LEAK_MESSAGE: bool = true;

    fn handle(&mut self, msg: SubmitFrame, sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.update_vsync_states();
        self.handle_submit_frame(msg, sender);
    }
}
