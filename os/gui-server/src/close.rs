// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use gui_server_api::{
    consts::CLOSE_TIMEOUT_EXIT_CODE,
    error::{GuiServerError, NavigationError},
    InputMessage,
};
use log::{debug, error, info, warn};
use server::MessageId as _;
use xous::PID;

use crate::registry::AppRole;
use crate::{
    handlers::{CloseAppTimeout, ForceShutdownTimeout},
    AppCloseState, AppWindow, Gui, GuiState,
};

const GRACEFUL_CLOSE_TIMEOUT_MS: usize = 1000;
const FORCE_SHUTDOWN_TIMEOUT_MS: usize = 1000;

impl Gui {
    /// Removes an app window from the GUI and frees associated resources.
    pub(crate) fn on_app_disconnected(&mut self, pid: PID) {
        info!("App with PID={pid} disconnected");

        self.app_registry.close_app(pid);

        let Some(window) = self.windows.remove(&pid) else {
            debug!("No app with PID={pid} is registered");
            return;
        };

        if matches!(self.notified_nav_request, Some((p, _)) if p == pid) {
            self.notified_nav_request = None;
        }
        if matches!(self.waiting_for_pid, Some((p, _)) if p == pid) {
            self.waiting_for_pid = None;
        }

        if !window.kiosk_policy.auto_lock_enabled {
            self.reset_auto_lock();
        }

        if self.shutting_down.is_none() {
            // If the modal app died, collapse back to its background; if the background
            // died, exit the modal and go to the launcher (the modal's requestor is gone).
            if let GuiState::Modal(modal_state) = &mut self.state {
                if pid == modal_state.modal_pid() {
                    modal_state.respond(Err(NavigationError::ModalExited));
                    let change_to_pid = modal_state.background_pid();
                    self.change_state_single_window(change_to_pid, None);
                } else if pid == modal_state.background_pid() {
                    self.change_state_single_window(
                        self.app_registry
                            .pid(AppRole::Launcher)
                            .expect("Closed an app when the launcher has not even started"),
                        None,
                    );
                }
            }

            // If the app we're closing is active, switch to the launcher immediately
            if self.active_app_pid() == Some(pid) {
                #[cfg(not(keyos))]
                if let Some(name) = self.windows.get(&pid).map(|w| w.name.clone()) {
                    self.pending_restore_name = Some(name);
                }
                self.change_state_single_window(
                    self.app_registry
                        .pid(AppRole::Launcher)
                        .expect("Closed an app when the launcher has not even started"),
                    None,
                );
            }
        }

        if !self.windows.values().any(|w| matches!(w.close_state, AppCloseState::Closing)) {
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
    }

    pub(crate) fn close_app(&mut self, pid: PID) -> Result<(), GuiServerError> {
        if self.app_registry.is_essential_app(pid) {
            warn!(
                "close_app: rejecting request to close essential app {:?} (pid={pid})",
                self.app_registry.role(pid)
            );
            return Err(GuiServerError::CannotCloseEssentialApp);
        }

        let Some(window) = self.windows.get_mut(&pid) else {
            error!("close_app: no window registered for pid {pid}");
            return Err(GuiServerError::AppNotFound);
        };
        if matches!(window.close_state, AppCloseState::Closing | AppCloseState::Terminating) {
            error!("Can't close app with pid {pid}: already closing.");
            return Err(GuiServerError::AppAlreadyClosing);
        }
        info!("Closing app {} (pid={pid})", window.name);
        window.close_state = AppCloseState::Closing;
        let msg = xous::Message::new_scalar(InputMessage::CloseRequested as usize, 0, 0, 0, 0);
        if let Err(e) = xous::send_message(window.input_cid, msg) {
            error!("Failed to notify the app (PID {pid}) about being closed: {e:?}");
        }
        match &mut self.close_app_callback {
            Some(cb) => cb.request(GRACEFUL_CLOSE_TIMEOUT_MS, CloseAppTimeout::ID, 0),
            None => error!("Close app callback not initialized"),
        }

        Ok(())
    }

    pub(crate) fn close_all_apps(&mut self) {
        // With no windows registered, neither finalize path ever runs: on_app_disconnected
        // (the graceful one) only fires when a window disconnects, and close_app_timed_out
        // only force-shuts-down when a window fails to terminate. Arming the close timeout
        // here would just leave the simulator running on its close button, so finalize now.
        if self.windows.is_empty() {
            self.finalize_shutdown();
            return;
        }

        for (pid, window) in &mut self.windows {
            if matches!(window.close_state, AppCloseState::Closing | AppCloseState::Terminating) {
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
                if self.app_registry.is_essential_app(*pid)
                    || Some(*pid) == self.active_app_pid()
                    || Some(*pid) == self.modal_background_pid()
                {
                    None
                } else {
                    Some((window.last_active, *pid))
                }
            })
            .min()
        else {
            error!("Can't kill any apps, no non-essential apps are active");
            return;
        };
        let _ = self.close_app(pid);
    }

    pub(crate) fn close_app_timed_out(&mut self) {
        let mut should_force_shutdown = false;
        for (pid, window) in &mut self.windows {
            if let Err(e) = terminate_closing_app(window, *pid) {
                error!("Error terminating `{}` (PID {pid}): {e:?}, forcing shutdown", window.name);
                should_force_shutdown = true;
            }
        }

        if self.shutting_down.is_some() && should_force_shutdown {
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
    if matches!(window.close_state, AppCloseState::Closing) {
        warn!("Closing `{}` (PID {pid}) timed out, terminating", window.name);
        window.close_state = AppCloseState::Terminating;
        return xous::terminate_pid(pid, CLOSE_TIMEOUT_EXIT_CODE);
    }

    Ok(())
}

fn notify_close_app(window: &mut AppWindow, pid: PID) {
    window.close_state = AppCloseState::Closing;
    let msg = xous::Message::new_scalar(InputMessage::CloseRequested as usize, 0, 0, 0, 0);
    if let Err(e) = xous::send_message(window.input_cid, msg) {
        error!("Failed to notify the app `{}` (PID {pid}) about being closed: {e:?}", window.name);
    }
}
