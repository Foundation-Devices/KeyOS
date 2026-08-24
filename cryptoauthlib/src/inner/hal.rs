// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{iter::once, time::Duration};

use ::std::os::raw::{c_int, c_void};
use atsama5d27::{
    dma::{DmaChunkSize, DmaDataWidth, DmaPeripheralId, DmaPeripheralTransferConfig, DmaTransferDirection},
    flexcom::{ChMode, CharLength, Flexcom, FlexcomStatus, Parity, HW_FLEXCOM2_BASE},
};
use dma::DmaTransfer;
use server::{CheckedPermissions, MessageAllowed};
use xous::{
    arch::irq::IrqNumber,
    keyos::{MASTER_CLOCK_SPEED, PAGE_SIZE},
    map_memory, MemoryAddress, MemoryFlags, MemoryRange,
};

use super::bindings::{self, ATCAIfaceType, ATCA_NOT_INITIALIZED, ATCA_UNIMPLEMENTED};
use crate::{
    inner::bindings::{
        ATCAIface, ATCAIfaceCfg, ATCA_ASSERT_FAILURE, ATCA_COMM_FAIL, ATCA_STATUS, ATCA_SUCCESS,
    },
    Error,
};

pub const INTERFACE_TYPE: ATCAIfaceType = bindings::ATCAIfaceType_ATCA_SWI_IFACE;

const BIT_ONE: u8 = 0x7f;
const BIT_ZERO: u8 = 0x7d;
const BAUD_RATE: u32 = 230400;
const WAKE_PULSE_BAUD_RATE: u32 = 115200;

// The RX buffer is started on select() and gets filled throughout the execution, so only a fixed
// number of bytes can be sent/received in a single select() cycle. This is to prevent any costly
// copies, allocations, etc. between stopping and restarting the DMA opration.
// The buffer stores bits as bytes (as this is how they are sent over UART)
// 32Kbits, i.e. max 4KBytes transmitted per select/deselect
const BUFFER_SIZE: usize = 0x20000;
const MAX_SEND_SIZE: usize = 0x1000; // in decoded bytes

static mut HAL: Option<Hal> = None;

// Manually declare the permissions, as we don't have a manifest.toml,
// since this is a library.
// The library using code is forced to have these same permissions
// by requiring them at the init() call.
#[derive(Default, Clone)]
struct DmaPermissions;

impl CheckedPermissions for DmaPermissions {
    const NAME: &str = "os/dma";
}
impl MessageAllowed<dma::messages::PeripheralTransferMsg> for DmaPermissions {}
impl MessageAllowed<dma::messages::DropTransferMsg> for DmaPermissions {}
impl MessageAllowed<dma::messages::ExecuteTransferMsg> for DmaPermissions {}
impl MessageAllowed<dma::messages::WaitTransferMsg> for DmaPermissions {}
impl MessageAllowed<dma::messages::StopTransferMsg> for DmaPermissions {}
impl MessageAllowed<dma::messages::FlushTransferMsg> for DmaPermissions {}

static HAL_FUNCTIONS: bindings::ATCAHAL_t = bindings::ATCAHAL_t {
    halinit: Some(halinit),
    halpostinit: Some(halpostinit),
    halsend: Some(halsend),
    halreceive: Some(halreceive),
    halcontrol: Some(halcontrol),
    halrelease: Some(halrelease),
};

pub struct Hal {
    flexcom: Flexcom,
    tx_buffer: MemoryRange,
    rx_buffer: MemoryRange,
    tx_dma: DmaTransfer<DmaPermissions>,
    rx_dma: DmaTransfer<DmaPermissions>,
    recv_offset: usize,
    received: usize,
}

fn with_hal(f: impl FnOnce(&mut Hal) -> Result<(), Error>) -> ATCA_STATUS {
    if let Some(hal) = unsafe { (*core::ptr::addr_of_mut!(HAL)).as_mut() } {
        match f(hal) {
            Ok(()) => ATCA_SUCCESS as ATCA_STATUS,
            Err(e) => e.status,
        }
    } else {
        ATCA_NOT_INITIALIZED as ATCA_STATUS
    }
}

impl Hal {
    pub fn init<P>(_perimissions: P) -> Result<(), Error>
    where
        P: MessageAllowed<dma::messages::PeripheralTransferMsg>,
        P: MessageAllowed<dma::messages::DropTransferMsg>,
        P: MessageAllowed<dma::messages::ExecuteTransferMsg>,
        P: MessageAllowed<dma::messages::WaitTransferMsg>,
        P: MessageAllowed<dma::messages::StopTransferMsg>,
        P: MessageAllowed<dma::messages::FlushTransferMsg>,
    {
        unsafe { HAL = Some(Hal::new()?) }
        let status = unsafe {
            bindings::hal_iface_register_hal(
                INTERFACE_TYPE,
                &HAL_FUNCTIONS as *const _ as *mut _,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            )
        };
        if status == ATCA_SUCCESS as ATCA_STATUS {
            Ok(())
        } else {
            Err(Error { status })
        }
    }

    fn new() -> Result<Self, Error> {
        let mem = map_memory(
            MemoryAddress::new(HW_FLEXCOM2_BASE),
            None,
            PAGE_SIZE, // FLEXCOM's last 32-bit register is at offset 0x6e8, so all registers fit one page
            MemoryFlags::W | MemoryFlags::DEV,
        )
        .expect("map FLEXCOM");
        let addr = mem.as_ptr() as u32;
        log::info!("Mapped Flexcom to 0x{:08x}", addr);
        let mut flexcom = Flexcom::with_base_addr(addr);
        flexcom.init_usart(
            MASTER_CLOCK_SPEED,
            BAUD_RATE,
            atsama5d27::flexcom::UsartMode::Normal,
            atsama5d27::flexcom::ClockSource::Mck,
        );
        flexcom.set_parity(Parity::No);
        flexcom.set_ch_mode(ChMode::Normal);
        flexcom.set_char_length(CharLength::SevenBit);
        flexcom.enable_fifo(true);
        flexcom.set_tx(true);
        flexcom.set_rx(true);
        flexcom.enable_overrun_interrupt();

        xous::claim_interrupt(IrqNumber::Flexcom2, interrupt_handler, core::ptr::null_mut())?;

        let tx_buffer = map_memory(None, None, BUFFER_SIZE, MemoryFlags::W | MemoryFlags::POPULATE)?;
        let rx_buffer = map_memory(None, None, BUFFER_SIZE, MemoryFlags::W | MemoryFlags::POPULATE)?;
        let dma = dma::Dma::default();
        let tx_dma = dma.peripheral_transfer(
            flexcom.dma_tx_addr(),
            DmaPeripheralTransferConfig {
                peripheral_id: DmaPeripheralId::Flexcom2Tx,
                direction: DmaTransferDirection::MemoryToPeripheral,
                data_width: DmaDataWidth::D8,
                chunk_size: DmaChunkSize::C1,
            },
        )?;
        let rx_dma = dma.peripheral_transfer(
            flexcom.dma_rx_addr(),
            DmaPeripheralTransferConfig {
                peripheral_id: DmaPeripheralId::Flexcom2Rx,
                direction: DmaTransferDirection::PeripheralToMemory,
                data_width: DmaDataWidth::D8,
                chunk_size: DmaChunkSize::C1,
            },
        )?;

        Ok(Self { flexcom, tx_buffer, rx_buffer, tx_dma, rx_dma, recv_offset: 0, received: 0 })
    }

    fn send(&mut self, address: u8, data: &[u8]) -> Result<(), Error> {
        log::trace!("Sending {} bytes to 0x{address:02x}", data.len());
        let transfer_len = (data.len() + 1) * 8;
        if transfer_len > MAX_SEND_SIZE {
            log::error!("Data to send is too big: {transfer_len} bytes");
            return Err(Error { status: ATCA_ASSERT_FAILURE });
        }
        for (i, byte) in once(&address).chain(data.iter()).enumerate() {
            encode_byte(*byte, &mut self.tx_buffer.as_slice_mut()[i * 8..]);
        }
        let tx_range = self.tx_buffer.subrange(0, transfer_len).unwrap();
        xous::flush_cache(tx_range, xous::CacheOperation::Clean)?;
        unsafe {
            self.tx_dma.execute(tx_range)?;
        }
        self.tx_dma.wait()?;

        // TX and RX are shorted together, so drain the echoed bytes
        let mut echo_buffer = [0u8; MAX_SEND_SIZE];
        self.receive(&mut echo_buffer[..data.len() + 1])?;
        if echo_buffer[0] != address || &echo_buffer[1..data.len() + 1] != data {
            log::error!(
                "Probable electrical issue, received != sent: {:02x?} != {address:02x} {:02x?}",
                &echo_buffer[..data.len()],
                data
            );
            Err(Error { status: ATCA_COMM_FAIL as ATCA_STATUS })
        } else {
            Ok(())
        }
    }

    fn start_rx_dma(&mut self, len: Option<usize>) -> Result<(), Error> {
        let buffer_part = self
            .rx_buffer
            .subrange(self.received, len.unwrap_or(self.rx_buffer.len() - self.received))
            .ok_or_else(|| {
                log::error!("Buffer overrun on {len:?} bytes.");
                Error { status: ATCA_ASSERT_FAILURE }
            })?;
        unsafe {
            self.rx_dma.execute(buffer_part)?;
        }
        Ok(())
    }

    fn receive(&mut self, data: &mut [u8]) -> Result<usize, Error> {
        log::trace!("Receiving {} bytes", data.len());
        let transfer_len = data.len() * 8;

        // The trick with the receive DMA is that the DMA is always running with the full buffer,
        // and we only check how many bytes it has received already. This is because the device
        // usually sends everything at once, while `calib` uses 2-3 read operations per actual
        // execution.
        self.received = self.rx_dma.flush()?;
        let available = self.received - self.recv_offset;

        if transfer_len > available {
            // We don't have enough bits, let's wait for them.
            // Components of the calculation, according top the datasheet:
            // - maximum time from the initial falling edge of the last MCU => SE bit to the initial falling
            //   edge of the SE => MCU bit is 131 us
            // - maximum time between two bits coming from the SE is 78 us.
            let wait_us = 131 + 78 * (transfer_len - available) as u64;
            // Our sleep granularity is 1ms
            std::thread::sleep(std::time::Duration::from_millis(wait_us.next_multiple_of(1000) / 1000));
            self.received = self.rx_dma.flush()?;
        }
        let received_bits = (self.received - self.recv_offset).min(transfer_len);
        let received_bytes = received_bits / 8;
        if received_bits > 0 {
            let buffer_part = self.rx_buffer.subrange(self.recv_offset, received_bits).unwrap();
            xous::flush_cache(buffer_part, xous::CacheOperation::Invalidate)?;

            for (i, byte) in data[..received_bytes].iter_mut().enumerate() {
                *byte = decode_byte(&buffer_part.as_slice::<u8>()[i * 8..]);
            }
            self.recv_offset += received_bits;
        }

        log::trace!("Received {}/{}: {:02x?}", received_bytes, data.len(), &data[..received_bytes]);
        Ok(received_bytes)
    }

    fn wake(&mut self) -> Result<(), Error> {
        log::trace!("Sending wakeup pulse");
        // The wakeup pulse is a pulse that's longer than a normal bit to signify a start of transmission.
        // This is why we need to adjust the baud rate first.
        self.flexcom.set_baud(MASTER_CLOCK_SPEED, WAKE_PULSE_BAUD_RATE);
        self.flexcom.write_byte(bindings::CALIB_SWI_FLAG_WAKE as u8)?;
        self.flexcom.set_baud(MASTER_CLOCK_SPEED, BAUD_RATE);

        // The minimum is wakeup time from sleep is 1500uS according to the datasheet.
        std::thread::sleep(Duration::from_millis(2));

        self.select()?;
        self.send(0xff, &[bindings::CALIB_SWI_FLAG_TX as u8])?;
        let mut wake_buffer = [0u8; 4];
        self.receive(&mut wake_buffer)?;
        self.deselect()?;

        let status = unsafe { bindings::hal_check_wake(wake_buffer.as_ptr(), wake_buffer.len() as _) };
        if status == ATCA_SUCCESS as ATCA_STATUS {
            Ok(())
        } else {
            Err(Error { status })
        }
    }

    fn idle(&mut self) -> Result<(), Error> {
        log::trace!("Sending idle");
        let mut idle_bits = [0u8; 8];
        encode_byte(bindings::CALIB_SWI_FLAG_IDLE as u8, &mut idle_bits);
        for bit in idle_bits {
            self.flexcom.write_byte(bit)?;
        }
        Ok(())
    }

    fn sleep(&mut self) -> Result<(), Error> {
        log::trace!("Sending sleep");
        let mut idle_bits = [0u8; 8];
        encode_byte(bindings::CALIB_SWI_FLAG_SLEEP as u8, &mut idle_bits);
        for bit in idle_bits {
            self.flexcom.write_byte(bit)?;
        }
        Ok(())
    }

    fn select(&mut self) -> Result<(), Error> {
        log::trace!("Selected");
        self.flexcom.flush();
        self.received = 0;
        self.recv_offset = 0;
        self.start_rx_dma(None)?;
        Ok(())
    }

    fn deselect(&mut self) -> Result<(), Error> {
        log::trace!("Deselected");
        self.rx_dma.stop()?;
        self.rx_dma.wait()?;
        Ok(())
    }

    fn interrupt(&mut self) {
        let status = self.flexcom.status();
        if status.contains(FlexcomStatus::OVRE) {
            log::warn!("RX overrun");
            self.flexcom.reset_status();
        }
    }
}

fn interrupt_handler(_irq_no: usize, _arg: *mut usize) {
    if let Some(hal) = unsafe { (*core::ptr::addr_of_mut!(HAL)).as_mut() } {
        hal.interrupt();
    }
}

fn encode_byte(byte: u8, buffer: &mut [u8]) {
    assert!(buffer.len() >= 8);
    for bit in 0..8 {
        buffer[bit] = if (1 << bit) & byte != 0 { BIT_ONE } else { BIT_ZERO }
    }
}

fn decode_byte(buffer: &[u8]) -> u8 {
    assert!(buffer.len() >= 8);
    let mut byte = 0;
    for bit in 0..8 {
        if buffer[bit] == BIT_ONE {
            byte |= 1 << bit;
        }
    }
    byte
}

#[no_mangle]
extern "C" fn hal_delay_ms(delay: u32) { std::thread::sleep(Duration::from_millis(delay as u64)); }

#[no_mangle]
extern "C" fn hal_delay_us(delay: u32) {
    log::warn!("Delay us {delay} (unimplemented!)");
}

unsafe extern "C" fn halinit(_iface: ATCAIface, _cfg: *mut ATCAIfaceCfg) -> ATCA_STATUS {
    ATCA_SUCCESS as ATCA_STATUS
}

unsafe extern "C" fn halpostinit(_iface: ATCAIface) -> ATCA_STATUS { ATCA_SUCCESS as ATCA_STATUS }
unsafe extern "C" fn halsend(
    _iface: ATCAIface,
    word_address: u8,
    txdata: *mut u8,
    txlength: c_int,
) -> ATCA_STATUS {
    with_hal(|hal| hal.send(word_address, core::slice::from_raw_parts_mut(txdata, txlength as _)))
}
unsafe extern "C" fn halreceive(
    _iface: ATCAIface,
    _word_address: u8,
    rxdata: *mut u8,
    rxlength: *mut u16,
) -> ATCA_STATUS {
    with_hal(|hal| {
        *rxlength = hal.receive(core::slice::from_raw_parts_mut(rxdata, *rxlength as _))? as u16;
        Ok(())
    })
}
unsafe extern "C" fn halcontrol(
    _iface: ATCAIface,
    option: u8,
    _param: *mut c_void,
    _paramlen: usize,
) -> ATCA_STATUS {
    match option as u32 {
        bindings::ATCA_HAL_CONTROL_WAKE => with_hal(|hal| hal.wake()),
        bindings::ATCA_HAL_CONTROL_IDLE => with_hal(|hal| hal.idle()),
        bindings::ATCA_HAL_CONTROL_SLEEP => with_hal(|hal| hal.sleep()),
        bindings::ATCA_HAL_CONTROL_SELECT => with_hal(|hal| hal.select()),
        bindings::ATCA_HAL_CONTROL_DESELECT => with_hal(|hal| hal.deselect()),
        bindings::ATCA_HAL_CHANGE_BAUD => ATCA_SUCCESS as ATCA_STATUS,
        _ => ATCA_UNIMPLEMENTED,
    }
}
unsafe extern "C" fn halrelease(_hal_data: *mut c_void) -> ATCA_STATUS { ATCA_SUCCESS as ATCA_STATUS }
