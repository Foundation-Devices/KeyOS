// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
//
use gui_server_api::{
    error::NavigationError,
    msg::ShowModal,
    navigation::{
        filepicker::{SelectFileOptions, SelectFileResult},
        lockscreen::{VerifyPinOptions, VerifyPinResult},
        qrscanner::{ScanQrOptions, ScanQrResult},
        FILE_BROWSER_APP_ID, LOCK_SCREEN_APP_ID, QR_SCANNER_APP_ID,
    },
    GuiServerError, ModalStyle,
};
use server::{CheckedPermissions, MessageAllowed};

use crate::async_archive;

pub fn open_qr_scanner<P>(options: ScanQrOptions) -> Result<Option<ScanQrResult>, GuiServerError>
where
    P: CheckedPermissions + MessageAllowed<ShowModal>,
{
    let msg = ShowModal {
        app_id: QR_SCANNER_APP_ID,
        modal_style: ModalStyle::SlideUpFullscreen,
        args: options.serialize(),
    };

    let res = async_archive::<P, _>(msg).block_on();

    match res {
        Ok(response) => Ok(ScanQrResult::from_slice(response.as_slice())),
        Err(NavigationError::CanceledBySystem) | Err(NavigationError::CanceledByUser) => {
            Ok(Some(ScanQrResult::new_cancelled()))
        }
        Err(e) => Err(GuiServerError::Navigation(e)),
    }
}

pub async fn async_open_qr_scanner<P>(options: ScanQrOptions) -> Result<Option<ScanQrResult>, GuiServerError>
where
    P: CheckedPermissions + MessageAllowed<ShowModal>,
{
    let msg = ShowModal {
        app_id: QR_SCANNER_APP_ID,
        modal_style: ModalStyle::SlideUpFullscreen,
        args: options.serialize(),
    };

    let res = crate::async_archive::<P, _>(msg).await;

    match res {
        Ok(response) => Ok(ScanQrResult::from_slice(response.as_slice())),
        Err(NavigationError::CanceledBySystem) | Err(NavigationError::CanceledByUser) => {
            Ok(Some(ScanQrResult::new_cancelled()))
        }
        Err(e) => Err(GuiServerError::Navigation(e)),
    }
}

pub fn select_file<P>(options: SelectFileOptions) -> Result<Option<SelectFileResult>, GuiServerError>
where
    P: CheckedPermissions + MessageAllowed<ShowModal>,
{
    let msg = ShowModal {
        app_id: FILE_BROWSER_APP_ID,
        modal_style: ModalStyle::SlideUpDraggablePopup,
        args: options.serialize(),
    };

    let res = async_archive::<P, _>(msg).block_on();

    match res {
        Ok(response) => Ok(SelectFileResult::from_slice(response.as_slice())),
        Err(NavigationError::CanceledBySystem) | Err(NavigationError::CanceledByUser) => Ok(None),
        Err(e) => Err(GuiServerError::Navigation(e)),
    }
}

pub fn verify_pin<P>(request: VerifyPinOptions) -> Result<VerifyPinResult, GuiServerError>
where
    P: CheckedPermissions + MessageAllowed<ShowModal>,
{
    let msg = ShowModal {
        app_id: LOCK_SCREEN_APP_ID,
        modal_style: ModalStyle::SlideUpFullscreen,
        args: request.serialize(),
    };

    let res = async_archive::<P, _>(msg).block_on();

    match res {
        Ok(response) => match VerifyPinResult::from_slice(response.as_slice()) {
            Some(result) => Ok(result),
            None => Err(GuiServerError::Navigation(NavigationError::InternalError)),
        },
        Err(NavigationError::CanceledBySystem) | Err(NavigationError::CanceledByUser) => {
            Ok(VerifyPinResult { success: false, security_words: None })
        }
        Err(e) => Err(GuiServerError::Navigation(e)),
    }
}
