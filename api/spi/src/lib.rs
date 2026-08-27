// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg(keyos)]

mod error;
pub mod messages;
mod periph;

use atsama5d27::spi::BitsPerTransfer;
use eh_1::spi::SpiDevice;
use server::{CheckedConn, CheckedPermissions, MessageAllowed};
use xous::{map_memory, MemoryFlags, MemoryRange};

use crate::messages::*;
pub use crate::{error::SpiError, periph::Peripheral};

#[macro_export]
macro_rules! use_api {
    () => {
        mod spi_permissions {
            use spi::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/spi"]
            pub struct SpiPermissions;
        }
        type SpiApi = spi::SpiApi<spi_permissions::SpiPermissions>;
        type SpiPeripheral = spi::SpiPeripheral<spi_permissions::SpiPermissions>;
    };
}

#[derive(Default)]
pub struct SpiApi<P: CheckedPermissions> {
    conn: CheckedConn<P>,
}

impl<P: CheckedPermissions> SpiApi<P> {
    pub fn claim_peripheral(&self, peripheral: Peripheral) -> Result<SpiPeripheral<P>, SpiError>
    where
        P: MessageAllowed<ClaimPeripheral>,
    {
        self.conn
            .try_send_blocking_scalar(ClaimPeripheral(peripheral))
            .map_err(|_| SpiError::InternalError)??;
        let buffer = map_memory(None, None, 0x1000, MemoryFlags::W)?;
        Ok(SpiPeripheral { conn: self.conn.clone(), peripheral, buffer, add_offset: 0 })
    }
}

pub struct SpiPeripheral<P: CheckedPermissions> {
    conn: CheckedConn<P>,
    peripheral: Peripheral,
    buffer: MemoryRange,
    add_offset: usize,
}

impl<P: CheckedPermissions> SpiPeripheral<P> {
    fn check_word_size<Word>(&self) -> Result<(), SpiError> {
        if self.peripheral.bit_per_transfer() == BitsPerTransfer::Bits8 {
            if core::mem::size_of::<Word>() == 1 {
                Ok(())
            } else {
                Err(SpiError::InvalidWordSize)
            }
        } else {
            if core::mem::size_of::<Word>() == 2 {
                Ok(())
            } else {
                Err(SpiError::InvalidWordSize)
            }
        }
    }

    fn add_to_buffer<Word: Copy>(&mut self, data: &[Word]) -> Result<(), SpiError> {
        self.check_word_size::<Word>()?;
        if data.is_empty() {
            return Ok(());
        }
        let data_len_bytes = data.len() * core::mem::size_of::<Word>();
        let mut subrange =
            self.buffer.subrange(self.add_offset, data_len_bytes).ok_or(SpiError::MessageTooLong)?;
        subrange.as_slice_mut().copy_from_slice(data);
        self.add_offset += data_len_bytes;
        Ok(())
    }

    fn write_to_buffer<Word: Copy>(&mut self, data: &[Word]) -> Result<(), SpiError> {
        self.add_offset = 0;
        self.add_to_buffer(data)
    }

    fn read_from_buffer<Word: Copy>(&self, data: &mut [Word]) -> Result<(), SpiError> {
        self.check_word_size::<Word>()?;
        if data.is_empty() {
            return Ok(());
        }
        let data_len_bytes = data.len() * core::mem::size_of::<Word>();
        let subrange = self.buffer.subrange(0, data_len_bytes).ok_or(SpiError::MessageTooLong)?;
        data.copy_from_slice(subrange.as_slice());
        Ok(())
    }

    fn xfer<Word>(&self, len: usize) -> Result<usize, SpiError>
    where
        P: MessageAllowed<SpiXfer>,
    {
        if len > self.buffer.len() {
            return Err(SpiError::MessageTooLong);
        }
        self.conn.lend_mut(SpiXfer {
            buffer: self.buffer,
            bytes: len * core::mem::size_of::<Word>(),
            peripheral: self.peripheral,
        })
    }

    pub fn nrf_read_data(&self, buffer: &mut [u8], timeout_ms: usize) -> Result<usize, SpiError>
    where
        P: MessageAllowed<NrfReadData>,
    {
        let len = self
            .conn
            .lend_mut(NrfReadData {
                buffer: self.buffer,
                bytes: buffer.len(),
                peripheral: self.peripheral,
                timeout_ms,
            })
            .inspect_err(|e| log::error!("lend_mut error: {e:?}"))?;
        self.read_from_buffer(&mut buffer[..len])?;
        Ok(len)
    }

    pub fn st25r95_read_data(&mut self) -> Result<(u8, Vec<u8>), SpiError>
    where
        P: MessageAllowed<St25r95ReadData>,
    {
        let result = self
            .conn
            .lend_mut(St25r95ReadData { buffer: self.buffer, peripheral: self.peripheral })
            .inspect_err(|e| log::error!("lend_mut error: {e:?}"))?;
        let code = (result & 0xFF) as u8;
        let buffer_len = result >> 8;
        let data = if buffer_len > 0 {
            let mut data = vec![0; buffer_len];
            self.read_from_buffer(&mut data).inspect_err(|e| log::error!("read_from_buffer error: {e:?}"))?;
            data
        } else {
            vec![]
        };
        log::trace!("read_data: code=0x{code:02x?} len={buffer_len} data={data:02x?}");
        Ok((code, data))
    }
}

impl<P: CheckedPermissions> eh_1::spi::ErrorType for SpiPeripheral<P> {
    type Error = SpiError;
}

// implement SpiDevice to take advantage of hardware CS management
impl<P, Word: Copy + 'static> eh_1::spi::SpiDevice<Word> for SpiPeripheral<P>
where
    P: CheckedPermissions,
    P: MessageAllowed<SpiXfer>,
{
    fn transaction(&mut self, operations: &mut [eh_1::spi::Operation<'_, Word>]) -> Result<(), Self::Error> {
        for operation in operations {
            match operation {
                eh_1::spi::Operation::Read(buf_read) => {
                    self.xfer::<Word>(buf_read.len())?;
                    self.read_from_buffer(buf_read)?;
                }
                eh_1::spi::Operation::Write(buf_write) => {
                    self.write_to_buffer(buf_write)?;
                    self.xfer::<Word>(buf_write.len())?;
                }
                eh_1::spi::Operation::Transfer(buf_read, buf_write) => {
                    self.write_to_buffer(buf_write)?;
                    self.xfer::<Word>(buf_write.len())?;
                    self.read_from_buffer(buf_read)?;
                }
                eh_1::spi::Operation::TransferInPlace(buf) => {
                    self.write_to_buffer(buf)?;
                    self.xfer::<Word>(buf.len())?;
                    self.read_from_buffer(buf)?;
                }
                eh_1::spi::Operation::DelayNs(ns) => {
                    std::thread::sleep(std::time::Duration::from_nanos(*ns as u64))
                }
            }
        }
        Ok(())
    }
}

impl<P> st25r95::St25r95Spi for SpiPeripheral<P>
where
    P: CheckedPermissions,
    P: MessageAllowed<SpiXfer>,
    P: MessageAllowed<St25r95ReadData>,
{
    fn poll(&mut self, flag: st25r95::PollFlags) -> st25r95::Result<()> {
        let mut curr_flags = [st25r95::Control::Poll as u8, st25r95::Control::Poll as u8];
        self.transfer_in_place(&mut curr_flags)?;
        match st25r95::PollFlags::from_bits_truncate(curr_flags[1]).contains(flag) {
            true => Ok(()),
            false => Err(st25r95::Error::PollTimeout),
        }
    }

    fn reset(&mut self) -> st25r95::Result<()> {
        self.write(&[st25r95::Control::Reset as u8])?;
        std::thread::sleep(std::time::Duration::from_millis(3));
        Ok(())
    }

    fn send_command(&mut self, cmd: st25r95::Command, data: &[u8], sod: bool) -> st25r95::Result<()> {
        log::trace!("send_command: cmd={cmd:?}, data={data:02x?}");
        let mut packet = vec![st25r95::Control::Send as u8, cmd as u8];
        let data_len = data.len() as u8;
        if cmd != st25r95::Command::Echo {
            if sod {
                packet.push(data_len + 2);
                packet.push(0xF0);
                packet.push(data_len + 1);
            } else {
                packet.push(data_len);
            }
            packet.extend_from_slice(data);
        }
        self.write(&packet)?;
        Ok(())
    }

    fn read_data(&mut self) -> st25r95::Result<st25r95::ReadResponse> {
        let (code, data) = self.st25r95_read_data()?;
        Ok(st25r95::ReadResponse { code, data: heapless::Vec::from_slice(&data)? })
    }

    fn flush(&mut self) -> st25r95::Result<()> {
        const ZEROES: [u8; st25r95::MAX_BUFFER_SIZE] = [0u8; st25r95::MAX_BUFFER_SIZE];
        self.write(&ZEROES)?;
        Ok(())
    }
}
