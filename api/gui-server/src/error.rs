// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{AsScalar, FromScalar};

#[derive(Debug, Default, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum NavigationError {
    #[default]
    #[error("Internal error")]
    InternalError,

    #[error("Request buffer is too small for the response")]
    RequestBufferTooSmall,

    #[error("An other PID tried to navigate there at the same time (PID={0})")]
    ConcurrentNavigationRequest(xous::PID),

    #[error("Couldn't find an running app with the given AppId")]
    AppIdNotFound,

    #[error("No pending navigation request")]
    NoNavigationRequest,

    #[error("Modal app exited unexpectedly")]
    ModalExited,

    #[error("The screen is locked")]
    Locked,

    #[error("The navigation was cancelled by the system")]
    CanceledBySystem,

    #[error("The navigation was cancelled by the user")]
    CanceledByUser,
}

impl AsScalar<3> for NavigationError {
    fn as_scalar(&self) -> [u32; 3] {
        match self {
            NavigationError::InternalError => [1, 0, 0],
            NavigationError::RequestBufferTooSmall => [2, 0, 0],
            NavigationError::ConcurrentNavigationRequest(pid) => [3, pid.get() as u32, 0],
            NavigationError::AppIdNotFound => [4, 0, 0],
            NavigationError::NoNavigationRequest => [5, 0, 0],
            NavigationError::ModalExited => [6, 0, 0],
            NavigationError::Locked => [7, 0, 0],
            NavigationError::CanceledBySystem => [8, 0, 0],
            NavigationError::CanceledByUser => [9, 0, 0],
        }
    }
}

impl FromScalar<3> for NavigationError {
    fn from_scalar([kind, arg, _]: [u32; 3]) -> Self {
        match kind {
            1 => NavigationError::InternalError,
            2 => NavigationError::RequestBufferTooSmall,
            3 => u8::try_from(arg)
                .ok()
                .and_then(xous::PID::new)
                .map(NavigationError::ConcurrentNavigationRequest)
                .unwrap_or(NavigationError::InternalError),
            4 => NavigationError::AppIdNotFound,
            5 => NavigationError::NoNavigationRequest,
            6 => NavigationError::ModalExited,
            7 => NavigationError::Locked,
            8 => NavigationError::CanceledBySystem,
            9 => NavigationError::CanceledByUser,
            _ => NavigationError::InternalError,
        }
    }
}

#[derive(Debug, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum GuiServerError {
    #[error("Unknown internal error")]
    InternalError,

    #[error("OS error: {:?}", xous::Error::from_usize(*.0))]
    XousError(usize),

    #[error("navigation error: {0:?}")]
    Navigation(NavigationError),

    #[error("Cannot close an essential app")]
    CannotCloseEssentialApp,

    #[error("No app window is registered for that PID")]
    AppNotFound,

    #[error("The app is already closing")]
    AppAlreadyClosing,
}

impl From<xous::Error> for GuiServerError {
    fn from(e: xous::Error) -> Self { GuiServerError::XousError(e.to_usize()) }
}

impl AsScalar<3> for GuiServerError {
    fn as_scalar(&self) -> [u32; 3] {
        match self {
            GuiServerError::InternalError => [1, 0, 0],
            GuiServerError::XousError(e) => [2, *e as u32, 0],
            GuiServerError::Navigation(e) => {
                let [kind, arg0, _] = e.as_scalar();
                [3, kind, arg0]
            }
            GuiServerError::CannotCloseEssentialApp => [4, 0, 0],
            GuiServerError::AppNotFound => [5, 0, 0],
            GuiServerError::AppAlreadyClosing => [6, 0, 0],
        }
    }
}

impl FromScalar<3> for GuiServerError {
    fn from_scalar([kind, arg0, arg1]: [u32; 3]) -> Self {
        match kind {
            1 => GuiServerError::InternalError,
            2 => GuiServerError::XousError(arg0 as usize),
            3 => GuiServerError::Navigation(NavigationError::from_scalar([arg0, arg1, 0])),
            4 => GuiServerError::CannotCloseEssentialApp,
            5 => GuiServerError::AppNotFound,
            6 => GuiServerError::AppAlreadyClosing,
            _ => GuiServerError::InternalError,
        }
    }
}
