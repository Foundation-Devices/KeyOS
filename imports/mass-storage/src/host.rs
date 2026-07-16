use log::debug;
use zerocopy::TryFromBytes;

use crate::{
    commands::{
        CapacityResponse, Cbw, CbwDirection, Command, Csw, Inquiry6, SenseKey, SenseResponse, Typical10,
        Typical6,
    },
    error::{MassStorageError, Result, UsbError},
};

/// Actual USB implementation
pub trait UsbHostCommands {
    /// Receive bulk transfer through the mass storage IN endpoint (from device)
    fn bulk_in(&mut self, data: &mut [u8]) -> core::result::Result<usize, UsbError>;
    /// Send bulk transfer through the mass storage OUT endpoint (to device)
    fn bulk_out(&mut self, data: &[u8]) -> core::result::Result<usize, UsbError>;
}

/// Main Mass Storage driver. Instantiate to access all functionality.
pub struct MassStorageHost<UsbHost: UsbHostCommands> {
    usb: UsbHost,
    tag: u32,
    block_count: u32,
    block_size: u16,
}

impl<UsbHost: UsbHostCommands> MassStorageHost<UsbHost> {
    /// Create new Mass Storage device. Assumes that the device was just connected
    /// (or reset) via USB.
    pub fn new(usb: UsbHost) -> Result<Self> {
        let mut this = Self { usb, tag: 1, block_count: 0, block_size: 0 };

        debug!("Testing if unit is ready");
        retry(5, || this.send_out_command(Cbw::new(0, 0, Command::TestUnitReady(Typical6::default())), &[]))?;
        debug!("Sending inquiry");
        let mut inquiry_result = [0u8; 36];
        retry(5, || {
            this.send_in_command(
                Cbw::new(36, 0, Command::Inquiry(Inquiry6 { length: 36, ..Default::default() })),
                &mut inquiry_result,
            )
        })?;
        // Either qualifier is not 0 (connected)
        // Or device type is not 0 (direct access), so not pendrive
        if inquiry_result[0] != 0 {
            return Err(MassStorageError::NotDirectAccess);
        }
        debug!("Getting capacity");
        let mut capacity_response = [0u8; 8];
        this.send_in_command(
            Cbw::new(8, 0, Command::ReadCapacity10(Typical10::default())),
            &mut capacity_response,
        )?;
        let capacity = CapacityResponse::try_ref_from_bytes(&capacity_response)
            .map_err(|_| MassStorageError::OtherError)?;
        this.block_size = u16::try_from(u32::from(capacity.block_size))
            .ok()
            .filter(|&size| size != 0)
            .ok_or(MassStorageError::OtherError)?;
        this.block_count = capacity.last_block.into();
        debug!("Capacity is {} blocks with size={}", this.block_count, this.block_size);
        Ok(this)
    }

    /// Read sectors from block number `lba`. Data size must be a multiple of block size.
    pub fn read(&mut self, lba: u32, data: &mut [u8]) -> Result<usize> {
        debug!("Reading {} bytes from block {lba}", data.len());
        if data.len() % self.block_size as usize != 0 {
            return Err(MassStorageError::InvalidArgument);
        }
        let blocks = u16::try_from(data.len() / self.block_size as usize)
            .map_err(|_| MassStorageError::InvalidArgument)?;
        self.send_in_command(
            Cbw::new(
                blocks as u32 * self.block_size as u32,
                0,
                Command::Read10(Typical10 { lba: lba.into(), length: blocks.into(), ..Default::default() }),
            ),
            data,
        )
    }

    /// Write sectors from block number `lba`. Data size must be a multiple of block size.
    pub fn write(&mut self, lba: u32, data: &[u8]) -> Result<usize> {
        debug!("Writing {} bytes to block {lba}", data.len());
        if data.len() % self.block_size as usize != 0 {
            return Err(MassStorageError::InvalidArgument);
        }
        let blocks = u16::try_from(data.len() / self.block_size as usize)
            .map_err(|_| MassStorageError::InvalidArgument)?;

        self.send_out_command(
            Cbw::new(
                blocks as u32 * self.block_size as u32,
                0,
                Command::Write10(Typical10 { lba: lba.into(), length: blocks.into(), ..Default::default() }),
            ),
            data,
        )
    }

    /// Flush all on-device caches
    pub fn flush(&mut self) -> Result<()> {
        self.send_out_command(Cbw::new(0, 0, Command::SynchronizeCache10(Typical10::default())), &[])
            .map(|_| ())
    }

    /// Get the block size. Usually 512.
    pub fn block_size(&self) -> u16 { self.block_size }

    /// Get the size of the device in blocks.
    pub fn block_count(&self) -> u32 { self.block_count }

    /// Get back the underlying usb object
    pub fn usb(&self) -> &UsbHost { &self.usb }

    /// Get back the underlying usb object (mutable)
    pub fn usb_mut(&mut self) -> &mut UsbHost { &mut self.usb }

    fn read_csw(&mut self) -> Result<Csw> {
        let mut csw_data = [0u8; 13];
        if self.usb.bulk_in(&mut csw_data)? != 13 {
            return Err(MassStorageError::OtherError);
        }
        Csw::try_read_from_bytes(&csw_data).map_err(|_| MassStorageError::OtherError)
    }

    fn send_out_command_raw(&mut self, mut cbw: Cbw, data: &[u8]) -> Result<(Csw, usize)> {
        self.tag += 1;
        cbw.set_tag(self.tag);
        cbw.set_direction(CbwDirection::Out);
        self.usb.bulk_out(&cbw.into_bytes())?;
        let len = if data.is_empty() { 0 } else { self.usb.bulk_out(data)? };
        Ok((self.read_csw()?, len))
    }

    fn send_in_command_raw(&mut self, mut cbw: Cbw, data: &mut [u8]) -> Result<(Csw, usize)> {
        self.tag += 1;
        cbw.set_tag(self.tag);
        cbw.set_direction(CbwDirection::In);
        self.usb.bulk_out(&cbw.into_bytes())?;
        let len = if data.is_empty() { 0 } else { self.usb.bulk_in(data)? };
        Ok((self.read_csw()?, len))
    }

    fn send_out_command(&mut self, cbw: Cbw, data: &[u8]) -> Result<usize> {
        let (csw, len) = self.send_out_command_raw(cbw, data)?;
        match csw.check(self.tag) {
            Ok(()) => Ok(len),
            Err(MassStorageError::CommandFailed) => self.check_sense().map(|_| 0),
            Err(e) => Err(e),
        }
    }

    fn send_in_command(&mut self, cbw: Cbw, data: &mut [u8]) -> Result<usize> {
        let (csw, result) = self.send_in_command_raw(cbw, data)?;
        match csw.check(self.tag) {
            Ok(()) => Ok(result),
            Err(MassStorageError::CommandFailed) => {
                self.check_sense()?;
                Ok(result)
            }
            Err(e) => Err(e),
        }
    }

    fn check_sense(&mut self) -> Result<()> {
        let mut sense = [0u8; size_of::<SenseResponse>()];
        let (csw, _sense_len) = self.send_in_command_raw(
            Cbw::new(
                size_of::<SenseResponse>() as u32,
                0,
                Command::RequestSense(Typical6 {
                    length: size_of::<SenseResponse>() as u8,
                    ..Default::default()
                }),
            ),
            &mut sense,
        )?;
        csw.check(self.tag)?;
        let sense_response =
            SenseResponse::try_ref_from_bytes(&sense).map_err(|_| MassStorageError::OtherError)?;
        if sense_response.flags_and_sense_key == SenseKey::NoSense {
            Ok(())
        } else {
            Err(MassStorageError::SenseError(sense_response.flags_and_sense_key))
        }
    }
}

fn retry<T>(try_count: usize, mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut fails = 0;
    loop {
        match f() {
            Ok(v) => break Ok(v),
            Err(MassStorageError::SenseError(reason)) => {
                fails += 1;
                if fails > try_count {
                    break Err(MassStorageError::SenseError(reason));
                }
            }
            Err(e) => break Err(e),
        }
    }
}
