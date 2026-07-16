// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{AppWindow, Gui},
    log::{info, warn},
    xous::PID,
};

camera::use_api!();

#[derive(Default)]
pub(crate) struct CameraWindow {
    #[allow(dead_code)]
    pub(crate) latest_frame: Option<camera::Frame>,
    pub(crate) notified_visible: bool,
    pub(crate) api: CameraApi,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CameraState {
    pub(crate) y_pos: u16,
    state: CameraVisibilityState,
}

#[derive(Debug, Copy, Clone, Default)]
pub(crate) enum CameraVisibilityState {
    #[default]
    Hidden,
    Showing,
}

impl AppWindow {
    pub(crate) fn is_camera_visible(&self) -> bool {
        matches!(self.camera_state.state, CameraVisibilityState::Showing)
    }
}

impl Gui {
    pub(crate) fn show_camera_for_app(&mut self, pid: PID) {
        let Some(window) = self.windows.get_mut(&pid) else {
            warn!("Requested to show camera for PID={pid} but no window found");
            return;
        };

        info!("Requested to show camera by PID={pid}");
        window.camera_state.state = CameraVisibilityState::Showing;
        self.update_camera_window();
    }

    pub(crate) fn hide_camera_for_app(&mut self, pid: PID) {
        let Some(window) = self.windows.get_mut(&pid) else {
            warn!("Requested to hide camera for PID={pid} but no window found");
            return;
        };

        info!("Requested to hide the camera by PID={pid}");
        window.camera_state.state = CameraVisibilityState::Hidden;
        self.update_camera_window();
    }

    pub(crate) fn update_camera_window(&mut self) {
        let Some(pid) = self.active_app_pid() else { return };
        let Some(window) = self.windows.get_mut(&pid) else { return };

        let visible = window.is_camera_visible();
        if visible != self.camera_window.notified_visible {
            self.camera_window.api.notify_visible(visible);
            self.camera_window.notified_visible = visible;
        }
    }

    pub(crate) fn camera_window_notify_hidden(&mut self) {
        if self.camera_window.notified_visible {
            self.camera_window.api.notify_visible(false);
            self.camera_window.notified_visible = false;
        }
    }
}
