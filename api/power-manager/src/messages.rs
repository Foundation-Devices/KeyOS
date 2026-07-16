// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(keyos)]
use atsama5d27::pmc::PeripheralId;
use num_traits::{FromPrimitive, ToPrimitive};
use server::{AsScalar, FromScalar};

use crate::{AttachedState, ChargeStatus, OtgPriority, Status};

#[cfg(not(keyos))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PeripheralId {
    Reserved1 = 1,
}

#[cfg(not(keyos))]
impl TryFrom<u8> for PeripheralId {
    type Error = ();

    fn try_from(_value: u8) -> Result<Self, Self::Error> { Err(()) }
}

#[derive(Debug, server::Message)]
#[response(())]
pub struct Shutdown;

#[derive(Debug, server::Message)]
#[response(())]
pub struct Reboot;

#[derive(Debug, server::Message)]
#[response(Status)]
pub struct GetStatus;

impl FromScalar<3> for Status {
    fn from_scalar([status, percent, state]: [u32; 3]) -> Self {
        Status {
            charge_status: ChargeStatus::from_u32(status).unwrap_or(ChargeStatus::Fault),
            battery_percent: percent as u8,
            attached_state: AttachedState::from_u32(state).unwrap_or(AttachedState::None),
        }
    }
}

impl AsScalar<3> for Status {
    fn as_scalar(&self) -> [u32; 3] {
        [
            self.charge_status.to_u32().unwrap(),
            self.battery_percent as u32,
            self.attached_state.to_u32().unwrap(),
        ]
    }
}

#[derive(Debug, server::Message)]
#[response(SetUsbBoostResponse)]
pub struct SetUsbBoost {
    pub enabled: bool,
}

pub struct SetUsbBoostResponse {
    pub success: bool,
    pub previous_state: bool,
}

impl FromScalar<1> for SetUsbBoost {
    fn from_scalar([value]: [u32; 1]) -> Self { Self { enabled: value != 0 } }
}

impl AsScalar<1> for SetUsbBoost {
    fn as_scalar(&self) -> [u32; 1] { [self.enabled.into()] }
}

impl FromScalar<2> for SetUsbBoostResponse {
    fn from_scalar(value: [u32; 2]) -> Self { Self { success: value[0] != 0, previous_state: value[1] != 0 } }
}

impl AsScalar<2> for SetUsbBoostResponse {
    fn as_scalar(&self) -> [u32; 2] { [self.success.into(), self.previous_state.into()] }
}

#[derive(Debug, server::Message)]
#[response(())]
pub struct SetPeripheralEnabled {
    pub peripheral: PeripheralId,
    pub enabled: bool,
}

impl FromScalar<2> for SetPeripheralEnabled {
    fn from_scalar(value: [u32; 2]) -> Self {
        Self {
            peripheral: PeripheralId::try_from(value[0] as u8).unwrap_or(PeripheralId::Reserved1),
            enabled: value[1] != 0,
        }
    }
}

impl AsScalar<2> for SetPeripheralEnabled {
    fn as_scalar(&self) -> [u32; 2] { [self.peripheral as u32, self.enabled as u32] }
}

#[derive(Debug, server::Message)]
pub struct SetOtgPriority(pub OtgPriority);

impl FromScalar<1> for OtgPriority {
    fn from_scalar([value]: [u32; 1]) -> Self { OtgPriority::from_u32(value).unwrap_or_default() }
}

impl AsScalar<1> for OtgPriority {
    fn as_scalar(&self) -> [u32; 1] { [self.to_u32().unwrap_or_default()] }
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Option<ExtendedStatus>)]
pub struct GetExtendedStatus;

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ExtendedStatus {
    pub current: i16,
    pub voltage_mv: i16,
    pub capacity_mah: u16,
    pub remaining_capacity_mah: u16,
    pub last_reported_fault: Option<ChargeFault>,
    pub num_reported_faults: u32,
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum ChargeFault {
    Normal,
    VbusOvp,
    SleepMode,
    BadAdaptor,
    OutputOvp,
    ThermalShutdown,
    TimerFault,
    NoBattery,
}

#[cfg(keyos)]
impl From<bq24157::ChargeFault> for ChargeFault {
    fn from(fault: bq24157::ChargeFault) -> Self {
        match fault {
            bq24157::ChargeFault::Normal => ChargeFault::Normal,
            bq24157::ChargeFault::VbusOvp => ChargeFault::VbusOvp,
            bq24157::ChargeFault::SleepMode => ChargeFault::SleepMode,
            bq24157::ChargeFault::BadAdaptor => ChargeFault::BadAdaptor,
            bq24157::ChargeFault::OutputOvp => ChargeFault::OutputOvp,
            bq24157::ChargeFault::ThermalShutdown => ChargeFault::ThermalShutdown,
            bq24157::ChargeFault::TimerFault => ChargeFault::TimerFault,
            bq24157::ChargeFault::NoBattery => ChargeFault::NoBattery,
        }
    }
}

#[derive(Debug, server::Message)]
pub struct ClearChargeFault;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(Status)]
pub struct StatusSubscribe;

#[derive(Debug, server::Message)]
pub struct SetBatteryPercent(pub(crate) u8);
