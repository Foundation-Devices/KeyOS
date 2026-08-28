// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::{
    error::NavigationError,
    msg::{LaunchFailureReason, NavigateTo, NavigationResult, RunApp, RunAppResponse, ShowModal},
    navigation::{lockscreen::VerifyPinOptions, LOCK_SCREEN_APP_ID, SETTINGS_APP_ID},
    InputMessage,
};
use log::{debug, error, trace, warn};
use server::ArchiveRequest;
use xous::{AppId, PID};

use crate::{Gui, GuiState, StartupState};

impl Gui {
    pub(crate) fn handle_show_modal_request(&mut self, request: ArchiveRequest<ShowModal>) {
        if self.is_locked() {
            request.response.respond(Err(NavigationError::Locked)).ok();
            return;
        }

        let sender = request.response.pid();
        let sender_app_id = xous::get_app_id(sender).ok().flatten();
        if let Err(error) =
            try_authorize_lockscreen_request(request.message.app_id, &request.message.args, sender_app_id)
        {
            warn!(
                "Rejected modal request for app {:?} from PID {sender} ({sender_app_id:?}): {error}",
                request.message.app_id
            );
            request.response.respond(Err(error)).ok();
            return;
        }

        let Some(pid) = self.launch_app(request.message.app_id) else {
            request.response.respond(Err(NavigationError::AppIdNotFound)).ok();
            return;
        };

        debug!("Created a new modal nav request to PID={} from PID={}", pid, request.response.pid(),);

        self.modal_activate(pid, request);
    }

    pub(crate) fn handle_navigate_to_request(&mut self, mut request: ArchiveRequest<NavigateTo>) {
        if self.is_locked() {
            request.response.respond(Err(NavigationError::Locked)).ok();
            return;
        }

        let sender = request.response.pid();
        let sender_app_id = xous::get_app_id(sender).ok().flatten();
        if let Err(error) =
            try_authorize_lockscreen_request(request.message.app_id, &request.message.args, sender_app_id)
        {
            warn!(
                "Rejected navigation request for app {:?} from PID {sender} ({sender_app_id:?}): {error}",
                request.message.app_id
            );
            request.response.respond(Err(error)).ok();
            return;
        }

        let NavigateTo { app_id, .. } = &request.message;

        let Some(pid) = self.launch_app(*app_id) else {
            request.response.respond(Err(NavigationError::AppIdNotFound)).ok();
            return;
        };

        debug!("Created a new switching nav request to PID={} from PID={}", pid, request.response.pid());
        request.response.set_response(|| Err(NavigationError::CanceledBySystem));
        self.switch_to_window_with_nav(pid, Some(request));
    }

    pub(crate) fn handle_run_app_request(&mut self, request: RunApp) -> RunAppResponse {
        if self.startup_state != StartupState::Started {
            return RunAppResponse::NotReady;
        }

        #[cfg(not(feature = "recovery-os"))]
        if self.is_locked() || self.active_app_role() == Some(crate::registry::AppRole::Onboarding) {
            return RunAppResponse::Locked;
        }

        let app_id = request.app_id;
        let (pid, already_running) = match self.launch_app_with_state(app_id) {
            Ok(result) => result,
            Err(error) => return error,
        };

        self.switch_to_window(pid);

        if already_running {
            RunAppResponse::AlreadyRunning { pid: pid.get() as usize }
        } else {
            RunAppResponse::Launched { pid: pid.get() as usize }
        }
    }

    pub(crate) fn launch_app(&self, app_id: AppId) -> Option<PID> {
        self.launch_app_with_state(app_id).ok().map(|(pid, _)| pid)
    }

    fn launch_app_with_state(&self, app_id: AppId) -> Result<(PID, bool), RunAppResponse> {
        let pid_res = xous::app_id_to_pid(&app_id)
            .map_err(|_| RunAppResponse::LaunchFailed { reason: LaunchFailureReason::Internal })?;
        if let Some(pid) = pid_res {
            return Ok((pid, true));
        }

        // An app that is not already running has to be launched through the app manager, which
        // the recovery image does not have.
        #[cfg(not(feature = "recovery-os"))]
        {
            let pid = self.app_manager.launch_app_blocking(&app_id).map_err(|error| {
                error!("Couldn't launch the app: {error:?}");
                app_manager_launch_failure(error)
            })?;
            Ok((pid, false))
        }
        #[cfg(feature = "recovery-os")]
        {
            log::warn!("No app manager on recovery; cannot launch {app_id:?}");
            Err(RunAppResponse::LaunchFailed { reason: LaunchFailureReason::Internal })
        }
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
                trace!("{response:?}");
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

/// If the navigation request is towards the lock screen, restrict security words to Settings.
fn try_authorize_lockscreen_request(
    destination: AppId,
    args: &[u8],
    caller_app_id: Option<AppId>,
) -> Result<(), NavigationError> {
    if destination != LOCK_SCREEN_APP_ID {
        return Ok(());
    }

    let options = VerifyPinOptions::from_slice(args).ok_or(NavigationError::InvalidRequest)?;
    if options.want_security_words && caller_app_id != Some(SETTINGS_APP_ID) {
        Err(NavigationError::PermissionDenied)
    } else {
        Ok(())
    }
}

#[cfg(not(feature = "recovery-os"))]
fn app_manager_launch_failure(error: app_manager::AppManagerError) -> RunAppResponse {
    let reason = match error {
        app_manager::AppManagerError::UnknownAppId => return RunAppResponse::AppIdNotFound,
        app_manager::AppManagerError::VerificationFailed => LaunchFailureReason::SignatureRejected,
        app_manager::AppManagerError::NoCertificate => LaunchFailureReason::NoCertificate,
        app_manager::AppManagerError::PublisherCertificateExpired => {
            LaunchFailureReason::PublisherCertificateExpired
        }
        app_manager::AppManagerError::PublisherCertificateNotYetActive => {
            LaunchFailureReason::PublisherCertificateNotYetActive
        }
        app_manager::AppManagerError::KeyOsVersionTooOld => LaunchFailureReason::KeyOsVersionTooOld,
        app_manager::AppManagerError::RunningKeyOsVersionUnavailable => {
            LaunchFailureReason::RunningKeyOsVersionUnavailable
        }
        _ => LaunchFailureReason::Internal,
    };
    RunAppResponse::LaunchFailed { reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNTRUSTED_APP_ID: AppId = AppId([0xff; 16]);

    fn pin_options(want_security_words: bool) -> Vec<u8> {
        VerifyPinOptions { title: None, want_security_words }.serialize()
    }

    #[cfg(not(feature = "recovery-os"))]
    #[test]
    fn compatibility_failure_keeps_its_gui_navigation_reason() {
        assert_eq!(
            app_manager_launch_failure(app_manager::AppManagerError::KeyOsVersionTooOld),
            RunAppResponse::LaunchFailed { reason: LaunchFailureReason::KeyOsVersionTooOld }
        );
        assert_eq!(
            app_manager_launch_failure(app_manager::AppManagerError::RunningKeyOsVersionUnavailable),
            RunAppResponse::LaunchFailed { reason: LaunchFailureReason::RunningKeyOsVersionUnavailable }
        );
    }

    #[test]
    fn settings_can_request_pin_verification_with_or_without_security_words() {
        assert!(try_authorize_lockscreen_request(
            LOCK_SCREEN_APP_ID,
            &pin_options(false),
            Some(SETTINGS_APP_ID),
        )
        .is_ok());
        assert!(try_authorize_lockscreen_request(
            LOCK_SCREEN_APP_ID,
            &pin_options(true),
            Some(SETTINGS_APP_ID),
        )
        .is_ok());
    }

    #[test]
    fn other_apps_can_verify_pin_but_cannot_request_security_words() {
        for caller in [Some(UNTRUSTED_APP_ID), None] {
            assert!(try_authorize_lockscreen_request(LOCK_SCREEN_APP_ID, &pin_options(false), caller).is_ok());
            assert!(matches!(
                try_authorize_lockscreen_request(LOCK_SCREEN_APP_ID, &pin_options(true), caller),
                Err(NavigationError::PermissionDenied)
            ));
        }
    }

    #[test]
    fn malformed_lock_screen_requests_are_rejected() {
        assert!(matches!(
            try_authorize_lockscreen_request(LOCK_SCREEN_APP_ID, b"not a request", Some(SETTINGS_APP_ID)),
            Err(NavigationError::InvalidRequest)
        ));
    }

    #[test]
    fn other_destinations_do_not_require_lock_screen_authorization() {
        assert!(try_authorize_lockscreen_request(SETTINGS_APP_ID, b"opaque args", None).is_ok());
    }
}
