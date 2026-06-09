// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use hex_literal::hex;
use server::{CheckedPermissions, MessageAllowed};
use xous::AppId;

use crate::error::NavigationError;
use crate::msg::{
    FinishResponse, GetPendingNavRequest, NavigateTo, NavigationCancel, NavigationResult, RunApp,
    RunAppResponse, ShowModal,
};
use crate::navigation::securitykeys::{SecurityKeysNavRequest, UserPresenceOptions, UserPresenceResult};
use crate::{GuiApi, GuiApiLight, GuiServerError, ModalStyle};

pub mod alerts;
pub mod filepicker;
pub mod lockscreen;
pub mod qrscanner;
pub mod securitykeys;

pub const QR_SCANNER_APP_ID: AppId = AppId(hex!("6775692d6170702d6578616d706c652e"));
pub const FILE_BROWSER_APP_ID: AppId = AppId(hex!("46696c652042726f7773657200000000"));
pub const SECURITY_KEYS_APP_ID: AppId = AppId(hex!("5365637572697479204b657973000000"));
pub const LOCK_SCREEN_APP_ID: AppId = AppId(hex!("0a000000000000000000000000000000"));
pub const ONBOARDING_APP_ID: AppId = AppId(hex!("dac5321775d449c11bc9c90f38067f8f"));
pub const ALERTS_APP_ID: AppId = AppId(hex!("32defc0867555fe8002759667000b22a"));
pub const BITCOIN_APP_ID: AppId = AppId(hex!("426974636f696e2057616c6c65740000"));

impl<P: CheckedPermissions> GuiApiLight<P> {
    /// Shows a modal of the app, giving it a navigation object
    pub fn show_modal(
        &self,
        app_id: AppId,
        modal_style: ModalStyle,
        args: &[u8],
    ) -> Result<NavigationResult, GuiServerError>
    where
        P: MessageAllowed<ShowModal>,
    {
        let nav_req = ShowModal { modal_style, app_id: app_id.0, args: args.to_vec() };
        let response = self.conn.try_send_blocking_archive(nav_req)?;
        Ok(response.or_else(|_| Err(NavigationError::RequestBufferTooSmall)))
    }

    // Switch to the app, giving it a navigation object
    pub fn navigate_to(&self, app_id: AppId, args: &[u8]) -> Result<NavigationResult, GuiServerError>
    where
        P: MessageAllowed<NavigateTo>,
    {
        let nav_req = NavigateTo { app_id: app_id.0, args: args.to_vec() };
        let response = self.conn.try_send_blocking_archive(nav_req)?;
        Ok(response.or_else(|_| Err(NavigationError::RequestBufferTooSmall)))
    }

    pub fn run_app(&self, app_id: AppId) -> Result<RunAppResponse, GuiServerError>
    where
        P: MessageAllowed<RunApp>,
    {
        Ok(self.conn.try_send_blocking_archive(RunApp { app_id: app_id.0 })?)
    }

    pub fn check_user_presence(
        &self,
        options: UserPresenceOptions,
    ) -> Result<Option<UserPresenceResult>, GuiServerError>
    where
        P: MessageAllowed<NavigateTo>,
    {
        let request = SecurityKeysNavRequest::UserPresence(options);
        let res = self.navigate_to(SECURITY_KEYS_APP_ID, &request.serialize())?;

        match res {
            Ok(response) => Ok(UserPresenceResult::from_slice(response.as_slice())),
            Err(NavigationError::CanceledBySystem) | Err(NavigationError::CanceledByUser) => {
                Ok(Some(UserPresenceResult::new_cancelled()))
            }
            Err(e) => Err(GuiServerError::Navigation(e)),
        }
    }

    pub fn invoke_alert(&self, alert: alerts::InvokeAlert) -> Result<alerts::AlertResult, GuiServerError>
    where
        P: MessageAllowed<ShowModal>,
    {
        let res = self.show_modal(ALERTS_APP_ID, ModalStyle::SlideUpFullscreen, &alert.serialize())?;

        match res {
            Ok(response) => Ok(alerts::AlertResult::from_slice(response.as_slice())
                .ok_or(GuiServerError::Navigation(NavigationError::InternalError))?),
            Err(NavigationError::CanceledBySystem) | Err(NavigationError::CanceledByUser) => {
                Ok(alerts::AlertResult::Canceled)
            }
            Err(e) => Err(GuiServerError::Navigation(e)),
        }
    }
}

impl<P: CheckedPermissions> GuiApi<P> {
    pub fn navigate_finish(&self, response: Vec<u8>) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<FinishResponse>,
    {
        let response = FinishResponse { response };
        Ok(self.conn.try_send_blocking_archive(response)?)
    }

    pub fn navigate_cancel(&self) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<NavigationCancel>,
    {
        self.conn.try_send_scalar(NavigationCancel)?;
        Ok(())
    }

    pub fn navigate_pending(&self) -> Result<Option<Vec<u8>>, GuiServerError>
    where
        P: MessageAllowed<GetPendingNavRequest>,
    {
        Ok(self.conn.try_send_blocking_archive(GetPendingNavRequest)?)
    }
}
