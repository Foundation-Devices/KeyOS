// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub use ehci::EhciError;
use server::{AsScalar, FromScalar};

#[derive(Debug, Clone, Copy, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum UsbError {
    #[error("OS error: {:?}", xous::Error::from_usize(*.0))]
    XousError(usize),
    #[error("EHCI error: {0:?}")]
    EhciError(EhciError),
    #[error("Other Usb error")]
    Other,
    #[error("Usb device not found")]
    NotFound,
    #[error("Device was not claimed")]
    NotClaimed,
    #[error("Data buffer was too large")]
    DataTooLarge,
    #[error("An interface was already registered")]
    AlreadyRegistered,
    #[error("An endpoint operation was attempted in the wrong direction (e.g. read on an IN endpoint)")]
    WrongDirection,
    #[error("The endpoint already has a queued operation")]
    Busy,
    #[error("The usb host has disconnected")]
    HostDisconnected,
    #[error("One of the parameters had a wrong value")]
    InvalidParameter,
    #[error("The USB interface is disabled")]
    InterfaceDisabled,
}

impl From<xous::Error> for UsbError {
    fn from(value: xous::Error) -> Self { UsbError::XousError(value.to_usize()) }
}

impl From<EhciError> for UsbError {
    fn from(value: EhciError) -> Self { UsbError::EhciError(value) }
}

impl AsScalar<2> for UsbError {
    fn as_scalar(&self) -> [u32; 2] {
        match self {
            UsbError::XousError(e) => [1, *e as u32],
            UsbError::EhciError(e) => [
                2,
                match e {
                    EhciError::InvalidCapsLen => 1,
                    EhciError::EndpointNotOpen => 2,
                    EhciError::InvalidAddress => 3,
                    EhciError::Disconnected => 4,
                    EhciError::DescriptorError => 5,
                    EhciError::OutOfPoolItems => 6,
                    EhciError::SetupUnsuccessful => 7,
                    EhciError::Stalled => 8,
                    EhciError::ControllerDisabled => 9,
                },
            ],
            UsbError::Other => [3, 0],
            UsbError::NotFound => [4, 0],
            UsbError::NotClaimed => [5, 0],
            UsbError::DataTooLarge => [6, 0],
            UsbError::AlreadyRegistered => [7, 0],
            UsbError::WrongDirection => [8, 0],
            UsbError::Busy => [9, 0],
            UsbError::HostDisconnected => [10, 0],
            UsbError::InvalidParameter => [11, 0],
            UsbError::InterfaceDisabled => [12, 0],
        }
    }
}

impl FromScalar<2> for UsbError {
    fn from_scalar(value: [u32; 2]) -> Self {
        match value[0] {
            1 => UsbError::XousError(value[1] as usize),
            2 => UsbError::EhciError(match value[1] {
                1 => EhciError::InvalidCapsLen,
                2 => EhciError::EndpointNotOpen,
                3 => EhciError::InvalidAddress,
                4 => EhciError::Disconnected,
                5 => EhciError::DescriptorError,
                6 => EhciError::OutOfPoolItems,
                7 => EhciError::SetupUnsuccessful,
                8 => EhciError::Stalled,
                9 => EhciError::ControllerDisabled,
                _ => EhciError::Disconnected,
            }),
            3 => UsbError::Other,
            4 => UsbError::NotFound,
            5 => UsbError::NotClaimed,
            6 => UsbError::DataTooLarge,
            7 => UsbError::AlreadyRegistered,
            8 => UsbError::WrongDirection,
            9 => UsbError::Busy,
            10 => UsbError::HostDisconnected,
            11 => UsbError::InvalidParameter,
            12 => UsbError::InterfaceDisabled,
            _ => UsbError::Other,
        }
    }
}

impl From<usize> for UsbError {
    fn from(value: usize) -> Self { Self::from_scalar([value as u32, 0]) }
}

impl From<UsbError> for usize {
    fn from(value: UsbError) -> Self { server::AsScalar::<2>::as_scalar(&value)[0] as usize }
}
