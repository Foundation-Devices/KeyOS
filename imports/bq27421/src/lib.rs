// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

#![no_std]

mod reg;

use reg::{ControlCommand, Flags};
pub use reg::{Registers, Status};

const BQ27421_ADDRESS: u8 = 0x55;
const EXPECTED_DEVICE_TYPE: u16 = 0x0421;

#[derive(Debug)]
pub enum Bq27421Error<I2C> {
    I2cError(I2C),
}

pub struct Bq27421<I2C> {
    i2c: I2C,
}

impl<I2C: embedded_hal::i2c::I2c> Bq27421<I2C> {
    pub fn new(i2c: I2C) -> Self { Self { i2c } }

    pub fn verify_chip_id(&mut self) -> Result<bool, Bq27421Error<I2C::Error>> {
        self.write_reg(Registers::Control, ControlCommand::DeviceType as u16)?;
        Ok(self.read_reg(Registers::Control)? == EXPECTED_DEVICE_TYPE)
    }

    pub fn status(&mut self) -> Result<Status, Bq27421Error<I2C::Error>> {
        self.write_reg(Registers::Control, ControlCommand::Status as u16)?;
        Ok(Status(self.read_reg(Registers::Control)?))
    }

    pub fn set_hibernate(&mut self) -> Result<(), Bq27421Error<I2C::Error>> {
        self.write_reg(Registers::Control, ControlCommand::SetHibernate as u16)
    }

    pub fn flags(&mut self) -> Result<Flags, Bq27421Error<I2C::Error>> {
        Ok(Flags(self.read_reg(Registers::Flags)?))
    }

    pub fn state_of_charge(&mut self) -> Result<u8, Bq27421Error<I2C::Error>> {
        Ok(self.read_reg(Registers::StateOfCharge)? as u8)
    }

    pub fn voltage(&mut self) -> Result<i16, Bq27421Error<I2C::Error>> {
        Ok(self.read_reg(Registers::BatteryVoltage)? as i16)
    }

    pub fn charge_current(&mut self) -> Result<i16, Bq27421Error<I2C::Error>> {
        Ok(self.read_reg(Registers::AverageCurrent)? as i16)
    }

    pub fn remaining_capacity(&mut self) -> Result<u16, Bq27421Error<I2C::Error>> {
        self.read_reg(Registers::RemainingCapacityFiltered)
    }

    pub fn capacity(&mut self) -> Result<u16, Bq27421Error<I2C::Error>> {
        self.read_reg(Registers::FullChargeCapacityFiltered)
    }

    fn read_reg(&mut self, reg: Registers) -> Result<u16, Bq27421Error<I2C::Error>> {
        let reg_addr = reg as u8;
        let mut buf = [0u8; 2];
        self.i2c.write_read(BQ27421_ADDRESS, &[reg_addr], &mut buf).map_err(Bq27421Error::I2cError)?;

        Ok(u16::from_le_bytes(buf))
    }

    fn write_reg(&mut self, reg: Registers, data: u16) -> Result<(), Bq27421Error<I2C::Error>> {
        let tx_buf = [reg as u8, data as u8, (data >> 8) as u8];
        self.i2c.write_read(BQ27421_ADDRESS, &tx_buf, &mut []).map_err(Bq27421Error::I2cError)?;

        Ok(())
    }
}
