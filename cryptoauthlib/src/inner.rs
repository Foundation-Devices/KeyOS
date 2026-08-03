// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod hal;

#[allow(dead_code)]
#[allow(non_upper_case_globals)]
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
#[allow(clippy::upper_case_acronyms)]
mod bindings;

use std::ptr::addr_of_mut;

pub use bindings::{ATCA_CHECKMAC_VERIFY_FAILED, ATCA_STATUS};
use server::MessageAllowed;

pub struct Device(bindings::ATCADevice);

static mut CONFIG: bindings::ATCAIfaceCfg = {
    let mut config: bindings::ATCAIfaceCfg = unsafe { std::mem::MaybeUninit::zeroed().assume_init() };
    config.iface_type = hal::INTERFACE_TYPE;
    config.devtype = bindings::ATECC608B as u8;
    config.wake_delay = 1500;
    config.rx_retries = 10;
    config
};

impl Device {
    pub fn init<P>(perimissions: P) -> Result<Self, Error>
    where
        P: MessageAllowed<dma::messages::PeripheralTransferMsg>,
        P: MessageAllowed<dma::messages::DropTransferMsg>,
        P: MessageAllowed<dma::messages::ExecuteTransferMsg>,
        P: MessageAllowed<dma::messages::WaitTransferMsg>,
        P: MessageAllowed<dma::messages::StopTransferMsg>,
        P: MessageAllowed<dma::messages::FlushTransferMsg>,
    {
        hal::Hal::init(perimissions)?;
        let mut device = std::ptr::null_mut();
        let status = unsafe { bindings::atcab_init_ext(&mut device, addr_of_mut!(CONFIG)) };
        handle_error(status)?;
        Ok(Self(device))
    }

    pub fn device_info(&self) -> Result<[u8; 4], Error> {
        let mut info = [0; 4];
        let status = unsafe { bindings::calib_info(self.0, info.as_mut_ptr()) };
        handle_error(status)?;
        Ok(info)
    }

    pub fn self_test(&self) -> Result<bool, Error> {
        let mut result = 42;
        let status = unsafe { bindings::calib_selftest(self.0, 0x3B, 0, std::ptr::addr_of_mut!(result)) };
        handle_error(status)?;
        Ok(result == 0)
    }

    pub unsafe fn sha_base(
        &self,
        mode: u8,
        length: u16,
        data_in: *const u8,
        data_out: *mut u8,
        data_out_size: *mut u16,
    ) -> Result<(), Error> {
        let status =
            unsafe { bindings::calib_sha_base(self.0, mode, length, data_in, data_out, data_out_size) };
        handle_error(status)
    }

    pub fn counter_read(&self, counter_id: u16) -> Result<u32, Error> {
        let mut result = 42;
        let status =
            unsafe { bindings::calib_counter_read(self.0, counter_id, std::ptr::addr_of_mut!(result)) };
        handle_error(status)?;
        Ok(result)
    }

    pub fn counter_increment(&self, counter_id: u16) -> Result<u32, Error> {
        let mut result = 42;
        let status =
            unsafe { bindings::calib_counter_increment(self.0, counter_id, std::ptr::addr_of_mut!(result)) };
        handle_error(status)?;
        Ok(result)
    }

    pub fn read_zone(
        &self,
        zone: u8,
        slot: u16,
        block: u8,
        offset: u8,
        data: &mut [u8],
    ) -> Result<(), Error> {
        let status = unsafe {
            bindings::calib_read_zone(self.0, zone, slot, block, offset, data.as_mut_ptr(), data.len() as _)
        };
        handle_error(status)
    }

    pub fn read_enc(
        &self,
        key_id: u16,
        block: u8,
        data: &mut [u8],
        enc_key: &[u8],
        enc_key_id: u16,
        num_in: &[u8; 20],
    ) -> Result<(), Error> {
        let status = unsafe {
            bindings::calib_read_enc(
                self.0,
                key_id,
                block,
                data.as_mut_ptr(),
                enc_key.as_ptr(),
                enc_key_id,
                num_in,
            )
        };
        handle_error(status)
    }

    pub fn write(&self, zone: u8, address: u16, value: &[u8], mac: Option<&[u8]>) -> Result<(), Error> {
        let status = unsafe {
            bindings::calib_write(
                self.0,
                zone,
                address,
                value.as_ptr(),
                mac.map_or(std::ptr::null(), |m| m.as_ptr()),
            )
        };
        handle_error(status)
    }

    /// Data must be either 4 or 32 bytes
    pub fn write_zone(&self, zone: u8, slot: u16, block: u8, offset: u8, data: &[u8]) -> Result<(), Error> {
        let status = unsafe {
            bindings::calib_write_zone(self.0, zone, slot, block, offset, data.as_ptr(), data.len() as _)
        };
        handle_error(status)
    }

    pub fn lock_data_slot(&self, slot: u16) -> Result<(), Error> {
        let status = unsafe { bindings::calib_lock_data_slot(self.0, slot) };
        handle_error(status)
    }

    pub fn lock_data_zone(&self) -> Result<(), Error> {
        let status = unsafe { bindings::calib_lock_data_zone(self.0) };
        handle_error(status)
    }

    pub fn mac(&self, mode: u8, key_id: u16, challenge: Option<&[u8]>) -> Result<[u8; 32], Error> {
        let mut digest = [0; 32];
        let status = unsafe {
            bindings::calib_mac(
                self.0,
                mode,
                key_id,
                challenge.map_or(std::ptr::null(), |c| c.as_ptr()),
                digest.as_mut_ptr(),
            )
        };
        handle_error(status)?;
        Ok(digest)
    }

    pub fn checkmac(
        &self,
        mode: u8,
        key_id: u16,
        challenge: &[u8],
        response: &[u8],
        other_data: &[u8],
    ) -> Result<(), Error> {
        let status = unsafe {
            bindings::calib_checkmac(
                self.0,
                mode,
                key_id,
                challenge.as_ptr(),
                response.as_ptr(),
                other_data.as_ptr(),
            )
        };
        handle_error(status)
    }

    /// Read 32 bytes from the device's internal random number generator.
    ///
    /// This does not wear out any EEPROM: on the ATECC608B, the Random
    /// command's mode byte (which selects "update seed" vs. "no seed update"
    /// on older CryptoAuthentication parts) is documented as ignored, and its
    /// execution time -- ~7 ms for a repeat call within one wake cycle, ~13 ms
    /// for the first, against ~18-19 ms for a real EEPROM `Write` and ~1 ms for
    /// `Read`/`Sha` -- doesn't fit a command that includes an EEPROM program
    /// cycle either. The ~7 ms floor is consistent with the noise-source
    /// sampling and SP 800-90B health testing the datasheet says run "at the
    /// time of use" on every call, and the first-call delta with the one-time
    /// NIST DRBG instantiation that reruns on every wake/power cycle
    /// (confirmed by the datasheet's timing table notes) -- not with any
    /// per-call persistence.
    ///
    /// Note that the device returns a fixed value instead of random data while
    /// its configuration zone is unlocked, so callers must not treat this as
    /// the sole source of entropy
    pub fn random(&self) -> Result<[u8; 32], Error> {
        let mut rand_out = [0; 32];
        // SAFETY: `self.0` was returned by `atcab_init_ext` in `Device::init` and is only
        // ever handed out by this type once that call succeeded, so it is a valid,
        // initialized device handle. `rand_out.as_mut_ptr()` is valid for a 32-byte write,
        // matching the fixed `RANDOM_NUM_SIZE` the C side `memcpy`s into it.
        let status = unsafe { bindings::atcab_random_ext(self.0, rand_out.as_mut_ptr()) };
        handle_error(status)?;
        Ok(rand_out)
    }

    pub fn nonce_rand(&self, num_in: &[u8]) -> Result<[u8; 32], Error> {
        let mut result = [0; 32];
        let status = unsafe { bindings::calib_nonce_rand(self.0, num_in.as_ptr(), result.as_mut_ptr()) };
        handle_error(status)?;
        Ok(result)
    }

    pub fn read_config_zone(&self, config_data: &mut [u8]) -> Result<(), Error> {
        let status = unsafe { bindings::calib_read_config_zone(self.0, config_data.as_mut_ptr()) };
        handle_error(status)
    }

    pub fn read_serial_number(&self, serial_number: &mut [u8; 9]) -> Result<(), Error> {
        let status = unsafe { bindings::calib_read_serial_number(self.0, serial_number.as_mut_ptr()) };
        handle_error(status)
    }

    pub fn gendig(&self, zone: u8, key_id: u16, other_data: Option<&[u8]>) -> Result<(), Error> {
        let status = unsafe {
            bindings::calib_gendig(
                self.0,
                zone,
                key_id,
                other_data.map_or(std::ptr::null(), |c| c.as_ptr()),
                other_data.map_or(0, |c| c.len() as _),
            )
        };
        handle_error(status)
    }

    pub fn write_config_zone(&self, config_data: &[u8]) -> Result<(), Error> {
        let status = unsafe { bindings::calib_write_config_zone(self.0, config_data.as_ptr()) };
        handle_error(status)
    }

    pub fn lock_config_zone_crc(&self, summary_crc: u16) -> Result<(), Error> {
        let status = unsafe { bindings::calib_lock_config_zone_crc(self.0, summary_crc) };
        handle_error(status)
    }

    pub fn genkey(&self, key_id: u16) -> Result<[u8; 64], Error> {
        let mut pubkey = [0; 64];
        let status = unsafe { bindings::calib_genkey(self.0, key_id, pubkey.as_mut_ptr()) };
        handle_error(status)?;
        Ok(pubkey)
    }

    pub fn sign(&self, key_id: u16, msg: &[u8; 32]) -> Result<[u8; 64], Error> {
        let mut sig = [0; 64];
        let status = unsafe { bindings::calib_sign(self.0, key_id, msg.as_ptr(), sig.as_mut_ptr()) };
        handle_error(status)?;
        Ok(sig)
    }

    pub fn verify_stored(
        &self,
        key_id: u16,
        message: &[u8; 32],
        signature: &[u8; 64],
    ) -> Result<bool, Error> {
        let mut result = false;
        let status = unsafe {
            bindings::calib_verify_stored(
                self.0,
                message.as_ptr(),
                signature.as_ptr(),
                key_id,
                std::ptr::addr_of_mut!(result),
            )
        };
        handle_error(status)?;
        Ok(result)
    }

    /// Get public key from private key slot.
    pub fn get_pubkey(&self, key_id: u16) -> Result<[u8; 64], Error> {
        let mut pubkey = [0; 64];
        let status = unsafe { bindings::calib_get_pubkey(self.0, key_id, pubkey.as_mut_ptr()) };
        handle_error(status)?;
        Ok(pubkey)
    }

    pub fn priv_write(&self, key_id: u16, priv_key: &[u8; 32]) -> Result<(), Error> {
        let num_in = [0; 20];
        let mut padded_priv_key = [0u8; 36];
        padded_priv_key[4..].copy_from_slice(priv_key);
        let status = unsafe {
            bindings::calib_priv_write(self.0, key_id, &padded_priv_key, 0, std::ptr::null(), &num_in)
        };
        handle_error(status)
    }
}

fn handle_error(status: ATCA_STATUS) -> Result<(), Error> {
    if status == bindings::ATCA_SUCCESS as i32 {
        Ok(())
    } else {
        Err(Error { status })
    }
}

#[derive(Debug)]
pub struct Error {
    pub status: ATCA_STATUS,
}

impl From<xous::Error> for Error {
    fn from(value: xous::Error) -> Self {
        log::error!("Xous error encountered: {value:?}");
        Self { status: bindings::ATCA_GEN_FAIL as _ }
    }
}

impl From<dma::error::DmaError> for Error {
    fn from(value: dma::error::DmaError) -> Self {
        log::error!("DMA error encountered: {value:?}");
        Self { status: bindings::ATCA_GEN_FAIL as _ }
    }
}

impl From<atsama5d27::flexcom::FlexcomError> for Error {
    fn from(value: atsama5d27::flexcom::FlexcomError) -> Self {
        log::error!("Flexcom error encountered: {value:?}");
        Self { status: bindings::ATCA_GEN_FAIL as _ }
    }
}
