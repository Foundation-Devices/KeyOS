// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::{
    error::NavigationError,
    msg::{NavigateTo, NavigationResult, ShowModal},
    InputMessage,
};
use log::{debug, error, warn};
use server::ArchiveRequest;
use xous::{AppId, PID};

use crate::{AppManagerApi, Gui, GuiState};

impl Gui {
    pub(crate) fn handle_show_modal_request(&mut self, request: ArchiveRequest<ShowModal>) {
        if self.active_app_pid() == self.app_registry.lock_screen_pid() {
            request.response.respond(Err(NavigationError::Locked)).ok();
            return;
        }

        let Some(pid) = self.launch_app(AppId(request.message.app_id)) else {
            request.response.respond(Err(NavigationError::AppIdNotFound)).ok();
            return;
        };

        debug!("Created a new modal nav request to PID={} from PID={}", pid, request.response.pid(),);

        self.modal_activate(pid, request);
    }

    pub(crate) fn handle_navigate_to_request(&mut self, mut request: ArchiveRequest<NavigateTo>) {
        if self.active_app_pid() == self.app_registry.lock_screen_pid() {
            request.response.respond(Err(NavigationError::Locked)).ok();
            return;
        }
        let NavigateTo { app_id, .. } = &request.message;

        let Some(pid) = self.launch_app(AppId(*app_id)) else {
            request.response.respond(Err(NavigationError::AppIdNotFound)).ok();
            return;
        };

        debug!("Created a new switching nav request to PID={} from PID={}", pid, request.response.pid());
        request.response.set_response(|| Err(NavigationError::CanceledBySystem));
        self.switch_to_window_with_nav(pid, Some(request));
    }

    fn launch_app(&self, app_id: AppId) -> Option<PID> {
        let mut pid_res = xous::app_id_to_pid(&app_id).ok()?;
        let app_already_running = pid_res.is_some();
        if !app_already_running {
            let app_manager_api = AppManagerApi::default();
            pid_res = app_manager_api
                .launch_app_blocking(&app_id)
                .inspect_err(|e| error!("Couldn't launch the app: {e:?}"))
                .ok();
        }

        pid_res
    }

    pub(crate) fn respond_to_nav_request(&mut self, response: NavigationResult) {
        match &mut self.state {
            GuiState::Modal(modal_state) => {
                modal_state.respond(response);
            }
            GuiState::Switching { navigation_request, .. }
            | GuiState::SingleWindow { navigation_request, .. }
                if navigation_request.is_some() =>
            {
                let request = core::mem::take(navigation_request).unwrap();
                let _ = request.response.respond(response);
                self.notified_nav_request = None;
            }
            _ => {
                warn!("Response got while no navigation present");
                debug!("{response:?}");
            }
        }
    }

    pub(crate) fn get_pending_nav_request(&self) -> Option<Vec<u8>> {
        match &self.state {
            GuiState::Modal(modal_state) if self.windows.contains_key(&modal_state.modal_pid()) => {
                modal_state.get_navigation_request().map(|a| a.to_owned())
            }
            GuiState::Switching { navigation_request, .. }
            | GuiState::SingleWindow { navigation_request, .. } => {
                navigation_request.as_ref().map(|r| r.message.args.clone())
            }
            _ => None,
        }
    }

    pub(crate) fn send_navigation_focused_event(&self, pid: PID) {
        if let Some(window) = self.windows.get(&pid) {
            let msg = xous::Message::new_scalar(InputMessage::NavigationFocused as usize, 0, 0, 0, 0);
            xous::send_message(window.input_cid, msg)
                .map_err(|e| error!("Failed to notify the app (PID {pid}) about being navigated to: {e:?}"))
                .ok();
        } else {
            error!("Can't notify navigation, no app window with PID={pid} is known");
        }
    }

    pub(crate) fn send_navigation_cancelled_event(&self, pid: PID) {
        if let Some(window) = self.windows.get(&pid) {
            let msg = xous::Message::new_scalar(InputMessage::NavigationCancelled as usize, 0, 0, 0, 0);
            xous::send_message(window.input_cid, msg)
                .map_err(|e| error!("Failed to notify the app (PID {pid}) about navigation cancel: {e:?}"))
                .ok();
        } else {
            error!("Can't notify navigation cancel, no app window with PID={pid} is known");
        }
    }

    pub(crate) fn update_navigation_request_state(&mut self) {
        let previous_pid = self.notified_nav_request.as_ref().map(|n| n.0);
        let previous_nav_request = self.notified_nav_request.as_ref().map(|n| &n.1);
        let current_pid = self.active_app_pid();
        let current_nav_request = self.get_pending_nav_request();
        if previous_pid != current_pid || previous_nav_request != current_nav_request.as_ref() {
            if let Some(previous_active_pid) = previous_pid {
                if previous_nav_request.is_some() {
                    self.send_navigation_cancelled_event(previous_active_pid);
                }
            }
            if let Some(active_pid) = current_pid {
                if current_nav_request.is_some() {
                    self.send_navigation_focused_event(active_pid);
                }
            }
        }
        if let Some(pid) = current_pid
            && let Some(nav_request) = current_nav_request
        {
            self.notified_nav_request = Some((pid, nav_request))
        } else {
            self.notified_nav_request = None;
        }
    }
}
