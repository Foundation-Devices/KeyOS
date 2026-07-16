// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(keyos)]

mod error;
pub mod messages;
mod periph;

use eh_0::prelude::_embedded_hal_blocking_i2c_WriteRead;
pub use error::I2cError;
pub use periph::Peripheral;
use server::MessageAllowed;

use crate::messages::*;

#[macro_export]
macro_rules! use_api {
    () => {
        mod i2c_permissions {
            use i2c::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/i2c"]
            pub struct I2cPermissions;
        }
        type I2cApi = i2c::I2cApi<i2c_permissions::I2cPermissions>;
        type I2cPeripheral = i2c::I2cPeripheral<i2c_permissions::I2cPermissions>;
    };
}

#[derive(Default)]
pub struct I2cApi<P: server::CheckedPermissions> {
    conn: server::CheckedConn<P>,
}

impl<P: server::CheckedPermissions> I2cApi<P> {
    pub fn claim_peripheral(&self, peripheral: Peripheral) -> Result<I2cPeripheral<P>, I2cError>
    where
        P: MessageAllowed<ClaimPeripheral>,
    {
        server::CheckedConn::try_send_blocking_scalar(&self.conn, ClaimPeripheral(peripheral))??;
        Ok(I2cPeripheral { conn: self.conn.clone(), peripheral })
    }
}

#[derive(Clone)]
pub struct I2cPeripheral<P: server::CheckedPermissions> {
    conn: server::CheckedConn<P>,
    peripheral: Peripheral,
}

impl<P: server::CheckedPermissions> eh_0::blocking::i2c::Write for I2cPeripheral<P>
where
    P: MessageAllowed<SingleTransfer>,
{
    type Error = I2cError;

    fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), Self::Error> {
        self.write_read(address, bytes, &mut [])
    }
}
impl<P: server::CheckedPermissions> eh_0::blocking::i2c::Read for I2cPeripheral<P>
where
    P: MessageAllowed<SingleTransfer>,
{
    type Error = I2cError;

    fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        self.write_read(address, &[], buffer)
    }
}
impl<P: server::CheckedPermissions> eh_0::blocking::i2c::WriteRead for I2cPeripheral<P>
where
    P: MessageAllowed<SingleTransfer>,
{
    type Error = I2cError;

    fn write_read(&mut self, address: u8, bytes: &[u8], buffer: &mut [u8]) -> Result<(), Self::Error> {
        eh_1::i2c::I2c::write_read(self, address, bytes, buffer)
    }
}

impl<P: server::CheckedPermissions> eh_1::i2c::ErrorType for I2cPeripheral<P> {
    type Error = I2cError;
}

impl<P: server::CheckedPermissions> eh_1::i2c::I2c<eh_1::i2c::SevenBitAddress> for I2cPeripheral<P>
where
    P: MessageAllowed<SingleTransfer>,
{
    fn read(&mut self, address: eh_1::i2c::SevenBitAddress, buffer: &mut [u8]) -> Result<(), Self::Error> {
        eh_1::i2c::I2c::write_read(self, address, &[], buffer)
    }

    fn write(&mut self, address: eh_1::i2c::SevenBitAddress, bytes: &[u8]) -> Result<(), Self::Error> {
        eh_1::i2c::I2c::write_read(self, address, bytes, &mut [])
    }

    fn write_read(
        &mut self,
        _addr: eh_1::i2c::SevenBitAddress,
        bytes: &[u8],
        buffer: &mut [u8],
    ) -> Result<(), Self::Error> {
        if buffer.len() > 255 || bytes.len() > 255 {
            return Err(I2cError::UnsupportedDataSize);
        }
        let result = self.conn.send_blocking_archive(SingleTransfer {
            peripheral: self.peripheral,
            write_data: bytes.to_owned(),
            read_len: buffer.len(),
        })?;
        // result.len() should be the same as buffer.len(). If not, a panic!() is warranted anyway.
        buffer.copy_from_slice(&result);
        Ok(())
    }

    fn transaction(
        &mut self,
        _address: u8,
        _operations: &mut [eh_1::i2c::Operation<'_>],
    ) -> Result<(), Self::Error> {
        unimplemented!()
    }
}
