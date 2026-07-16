// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod flash;

use std::ffi::CStr;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serialport::SerialPort;

const VID: u16 = 0x3eb;
const PID: u16 = 0x6124;
const SAMA5D2_CIDR: u32 = 0x8a5c08c0;
const SFR_L2CC_HRAMC: u32 = 0xf8030058;

const SDMMC_APPLET: &[u8] = include_bytes!("../app/applet-sdmmc_sama5d2-generic_sram.bin");
const SDMMC_APPLET_CODE_ADDR: u32 = 0x220000;
const SDMMC_APPLET_MAILBOX_ADDR: u32 = 0x220004;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const SHORT_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct Sambuca {
    connection: Box<dyn SerialPort>,
}

impl Sambuca {
    pub fn new() -> Result<Self> {
        for serial in serialport::available_ports()? {
            if let serialport::SerialPortType::UsbPort(usb_port_info) = serial.port_type {
                if usb_port_info.vid == VID && usb_port_info.pid == PID {
                    let mut result = Self {
                        connection: serialport::new(serial.port_name, 921600)
                            .timeout(DEFAULT_TIMEOUT)
                            .open()?,
                    };
                    result.switch_to_binary()?;
                    let cidr = result.read_u32(0xfc069000)? & 0xffffffe0;
                    if cidr != SAMA5D2_CIDR {
                        bail!("Incorrect CIDR: {cidr:08x} (should be {SAMA5D2_CIDR:08x})");
                    }
                    // Reconfigure L2-Cache as SRAM
                    result.write_u32(SFR_L2CC_HRAMC, 0)?;

                    return Ok(result);
                }
            }
        }
        Err(anyhow!("No USB serial ports with with VID:PID {VID:04x}:{PID:04x} found"))
    }

    fn switch_to_binary(&mut self) -> Result<()> {
        self.connection.write_all(b"N#")?;
        self.connection.flush()?;
        let mut ack = [0u8; 2];
        self.connection.read_exact(&mut ack)?;
        Ok(())
    }

    pub fn version(&mut self) -> Result<String> {
        self.connection.write_all(b"V#")?;
        self.connection.flush()?;
        let mut buf = [0; 256];
        self.connection.set_timeout(SHORT_TIMEOUT)?;
        // Swallowing the error here, since we know we're going to time out.
        self.connection.read_exact(&mut buf).ok();
        self.connection.set_timeout(DEFAULT_TIMEOUT)?;

        let version = CStr::from_bytes_until_nul(&buf[..])?.to_str()?.trim();
        Ok(version.to_string())
    }

    pub fn read_u32(&mut self, address: u32) -> Result<u32> {
        self.connection.write_all(format!("w{address:x},#").as_bytes())?;
        self.connection.flush()?;
        let mut bytes = [0u8; 4];
        self.connection.read_exact(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    pub fn write_u32(&mut self, address: u32, value: u32) -> Result<()> {
        self.connection.write_all(format!("W{address:x},{value:08x}#").as_bytes())?;
        self.connection.flush()?;
        Ok(())
    }

    pub fn write(&mut self, address: u32, data: &[u8]) -> Result<()> {
        // FIXME: On macOS, the writing without chunking fails randomly
        #[cfg(target_os = "macos")]
        {
            const CHUNK_SIZE: usize = 300;
            for (offset, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
                let addr = address + (offset * CHUNK_SIZE) as u32;
                self.connection.write_all(format!("S{addr:x},{:x}#", chunk.len()).as_bytes())?;
                self.connection.flush()?;
                self.connection.write_all(chunk)?;
                self.connection.flush()?;
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            self.connection.write_all(format!("S{address:x},{:x}#", data.len()).as_bytes())?;
            self.connection.flush()?;
            self.connection.write_all(data)?;
            self.connection.flush()?;
        }

        Ok(())
    }

    pub fn read(&mut self, address: u32, data: &mut [u8]) -> Result<()> {
        // Increase timeout proportionally to data size (at least 5s, plus 1s per 64KB)
        let timeout = DEFAULT_TIMEOUT + Duration::from_millis((data.len() / 64) as u64);
        self.connection.set_timeout(timeout)?;

        let result = self.read_inner(address, data);

        // Always restore timeout, even if read_inner failed
        self.connection.set_timeout(DEFAULT_TIMEOUT)?;

        result
    }

    fn read_inner(&mut self, address: u32, data: &mut [u8]) -> Result<()> {
        // FIXME: On macOS, reading large chunks without chunking fails randomly (similar to write)
        #[cfg(target_os = "macos")]
        {
            const CHUNK_SIZE: usize = 4096;
            for (i, chunk) in data.chunks_mut(CHUNK_SIZE).enumerate() {
                let addr = address + (i * CHUNK_SIZE) as u32;
                self.connection.write_all(format!("R{addr:x},{:x}#", chunk.len()).as_bytes())?;
                self.connection.flush()?;
                self.connection.read_exact(chunk)?;
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            self.connection.write_all(format!("R{address:x},{:x}#", data.len()).as_bytes())?;
            self.connection.flush()?;
            self.connection.read_exact(data)?;
        }

        Ok(())
    }

    pub fn go(&mut self, address: u32) -> Result<()> {
        self.connection.write_all(format!("G{address:x}#").as_bytes())?;
        self.connection.flush()?;
        Ok(())
    }

    pub fn execute_applet_command(
        &mut self,
        mailbox: u32,
        address: u32,
        cmd: u32,
        args: &[u32],
    ) -> Result<u32> {
        self.write_u32(mailbox, cmd)?;
        self.write_u32(mailbox + 4, 0xFFFFFFFF)?;
        for (i, arg) in args.iter().enumerate() {
            self.write_u32(mailbox + 8 + i as u32 * 4, *arg)?;
        }
        self.go(address)?;

        // Give the applet time to execute the command
        std::thread::sleep(Duration::from_millis(1));

        let cmd_ack = self.read_u32(mailbox)?;
        if cmd_ack != 0xffffffff - cmd {
            bail!("Invalid Ack to CMD: {cmd_ack:08x}");
        }
        self.read_u32(mailbox + 4)
    }

    pub fn initialize_flash_applet(
        &mut self,
        instance: u32,
        ioset: u32,
        partition: u32,
        bus_width: u32,
        voltages: u32,
    ) -> Result<FlashApplet<'_>> {
        self.write(SDMMC_APPLET_CODE_ADDR, SDMMC_APPLET)?;
        let status = self.execute_applet_command(
            SDMMC_APPLET_MAILBOX_ADDR,
            SDMMC_APPLET_CODE_ADDR,
            0, /* Initialize */
            &[
                // Default applet parameters:
                0,        /* Connection: USB */
                3,        /* Trace level */
                instance, /*  */
                ioset,    /*  */
                // SDMMC-specific parameters
                instance, ioset, partition, bus_width, voltages,
            ],
        )?;
        if status != 0 {
            bail!("Could not initialize flash applet. Status: {status}");
        }
        Ok(FlashApplet {
            buffer_addr: self.read_u32(SDMMC_APPLET_MAILBOX_ADDR + 8)?,
            buffer_size: self.read_u32(SDMMC_APPLET_MAILBOX_ADDR + 0xC)?,
            page_size: self.read_u32(SDMMC_APPLET_MAILBOX_ADDR + 0x10)?,
            outer: self,
        })
    }
}

#[derive(Debug)]
pub struct FlashApplet<'a> {
    outer: &'a mut Sambuca,
    buffer_addr: u32,
    buffer_size: u32,
    page_size: u32,
}

pub struct VerificationStats {
    pub num_chunks_patched: usize,
    pub num_attempts: usize,
}

impl<'a> FlashApplet<'a> {
    pub fn write_flash(&mut self, offset: u64, data: &[u8], mut progress: impl FnMut(usize)) -> Result<()> {
        if offset & (self.page_size as u64 - 1) != 0 {
            bail!("Offset is not aligned");
        }
        if data.len() & (self.page_size as usize - 1) != 0 {
            bail!("Data length is not aligned");
        }
        let mut page_offset = (offset / self.page_size as u64) as u32;
        let buffer_pages = self.buffer_size / self.page_size;
        for chunk in data.chunks((buffer_pages * self.page_size) as usize) {
            self.outer.write(self.buffer_addr, chunk)?;
            let pages_to_write = (chunk.len() / self.page_size as usize) as u32;
            self.write_chunk(page_offset, chunk)?;
            page_offset += pages_to_write;
            progress(page_offset as usize * self.page_size as usize - offset as usize);
        }
        Ok(())
    }

    /// If `auto_patch` is `true`, the function will attempt to rewrite the chunks that failed to verify
    pub fn verify_flash(
        &mut self,
        offset: u64,
        data: &[u8],
        mut progress: impl FnMut(usize),
        auto_patch: bool,
    ) -> Result<VerificationStats> {
        if offset & (self.page_size as u64 - 1) != 0 {
            bail!("Offset is not aligned");
        }
        if data.len() & (self.page_size as usize - 1) != 0 {
            bail!("Data length is not aligned");
        }
        let mut page_offset = (offset / self.page_size as u64) as u32;
        let buffer_pages = self.buffer_size / self.page_size;

        let mut num_chunks_patched = 0;
        let mut num_attempts = 0;

        for chunk in data.chunks((buffer_pages * self.page_size) as usize) {
            let pages_to_read = (chunk.len() / self.page_size as usize) as u32;
            if !self.verify_chunk(page_offset, chunk)? {
                if !auto_patch {
                    bail!(
                        "Flash page difference at page range {page_offset}..{}",
                        page_offset + pages_to_read
                    );
                } else {
                    loop {
                        self.write_chunk(page_offset, chunk)?;
                        if self.verify_chunk(page_offset, chunk)? {
                            num_chunks_patched += 1;
                            break;
                        } else {
                            num_attempts += 1;
                        }
                    }
                }
            }

            page_offset += pages_to_read;
            progress(page_offset as usize * self.page_size as usize - offset as usize);
        }
        Ok(VerificationStats { num_chunks_patched, num_attempts })
    }

    fn verify_chunk(&mut self, page_offset: u32, chunk: &[u8]) -> Result<bool> {
        let pages_to_read = (chunk.len() / self.page_size as usize) as u32;
        let status = self.outer.execute_applet_command(
            SDMMC_APPLET_MAILBOX_ADDR,
            SDMMC_APPLET_CODE_ADDR,
            0x32, /* Read pages */
            &[page_offset, pages_to_read],
        )?;
        if status != 0 {
            bail!("Status after reading {pages_to_read} from {page_offset} was {status}");
        }
        let mut read_data = vec![0; chunk.len()];
        self.outer.read(self.buffer_addr, &mut read_data)?;
        Ok(chunk == read_data)
    }

    fn write_chunk(&mut self, page_offset: u32, chunk: &[u8]) -> Result<()> {
        self.outer.write(self.buffer_addr, chunk)?;
        let pages_to_write = (chunk.len() / self.page_size as usize) as u32;
        let status = self.outer.execute_applet_command(
            SDMMC_APPLET_MAILBOX_ADDR,
            SDMMC_APPLET_CODE_ADDR,
            0x33, /* Write pages */
            &[page_offset, pages_to_write],
        )?;
        if status != 0 {
            bail!("Status after writing {pages_to_write} to {page_offset} was {status}");
        }

        Ok(())
    }

    fn read_chunk(&mut self, page_offset: u32, chunk: &mut [u8]) -> Result<()> {
        let pages_to_read = (chunk.len() / self.page_size as usize) as u32;
        let status = self.outer.execute_applet_command(
            SDMMC_APPLET_MAILBOX_ADDR,
            SDMMC_APPLET_CODE_ADDR,
            0x32, /* Read pages */
            &[page_offset, pages_to_read],
        )?;
        if status != 0 {
            bail!("Status after reading {pages_to_read} from {page_offset} was {status}");
        }
        self.outer.read(self.buffer_addr, chunk)?;
        Ok(())
    }

    pub fn read_flash(
        &mut self,
        offset: u64,
        total_len: usize,
        mut writer: impl std::io::Write,
        mut progress: impl FnMut(usize),
    ) -> Result<()> {
        if offset & (self.page_size as u64 - 1) != 0 {
            bail!("Offset is not aligned");
        }
        if total_len & (self.page_size as usize - 1) != 0 {
            bail!("Data length is not aligned");
        }
        let mut page_offset = (offset / self.page_size as u64) as u32;
        let buffer_pages = self.buffer_size / self.page_size;
        let chunk_size = (buffer_pages * self.page_size) as usize;
        let mut chunk = vec![0u8; chunk_size];
        let mut bytes_read = 0usize;

        while bytes_read < total_len {
            let remaining = total_len - bytes_read;
            let read_size = remaining.min(chunk_size);
            let chunk_slice = &mut chunk[..read_size];

            self.read_chunk(page_offset, chunk_slice)?;
            writer.write_all(chunk_slice)?;

            let pages_read = (read_size / self.page_size as usize) as u32;
            page_offset += pages_read;
            bytes_read += read_size;
            progress(bytes_read);
        }
        Ok(())
    }
}
