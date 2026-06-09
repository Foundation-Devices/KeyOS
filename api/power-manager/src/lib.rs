// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
pub mod error;
pub mod messages;

pub use error::PowerManagerError;
use messages::*;
use server::{CheckedConn, CheckedPermissions, MessageAllowed};

#[macro_export]
macro_rules! use_api {
    () => {
        mod power_manager_permissions {
            use power_manager::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/power-manager"]
            pub struct PowerManagerPermissions;
        }
        type PowerManagerApi =
            power_manager::PowerManagerApi<power_manager_permissions::PowerManagerPermissions>;
    };
}

#[macro_export]
macro_rules! use_ext_api {
    () => {
        mod power_manager_ext_permissions {
            use power_manager::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/power-manager-ext"]
            pub struct PowerManagerExtPermissions;
        }
        type PowerManagerExtApi =
            power_manager::PowerManagerExtApi<power_manager_ext_permissions::PowerManagerExtPermissions>;
    };
}

#[derive(Default)]
pub struct PowerManagerApi<P: CheckedPermissions> {
    conn: CheckedConn<P>,
}

impl<P: CheckedPermissions> PowerManagerApi<P> {
    pub fn reboot(&self)
    where
        P: MessageAllowed<Reboot>,
    {
        self.conn.send_blocking_scalar(Reboot)
    }

    pub fn shutdown(&self)
    where
        P: MessageAllowed<Shutdown>,
    {
        self.conn.send_blocking_scalar(Shutdown)
    }

    #[cfg(keyos)]
    pub fn enable_peripheral(&self, peripheral: atsama5d27::pmc::PeripheralId) -> Result<(), xous::Error>
    where
        P: MessageAllowed<SetPeripheralEnabled>,
    {
        self.conn.try_send_blocking_scalar(SetPeripheralEnabled { peripheral, enabled: true })
    }

    #[cfg(keyos)]
    pub fn disable_peripheral(&self, peripheral: atsama5d27::pmc::PeripheralId) -> Result<(), xous::Error>
    where
        P: MessageAllowed<SetPeripheralEnabled>,
    {
        self.conn.try_send_blocking_scalar(SetPeripheralEnabled { peripheral, enabled: false })
    }
}

#[derive(Default)]
pub struct PowerManagerExtApi<P: CheckedPermissions> {
    conn: CheckedConn<P>,
}

impl<P: CheckedPermissions> PowerManagerExtApi<P> {
    pub fn status(&self) -> Result<Status, xous::Error>
    where
        P: MessageAllowed<GetStatus>,
    {
        self.conn.try_send_blocking_scalar(GetStatus)
    }

    pub fn extended_status(&self) -> Option<ExtendedStatus>
    where
        P: MessageAllowed<GetExtendedStatus>,
    {
        self.conn.send_blocking_archive(GetExtendedStatus)
    }

    pub fn set_usb_boost(&self, enabled: bool) -> Result<SetUsbBoostResponse, xous::Error>
    where
        P: MessageAllowed<SetUsbBoost>,
    {
        self.conn.try_send_blocking_scalar(SetUsbBoost { enabled })
    }

    #[cfg(not(keyos))]
    pub fn set_battery_percent(&self, level: u8) -> Result<(), xous::Error>
    where
        P: MessageAllowed<SetBatteryPercent>,
    {
        self.conn.try_send_scalar(SetBatteryPercent(level))?;
        Ok(())
    }

    #[cfg(keyos)]
    pub fn set_otg_priority(&self, priority: OtgPriority) -> Result<(), xous::Error>
    where
        P: MessageAllowed<SetOtgPriority>,
    {
        self.conn.try_send_scalar(SetOtgPriority(priority))?;
        Ok(())
    }

    #[cfg(keyos)]
    pub fn clear_charger_fault(&self) -> Result<(), xous::Error>
    where
        P: MessageAllowed<ClearChargeFault>,
    {
        self.conn.try_send_scalar(ClearChargeFault)?;
        Ok(())
    }

    pub fn subscribe_status<S>(&self, context: &mut server::ServerContext<S>)
    where
        S: server::Server + server::ScalarEventHandler<Status>,
        P: MessageAllowed<StatusSubscribe>,
    {
        self.conn.subscribe_scalar_infallible(StatusSubscribe, context)
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct Status {
    pub charge_status: ChargeStatus,
    pub attached_state: AttachedState,
    pub battery_percent: u8,
}

#[derive(num_derive::FromPrimitive, num_derive::ToPrimitive, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeStatus {
    Idle = 0,
    Charging = 1,
    ChargeDone = 2,
    Boosting = 3,
    Fault = 4,
}

/// Priority of the USB OTG mode.
#[derive(num_derive::FromPrimitive, num_derive::ToPrimitive, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OtgPriority {
    /// Never host USB peripherals, always device mode.
    #[default]
    Never,
    /// Prefer the device mode but allow host (OTG) mode if a peripheral is connected.
    Automatic,
    /// Always host USB peripherals, never device mode.
    Forced,
}

#[cfg(keyos)]
impl From<OtgPriority> for tusb320::ModeSelect {
    fn from(priority: OtgPriority) -> Self {
        match priority {
            OtgPriority::Never => tusb320::ModeSelect::Ufp,
            OtgPriority::Automatic => tusb320::ModeSelect::Drp,
            OtgPriority::Forced => tusb320::ModeSelect::Dfp,
        }
    }
}

#[derive(num_derive::FromPrimitive, num_derive::ToPrimitive, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachedState {
    None = 0,
    Source = 1,
    Sink = 2,
    Accessory = 3,
}

#[cfg(keyos)]
impl From<tusb320::AttachedState> for AttachedState {
    fn from(value: tusb320::AttachedState) -> Self {
        match value {
            tusb320::AttachedState::NotAttached => AttachedState::None,
            tusb320::AttachedState::AttachedSrc => AttachedState::Source,
            tusb320::AttachedState::AttachedSnk => AttachedState::Sink,
            tusb320::AttachedState::AttachedAccessory => AttachedState::Accessory,
        }
    }
}
