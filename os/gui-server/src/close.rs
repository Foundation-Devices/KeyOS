// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use gui_server_api::{consts::CLOSE_TIMEOUT_EXIT_CODE, error::NavigationError, InputMessage};
use log::{debug, error, info, warn};
use server::MessageId as _;
use xous::PID;

use crate::{
    handlers::{CloseAppTimeout, ForceShutdownTimeout},
    AppState, AppWindow, Gui, GuiState,
};

const GRACEFUL_CLOSE_TIMEOUT_MS: usize = 1000;
const FORCE_SHUTDOWN_TIMEOUT_MS: usize = 1000;

impl Gui {
    /// Removes an app window from the GUI and frees associated resources.
    pub(crate) fn on_app_disconnected(&mut self, pid: PID) {
        info!("App with PID={pid} disconnected");

        if !self.windows.contains_key(&pid) {
            debug!("No app with PID={pid} is registered");
            return;
        }

        self.kiosk_policies.remove(&pid);

        if self.shutting_down.is_none() {
            // If the app was a modal, collapse it
            if let GuiState::Modal(modal_state) = &mut self.state {
                if pid == modal_state.modal_pid() {
                    modal_state.respond(Err(NavigationError::ModalExited));
                    let change_to_pid = modal_state.background_pid();
                    self.change_state_single_window(change_to_pid, None);
                }
            }

            // If the app we're closing is active, switch to the launcher immediately
            if self.active_app_pid() == Some(pid) {
                self.change_state_single_window(
                    self.app_registry
                        .launcher_app_pid()
                        .expect("Closed an app when the launcher has not even started"),
                    None,
                );
            }
        }

        if let Some(window) = self.windows.remove(&pid) {
            // Drain the refcount from the actual connection
            while xous::disconnect(window.input_cid).is_ok() {}
        }

        if !self.windows.values().any(|w| matches!(w.state, AppState::Closing)) {
            match &mut self.close_app_callback {
                Some(cb) => {
                    cb.cancel(CloseAppTimeout::ID);
                }
                None => error!("Close app callback not initialized"),
            }
        }

        // If we are still low on memory, continue closing apps.
        #[cfg(keyos)]
        if xous::get_system_stat(xous::SystemStat::IsSystemLowOnMemory).unwrap() != 0 {
            self.close_least_recently_used_app();
        }

        if self.shutting_down.is_some() && self.windows.is_empty() {
            log::info!("All apps closed, shutting down.");
            self.finalize_shutdown();
        }

        self.notify_switcher_app_closed(pid);
        self.app_registry.close_app(pid);
    }

    pub(crate) fn close_app(&mut self, pid: PID) {
        let Some(window) = self.windows.get_mut(&pid) else {
            error!("Can't close app with pid {pid}: there is no associated window.");
            return;
        };
        if matches!(window.state, AppState::Closing | AppState::Terminating) {
            error!("Can't close app with pid {pid}: already closing.");
            return;
        }
        info!("Closing app {} (pid={pid})", window.name);
        window.state = AppState::Closing;
        let msg = xous::Message::new_scalar(InputMessage::CloseRequested as usize, 0, 0, 0, 0);
        if let Err(e) = xous::send_message(window.input_cid, msg) {
            error!("Failed to notify the app (PID {pid}) about being closed: {e:?}");
        }
        match &mut self.close_app_callback {
            Some(cb) => cb.request(GRACEFUL_CLOSE_TIMEOUT_MS, CloseAppTimeout::ID, 0),
            None => error!("Close app callback not initialized"),
        }
    }

    pub(crate) fn close_all_apps(&mut self) {
        // Close the active app first
        if let Some(active_pid) = self.active_app_pid() {
            if let Some(window) = self.windows.get_mut(&active_pid) {
                info!("Closing focused app `{}` (PID {active_pid})", window.name);
                notify_close_app(window, active_pid);
            }
        }

        for (pid, window) in &mut self.windows {
            if matches!(window.state, AppState::Closing | AppState::Terminating) {
                continue;
            }

            info!("Closing app `{}` (PID {pid})", window.name);
            notify_close_app(window, *pid);
        }

        match &mut self.close_app_callback {
            Some(cb) => cb.request(GRACEFUL_CLOSE_TIMEOUT_MS, CloseAppTimeout::ID, 0),
            None => error!("Close app callback not initialized"),
        }
    }

    pub(crate) fn close_least_recently_used_app(&mut self) {
        info!("Trying to close least recently used app");
        let Some((_last_active, pid)) = self
            .windows
            .iter()
            .filter_map(|(pid, window)| {
                if self.app_registry.is_essential_app(*pid) {
                    None
                } else if let AppState::Active { last_activated } = &window.state {
                    Some((*last_activated, *pid))
                } else {
                    None
                }
            })
            .min()
        else {
            error!("Can't kill any apps, no non-essential apps are active");
            return;
        };
        self.close_app(pid);
    }

    pub(crate) fn close_app_timed_out(&mut self) {
        let mut should_force_shutdown = false;
        for (pid, window) in &mut self.windows {
            if let Err(e) = terminate_closing_app(window, *pid) {
                error!("Error terminating `{}` (PID {pid}): {e:?}, forcing shutdown", window.name);
                should_force_shutdown = true;
            }
        }

        if should_force_shutdown {
            match &mut self.close_app_callback {
                Some(cb) => cb.request(FORCE_SHUTDOWN_TIMEOUT_MS, ForceShutdownTimeout::ID, 0),
                None => error!("Close app callback not initialized"),
            }
        }
    }

    pub(crate) fn force_shutdown_timeout(&mut self) {
        info!("Forcing shutdown");
        self.finalize_shutdown();
    }
}

fn terminate_closing_app(window: &mut AppWindow, pid: PID) -> Result<(), xous::Error> {
    if matches!(window.state, AppState::Closing) {
        warn!("Closing `{}` (PID {pid}) timed out, terminating", window.name);
        window.state = AppState::Terminating;
        return xous::terminate_pid(pid, CLOSE_TIMEOUT_EXIT_CODE);
    }

    Ok(())
}

fn notify_close_app(window: &mut AppWindow, pid: PID) {
    window.state = AppState::Closing;
    let msg = xous::Message::new_scalar(InputMessage::CloseRequested as usize, 0, 0, 0, 0);
    if let Err(e) = xous::send_message(window.input_cid, msg) {
        error!("Failed to notify the app `{}` (PID {pid}) about being closed: {e:?}", window.name);
    }
}
