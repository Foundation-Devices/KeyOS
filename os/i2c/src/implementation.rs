// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::VecDeque;
use std::time::Duration;

use atsama5d27::twi::TWIStatus;
use i2c::messages::*;
use i2c::{I2cError, Peripheral};
use server::{
    ArchiveRequest, BlockingArchiveAsyncHandler, CheckedPermissions, MessageAllowed, ScalarHandler,
};
use xous::arch::irq::IrqNumber;
use xous::MemoryRange;
use {
    atsama5d27::twi::Twi,
    server::{BlockingScalarHandler, Server, ServerContext},
    std::collections::HashMap,
    utralib::HW_TWIHS0_BASE,
    xous::{keyos::MASTER_CLOCK_SPEED, PID},
};

gpio::use_api!();
dma::use_api!();

/// TWI (I2C) bus speed.
const TWI_BUS_SPEED_HZ: usize = 100_000;

#[derive(server::Server)]
#[name = "os/i2c"]
pub struct I2cServer {
    claimed_peripherals: HashMap<Peripheral, PID>,
    twi: Twi,
    ongoing_transfer: Option<(Box<[u8]>, ArchiveRequest<SingleTransfer>)>,
    queue: VecDeque<ArchiveRequest<SingleTransfer>>,
    dma_rx: DmaTransfer,
}

#[derive(Debug, server::Message)]
pub struct TransferCompleteMsg(pub u32);

#[derive(Default, Clone)]
struct InterruptConnection;

impl CheckedPermissions for InterruptConnection {
    const NAME: &str = "os/i2c";
}

impl MessageAllowed<TransferCompleteMsg> for InterruptConnection {}

struct InterruptContext {
    conn: server::CheckedConn<InterruptConnection>,
    twi: Twi,
}

impl I2cServer {
    pub fn new() -> Self {
        log::debug!("Initializing TWI0");

        // Wait for gpio server to mux TWI pins.
        let _gpio = GpioApi::default();

        let mem = xous::map_memory(
            xous::MemoryAddress::new(HW_TWIHS0_BASE),
            None,
            0x1000,
            xous::MemoryFlags::W | xous::MemoryFlags::DEV,
        )
        .expect("map TWI0");
        let addr = mem.as_ptr() as u32;
        log::debug!("Mapped TWI0 to 0x{:08x}", addr);

        let twi = Twi::with_base_addr(addr);
        twi.init_master(MASTER_CLOCK_SPEED as usize, TWI_BUS_SPEED_HZ);

        log::debug!("Initialized TWI0 master at {} Hz bus speed", TWI_BUS_SPEED_HZ);

        let dma = Dma::default();
        let dma_rx = dma
            .peripheral_transfer(twi.dma_rx_addr(), Twi::RX_DMA_CONFIG_TWI0)
            .expect("Could not get DMA RX channel");

        Self {
            twi,
            claimed_peripherals: Default::default(),
            ongoing_transfer: None,
            queue: Default::default(),
            dma_rx,
        }
    }

    pub fn run_queue(&mut self) {
        if self.ongoing_transfer.is_some() {
            return;
        }
        while let Some(request) = self.queue.pop_front() {
            self.twi.setup_write_read(
                request.message.peripheral.i2c_addr(),
                request.message.write_data.len(),
                request.message.read_len,
            );
            if let Err(e) = self.twi.send_bytes(&request.message.write_data) {
                request.response.respond(Err(convert_i2c_error(e))).ok();
                continue;
            };
            if request.message.read_len <= 4 {
                let mut buffer = vec![0; request.message.read_len];
                if let Err(e) = self.twi.receive_bytes(&mut buffer) {
                    request.response.respond(Err(convert_i2c_error(e))).ok();
                } else {
                    request.response.respond(Ok(buffer)).ok();
                }
                // There appears to be a minimum wait time, or else we
                // hammer the peripherals too fast and they get confused.
                // On the other branch there is enough overhead to take care
                // of this.
                std::thread::sleep(Duration::from_millis(1));
            } else {
                let buffer: Box<[u8]> = std::iter::repeat(0).take(request.message.read_len).collect();
                let buffer_range =
                    unsafe { MemoryRange::new(buffer.as_ptr() as usize, buffer.len()).unwrap() };
                xous::flush_cache(buffer_range, xous::CacheOperation::Clean).ok();
                unsafe {
                    self.dma_rx.execute(buffer_range).unwrap();
                };
                self.ongoing_transfer = Some((buffer, request));
                self.twi.set_txcomp_interrupt(true);
                self.twi.set_nack_interrupt(true);
                return;
            }
        }
    }
}

impl Server for I2cServer {
    fn on_start(&mut self, _context: &mut ServerContext<Self>) {
        let int_ctx = Box::into_raw(Box::new(InterruptContext {
            conn: Default::default(),
            twi: unsafe { self.twi.clone() },
        }));
        xous::claim_interrupt(IrqNumber::Twi0, twi_irq_handler, int_ctx as *mut usize)
            .expect("Could not claim Twi0 interrupt");
    }
}

impl BlockingScalarHandler<ClaimPeripheral> for I2cServer {
    fn handle(
        &mut self,
        ClaimPeripheral(peripheral): ClaimPeripheral,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), I2cError> {
        log::debug!("PID={sender:} tries to claim {peripheral:?} I2C peripheral");

        if self.claimed_peripherals.contains_key(&peripheral) {
            log::error!("{peripheral:?} is already claimed");
            return Err(I2cError::AlreadyClaimed);
        }

        self.claimed_peripherals.insert(peripheral, sender);
        log::debug!("{peripheral:?} is now claimed by PID={sender:}");

        Ok(())
    }
}

impl BlockingArchiveAsyncHandler<SingleTransfer> for I2cServer {
    fn handle(&mut self, request: ArchiveRequest<SingleTransfer>, _context: &mut ServerContext<Self>) {
        let peripheral = request.message.peripheral;
        let sender = request.response.pid();
        let Some(claimed_by) = self.claimed_peripherals.get(&peripheral) else {
            request.response.respond(Err(I2cError::PeripheralNotClaimed)).ok();
            return;
        };
        if *claimed_by != sender {
            log::error!("PID={sender:} tried to access {peripheral:?} that's claimed by PID={claimed_by:}");
            request.response.respond(Err(I2cError::AccessDenied)).ok();
            return;
        }
        self.queue.push_back(request);

        self.run_queue();
    }

    fn default_response() -> Result<Vec<u8>, I2cError> { Err(I2cError::InternalError) }
}

impl ScalarHandler<TransferCompleteMsg> for I2cServer {
    fn handle(
        &mut self,
        TransferCompleteMsg(status_bits): TransferCompleteMsg,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        let Some((buffer, transfer)) = self.ongoing_transfer.take() else {
            log::error!("TransferComplete without ongoing transfer");
            return;
        };
        let status = TWIStatus::from_bits_retain(status_bits);
        if status.contains(TWIStatus::NACK) {
            // DMA never completes on NACK (peripheral handshake stalls); stop unblocks wait.
            self.dma_rx.stop().ok();
            self.dma_rx.wait().ok();
            transfer.response.respond(Err(I2cError::Nack)).ok();
        } else {
            let rx_bytes = self.dma_rx.wait().unwrap();
            if rx_bytes != transfer.message.read_len {
                log::warn!("Read less than requested: {rx_bytes}<{}", transfer.message.read_len);
            }
            let buffer_range = unsafe { MemoryRange::new(buffer.as_ptr() as usize, buffer.len()).unwrap() };
            xous::flush_cache(buffer_range, xous::CacheOperation::Invalidate).ok();
            transfer.response.respond(Ok(buffer.into())).ok();
        }
        self.run_queue();
    }
}

fn convert_i2c_error(e: atsama5d27::twi::I2cError) -> I2cError {
    match e {
        atsama5d27::twi::I2cError::Nack => I2cError::Nack,
        atsama5d27::twi::I2cError::Timeout => I2cError::Timeout,
    }
}

fn twi_irq_handler(_irq_no: usize, arg: *mut usize) {
    let context = unsafe { &*(arg as *const InterruptContext) };
    let status = context.twi.status();
    if status.intersects(TWIStatus::TXCOMP | TWIStatus::NACK) {
        context.twi.set_txcomp_interrupt(false);
        context.twi.set_nack_interrupt(false);
        if let Err(e) = context.conn.send_scalar_nowait(TransferCompleteMsg(status.bits())) {
            log::error!("Could not send I2C TransferCompleteMsg: {e:?}");
        }
    } else {
        log::warn!("Spurious TWI interrupt");
    }
}
