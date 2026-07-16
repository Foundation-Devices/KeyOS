// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    atsama5d27::spi::{BitsPerTransfer, Spi},
    server::{BlockingScalarHandler, LendMutHandler, Server, ServerContext},
    spi::{messages::*, Peripheral, SpiError},
    std::collections::HashMap,
    std::time::{Duration, Instant},
    utralib::HW_SPI0_BASE,
    xous::{keyos::MASTER_CLOCK_SPEED, PID},
};

power_manager::use_api!();

dma::use_api!();

gpio::use_api!();

#[derive(server::Server)]
#[name = "os/spi"]
pub struct SpiServer {
    claimed_peripherals: HashMap<Peripheral, PID>,
    spi: Spi,
    dma_rx: DmaTransfer,
    dma_tx: DmaTransfer,
}
impl SpiServer {
    pub fn init() -> Self {
        log::debug!("Initializing SPI0");

        // Wait for gpio server to mux SPI0 pins.
        let _gpio = GpioApi::default();

        PowerManagerApi::default()
            .enable_peripheral(atsama5d27::pmc::PeripheralId::Spi0)
            .expect("Could not enable SPI clock");

        let mem = xous::map_memory(
            xous::MemoryAddress::new(HW_SPI0_BASE),
            None,
            0x1000,
            xous::MemoryFlags::W | xous::MemoryFlags::DEV,
        )
        .expect("map SPI0");
        let addr = mem.as_ptr() as u32;
        log::debug!("Mapped SPI0 to 0x{:08x}", addr);

        let mut spi = Spi::with_alt_base_addr(addr);
        spi.init();
        spi.master_enable(true);
        spi.set_enabled(true);

        log::debug!("Initialized SPI0 master");

        log::debug!("Initializing DMA");
        let dma = Dma::default();
        let dma_rx = dma
            .peripheral_transfer(spi.dma_rx_addr(), Spi::RX_DMA_CONFIG_SPI0_8)
            .expect("Could not get DMA RX channel");
        let dma_tx = dma
            .peripheral_transfer(spi.dma_tx_addr(), Spi::TX_DMA_CONFIG_SPI0_8)
            .expect("Could not get DMA TX channel");

        Self { claimed_peripherals: Default::default(), spi, dma_rx, dma_tx }
    }

    fn bulk_transfer(
        peripheral: &Peripheral,
        mut buffer: xous::MemoryRange,
        spi: &mut Spi,
        dma_rx: &DmaTransfer,
        dma_tx: &DmaTransfer,
    ) -> Result<usize, SpiError> {
        // This is a rough heuristic. We can do about 10K DMA requests per second, so if the peripheral
        // can do e.g. 2Mhz, i.e. 250kBps, DMA is worth it above 25 bytes
        let dma_threshold = peripheral.bitrate() as usize / 8 / 10000;
        if peripheral.bit_per_transfer() == BitsPerTransfer::Bits8 && buffer.len() > dma_threshold {
            log::trace!("Using DMA for buffer: {:02x?}", buffer.as_slice::<u8>());
            xous::flush_cache(buffer, xous::CacheOperation::CleanAndInvalidate)?;
            unsafe {
                dma_rx.execute(buffer)?;
                dma_tx.execute(buffer)?;
            };
            let bytes = dma_rx.wait()?;
            dma_tx.wait()?;
            xous::flush_cache(buffer, xous::CacheOperation::Invalidate)?;
            log::trace!("Result (bytes={bytes}): {:02x?}", buffer.as_slice::<u8>());
            Ok(bytes)
        } else {
            if peripheral.bit_per_transfer() == BitsPerTransfer::Bits8 {
                for word in buffer.as_slice_mut::<u8>() {
                    log::trace!("Writing u8 {:02x}", *word);
                    spi.write_8(*word)?;
                    *word = spi.read_8()?;
                    log::trace!("Read u8 {:02x}", *word);
                }
            } else {
                for word in buffer.as_slice_mut::<u16>() {
                    log::trace!("Writing u16 {:04x}", *word);
                    spi.write_16(*word)?;
                    *word = spi.read_16()?;
                    log::trace!("Read u16 {:04x}", *word);
                }
            }
            Ok(buffer.len())
        }
    }

    fn xfer(&mut self, pid: PID, msg: SpiXfer) -> Result<usize, SpiError> {
        let peripheral = msg.peripheral;
        let buffer = msg.buffer.subrange(0, msg.bytes).ok_or(SpiError::MessageTooLong)?;
        log::trace!("PID={pid} xfer {} bytes to {peripheral:?}", buffer.len());

        self.spi.with_cs(peripheral.cs(), |spi| {
            Self::bulk_transfer(&peripheral, buffer, spi, &self.dma_rx, &self.dma_tx)
        })
    }

    fn st25r95_read_data(&mut self, pid: PID, msg: St25r95ReadData) -> Result<usize, SpiError> {
        let peripheral = msg.peripheral;
        self.spi.with_cs(peripheral.cs(), |spi| {
            spi.write_8(st25r95::Control::Read as u8)?;
            let _ = spi.read_8()?;
            spi.write_8(0)?;
            let resp_b0 = spi.read_8()?;
            spi.write_8(0)?;
            let resp_b1 = spi.read_8()?;
            let (code, data_len) = if resp_b0 == st25r95::Command::Echo as u8 {
                (st25r95::Command::Echo as u8, 1)
            } else {
                (
                    st25r95::ReadResponse::code(resp_b0),
                    st25r95::ReadResponse::data_len([resp_b0, resp_b1]).min(st25r95::MAX_BUFFER_SIZE),
                )
            };
            if data_len == 0 {
                return Ok(code as usize);
            }
            let buffer = msg.buffer.subrange(0, data_len).ok_or(SpiError::MessageTooLong)?;
            log::trace!("PID={pid} st25r95read_data {data_len} bytes");

            Self::bulk_transfer(&peripheral, buffer, spi, &self.dma_rx, &self.dma_tx)?;
            Ok(data_len << 8 | code as usize)
        })
    }

    fn nrf_read_data(&mut self, pid: PID, msg: NrfReadData) -> Result<usize, SpiError> {
        let peripheral = msg.peripheral;
        let mut loops = 0;
        let timeout_start = Instant::now();
        // We poll the response here by seeing if we get anything but zeroes.
        // This usually works first try. Some rare commands are slower, e.g.
        // ChallengeRequest takes 2ms, BootFirmware takes 4-600ms
        loop {
            let len = self.spi.with_cs(peripheral.cs(), |spi| -> Result<usize, SpiError> {
                spi.write_8(0)?;
                let len_0 = spi.read_8()?;
                spi.write_8(0)?;
                let len_1 = spi.read_8()?;
                let len = u16::from_le_bytes([len_1, len_0]);

                // If we only received 0s, it means there wasn't any transfer active.
                if len == 0 {
                    return Ok(0);
                }

                // If we receive 0x51 or 0x69, a transfer was active, but with 0
                // buffer size, i.e. the BLE firmware was expecting a command
                // Do another loop to receive the likely error that this caused.
                if len == 0x5151 || len == 0x6969 {
                    log::warn!("ORC bytes received as length");
                    return Ok(0);
                }

                log::trace!("Raw len: {len:04x} {len_0} {len_1}");
                let mut buffer =
                    msg.buffer.subrange(0, (len as usize).min(msg.bytes)).ok_or(SpiError::MessageTooLong)?;

                // Send a known pattern over MOSI instead of whatever junk we have
                // in the supplied buffer.
                buffer.as_slice_mut().fill(0xAA);

                log::trace!("PID={pid} nrf_read_data {} bytes", buffer.len());
                Self::bulk_transfer(&peripheral, buffer, spi, &self.dma_rx, &self.dma_tx)
            })?;
            if len > 0 {
                log::trace!("Responded to in {:?} ({loops:?} loops), len={len}", timeout_start.elapsed());
                return Ok(len);
            }
            loops += 1;
            if loops > 500 {
                // Looks like this is a slower operation. Let other processes run.
                std::thread::sleep(Duration::from_millis(loops - 500));
                if timeout_start.elapsed() > Duration::from_millis(msg.timeout_ms as u64) {
                    log::trace!("Timed out");
                    return Err(SpiError::Timeout);
                }
            }
        }
    }

    fn check_claim(&self, peripheral: Peripheral, pid: PID) -> Result<(), SpiError> {
        let claimed_by = self.claimed_peripherals.get(&peripheral).ok_or(SpiError::PeripheralNotClaimed)?;

        if *claimed_by != pid {
            log::error!("PID={pid} tried to access {peripheral:?} that's claimed by PID={claimed_by:}",);
            return Err(SpiError::AccessDenied);
        }
        Ok(())
    }
}

impl Server for SpiServer {}

impl BlockingScalarHandler<ClaimPeripheral> for SpiServer {
    fn handle(
        &mut self,
        ClaimPeripheral(peripheral): ClaimPeripheral,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), SpiError> {
        log::debug!("PID={sender:} tries to claim {peripheral:?} SPI peripheral");

        if self.claimed_peripherals.contains_key(&peripheral) {
            log::error!("{peripheral:?} is already claimed");
            return Err(SpiError::AlreadyClaimed);
        }

        self.claimed_peripherals.insert(peripheral, sender);
        log::debug!("{peripheral:?} is now claimed by PID={sender:}");

        self.spi.init_cs(
            peripheral.cs(),
            peripheral.bit_per_transfer(),
            atsama5d27::spi::SpiMode::Mode0,
            true,
        );
        self.spi.set_bitrate(MASTER_CLOCK_SPEED, peripheral.cs(), peripheral.bitrate());
        self.spi.set_dlybs(MASTER_CLOCK_SPEED, peripheral.cs(), peripheral.dlybs());

        Ok(())
    }
}

impl LendMutHandler<SpiXfer> for SpiServer {
    fn handle(
        &mut self,
        msg: SpiXfer,
        sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <SpiXfer as server::LendMut>::Response {
        let peripheral = msg.peripheral;

        self.check_claim(peripheral, sender)?;
        self.xfer(sender, msg)
    }
}

impl LendMutHandler<St25r95ReadData> for SpiServer {
    fn handle(
        &mut self,
        msg: St25r95ReadData,
        sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <St25r95ReadData as server::LendMut>::Response {
        let peripheral = msg.peripheral;

        self.check_claim(peripheral, sender)?;
        if peripheral != Peripheral::Nfc {
            return Err(SpiError::InvalidPeripheral);
        }
        self.st25r95_read_data(sender, msg)
    }
}

impl LendMutHandler<NrfReadData> for SpiServer {
    fn handle(
        &mut self,
        msg: NrfReadData,
        sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <NrfReadData as server::LendMut>::Response {
        let peripheral = msg.peripheral;

        self.check_claim(peripheral, sender)?;
        if peripheral != Peripheral::Ble {
            return Err(SpiError::InvalidPeripheral);
        }
        self.nrf_read_data(sender, msg)
    }
}
