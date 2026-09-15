use zerocopy::{IntoBytes, TryFromBytes};

use crate::{
    commands::{
        CapacityResponse, Cbw, CbwDirection, Command, Csw, FormatCapacityResponse, SenseKey, SenseResponse,
        INVALID_COMMAND_OPERATION_CODE, INVALID_FIELD_IN_CBD, WRITE_PROTECTED,
    },
    BlockDeviceError, MassStorageError, UsbError,
};

const BUF_SIZE: usize = 0x8000;
const BLOCK_SIZE: usize = 512;

/// Actual Buffer implementation that will be used by [`UsbEmulationCommands`] and [`BlockDeviceCommands`]
pub trait Buffer {
    /// Create a new buffer with at least the specified capacity
    fn new(size: usize) -> Self;
    /// Return the slice representation of the buffer
    fn as_slice(&self) -> &[u8];
    /// Return the mutable slice representation of the buffer
    fn as_slice_mut(&mut self) -> &mut [u8];
}

/// Actual USB implementation
pub trait UsbEmulationCommands<Buffer> {
    /// Maximum packet length of the bulk endpoints, below which a packet ends a transfer
    const MAX_PACKET_LEN: usize;

    /// Receive bulk transfer through the mass storage OUT endpoint (from host)
    fn bulk_rx(&mut self, buffer: &mut Buffer, len: usize) -> core::result::Result<usize, UsbError>;
    /// Send bulk transfer through the mass storage IN endpoint (to host)
    fn bulk_tx(&mut self, buffer: &Buffer, len: usize) -> core::result::Result<usize, UsbError>;
}

/// Actual block device implementation
pub trait BlockDeviceCommands<Buffer> {
    /// Read a number of blocks from the specified index into the buffer
    fn read_blocks(
        &mut self,
        lun: u8,
        buffer: &mut Buffer,
        block_idx: u32,
        block_num: usize,
    ) -> Result<(), BlockDeviceError>;
    /// Write a number of blocks from the specified index from the buffer
    fn write_blocks(
        &mut self,
        lun: u8,
        buffer: &Buffer,
        block_idx: u32,
        block_num: usize,
    ) -> Result<(), BlockDeviceError>;
    /// Flush blocks to the device
    fn flush(&mut self, lun: u8) -> Result<(), BlockDeviceError>;
    /// Get the number of blocks
    fn block_count(&self, lun: u8) -> usize;
    /// Get the highest possible LUN number
    fn max_luns(&self) -> u8;
    /// Read-only or Read+Write
    fn allowed_access(&self) -> AllowedAccess;
}

/// Main Mass Storage Emulation driver.
pub struct MassStorageEmulation<B: Buffer, UE: UsbEmulationCommands<B>, BD: BlockDeviceCommands<B>> {
    buffer: B,
    usb: UE,
    block_device: BD,
    current_sense: SenseResponse,
    data_expected: usize,
    data_transferred: usize,
}

/// What kind of accesses will be allowed on the exposed USB interface
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowedAccess {
    /// Only allow reads
    ReadOnly,
    /// Allow reads and writes
    ReadWrite,
}

impl<B: Buffer, UE: UsbEmulationCommands<B>, BD: BlockDeviceCommands<B>> MassStorageEmulation<B, UE, BD> {
    /// Create a new Mass Storage Emulation driver
    pub fn new(usb: UE, block_device: BD) -> Self {
        Self {
            buffer: Buffer::new(BUF_SIZE),
            usb,
            block_device,
            current_sense: Default::default(),
            data_expected: 0,
            data_transferred: 0,
        }
    }

    /// Reset state and run the emulation.
    /// Can be run multiple times.
    /// Returns when encountering an unrecoverable error. In this case both endpoints should be stalled.
    pub fn run(&mut self) -> Result<(), MassStorageError> {
        loop {
            if self.usb.bulk_rx(&mut self.buffer, 0x1F)? != 0x1F {
                return Err(MassStorageError::InvalidLength);
            }
            let cbw = Cbw::try_ref_from_bytes(&self.buffer.as_slice()[..0x1F]).unwrap().clone();
            cbw.check()?;
            log::trace!("Got command: {cbw:02x?}");
            self.data_expected = cbw.transfer_len() as usize;
            self.data_transferred = 0;
            let response = if let Ok(command) = cbw.command() {
                let command = command.clone();
                log::trace!("Decoded into {command:02x?}");
                self.handle_command(cbw.lun(), command)?
            } else {
                log::warn!("Unknown command received: {cbw:x?}");
                self.set_current_sense(SenseKey::IllegalRequest, INVALID_COMMAND_OPERATION_CODE);
                Csw::new().failed()
            };
            // Finish the data phase the host asked for, or it hangs waiting and resets us.
            let residue = self.data_expected - self.data_transferred;
            match cbw.direction() {
                // A packet shorter than the maximum is what ends an IN transfer, so send an
                // empty one. Skip it once a short packet went out, or the host reads the
                // extra packet as the CSW.
                CbwDirection::In if residue > 0 && self.data_transferred % UE::MAX_PACKET_LEN == 0 => {
                    self.usb.bulk_tx(&self.buffer, 0)?;
                }
                CbwDirection::In => {}
                CbwDirection::Out => self.discard_out(residue)?,
            }
            log::trace!("Returning: {response:?}");
            let csw = response.with_tag(cbw.tag()).with_residue(residue as u32);
            let csw_bytes = csw.as_bytes();
            self.buffer.as_slice_mut()[..csw_bytes.len()].copy_from_slice(csw_bytes);
            self.usb.bulk_tx(&self.buffer, csw_bytes.len())?;
        }
    }

    fn set_current_sense(&mut self, key: SenseKey, (asc, ascq): (u8, u8)) {
        log::trace!("Setting sense to {key:?} {asc:02x} {ascq:02x}");
        self.current_sense.flags_and_sense_key = key;
        self.current_sense.additional_sense_code = asc;
        self.current_sense.additional_sense_code_qualifier = ascq;
    }

    fn send_buffer(&mut self, len: usize) -> Result<(), MassStorageError> {
        // Clamp, or a CDB asking for more than its CBW overruns the data phase.
        let len = len.min(self.data_expected - self.data_transferred);
        if len == 0 {
            return Ok(());
        }
        self.usb.bulk_tx(&self.buffer, len)?;
        self.data_transferred += len;
        Ok(())
    }

    fn send_bytes(&mut self, data: &[u8]) -> Result<(), MassStorageError> {
        self.buffer.as_slice_mut()[..data.len()].copy_from_slice(data);
        self.send_buffer(data.len())
    }

    /// Receive data phase bytes into the buffer, and report how many arrived.
    fn receive_buffer(&mut self, len: usize) -> Result<usize, MassStorageError> {
        // Clamp, or we block waiting for bytes the host was never going to send.
        let len = len.min(self.data_expected - self.data_transferred);
        if len == 0 {
            return Ok(0);
        }
        // Cap the count as well, or a backend overreporting underflows the residue.
        let received = self.usb.bulk_rx(&mut self.buffer, len)?.min(len);
        self.data_transferred += received;
        Ok(received)
    }

    /// Swallow the requested length, which BOT 6.7.3 (cases 9, 11-12) takes instead of a STALL.
    fn discard_out(&mut self, mut len: usize) -> Result<(), MassStorageError> {
        while len > 0 {
            let chunk = len.min(BUF_SIZE);
            let received = self.usb.bulk_rx(&mut self.buffer, chunk)?.min(chunk);
            len -= received;
            // A short packet is the host saying it has nothing more, whatever the CBW
            // promised. Asking again would block until the cable comes out.
            if received < chunk {
                break;
            }
        }
        Ok(())
    }

    fn handle_command(&mut self, lun: u8, command: Command) -> Result<Csw, MassStorageError> {
        if lun > self.block_device.max_luns() {
            self.set_current_sense(SenseKey::IllegalRequest, INVALID_FIELD_IN_CBD);
            return Ok(Csw::new().failed());
        }
        let mut result = Csw::new();
        match command {
            Command::TestUnitReady(_) => {
                // Just send a success CSW
            }
            Command::RequestSense(rs) => {
                let sense = core::mem::take(&mut self.current_sense);
                let sense_bytes = sense.as_bytes();
                self.send_bytes(&sense_bytes[..(rs.length as usize).min(sense_bytes.len())])?;
            }
            Command::Inquiry(inq) => {
                let inquiry_bytes: &[u8] = if inq.flags == 0 {
                    &[
                        0x00, // Device type: Direct access
                        0x80, /* Flags: Removable. It is of course not, and we get a lot of
                               *        PreventAllowMediumRemoval because of this flag, but not setting it
                               *        would make OS-es handle this as a permanent hard-disk, with
                               *        aggressive caching, indexing, etc., and we don't want that. */
                        0x04, // Compliance: SPC-2
                        0x02, // Response data format = SPC-2, no additional flags
                        0x20, // Additional length: 32
                        0x00, 0x00, 0x00, // No additional flags
                        b'U', b'S', b'B', 0x00, 0x00, 0x00, 0x00, 0x00, // 8-byte vendor id
                        b'U', b'S', b'B', b' ', b'd', b'i', b's', b'k', 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                        0x00, 0x00, // 16-byte product id
                        b'1', b'.', b'0', b'0', // 4-byte product revision
                    ]
                } else if inq.flags == 1 && inq.page == 0x80 {
                    // Unit serial number page
                    &[
                        0x00, // Device type: Direct access
                        0x80, // Page code: 0x80
                        0x00, // Reserved
                        0x10, // Serial number len
                        0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20,
                        0x20, 0x20, // Empty serial
                    ]
                } else {
                    self.set_current_sense(SenseKey::IllegalRequest, INVALID_FIELD_IN_CBD);
                    return Ok(Csw::new().failed());
                };
                self.send_bytes(&inquiry_bytes[..(inq.length as usize).min(inquiry_bytes.len())])?;
            }
            Command::Read6(r) => {
                result = self.handle_read(lun, u16::from(r.lba) as u32, r.length as usize)?
            }
            Command::Read10(r) => {
                result = self.handle_read(lun, u32::from(r.lba), u16::from(r.length) as usize)?
            }
            Command::Write6(w) => {
                result = self.handle_write(lun, u16::from(w.lba) as u32, w.length as usize)?
            }
            Command::Write10(w) => {
                result = self.handle_write(lun, u32::from(w.lba), u16::from(w.length) as usize)?
            }
            Command::SynchronizeCache10(_) => self.block_device.flush(lun)?,
            Command::ReadCapacity10(_) => {
                let response = CapacityResponse {
                    last_block: (self.block_device.block_count(lun) as u32).saturating_sub(1).into(),
                    block_size: (BLOCK_SIZE as u32).into(),
                };
                self.send_bytes(response.as_bytes())?;
            }
            Command::ReportLuns(_) => {
                self.send_bytes(&[
                    0, 0, 0, 8, // One LUN, 4 bytes additional length
                    0, 0, 0, 0, // reserved bytes
                    0, 0, 0, 0, 0, 0, 0, 0, // dummy LUN address
                ])?;
            }
            Command::ModeSense6(mode) => {
                let dsp = match self.block_device.allowed_access() {
                    AllowedAccess::ReadOnly => 0x80,
                    AllowedAccess::ReadWrite => 0x00,
                };

                if mode.page == 0x08 {
                    self.send_bytes(&[
                        0x17, // Mode data length (.len() - 1)
                        0x00, // Medium type (always 0 for block devices)
                        dsp,  // Device-specific parameter
                        0x00, // No block descriptors
                        // ---
                        0x08, // Page code (Cache)
                        0x12, // Page data length
                        0x01, // Flags: read cache disabled, write cache not enabled
                        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Unused
                        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Unused
                    ])?;
                } else if mode.page == 0x1c {
                    self.send_bytes(&[
                        0x0f, // Mode data length (.len() - 1)
                        0x00, // Medium type (always 0 for block devices)
                        dsp,  // Device-specific parameter
                        0x00, // No block descriptors
                        // ---
                        0x1C, // Page code ( Informational exceptions control page)
                        0x0a, // Page data length
                        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Nothing used
                    ])?;
                } else {
                    // For enything else, just return empty.
                    self.send_bytes(&[
                        0x03, // Mode data length (.len() - 1)
                        0x00, // Medium type (always 0 for block devices)
                        dsp,  // Device-specific parameter
                        0x00, // No block descriptors
                    ])?;
                }
            }
            Command::PreventAllowMediumRemoval(_) => {
                // Make sure not to remove the soldered-on flash chip at runtime :)
            }
            Command::ReadFormatCapacity(_) => {
                let response = FormatCapacityResponse {
                    reserved: [0, 0, 0],
                    list_length: 8,
                    number_of_blocks: (self.block_device.block_count(lun) as u32).into(),
                    descriptor_type: 2,      // Formatted media
                    block_length: [0, 2, 0], // 512, big endian
                };
                self.send_bytes(response.as_bytes())?;
            }
            Command::StartStopUnit(_) => {
                // We "spun down" the emmc successfully :)
            }
        }
        Ok(result)
    }

    fn handle_read(&mut self, lun: u8, mut lba: u32, mut block_num: usize) -> Result<Csw, MassStorageError> {
        while block_num > 0 {
            let blocks_read = block_num.min(BUF_SIZE / BLOCK_SIZE);
            if let Err(e) = self.block_device.read_blocks(lun, &mut self.buffer, lba, blocks_read) {
                log::error!("Read error at LBA 0x{lba:x?}, blocks={blocks_read}: {e:?}");
                self.set_current_sense(e.sense_key(), e.sense_code());
                return Ok(Csw::new().failed());
            }
            self.send_buffer(blocks_read * BLOCK_SIZE)?;
            lba += blocks_read as u32;
            block_num -= blocks_read;
        }
        Ok(Csw::new())
    }

    fn handle_write(&mut self, lun: u8, mut lba: u32, mut block_num: usize) -> Result<Csw, MassStorageError> {
        if self.block_device.allowed_access() == AllowedAccess::ReadOnly {
            self.set_current_sense(SenseKey::DataProtect, WRITE_PROTECTED);
            return Ok(Csw::new().failed());
        }
        while block_num > 0 {
            let blocks_wanted = block_num.min(BUF_SIZE / BLOCK_SIZE);
            let blocks_read = self.receive_buffer(blocks_wanted * BLOCK_SIZE)? / BLOCK_SIZE;
            if blocks_read > 0 {
                if let Err(e) = self.block_device.write_blocks(lun, &self.buffer, lba, blocks_read) {
                    log::error!("Write error at LBA 0x{lba:x?}, blocks={blocks_read}: {e:?}");
                    self.set_current_sense(e.sense_key(), e.sense_code());
                    return Ok(Csw::new().failed());
                };
            }
            // The rest of the buffer still holds the CBW and the last command's blocks, so
            // a host whose CBW promises less than its CDB writes must not reach the disk.
            if blocks_read < blocks_wanted {
                log::error!("Host sent {blocks_read} of {blocks_wanted} blocks for LBA 0x{lba:x?}");
                return Ok(Csw::new().phase_error());
            }
            lba += blocks_read as u32;
            block_num -= blocks_read;
        }
        Ok(Csw::new())
    }
}
