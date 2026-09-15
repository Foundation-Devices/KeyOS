/// Main error type for the crate
#[derive(Debug, Clone, Copy, num_derive::FromPrimitive, num_derive::ToPrimitive)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum EhciError {
    /// The CAPS_LEN field of the Capability Registers was wrong.
    /// Possibly means that the controller is not powered, or the
    /// base address was wrong.
    InvalidCapsLen,
    /// The specified endpoint was not opened (See [`crate::controller::Controller::open_endpoint`])
    EndpointNotOpen,
    /// The address specified was out of range.
    InvalidAddress,
    /// The addressed device is disconnected.
    Disconnected,
    /// The descriptor was malformed
    DescriptorError,
    /// Could not allocate a pool item for the transfer or async queue
    OutOfPoolItems,
    /// There was a fatal error during the setup process of the device
    SetupUnsuccessful,
    /// The endpoint stalled the transfer
    Stalled,
    /// The controller is disabled
    ControllerDisabled,
    /// The transfer failed without the device refusing it: a transaction error,
    /// babble, or a buffer problem
    TransferError,
    /// The host abandoned the transfer before the controller finished it
    Cancelled,
}

pub type Result<T> = core::result::Result<T, EhciError>;
