// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::msg::{HideCamera, ShowCamera};
use server::{ScalarHandler, ServerContext};
use xous::PID;

use crate::Gui;

impl ScalarHandler<ShowCamera> for Gui {
    #[cfg(not(feature = "recovery-os"))]
    fn handle(&mut self, msg: ShowCamera, sender: PID, _context: &mut ServerContext<Self>) {
        let Some(app_window) = self.windows.get_mut(&sender) else {
            log::warn!("PID={sender} requested to show camera while no window was registered");
            return;
        };

        log::debug!("Showing camera for PID={} @ y={}", sender, msg.y_pos);

        app_window.camera_state.y_pos = msg.y_pos;
        self.show_camera_for_app(sender);
    }

    #[cfg(feature = "recovery-os")]
    fn handle(&mut self, _msg: ShowCamera, sender: PID, _context: &mut ServerContext<Self>) {
        log::error!("Show camera received in recovery-mode from PID={sender}");
    }
}

impl ScalarHandler<HideCamera> for Gui {
    #[cfg(not(feature = "recovery-os"))]
    fn handle(&mut self, _msg: HideCamera, sender: PID, _context: &mut ServerContext<Self>) {
        self.hide_camera_for_app(sender);
    }

    #[cfg(feature = "recovery-os")]
    fn handle(&mut self, _msg: HideCamera, sender: PID, _context: &mut ServerContext<Self>) {
        log::error!("Show camera received in recovery-mode from PID={sender}");
    }
}

#[cfg(not(feature = "recovery-os"))]
impl server::ScalarEventHandler<camera::Frame> for Gui {
    fn handle(&mut self, msg: camera::Frame, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.camera_window.latest_frame = Some(msg);
        self.update_layers();
    }
}
