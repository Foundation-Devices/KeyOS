// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use app_manager::{
    PermissionGrantDecision, PermissionRequestInfo, PermissionRequestInfoResult, SetAppPermissionGrantResult,
};
use gui_server_api::msg::NavigationResult;
use gui_server_api::navigation::alerts::{AlertResult, InvokeAlert};
use gui_server_api::navigation::ALERTS_APP_ID;
use gui_server_api::ModalStyle;
use log::{error, warn};

use crate::modal::ModalRequest;
use crate::{AppManagerApi, Gui, GuiState};

impl Gui {
    /// Resolve a newly parked permission request against the current grant state. One that is
    /// already allowed or denied is answered on the spot; only one that needs the user's
    /// decision is queued and driven onto the screen.
    pub(crate) fn queue_permission_request(&mut self, request_id: u16) {
        // An already-decided request is answered here even while locked; this is how a grant
        // approved in Settings after the app connected reaches its connection.
        if resolve_or_prompt_info(&self.app_manager, request_id).is_none() {
            return;
        }
        self.pending_permission_requests.push_back(request_id);
        self.show_next_permission_prompt();
    }

    /// Put the next queued permission prompt on screen once the display can host one; queued
    /// prompts follow one another through the modal state machine.
    pub(crate) fn show_next_permission_prompt(&mut self) {
        // A prompt sits over a single foreground window (the background the modal restores)
        // and never over the lock screen; otherwise leave the queue for the next
        // `change_state` retry, so a locked device drains it after unlock.
        if self.is_locked() {
            return;
        }
        let background_pid = match &self.state {
            GuiState::SingleWindow { pid, .. } => *pid,
            _ => return,
        };

        while let Some(request_id) = self.pending_permission_requests.pop_front() {
            // Re-resolve at show time: the subgroup may have been decided while this request
            // waited, and one prompt per subgroup means it is then answered silently.
            let Some(info) = resolve_or_prompt_info(&self.app_manager, request_id) else {
                continue;
            };
            let Some(modal_pid) = self.launch_app(ALERTS_APP_ID) else {
                warn!("permission prompt: alerts app is unavailable");
                resolve_permission(request_id, false);
                continue;
            };

            // TODO(SFT-7229): localize the prompt strings (and the subgroup labels they name).
            let alert = InvokeAlert {
                app_title: None,
                title: "Permission Request".to_string(),
                icon: "alert".to_string(),
                // The app name is attacker-controlled; cap it so it can't overflow the
                // fixed-height prompt. The subgroup label is OS-owned and bounded.
                line1: format!("{} wants permission: {}.", cap_app_name(&info.app_name), info.label),
                line2: Some("Allow this permission?".to_string()),
                button1_title: "Allow Always".to_string(),
                button2_title: Some("Not Now".to_string()),
                button3_title: Some("Never Allow".to_string()),
            };
            let prompt = PendingPermissionPrompt {
                request_id: Some(request_id),
                args: alert.serialize(),
                app_manager: self.app_manager.clone(),
                info,
            };
            self.modal_activate_inner(
                modal_pid,
                background_pid,
                ModalStyle::SlideUpFixedPopup,
                ModalRequest::Permission(prompt),
            );
            return;
        }
    }
}

/// A permission prompt shown to the user. Dropping it unanswered (modal replaced, alerts app
/// crash) still resolves the parked send with a denial, so the sender can never be left parked
/// in the kernel forever.
pub(crate) struct PendingPermissionPrompt {
    request_id: Option<u16>,
    info: PermissionRequestInfo,
    args: Vec<u8>,
    app_manager: AppManagerApi,
}

impl std::fmt::Debug for PendingPermissionPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `AppManagerApi` is a bare connection handle with no useful Debug; skip it.
        f.debug_struct("PendingPermissionPrompt")
            .field("request_id", &self.request_id)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl PendingPermissionPrompt {
    /// The serialized [`InvokeAlert`] handed to the alerts app as its navigation request.
    pub(crate) fn args(&self) -> &[u8] { &self.args }

    /// Apply the user's choice and resolve the parked send.
    pub(crate) fn finish(mut self, result: NavigationResult) {
        let Some(request_id) = self.request_id.take() else { return };
        let alert_result = match &result {
            Ok(response) => AlertResult::from_slice(response.as_slice()).unwrap_or(AlertResult::Canceled),
            Err(_) => AlertResult::Canceled,
        };
        match alert_result {
            AlertResult::Button1Pressed => self.record_decision(request_id, PermissionGrantDecision::Allow),
            AlertResult::Button3Pressed => self.record_decision(request_id, PermissionGrantDecision::Deny),
            AlertResult::Button2Pressed => {
                self.record_decision(request_id, PermissionGrantDecision::DenyForRun)
            }
            AlertResult::Canceled => resolve_permission(request_id, false),
        }
    }

    /// Send the user's decision to the app manager, then resolve the parked send: allowed only
    /// when the user chose `Allow` and the grant was recorded.
    fn record_decision(&self, request_id: u16, decision: PermissionGrantDecision) {
        let result = self.app_manager.set_app_permission_grant(
            &format!("0x{}", self.info.app_id),
            &self.info.subgroup,
            decision,
        );
        let approved = if result == SetAppPermissionGrantResult::Updated {
            decision == PermissionGrantDecision::Allow
        } else {
            warn!("failed to record permission decision {decision:?} for {:?}: {result:?}", self.info);
            false
        };
        resolve_permission(request_id, approved);
    }
}

impl Drop for PendingPermissionPrompt {
    fn drop(&mut self) {
        if let Some(request_id) = self.request_id.take() {
            warn!("permission prompt for request {request_id} dropped unanswered; denying");
            resolve_permission(request_id, false);
        }
    }
}

fn resolve_permission(request_id: u16, allow: bool) {
    if let Err(err) = xous::resolve_message_permission(request_id, allow) {
        error!("failed to resolve permission request {request_id}: {err:?}");
    }
}

/// Resolve `request_id` against the current grant state: a request that is already allowed or
/// denied (or gone) is answered here and yields `None`; `Some` carries the prompt info for one
/// that needs the user's decision. The sender's app id was captured at park time rather than
/// re-derived from a possibly recycled pid, so the prompt and the eventual grant always
/// describe the same recorded target.
fn resolve_or_prompt_info(app_manager: &AppManagerApi, request_id: u16) -> Option<PermissionRequestInfo> {
    let (Ok(data), Ok(sender_app_id)) =
        (xous::get_permission_request_data(request_id), xous::get_permission_request_app_id(request_id))
    else {
        // The sender was tombstoned before we drained the event; the kernel keeps the slot
        // until a resolve takes it, so deny to free it rather than leak it from the table.
        warn!("permission request {request_id} vanished before it was handled; denying to free its slot");
        resolve_permission(request_id, false);
        return None;
    };

    match app_manager.get_permission_request_info(
        sender_app_id.0,
        data.server_sid.to_array(),
        data.message_id,
        "en",
    ) {
        PermissionRequestInfoResult::AlreadyApproved => {
            resolve_permission(request_id, true);
            None
        }
        PermissionRequestInfoResult::Denied
        | PermissionRequestInfoResult::NotGrantable
        | PermissionRequestInfoResult::AppNotFound
        | PermissionRequestInfoResult::Unauthorized
        | PermissionRequestInfoResult::InternalError => {
            resolve_permission(request_id, false);
            None
        }
        PermissionRequestInfoResult::Prompt(info) => Some(info),
    }
}

/// Truncate an app name (counted in characters, not bytes, so UTF-8 is never split) to a bound
/// that fits the prompt, appending an ellipsis when clipped.
fn cap_app_name(name: &str) -> String {
    const MAX_CHARS: usize = 24;
    let mut chars = name.chars();
    let capped: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{capped}…")
    } else {
        capped
    }
}
