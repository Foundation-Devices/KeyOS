#![cfg_attr(target_os = "none", no_std)]

pub mod api;
use core::num::NonZeroUsize;

use num_derive::FromPrimitive;
use num_traits::*;
use xous::{send_message, MemoryAddress, CID};

#[derive(Debug)]
pub struct Trng {
    conn: CID,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive)]
pub enum TrngSource {
    Combined = 0,
    Mcu = 1,
    Avalanche = 2,
}

impl Trng {
    /// Connect to the TRNG server, a mandatory base-tier service every process
    /// depends on transitively through `getrandom`. Failure to connect means
    /// the system is already unable to boot, so this does not return a
    /// `Result` for callers to recover from.
    pub fn new() -> Self {
        let conn = xous::connect(xous::SID::from_bytes(api::SERVER_NAME_TRNG).unwrap())
            .expect("could not connect to the TRNG server");
        Trng { conn }
    }

    pub fn get_u32(&self) -> Result<u32, xous::Error> { Ok(self.get_u64()? as u32) }

    pub fn get_u64(&self) -> Result<u64, xous::Error> {
        let response = send_message(
            self.conn,
            xous::Message::new_blocking_scalar(api::Opcode::GetTrng.to_usize().unwrap(), 0, 0, 0, 0),
        )
        .expect("TRNG|LIB: can't get_u32");
        if let xous::Result::Scalar2(lo, hi) = response {
            Ok(lo as u64 | ((hi as u64) << 32))
        } else {
            panic!("unexpected return value: {:#?}", response);
        }
    }

    pub fn fill_buf(&self, data: &mut [u32], source: TrngSource) -> Result<(), xous::Error> {
        // Manually mapping memory instead of using a Buffer, because we want to
        // set the `valid` field to the data length in the Message
        let aligned_buffer =
            xous::map_memory(None, None, (data.len() * 4).next_multiple_of(4096), xous::MemoryFlags::W)?;
        let result = xous::send_message(
            self.conn,
            xous::Message::MutableBorrow(xous::MemoryMessage {
                id: api::Opcode::FillTrng.to_usize().unwrap(),
                buf: aligned_buffer,
                offset: MemoryAddress::new(source as usize),
                valid: NonZeroUsize::new(data.len()),
            }),
        )
        .map(|_| ());
        if result.is_ok() {
            data.copy_from_slice(&aligned_buffer.as_slice()[0..data.len()]);
        }
        xous::unmap_memory(aligned_buffer)?;
        result
    }

    /// Fill `data` with raw 12-bit avalanche ADC samples for source diagnostics.
    ///
    /// Each returned `u32` contains one raw ADC sample in bits 0..=11. This is
    /// intentionally separate from [`TrngSource`] because these values are not
    /// conditioned random output.
    pub fn fill_avalanche_raw_samples(&self, data: &mut [u32]) -> Result<(), xous::Error> {
        let aligned_buffer =
            xous::map_memory(None, None, (data.len() * 4).next_multiple_of(4096), xous::MemoryFlags::W)?;
        let result = xous::send_message(
            self.conn,
            xous::Message::MutableBorrow(xous::MemoryMessage {
                id: api::Opcode::FillAvalancheRaw.to_usize().unwrap(),
                buf: aligned_buffer,
                offset: None,
                valid: NonZeroUsize::new(data.len()),
            }),
        )
        .map(|_| ());
        if result.is_ok() {
            data.copy_from_slice(&aligned_buffer.as_slice()[0..data.len()]);
        }
        xous::unmap_memory(aligned_buffer)?;
        result
    }
}
