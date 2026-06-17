// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    sync::{LazyLock, RwLock},
    thread,
    time::{Duration, Instant},
};

use fido::messages::{Transport, U2fApduCommand};
use gpio::{GpioPin, PinSettings};
use nfc::error::NfcError;
use {
    iso7816::{
        command::class::{NO_SM_CLA, ZERO_CLA},
        Aid, Command, Instruction, Status,
    },
    once_cell::sync::OnceCell,
    rfal::{
        ndefCapabilityContainer, ndefCapabilityContainerT2T, ndefState, rfalNfcDevType, rfalNfcState,
        Platform, Rfal, RFAL_NFC_LISTEN_TECH_A, RFAL_NFC_POLL_TECH_A,
    },
    st25r95::St25r95Spi,
};

use crate::{NfcImpl, NfcServer};

fido::use_api!();
gpio::use_api!();
spi::use_api!();

pub struct Implementation {
    rfal: Rfal,
    fido: Option<FidoApi>,
    consecutive_errors: u32,
    last_emulation_attempt: Option<Instant>,
    /// Timestamp of the most recent `emulate_t4t` that returned `Ok(())`. The IRQ handler
    /// suppresses back-to-back emulation attempts for `SUCCESS_COOLDOWN` after a success —
    /// the reader's field typically re-triggers our IRQ right after the tap completes, and
    /// we'd otherwise spin up a useless session that just times out on LinkLoss.
    last_successful_emulation: Option<Instant>,
}

static START: LazyLock<Instant> = LazyLock::new(|| Instant::now());
static SPI_PER: OnceCell<RwLock<SpiPeripheral>> = OnceCell::new();
static GPIO_API: LazyLock<GpioApi> = LazyLock::new(|| GpioApi::default());

// use 96 bytes to :
// - be smaller than 256 because our antenna does support long frames
// - make a U2F_AUTH response in a single chunk (multiple not accepted by Android)
const ISO7816_APDU_CHAINING_CHUNK_SIZE: usize = 96;

// NFCCTAP (CTAP2 over NFC) instruction bytes per CTAP2 spec section 11.3
const NFCCTAP_MSG: u8 = 0x10; // Encapsulated CTAP2 CBOR command
const NFCCTAP_GETRESPONSE: u8 = 0x11; // Get remaining CTAP2 response data

/// Timeout for the T4T emulation main loop (waiting for activation + transaction)
const EMULATE_T4T_TIMEOUT: Duration = Duration::from_secs(10);

/// NFC session timeout - exit if no APDU received within this time
const APDU_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for transceive_blocking busy-wait loop
const TRANSCEIVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Minimum cooldown between NFC emulation attempts after consecutive errors
const MIN_ERROR_COOLDOWN: Duration = Duration::from_millis(500);

/// Maximum cooldown between NFC emulation attempts after consecutive errors
const MAX_ERROR_COOLDOWN: Duration = Duration::from_secs(5);

/// Minimum delay after a successful emulation before accepting another reader activation.
/// The reader's field typically lingers after a successful tap; without this cooldown we'd
/// immediately start a new emulation session that just times out on LinkLoss.
const SUCCESS_COOLDOWN: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct ApduResponse {
    status: Status,
    data: Vec<u8>,
    transceive_done: bool,
}

impl Implementation {
    pub(crate) fn on_start_hook(&self, context: &mut server::ServerContext<NfcServer>) {
        GPIO_API.enable_irq(GpioPin::NfcIrqB, context).expect("Could not register for IRQ");
    }

    pub(crate) fn irq_out_handler(&mut self, enabled: bool) {
        let pin_state = match GPIO_API.get_pin(GpioPin::NfcIrqB) {
            Ok(state) => state,
            Err(e) => {
                log::error!("Failed to read NfcIrqB pin: {e:?}");
                return;
            }
        };

        if !pin_state {
            // Log "Field Detected" at INF only for actionable detections. While we're in the
            // post-success cooldown the reader's lingering field re-triggers our IRQ every
            // few hundred ms; those are noise, not new taps.
            let in_success_cooldown =
                self.last_successful_emulation.map(|t| t.elapsed() < SUCCESS_COOLDOWN).unwrap_or(false);
            if in_success_cooldown {
                log::debug!("NFC external reader Field Detected (within success cooldown)");
            } else {
                log::info!("NFC external reader Field Detected");
            }
            if let Err(e) = GPIO_API.set_irq(GpioPin::NfcIrqB, false) {
                log::error!("Failed to disable NfcIrqB IRQ: {e:?}");
                return;
            }
            let exited_sleep = self.exit_sleep_state().is_ok();
            if enabled && exited_sleep {
                if in_success_cooldown {
                    log::debug!("NFC success cooldown active, skipping emulation");
                } else {
                    // Enforce cooldown after consecutive errors to prevent rapid rfal cycling
                    if self.consecutive_errors > 0 {
                        let backoff = MIN_ERROR_COOLDOWN
                            .saturating_mul(1 << (self.consecutive_errors - 1).min(10))
                            .min(MAX_ERROR_COOLDOWN);
                        if let Some(last) = self.last_emulation_attempt {
                            let elapsed = last.elapsed();
                            if elapsed < backoff {
                                let remaining = backoff - elapsed;
                                log::warn!(
                                    "NFC error backoff: sleeping {}ms ({} consecutive errors)",
                                    remaining.as_millis(),
                                    self.consecutive_errors
                                );
                                thread::sleep(remaining);
                            }
                        }
                    }
                    self.last_emulation_attempt = Some(Instant::now());
                    match self.emulate_t4t() {
                        Ok(()) => {
                            self.consecutive_errors = 0;
                            self.last_successful_emulation = Some(Instant::now());
                        }
                        Err(e) => {
                            self.consecutive_errors = self.consecutive_errors.saturating_add(1);
                            log::error!("{e:?}");
                        }
                    }
                }
            }
            // give time to the reader field to completely disappear before trying to detect it again
            thread::sleep(Duration::from_millis(200));
            if let Err(e) = self.enter_sleep_state() {
                log::error!("{e:?}");
            }
        }

        if let Err(e) = GPIO_API.set_irq(GpioPin::NfcIrqB, true) {
            log::error!("Failed to re-enable NfcIrqB IRQ: {e:?}");
        }
    }

    fn exit_sleep_state(&mut self) -> Result<(), NfcError> {
        self.rfal.nfc.exit_wakeup_mode().map_err(|e| {
            log::error!("exit_wakeup_mode() failed: {:?}", e);
            NfcError::Internal
        })?;
        Ok(())
    }

    fn enter_sleep_state(&mut self) -> Result<(), NfcError> {
        self.rfal.nfc.enter_wakeup_mode().map_err(|e| {
            log::error!("enter_wakeup_mode() failed: {:?}", e);
            NfcError::Internal
        })?;
        Ok(())
    }

    fn emulate_t4t(&mut self) -> Result<(), NfcError> {
        if self.fido.is_none() {
            self.fido = Some(FidoApi::default());
        }
        /* Set configuration for NFC-A CE */
        self.rfal.discover.params.compMode = rfal::rfalComplianceMode::RFAL_COMPLIANCE_MODE_NFC; // Android/iOS are compliant to NFC forum
        self.rfal.discover.params.wakeupEnabled = false; // Disable wakeup for CE

        let mut nfcid = [0u8; 4];
        if let Err(e) = getrandom::getrandom(&mut nfcid) {
            log::error!("getrandom failed: {e:?}");
            return Err(NfcError::Internal);
        }
        nfcid[0] = 0x08; /* uid1 to uid3 is a random number which is dynamically generated */
        self.rfal.discover.params.lmConfigPA.nfcid[..4].copy_from_slice(&nfcid); /* NFCID / UID (4 bytes) */
        self.rfal.discover.params.lmConfigPA.nfcidLen = rfal::rfalLmNfcidLen::RFAL_LM_NFCID_LEN_04; /* Set NFCID length to 4 bytes */
        self.rfal.discover.params.lmConfigPA.SENS_RES.copy_from_slice(&[0x04, 0x00]); /* ATQA: UID size: single / bit frame anticollision supported */
        self.rfal.discover.params.lmConfigPA.SEL_RES = 0x20; /* SAK: Compliant with ISO/IEC 14443-4 */
        self.rfal.discover.params.techs2Find = RFAL_NFC_LISTEN_TECH_A as u16;

        self.rfal.discover.params.notifyCb = Some(disc_callback);
        self.rfal.discover.params.totalDuration = 9000;

        self.rfal.discover.start().map_err(|e| {
            log::error!("discover.start() failed: {:?}", e);
            NfcError::Internal
        })?;

        let start = Instant::now();
        let result = loop {
            if start.elapsed() > EMULATE_T4T_TIMEOUT {
                log::error!("NFC activation timeout after {}s", EMULATE_T4T_TIMEOUT.as_secs());
                break Err(NfcError::Timeout);
            }
            self.rfal.nfc.worker(); /* Run RFAL worker periodically */
            let state = self.rfal.nfc.state();
            if !matches!(state, rfalNfcState::RFAL_NFC_STATE_ACTIVATED) {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            let Ok(nfc_dev) = self.rfal.nfc.active_device() else { continue };
            log::trace!("[*] nfc_dev: activated");
            if nfc_dev.dev_type() == rfalNfcDevType::RFAL_NFC_POLL_TYPE_NFCA {
                match self.handle_type_a_reader() {
                    Ok(done) => {
                        if done {
                            break Ok(());
                        }
                    }
                    Err(e) => break Err(e),
                }
            }
        };
        self.rfal.nfc.deactivate_and_idle().ok();
        result
    }

    fn handle_type_a_reader(&mut self) -> Result<bool, NfcError> {
        log::debug!("RFAL_NFC_POLL_TYPE_NFCA");
        let mut remaining_data: Vec<u8> = Vec::new();
        let mut transceive_done = false;
        let mut last_activity = Instant::now();

        loop {
            // Check for session timeout
            if last_activity.elapsed() > APDU_TRANSACTION_TIMEOUT {
                log::warn!(
                    "NFC session timeout - no APDU received for {}s",
                    APDU_TRANSACTION_TIMEOUT.as_secs()
                );
                break;
            }

            self.rfal.nfc.worker();
            let state = self.rfal.nfc.state();
            log::debug!("state: {:?}", state);

            if matches!(state, rfal::rfalNfcState::RFAL_NFC_STATE_ACTIVATED) {
                if let Err(e) = self.transceive_blocking(None, 0) {
                    return Err(e);
                }
            } else if matches!(
                state,
                rfal::rfalNfcState::RFAL_NFC_STATE_DATAEXCHANGE
                    | rfal::rfalNfcState::RFAL_NFC_STATE_DATAEXCHANGE_DONE
            ) {
                log::debug!("[>] state: {:?}", state);
                let rx_data = match self.rfal.nfc.data_exchange.rx_data() {
                    Ok(rx_data) => rx_data.to_vec(),
                    Err(rfal::Error::Busy) => {
                        thread::sleep(Duration::from_micros(100));
                        continue;
                    }
                    Err(rfal::Error::SleepReq) => {
                        log::debug!("Reader requested sleep, ending session");
                        break;
                    }
                    Err(e) => {
                        log::error!("[*] rfalNfcDataExchange rx_data error: {:?}", e);
                        return Err(NfcError::Internal);
                    }
                };
                log::debug!("[>] rx_data: {:x?}", rx_data);
                if rx_data.is_empty() {
                    log::debug!("No more data from reader, ending session");
                    break;
                }

                // Reset activity timer on received APDU
                last_activity = Instant::now();

                let mut resp = self.handle_apdu_cmd(&rx_data, &mut remaining_data);
                transceive_done = resp.transceive_done;
                log::debug!("[>] reply: {:02x?}", resp);
                resp.data.extend_from_slice(&resp.status.to_u16().to_be_bytes());
                if let Err(e) = self.transceive_blocking(Some(&mut resp.data), rfal::RFAL_FWT_NONE) {
                    return Err(e);
                }

                if transceive_done {
                    log::debug!("APDU transaction complete, continuing to listen for more commands");
                }
            } else {
                log::debug!("[>] state: {:?}, reader disconnected", state);
                break;
            }
        }

        Ok(transceive_done)
    }

    fn transceive_blocking(&mut self, tx_buf: Option<&mut [u8]>, fwt: u32) -> Result<(), NfcError> {
        let trx_start = Instant::now();
        let mut res = self.rfal.nfc.data_exchange.start(tx_buf, fwt);
        log::trace!("[>] rfalNfcDataExchangeStart: {:?}", res);
        if res.is_ok() {
            res = loop {
                if trx_start.elapsed() > TRANSCEIVE_TIMEOUT {
                    log::error!("transceive_blocking timeout after {}s", TRANSCEIVE_TIMEOUT.as_secs());
                    break Err(rfal::Error::Timeout);
                }
                self.rfal.nfc.worker();
                res = self.rfal.nfc.data_exchange.get_status();
                log::trace!("[>] rfalNfcDataExchangeGetStatus error: {:?}", res);
                if res != Err(rfal::Error::Busy) {
                    break res;
                }
                // Yield CPU to avoid tight busy-loop
                thread::sleep(Duration::from_micros(100));
            };
        }
        if res.is_err() && res != Err(rfal::Error::SleepReq) {
            log::error!("[*] rfalNfcDataExchange(Start|GetStatus) error: {:?}", res);
            Err(NfcError::Internal)
        } else {
            Ok(())
        }
    }

    // TODO: add an RegisterApplet that fido server can call to register
    // the FIDO U2F applet (giving the AID), a little bit like the usbdev::RegisterInterface
    fn handle_apdu_cmd(&mut self, rx_data: &[u8], remaining_data: &mut Vec<u8>) -> ApduResponse {
        let Ok(cmd) = Command::<256>::try_from(rx_data) else {
            return ApduResponse { status: Status::CorruptedData, data: vec![], transceive_done: false };
        };
        log::debug!("[>] cmd: {:x?}", cmd);

        let cla = cmd.class();
        let ins = cmd.instruction();

        match (cla, ins) {
            // ISO7816 standard: SELECT (applet selection)
            (ZERO_CLA, Instruction::Select) => {
                // Both U2F and CTAP2 use the same FIDO applet AID
                let fido_aid = Aid::new(&[0xA0, 0x00, 0x00, 0x06, 0x47, 0x2F, 0x00, 0x01]);
                if fido_aid.matches(cmd.data()) {
                    ApduResponse { status: Status::Success, data: b"U2F_V2".to_vec(), transceive_done: false }
                } else {
                    ApduResponse {
                        status: Status::FunctionNotSupported,
                        data: vec![],
                        transceive_done: false,
                    }
                }
            }

            // ISO7816 standard: GET RESPONSE (response chaining for U2F)
            (ZERO_CLA, Instruction::GetResponse) => {
                let resp = std::mem::take(remaining_data);
                Self::next_apdu_chaining(resp, remaining_data)
            }

            // NFCCTAP: CTAP2 CBOR command (CLA=0x80, INS=0x10)
            (NO_SM_CLA, Instruction::Unknown(NFCCTAP_MSG)) => self.handle_nfcctap_msg(&cmd, remaining_data),

            // NFCCTAP: GET RESPONSE for CTAP2 chaining (CLA=0x80, INS=0x11)
            (NO_SM_CLA, Instruction::Unknown(NFCCTAP_GETRESPONSE)) => {
                let resp = std::mem::take(remaining_data);
                Self::next_apdu_chaining(resp, remaining_data)
            }

            // U2F: Forward unknown instructions on CLA=0x00 to U2F processing
            (ZERO_CLA, Instruction::Unknown(_)) => self.handle_u2f_apdu(&cmd, remaining_data),

            // Unsupported CLA
            _ if cla != ZERO_CLA && cla != NO_SM_CLA => {
                ApduResponse { status: Status::ClaNotSupported, data: vec![], transceive_done: false }
            }

            // Unsupported instruction
            _ => ApduResponse {
                status: Status::InstructionNotSupportedOrInvalid,
                data: vec![],
                transceive_done: false,
            },
        }
    }

    /// Handle NFCCTAP_MSG (0x10) - CTAP2 CBOR command over NFC
    fn handle_nfcctap_msg(&mut self, cmd: &Command<256>, remaining_data: &mut Vec<u8>) -> ApduResponse {
        let data = cmd.data();
        if data.is_empty() {
            return ApduResponse { status: Status::WrongLength, data: vec![], transceive_done: false };
        }

        // First byte is the CTAP2 command, rest is CBOR data
        let ctap_cmd = data[0];
        let ctap_data = data[1..].to_vec();

        log::debug!("NFCCTAP_MSG: cmd=0x{:02x}, data_len={}", ctap_cmd, ctap_data.len());

        // Forward to FIDO server for CTAP2 processing
        let resp = self.fido.as_ref().unwrap().ctap_process_cbor(ctap_cmd, ctap_data);
        log::debug!("[>] CTAP2 reply: len={}", resp.len());

        // CTAP2 response already contains status byte + CBOR data
        // We need to append SW_SUCCESS (0x9000) for APDU framing
        let mut apdu_resp = resp;
        apdu_resp.extend_from_slice(&[0x90, 0x00]);

        // Handle response chaining (96-byte limit)
        Self::next_apdu_chaining(apdu_resp, remaining_data)
    }

    /// Handle U2F APDU commands (CLA=0x00, unknown instructions)
    fn handle_u2f_apdu(&mut self, cmd: &Command<256>, remaining_data: &mut Vec<u8>) -> ApduResponse {
        let command = U2fApduCommand::from_command_view(cmd.as_view());
        let mut resp = self.fido.as_ref().unwrap().u2f_process_apdu(command, Transport::Nfc);
        log::debug!("[>] U2F reply: {:02x?}, len={}", resp, resp.len());

        if cmd.extended {
            // Specification says authenticator MUST respond using the extended length APDU response
            // format, but we know Prime NFC antenna can't do very long APDU (that why we do chunk of
            // 96 bytes and not 256), so log a warning here to at least have in the log that we expect
            // having issue in extended APDU case.
            log::warn!(
                "U2F request was in extended length APDU format, Prime may have difficulty sending the response!"
            );
            let status = Self::pop_status(&mut resp);
            ApduResponse { status, data: resp, transceive_done: true }
        } else {
            Self::next_apdu_chaining(resp, remaining_data)
        }
    }

    fn next_apdu_chaining(mut resp: Vec<u8>, remaining_data: &mut Vec<u8>) -> ApduResponse {
        if resp.len() < 2 {
            log::warn!("next_apdu_chaining: response too short ({} bytes)", resp.len());
            return ApduResponse {
                status: Status::UnspecifiedCheckingError,
                data: vec![],
                transceive_done: true,
            };
        }
        if resp.len() - 2 > ISO7816_APDU_CHAINING_CHUNK_SIZE {
            *remaining_data = resp.split_off(ISO7816_APDU_CHAINING_CHUNK_SIZE);
            let remaining_len = remaining_data.len() - 2; // ignore status
            return ApduResponse {
                status: Status::MoreAvailable(if remaining_len > 255 {
                    // in iso7816-4 spec, 0 means "more than 255 bytes remaining"
                    0
                } else {
                    remaining_len as u8
                }),
                data: resp,
                transceive_done: false,
            };
        } else {
            let status = Self::pop_status(&mut resp);
            return ApduResponse { status, data: resp, transceive_done: true };
        }
    }

    fn pop_status(resp: &mut Vec<u8>) -> Status {
        let Some(s2) = resp.pop() else {
            log::error!("pop_status: response too short");
            return Status::UnspecifiedCheckingError;
        };
        let Some(s1) = resp.pop() else {
            log::error!("pop_status: response too short");
            return Status::UnspecifiedCheckingError;
        };
        Status::from((s1, s2))
    }
}

impl NfcImpl for Implementation {
    /// PANIC: this function can panic if :
    /// - NFC server is not the first app to claim the SPI Nfc peripheral
    /// - a GPIO get_pin or set_pin failed (IRQ_IN and IRQ_OUT)
    fn new() -> Result<Self, NfcError> {
        log::debug!("Initialing IRQ_IN and IRQ_OUT pins");
        GPIO_API.claim_pin(GpioPin::NfcIntB, PinSettings::OutputHigh, false)?;
        GPIO_API.claim_pin(GpioPin::NfcIrqB, PinSettings::InterruptFalling, false)?;
        log::debug!("Initialized NFC pins client");

        log::debug!("Initializing RFAL");
        fn get_ticks_ms() -> u32 { START.elapsed().as_millis() as u32 }
        fn spi_per_lock() -> &'static RwLock<SpiPeripheral> {
            SPI_PER.get_or_init(|| {
                RwLock::new(
                    SpiApi::default()
                        .claim_peripheral(spi::Peripheral::Nfc)
                        .expect("Could not claim SPI peripheral"),
                )
            })
        }
        match Rfal::new(Platform {
            spi_poll_send: || match spi_per_lock().write() {
                Ok(mut spi) => spi
                    .poll(st25r95::PollFlags::CAN_SEND)
                    .inspect_err(|e| {
                        log::error!("spi_poll_send error: {:?}", e);
                    })
                    .is_ok(),
                Err(e) => {
                    log::error!("RWLock error: {:?}", e);
                    false
                }
            },
            spi_reset: || match spi_per_lock().write() {
                Ok(mut spi) => {
                    spi.reset()
                        .inspect_err(|e| {
                            log::error!("spi_reset error: {:?}", e);
                        })
                        .ok();
                }
                Err(e) => {
                    log::error!("RWLock error: {:?}", e);
                }
            },
            spi_send_cmd: |cmd, data, sod| match spi_per_lock().write() {
                Ok(mut spi) => match st25r95::Command::try_from(cmd) {
                    Ok(cmd) => {
                        if let Err(e) = spi.send_command(cmd, data, sod) {
                            log::error!("spi_send_cmd error: {:?}", e);
                        }
                    }
                    Err(e) => {
                        log::error!("Command error: {:?}", e);
                    }
                },
                Err(e) => {
                    log::error!("RWLock error: {:?}", e);
                }
            },
            spi_read: |code, data| match spi_per_lock().write() {
                Ok(mut spi) => match spi.read_data() {
                    Ok(resp) => {
                        *code = resp.code;
                        let data_len = resp.data.len().min(data.len());
                        let _ = &mut data[..data_len].copy_from_slice(&resp.data[..data_len]);
                        resp.data.len() as u16
                    }
                    Err(st25r95::Error::Hw(e)) => {
                        *code = e.into();
                        0
                    }
                    Err(e) => {
                        log::error!("spi_read error: {:?}", e);
                        *code = 0xFE; // unknown error
                        0
                    }
                },
                Err(e) => {
                    log::error!("RWLock error: {:?}", e);
                    *code = 0xFF; // unknown error
                    0
                }
            },
            spi_read_echo: || match spi_per_lock().write() {
                Ok(mut spi) => spi
                    .read_data()
                    .inspect_err(|e| {
                        log::error!("spi_read_echo error: {:?}", e);
                    })
                    .is_ok(),
                Err(e) => {
                    log::error!("RWLock error: {:?}", e);
                    false
                }
            },
            spi_flush: || match spi_per_lock().write() {
                Ok(mut spi) => {
                    spi.flush()
                        .inspect_err(|e| {
                            log::error!("spi_flush error: {:?}", e);
                        })
                        .ok();
                }
                Err(e) => {
                    log::error!("RWLock error: {:?}", e);
                }
            },
            handle_error: |e, line| match e.to_str() {
                Ok(e) => log::error!("[-] RFAL ERROR @ {}:{}", e, line),
                Err(e) => log::error!("to_str {:?}", e),
            },
            log: |msg, val| match msg.to_str() {
                Ok(msg) => log::debug!("[*] RFAL {}{}", msg, val),
                Err(e) => log::error!("to_str {:?}", e),
            },
            irq_in_pulse_low: || {
                GPIO_API.set_pin(GpioPin::NfcIntB, true).expect("set_pin NfcIntB failed");
                thread::sleep(Duration::from_millis(1)); // wait t0 > 100us
                GPIO_API.set_pin(GpioPin::NfcIntB, false).expect("set_pin NfcIntB failed");
                thread::sleep(Duration::from_millis(1)); // wait t1 > 10us
                GPIO_API.set_pin(GpioPin::NfcIntB, true).expect("set_pin NfcIntB failed");
                thread::sleep(Duration::from_millis(11)); // wait t3 > 10ms
            },
            wait_irq_out_falling_edge: |timeout| {
                for tries in 0..100 {
                    if !GPIO_API.get_pin(GpioPin::NfcIrqB).expect("get_pin NfcIrqB failed") {
                        log::debug!("Took {tries} to get a low signal");
                        return true;
                    }
                }
                // Looks like we are going to be here for a while, switch to a less intensive wait
                let start = Instant::now();
                let timeout = Duration::from_millis(timeout as u64);
                while start.elapsed() < timeout {
                    if !GPIO_API.get_pin(GpioPin::NfcIrqB).expect("get_pin NfcIrqB failed") {
                        return true;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                false
            },
            get_ticks_ms,
            delay_ms: |ms| thread::sleep(Duration::from_millis(ms as u64)),
        }) {
            Ok(rfal) => {
                log::debug!("Initialized RFAL");
                // Switch to low-power mode until the first actual use.
                rfal.nfc
                    .enter_wakeup_mode()
                    .map_err(|e| {
                        log::error!("init-time enter_wakeup_mode() failed: {:?}, is the NFC antenna properly attached ?", e);
                        NfcError::Internal
                    })
                    .ok();
                Ok(Self {
                    rfal,
                    fido: None,
                    consecutive_errors: 0,
                    last_emulation_attempt: None,
                    last_successful_emulation: None,
                })
            }
            Err(e) => {
                log::error!("RFAL init failed: {:?}", e);
                Err(NfcError::Internal)
            }
        }
    }

    fn read_ndef_raw_msg(&mut self, timeout: Duration) -> Result<(Vec<u8>, Vec<u8>), NfcError> {
        self.exit_sleep_state()?;
        self.rfal.reset();
        self.rfal.discover.params.techs2Find = RFAL_NFC_POLL_TECH_A as u16;
        self.rfal.discover.params.totalDuration = 1000;
        self.rfal.discover.start().map_err(|e| {
            log::error!("discover.start() failed: {:?}", e);
            NfcError::Internal
        })?;
        let start = Instant::now();
        let result = loop {
            if start.elapsed() > timeout {
                break Err(NfcError::Timeout);
            }
            self.rfal.nfc.worker();
            let state = self.rfal.nfc.state();
            if !matches!(state, rfalNfcState::RFAL_NFC_STATE_ACTIVATED) {
                log::debug!("state: {:?}", state);
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            let Ok(nfc_dev) = self.rfal.nfc.active_device() else { continue };
            if nfc_dev.dev_type() == rfalNfcDevType::RFAL_NFC_LISTEN_TYPE_NFCA {
                let dev_id = nfc_dev.id().unwrap_or_default().to_vec();
                if self.rfal.ndef.poller.initialize(&nfc_dev).is_ok() {
                    match self.rfal.ndef.poller.ndef_detect() {
                        Ok(ndef_info) if ndef_info.state == ndefState::NDEF_STATE_INITIALIZED => {
                            // nothing to read
                            break Ok((dev_id, Vec::new()));
                        }
                        Ok(_) => {
                            if let Ok(raw_msg) = self.rfal.ndef.poller.read_raw_message() {
                                // read raw_msg
                                break Ok((dev_id, raw_msg));
                            }
                        }
                        Err(e) => {
                            log::warn!("NDEF detect failed on activated tag: {e:?}");
                            break Ok((dev_id, Vec::new()));
                        }
                    }
                }
            }
        };
        // Note: We use `deactivate_and_idle` and not `deactivate_and_sleep" on purpose, because they do not
        //       mean what they should intuitively. "Sleep" means "keep the RF field on" while "Idle" turns
        //       most things off, except for the MCU.
        //       Even more confusingly, Sleep and Idle don't mean the same thing in RFAL and in the NFC chip's
        //       datasheet.
        self.rfal.nfc.deactivate_and_idle().ok();
        // This is what puts the MCU itself into low-power mode (called Sleep in the datasheet), and it's
        // separate functionality of RFAL.
        self.enter_sleep_state()?;
        result
    }

    fn write_ndef_raw_msg(&mut self, uid: Vec<u8>, msg: Vec<u8>, timeout: Duration) -> Result<(), NfcError> {
        self.exit_sleep_state()?;
        self.rfal.reset();
        self.rfal.discover.params.techs2Find = RFAL_NFC_POLL_TECH_A as u16;
        self.rfal.discover.params.totalDuration = 1000;
        self.rfal.discover.start().map_err(|e| {
            log::error!("discover.start() failed: {:?}", e);
            NfcError::Internal
        })?;
        let start = Instant::now();
        let mut needs_format = false;
        let result = loop {
            if start.elapsed() > timeout {
                break Err(NfcError::Timeout);
            }
            self.rfal.nfc.worker();
            let state = self.rfal.nfc.state();
            if !matches!(state, rfalNfcState::RFAL_NFC_STATE_ACTIVATED) {
                log::debug!("state: {:?}", state);
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            let Ok(nfc_dev) = self.rfal.nfc.active_device() else { continue };
            if nfc_dev.dev_type() == rfalNfcDevType::RFAL_NFC_LISTEN_TYPE_NFCA {
                let dev_id = nfc_dev.id().unwrap_or_default().to_vec();
                if dev_id != uid {
                    continue;
                }
                if self.rfal.ndef.poller.initialize(&nfc_dev).is_ok() {
                    if needs_format {
                        // Phase 2: fresh activation — format without prior ndef_detect
                        log::info!("Attempting tag_format on fresh NFC activation");
                        let cc = ndefCapabilityContainer {
                            t2t: ndefCapabilityContainerT2T {
                                magicNumber: 0xE1,
                                majorVersion: 1,
                                minorVersion: 0,
                                size: 109, // 872 bytes
                                readAccess: 0,
                                writeAccess: 0,
                            },
                        };
                        needs_format = false;
                        if let Err(e) = self.rfal.ndef.poller.tag_format(cc, 0) {
                            log::error!("tag_format failed: {e:?}");
                            continue;
                        }
                        log::info!("tag_format succeeded, re-initializing for write");
                        // Re-initialize context + detect after format
                        if self.rfal.ndef.poller.initialize(&nfc_dev).is_ok()
                            && self.rfal.ndef.poller.ndef_detect().is_ok()
                        {
                            break self.rfal.ndef.poller.write_raw_message(msg.as_slice()).map_err(|e| {
                                log::error!("write_raw_message failed: {:?}", e);
                                NfcError::Internal
                            });
                        }
                    } else {
                        // Phase 1: normal path — try ndef_detect first
                        if self.rfal.ndef.poller.ndef_detect().is_ok() {
                            break self.rfal.ndef.poller.write_raw_message(msg.as_slice()).map_err(|e| {
                                log::error!("write_raw_message failed: {:?}", e);
                                NfcError::Internal
                            });
                        }
                        // ndef_detect failed — tag is likely halted
                        log::warn!("NDEF detect failed, scheduling tag_format on next activation");
                        needs_format = true;
                        self.rfal.nfc.deactivate_and_idle().ok();
                        // Restart discovery so the tag can be re-polled and re-activated
                        self.rfal.discover.start().map_err(|e| {
                            log::error!("discover.start() failed: {:?}", e);
                            NfcError::Internal
                        })?;
                    }
                }
            }
        };
        self.rfal.nfc.deactivate_and_idle().ok();
        self.enter_sleep_state()?;
        result
    }
}

unsafe extern "C" fn disc_callback(state: rfal::rfalNfcState) {
    log::debug!("[*] state: {:?}", state);
}
