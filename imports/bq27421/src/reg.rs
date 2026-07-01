// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use bitfield::bitfield;

#[derive(Debug, Copy, Clone)]
pub enum Registers {
    Control = 0x00,
    BatteryVoltage = 0x04,
    Flags = 0x06,
    AverageCurrent = 0x10,
    StateOfCharge = 0x1C,
    RemainingCapacityFiltered = 0x2A,
    FullChargeCapacityFiltered = 0x2E,
}

#[derive(Debug, Copy, Clone)]
pub enum ControlCommand {
    Status = 0x0000,
    DeviceType = 0x0001,
    #[allow(dead_code)]
    Flags = 0x0006,
    SetHibernate = 0x0011,
}

bitfield! {
    pub struct Status(u16);
    impl Debug;
    pub voltage_ok, _: 1;
    pub ra_table_updates_disabled, _: 2;
    pub constant_power_model, _: 3;
    pub sleep, _: 4;
    pub hibernate, _: 6;
    pub initialization_complete, _: 7;
    pub resistance_updated, _: 8;
    pub qmax_updated, _: 9;
    pub board_calibration_active, _: 10;
    pub coulomb_conter_auto_calibration, _: 11;
    pub calibration_mode, _: 12;
    pub sealed, _: 13;
    pub watchdog_reset, _: 14;
    pub shutdown_enabled, _: 15;
}

bitfield! {
    pub struct Flags(u16);
    impl Debug;
    pub discharging, _: 0;
    pub socf, _: 1;
    pub soc1, _: 2;
    pub battery_detected, _: 3;
    pub config_update_mode, _: 4;
    pub reset, _: 5;
    pub ocv_taken, _: 7;
    pub fast_charging_allowed, _: 8;
    pub full, _: 9;
    pub under_temperature, _: 14;
    pub over_temperature, _: 15;
}
