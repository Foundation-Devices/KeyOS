// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::VecDeque,
    num::NonZeroU8,
    time::{Duration, Instant},
};

use bt::{messages::*, AdvChannel, BleVersionInfo, BluetoothError, BtAddr, State};
use cargo_metadata::semver::Version;
use cosign2::Header;
use crc::{Crc, CRC_32_ISCSI};
use embedded_hal::spi::SpiDevice;
use fw_consts::{APP_MTU, SIGNATURE_HEADER_SIZE};
use gpio::{GpioPin, PinSettings};
use host_protocol::{
    AdvChan, Bluetooth, BluetoothStatus, HostProtocolMessage, SendDataResponse, TrustLevel, MAX_MSG_SIZE,
};
use security::BluetoothChallengeSecret;
use server::{ArchiveSubList, MessageId as _, ScalarEventSubscriber, ServerContext};
use settings::global::DeviceName;
use xous_ticktimer::TicktimerCallback;

use crate::SubscriberDisconnected;

crypto::use_api!();
gpio::use_api!();
security::use_api!();
settings::use_api!();
spi::use_api!();

const FIRMWARE: &[u8] = include_bytes!("../BT_application_v4.0.0.bin");

const GENERAL_TIMEOUT: usize = 100;
const VERIFY_FIRMWARE_TIMEOUT: usize = 1500;
const ERASE_FIRMWARE_TIMEOUT: usize = 2000;
const WRITE_FIRMWARE_TIMEOUT: usize = 1500;

const INITIAL_BOOT_TIME: usize = 100;
const BOOT_TIME_GET_STATE_TIMEOUT: usize = 100;
const BOOT_TIME_GET_STATE_RETRIES: usize = 10;

const START_FIRMWARE_WAIT_TIME: usize = 2300;
const START_FIRMWARE_GET_STATE_TIMEOUT: usize = 100;
const START_FIRMWARE_TIME_GET_STATE_RETRIES: usize = 10;

const UPD_MAX_WRITE_RETRY_COUNT: usize = 10;

const BT_CHALLENGE_PERIOD_SECS: u64 = 60;

const BACKGROUND_POLL_MS: usize = 1000;

const STATS_PRINT_PERIOD_MS: u64 = 3000;

#[derive(Clone)]
struct Firmware {
    ver: Version,
    trust_level: TrustLevel,
    img: Vec<u8>,
}

#[derive(server::Server)]
#[name = "os/bt"]
pub struct BluetoothServer {
    pub packet_subscribers: ArchiveSubList<BlePacket>,
    pub state_subscribers: Vec<ScalarEventSubscriber<State>>,
    pub state: State,
    spi_peripheral: SpiPeripheral,
    rx_buffer: [u8; MAX_MSG_SIZE],
    gpio_api: GpioApi,
    poll_callback: Option<TicktimerCallback>,
    get_state_tries: usize,
    enable_after_boot: bool,
    firmware: Firmware,
    force_update: bool,
    version_info: Option<BleVersionInfo>,
    crypto: CryptoApi,
    security: Security,
    challenge_secret: BluetoothChallengeSecret,
    device_id_sent: bool,
    is_challenge_ok: bool,
    challenge_last_check: Instant,
    stats: Stats,
    device_name: String,
}

struct Stats {
    rx_packets: usize,
    rx_size: usize,
    tx_packets: usize,
    tx_size: usize,
    since: Instant,
}

macro_rules! send_protocol_msg_wrapper {
    ($self:ident, $msg:expr, $expected:pat => $body:expr) => {
        match $self.send_protocol_msg(HostProtocolMessage::Bluetooth($msg), GENERAL_TIMEOUT) {
            Ok(HostProtocolMessage::Bluetooth($expected)) => $body,
            Ok(unexpected) => {
                log::error!("Got unexpected response to {}", stringify!($msg));
                log::debug!("{unexpected:?}");
                $self.reset();
                Err(BluetoothError::SpiProtocolError)
            }
            Err(e) => {
                log::error!("Got error to {}: {e:?}", stringify!($msg));
                $self.reset();
                Err(e)
            }
        }
    };
}

fn firmware_from_img(img: Vec<u8>) -> Option<Firmware> {
    let header = match Header::parse_unverified(&img, SIGNATURE_HEADER_SIZE as usize, false) {
        Ok(Some(header)) => header,
        Ok(None) => {
            log::warn!("No update firmware header found.");
            return None;
        }
        Err(e) => {
            log::error!("Error parsing Update firmware header ({e:?}).");
            return None;
        }
    };
    log::info!("Bundled Firmware version is {}, size = {} bytes", &header.version(), img.len());
    let ver = match Version::parse(header.version()) {
        Ok(ver) => ver,
        Err(e) => {
            log::error!("Error parsing firmware version ({e:?}).");
            return None;
        }
    };
    let trust_level = if header.pubkey1() == [0; 33] || header.pubkey2() == [0; 33] {
        log::warn!("DEVELOPER firmware");
        TrustLevel::Developer
    } else {
        TrustLevel::Full
    };
    Some(Firmware { ver, trust_level, img })
}

impl Default for BluetoothServer {
    fn default() -> Self {
        log::debug!("Initializing Spi");
        let spi_peripheral =
            SpiApi::default().claim_peripheral(spi::Peripheral::Ble).expect("Could not claim SPI peripheral");

        let gpio_api = GpioApi::default();

        // Start by pulling the chip into reset. We will properly reset it later in on_start_hook()
        gpio_api
            .claim_pin(GpioPin::BtRst, PinSettings::OutputOpenDrainLow, false)
            .expect("Could not claim reset pin");

        gpio_api
            .claim_pin(GpioPin::BtIrqB, PinSettings::InterruptFalling, false)
            .expect("Could not claim irq pin");

        let firmware = firmware_from_img(FIRMWARE.to_vec()).expect("Invalid firmware image");

        let security = Security::default();

        let mut challenge_secret = security.bluetooth_challenge_secret();

        if firmware.trust_level == TrustLevel::Developer {
            // Challenge is wiped if BootFirmware is called with TrustLevel::Developer
            challenge_secret.secret = [0x0; 32]
        }

        Self {
            packet_subscribers: Default::default(),
            state_subscribers: Vec::new(),
            state: State::Booting,
            spi_peripheral,
            rx_buffer: [0; MAX_MSG_SIZE],
            gpio_api,
            get_state_tries: 0,
            poll_callback: None,
            enable_after_boot: false,
            firmware,
            force_update: false,
            version_info: None,
            crypto: CryptoApi::default(),
            security,
            device_id_sent: false,
            challenge_secret,
            is_challenge_ok: false,
            challenge_last_check: Instant::now(),
            stats: Stats { rx_packets: 0, rx_size: 0, tx_packets: 0, tx_size: 0, since: Instant::now() },
            device_name: String::new(),
        }
    }
}

impl BluetoothServer {
    pub(crate) fn set_state(&mut self, new_state: State) {
        if self.state != new_state {
            self.state = new_state;
            self.state_subscribers.retain(|s| match s.send(&new_state) {
                Ok(_) => true,
                Err(xous::Error::ServerQueueFull) => true,
                Err(_) => false,
            });
        }
    }

    pub fn refresh_connection(&mut self) -> Result<(), BluetoothError> {
        let status = self.get_bluetooth_status()?;
        if status.queue_overflow {
            log::error!("RX Queue overflowed on BLE chip (will cause packet loss)");
        }
        self.set_state(match status.connection {
            host_protocol::ConnectionStatus::Disabled => State::Disabled,
            host_protocol::ConnectionStatus::WaitingForConnection => State::WaitingForConnection,
            host_protocol::ConnectionStatus::Connected { rssi } => State::Connected { rssi },
        });
        Ok(())
    }

    pub fn on_start_hook(&mut self, context: &mut ServerContext<Self>) {
        self.poll_callback =
            Some(TicktimerCallback::new(context.sid()).expect("Could not register callback"));

        self.gpio_api.enable_irq(GpioPin::BtIrqB, context).expect("Could not register for IRQ");

        SettingsApi::default().server_subscribe_device_name(context);

        self.reset();
    }

    fn request_poll(&mut self, timeout_ms: usize) {
        self.poll_callback.as_mut().unwrap().request(timeout_ms, Poll::ID, 0);
    }

    fn send_protocol_msg<'s, 'o>(
        &'s mut self,
        req: HostProtocolMessage<'o>,
        timeout_ms: usize,
    ) -> Result<HostProtocolMessage<'s>, BluetoothError> {
        const ORC_FIRMWARE: u8 = 0x51;
        const ORC_BOOTLOADER: u8 = 0x69;

        let timeout_start = Instant::now();

        loop {
            let mut tx_msg = postcard::to_allocvec(&req)?;
            log::trace!(">>> {:02x?}", &tx_msg);
            self.spi_peripheral.transfer_in_place(&mut tx_msg)?;

            match tx_msg[0] {
                ORC_BOOTLOADER | ORC_FIRMWARE => {
                    // The BLE was using a 0 length tx buffer, i.e. it was in command reception mode
                    break;
                }
                0 => {
                    // There was no SPI transaction active on the BLE side.
                    // Spin retry as this is a very temporary state.
                }
                _ => {
                    // Both the MCU and the BLE firmware tried to send data.
                    log::warn!("Invalid response received to command: {tx_msg:02x?}");
                }
            }
            log::trace!("Repeated");
            if timeout_start.elapsed() > Duration::from_millis(timeout_ms as u64) {
                return Err(BluetoothError::SpiTimeout);
            }
        }

        let recv_len = self.spi_peripheral.nrf_read_data(&mut self.rx_buffer, timeout_ms)?;

        if matches!(self.state, State::Booting | State::StartingFirmware) {
            // Only bootloader padding after packet length: we are talking to a legacy bootloader,
            // which sends len and data in separate transfers.
            if self.rx_buffer.iter().take(recv_len).all(|c| *c == ORC_BOOTLOADER) {
                log::trace!("Legacy bootloader comms, retrying for data");
                // XXX: If we send the second message too fast, the NRF chip
                //      doesn't actually get the data ready
                std::thread::sleep(Duration::from_millis(1));
                self.spi_peripheral.read(&mut self.rx_buffer[..recv_len])?;
            }

            // XXX: The bootloader has a fixed sleep at the end of the SPI message loop,
            //      so let's match that here.
            std::thread::sleep(Duration::from_millis(2));
        }
        let rx_msg = &self.rx_buffer[..recv_len];
        match postcard::from_bytes(&rx_msg) {
            Ok(resp) => {
                log::trace!("<<< {:02x?}", &rx_msg);
                Ok(resp)
            }
            Err(e) => {
                log::error!("Error decoding response: {e:?}");
                log::debug!("Message: {rx_msg:02x?}");
                Err(BluetoothError::SpiProtocolError)
            }
        }
    }

    pub fn enable(&mut self) -> Result<(), BluetoothError> {
        self.enable_after_boot = true;
        match self.state {
            State::WaitingForConnection | State::Connected { .. } => Ok(()),
            State::Disabled => {
                log::info!("Enabling BLE");
                send_protocol_msg_wrapper!(
                    self,
                    Bluetooth::Enable,
                    Bluetooth::AckEnable => {
                        self.set_state(State::WaitingForConnection);
                        self.request_poll(BACKGROUND_POLL_MS);
                        Ok(())
                    }
                )
            }
            State::StartingFirmware | State::Booting => {
                log::debug!("Got Enable BLE while starting firmware");
                Ok(())
            }
            State::Unknown => {
                log::debug!("Got Enable BLE in Unknown state. ");
                // This is our escape hatch from the Unknown state.
                // Otherwise we stay there indefinitely.
                self.reset();
                Ok(())
            }
        }
    }

    pub fn disable(&mut self) -> Result<(), BluetoothError> {
        self.enable_after_boot = false;
        match self.state {
            State::Disabled => Ok(()),
            State::WaitingForConnection | State::Connected { .. } => {
                log::info!("Disabling BLE");
                send_protocol_msg_wrapper!(
                    self,
                    Bluetooth::Disable,
                    Bluetooth::AckDisable => {
                       self.set_state(State::Disabled);
                       Ok(())
                    }
                )
            }
            _ => {
                // Technically we are disabled, nothing to do.
                log::debug!("Got Disable BLE while firmware is not running.");
                Ok(())
            }
        }
    }

    pub fn disconnect(&mut self) -> Result<(), BluetoothError> {
        if !self.state.is_connected() {
            return Err(BluetoothError::InvalidState);
        }
        send_protocol_msg_wrapper!(
            self,
            Bluetooth::Disconnect,
            Bluetooth::AckDisconnect => Ok(())
        )
    }

    pub fn disable_adv_channels(&mut self, chans: AdvChannel) -> Result<(), BluetoothError> {
        match self.state {
            State::WaitingForConnection | State::Connected { .. } => {
                log::warn!("Got Disable Adv Channels while already advertising.");
                Ok(())
            }
            State::Disabled => {
                log::info!("Disable Adv Channels: {chans:?}");
                send_protocol_msg_wrapper!(
                    self,
                    Bluetooth::DisableChannels(to_adv_chan(chans)),
                    Bluetooth::AckDisableChannels => {
                        Ok(())
                    }
                )
            }
            State::StartingFirmware | State::Booting => {
                log::warn!("Got Disable Adv Channels while starting firmware.");
                Ok(())
            }
            State::Unknown => {
                log::debug!("Got Disable Adv Channels in Unknown state.");
                // This is our escape hatch from the Unknown state.
                // Otherwise we stay there indefinitely.
                self.reset();
                Ok(())
            }
        }
    }

    pub fn get_bt_addr(&mut self) -> Result<BtAddr, BluetoothError> {
        if !self.state.is_booted() {
            return Err(BluetoothError::InvalidState);
        }
        send_protocol_msg_wrapper!(
            self,
            Bluetooth::GetBtAddress,
            Bluetooth::AckBtAddress { bt_address } => Ok(bt_address.into())
        )
    }

    fn get_device_id(&mut self) -> Result<[u8; 8], BluetoothError> {
        if !self.state.is_booted() {
            return Err(BluetoothError::InvalidState);
        }
        send_protocol_msg_wrapper!(
            self,
            Bluetooth::GetDeviceId,
            Bluetooth::AckDeviceId { device_id } => Ok(device_id)
        )
    }

    pub fn get_bluetooth_status(&mut self) -> Result<BluetoothStatus, BluetoothError> {
        if !self.state.is_booted() {
            return Err(BluetoothError::InvalidState);
        }
        send_protocol_msg_wrapper!(
            self,
            Bluetooth::GetStatus,
            Bluetooth::Status(status) => Ok(status)
        )
    }

    pub fn send(&mut self, data: &[u8]) -> Result<(), BluetoothError> {
        if !self.state.is_enabled() {
            return Err(BluetoothError::InvalidState);
        }
        log::debug!("Sending packet {data:02x?}");
        let result = send_protocol_msg_wrapper!(
            self,
            Bluetooth::SendData(host_protocol::Message::from_slice(data).map_err(|_| BluetoothError::MessageTooLong)?),
            Bluetooth::SendDataResponse(response) => match response {
                SendDataResponse::Sent => Ok(()),
                SendDataResponse::BufferFull => Err(BluetoothError::BlePacketRejected),
            }
        );
        if result.is_ok() {
            self.stats.tx_packets += 1;
            self.stats.tx_size += data.len();
        }
        result
    }

    pub fn reset(&mut self) {
        log::info!("Resetting chip");
        self.gpio_api.set_pin(GpioPin::BtRst, false).expect("Could not set reset pin to low");
        std::thread::sleep(Duration::from_millis(10));
        self.gpio_api.set_pin(GpioPin::BtRst, true).expect("Could not set reset pin to high");
        self.get_state_tries = 0;
        self.set_state(State::Booting);
        self.request_poll(INITIAL_BOOT_TIME);
    }

    /// Returns true if we reached the proper state
    fn wait_for_state(
        &mut self,
        expected_state: host_protocol::State,
        timeout_ms: usize,
        retries: usize,
    ) -> bool {
        match self.send_protocol_msg(HostProtocolMessage::GetState, timeout_ms) {
            Ok(HostProtocolMessage::AckState(state)) if state == expected_state => true,
            Ok(msg) => {
                log::error!(
                    "Got invalid packet to GetState while waiting for state {expected_state:?} ({msg:02x?}). Stopping poll."
                );
                self.set_state(State::Unknown);
                false
            }
            Err(BluetoothError::SpiTimeout) => {
                self.get_state_tries += 1;
                if self.get_state_tries > retries {
                    log::warn!("Timed out waiting for state {expected_state:?}");
                    self.reset();
                } else {
                    log::debug!("Waiting for state {expected_state:?} {}/{}.", self.get_state_tries, retries);
                    self.request_poll(timeout_ms);
                }
                false
            }
            Err(e) => {
                log::error!("Error waiting for state {expected_state:?} ({e:?}). Stopping poll.");
                self.set_state(State::Unknown);
                false
            }
        }
    }

    fn send_device_name(&mut self) -> Result<(), BluetoothError> {
        if !self.state.is_booted() {
            return Err(BluetoothError::InvalidState);
        }
        let mut name = host_protocol::DeviceName::new();
        for c in self.device_name.chars() {
            if name.push(c).is_err() {
                // We hit max capacity, truncate the rest.
                break;
            }
        }
        log::debug!("Sending device name: {name}");
        send_protocol_msg_wrapper!(
            self,
            Bluetooth::SetDeviceName { name },
            Bluetooth::AckSetDeviceName => Ok(())
        )
    }

    fn read_version_info(&mut self) -> Option<&BleVersionInfo> {
        if self.state.is_booted() {
            log::warn!("read_version_info called while BLE firmware was already booted. Returning cached version info");
            return self.version_info.as_ref();
        }

        let Ok(HostProtocolMessage::Bootloader(host_protocol::Bootloader::AckBootloaderVersion {
            version: bootloader_version,
        })) = self
            .send_protocol_msg(
                HostProtocolMessage::Bootloader(host_protocol::Bootloader::BootloaderVersion),
                GENERAL_TIMEOUT,
            )
            .inspect_err(|e| {
                log::error!("Error reading bootloader version ({e:?})");
            })
        else {
            return None;
        };
        let bootloader_version = bootloader_version.to_string();

        let firmware_version = match self.send_protocol_msg(
            HostProtocolMessage::Bootloader(host_protocol::Bootloader::FirmwareVersion),
            VERIFY_FIRMWARE_TIMEOUT,
        ) {
            Ok(HostProtocolMessage::Bootloader(host_protocol::Bootloader::AckFirmwareVersion {
                version,
            })) => Some(version.to_string()),
            Ok(HostProtocolMessage::Bootloader(host_protocol::Bootloader::NoCosignHeader)) => None,
            _ => {
                self.set_state(State::Unknown);
                return None;
            }
        };

        if let Some(old_version) = &self.version_info {
            if old_version.bootloader_version != bootloader_version {
                log::info!(
                    "Bootloader version changed: {} -> {}",
                    old_version.bootloader_version,
                    bootloader_version
                );
            }

            if old_version.firmware_version != firmware_version {
                log::info!(
                    "Firmware version changed: {:?} -> {:?}",
                    old_version.firmware_version,
                    firmware_version
                );
            }
        } else {
            log::info!(
                "Read BLE bootloader version: {bootloader_version}, BLE firmware version: {}",
                firmware_version.as_ref().unwrap_or(&"N/A".to_string())
            );
        }

        self.version_info =
            Some(BleVersionInfo { bootloader_version: bootloader_version.to_string(), firmware_version });
        self.version_info.as_ref()
    }

    fn does_ble_firmware_need_update(&mut self) -> bool {
        if self.force_update {
            log::info!("Update firmware forced (single shot).");
            self.force_update = false;
            return true;
        }
        let version_info = self.read_version_info();
        let Some(BleVersionInfo { firmware_version, .. }) = version_info else {
            log::error!("Could not read BLE version info. Stopping Poll.");
            self.set_state(State::Unknown);
            return false;
        };

        match firmware_version {
            Some(version) => match Version::parse(version) {
                Ok(fw_ver) => {
                    if self.firmware.ver != fw_ver {
                        log::info!(
                                "Update firmware version ({}) is different from the running version ({}). Reflashing...",
                                self.firmware.ver,
                                fw_ver
                            );
                        return true;
                    } else {
                        log::info!("Already running the latest firmware version.");
                    }
                }
                Err(e) => {
                    log::error!("Error parsing firmware version ({e:?}). Updating...");
                    return true;
                }
            },
            None => {
                log::info!("Current firmware is broken. Updating...");
                return true;
            }
        }

        false
    }

    fn erase_firmware(&mut self) -> Result<(), BluetoothError> {
        log::info!("Erasing firmware");
        match self.send_protocol_msg(
            HostProtocolMessage::Bootloader(host_protocol::Bootloader::EraseFirmware),
            ERASE_FIRMWARE_TIMEOUT,
        ) {
            Ok(HostProtocolMessage::Bootloader(host_protocol::Bootloader::AckEraseFirmware)) => {
                log::info!("Firmware erased successfully");
                Ok(())
            }
            Ok(msg) => {
                log::error!("Got invalid packet to EraseFirmware. Stopping poll.");
                log::debug!("{msg:02x?}");
                Err(BluetoothError::SpiProtocolError)
            }
            Err(e) => {
                log::error!("Error erasing firmware ({e:?}). Stopping poll.");
                Err(e)
            }
        }
    }

    fn update_firmware(&mut self, upd: &[u8]) -> Result<(), BluetoothError> {
        log::info!("Updating firmware");
        let mut chunks_to_send = VecDeque::new();
        let mut retry_count = 0;
        chunks_to_send.extend(upd.chunks(256).enumerate());
        while let Some(app_chunk) = chunks_to_send.pop_front() {
            match self.send_protocol_msg(
                HostProtocolMessage::Bootloader(host_protocol::Bootloader::WriteFirmwareBlock {
                    block_idx: app_chunk.0,
                    block_data: app_chunk.1,
                }),
                WRITE_FIRMWARE_TIMEOUT,
            ) {
                Ok(HostProtocolMessage::Bootloader(host_protocol::Bootloader::AckWithIdxCrc {
                    block_idx,
                    crc,
                })) => {
                    let crc_pkt = Crc::<u32>::new(&CRC_32_ISCSI).checksum(app_chunk.1);
                    if (block_idx == app_chunk.0) && (crc == crc_pkt) {
                        if block_idx % 10 == 0 {
                            log::info!("Firmware block {block_idx} written successfully");
                        }
                    } else {
                        log::warn!("Firmware block {block_idx} CRC mismatch, will retry it later");
                        chunks_to_send.push_back((block_idx, app_chunk.1));
                        Self::check_retry(&mut retry_count)?;
                    }
                }
                Ok(HostProtocolMessage::Bootloader(host_protocol::Bootloader::NackWithIdx { block_idx })) => {
                    log::warn!("Firmware block {block_idx} write failed, will retry it later");
                    chunks_to_send.push_back((block_idx, app_chunk.1));
                    Self::check_retry(&mut retry_count)?;
                }
                Ok(msg) => {
                    log::error!("Got invalid packet to WriteFirmwareBlock. Stopping poll.");
                    log::debug!("{msg:02x?}");
                    return Err(BluetoothError::SpiProtocolError);
                }
                Err(e) => {
                    log::error!("Error updating firmware ({e:?}). Stopping poll.");
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    fn check_retry(retry_count: &mut usize) -> Result<(), BluetoothError> {
        *retry_count += 1;
        if *retry_count > UPD_MAX_WRITE_RETRY_COUNT {
            log::error!("Firmware update failed. Stopping poll.");
            return Err(BluetoothError::UnknownError);
        }
        Ok(())
    }

    fn boot_firmware(&mut self) {
        match self.send_protocol_msg(
            HostProtocolMessage::Bootloader(host_protocol::Bootloader::BootFirmware {
                trust: self.firmware.trust_level,
            }),
            VERIFY_FIRMWARE_TIMEOUT,
        ) {
            Ok(HostProtocolMessage::Bootloader(host_protocol::Bootloader::AckVerifyFirmware {
                result,
                hash,
            })) => {
                if result && hash != [0; 32] {
                    log::info!("Firmware verified successfully");
                    log::debug!("BT FW Hash: {hash:02x?}");
                    self.set_state(State::StartingFirmware);
                    self.get_state_tries = 0;
                    self.request_poll(START_FIRMWARE_WAIT_TIME);
                } else {
                    log::error!(
                        "Couldn't verify firmware: (valid={result:?}, hash={hash:02x?}). Forcing update."
                    );
                    self.force_update = true;
                    self.reset();
                }
            }
            Ok(msg) => {
                log::error!("Got invalid packet to BootFirmware. Stopping poll.");
                log::debug!("{msg:02x?}");
                self.set_state(State::Unknown);
            }
            Err(e) => {
                log::error!("Error starting firmware ({e:?}). Stopping poll.");
                self.set_state(State::Unknown);
            }
        }
    }

    fn set_challenge_secret(&mut self) -> Result<(), BluetoothError> {
        log::debug!("Setting challenge secret");
        let mut secret = [0u32; 8];
        for (i, chunk) in self.challenge_secret.secret.chunks(4).enumerate() {
            secret[i] = u32::from_le_bytes(chunk.try_into().unwrap());
        }
        return match self.send_protocol_msg(
            HostProtocolMessage::Bootloader(host_protocol::Bootloader::ChallengeSet { secret }),
            WRITE_FIRMWARE_TIMEOUT,
        ) {
            Ok(HostProtocolMessage::Bootloader(host_protocol::Bootloader::AckChallengeSet { result })) => {
                match result {
                    host_protocol::SecretSaveResponse::Sealed => Ok(()),
                    host_protocol::SecretSaveResponse::NotAllowed => {
                        log::error!("Challenge secret already set.");
                        Ok(())
                    }
                    host_protocol::SecretSaveResponse::Error => {
                        log::error!("Error setting challenge secret.");
                        Err(BluetoothError::UnknownError)
                    }
                }
            }
            Err(e) => {
                log::error!("Error setting challenge secret ({e:?}). Stopping poll.");
                Err(e)
            }
            _ => {
                log::error!("Got invalid packet to ChallengeSet. Stopping poll.");
                Err(BluetoothError::SpiProtocolError)
            }
        };
    }

    fn challenge(&mut self) -> Result<(), BluetoothError> {
        log::debug!("Challenging BT FW");
        self.challenge_last_check = Instant::now();
        let mut buf = [0u8; 8];
        getrandom::getrandom(&mut buf).map_err(|_| BluetoothError::Random)?;
        let nonce = u64::from_be_bytes(buf);
        log::debug!("Nonce: {nonce}");
        return match self
            .send_protocol_msg(HostProtocolMessage::ChallengeRequest { nonce }, VERIFY_FIRMWARE_TIMEOUT)
        {
            Ok(HostProtocolMessage::ChallengeResult { result }) => {
                let expected = self
                    .crypto
                    .hmac256(self.challenge_secret.secret.to_vec(), nonce.to_be_bytes().to_vec())
                    .map_err(|_| BluetoothError::Crypto)?;
                if expected != result {
                    log::error!("Challenge failed.");
                    Err(BluetoothError::UnknownError)
                } else {
                    Ok(())
                }
            }
            Err(e) => {
                log::error!("Error sending challenge ({e:?}). Stopping poll.");
                Err(e)
            }
            _ => {
                log::error!("Got invalid packet to ChallengeRequest. Stopping poll.");
                Err(BluetoothError::SpiProtocolError)
            }
        };
    }

    fn recv_messages(&mut self) -> Result<(), BluetoothError> {
        for _ in 0..32 {
            log::trace!("Polling messages");
            match self.send_protocol_msg(
                HostProtocolMessage::Bluetooth(Bluetooth::GetReceivedData),
                GENERAL_TIMEOUT,
            )? {
                HostProtocolMessage::Bluetooth(Bluetooth::ReceivedData(data)) => {
                    log::debug!("Received packet: {data:02x?}");
                    let event = BlePacket(data.to_vec());
                    self.packet_subscribers.send_nowait(&event);
                    self.stats.rx_size += data.len();
                    self.stats.rx_packets += 1;
                }
                HostProtocolMessage::Bluetooth(Bluetooth::NoReceivedData) => {
                    self.request_poll(BACKGROUND_POLL_MS);

                    return Ok(());
                }
                msg => {
                    log::error!("Got invalid packet to GetReceivedData.");
                    log::error!("{msg:02x?}");
                    return Err(BluetoothError::SpiProtocolError);
                }
            }
        }
        // There still are messages, but let's process other stuff
        // and get back to this in a millisecond
        self.request_poll(1);
        Ok(())
    }

    pub fn test_echo(&mut self, size: usize, character: u8) -> Result<(), BluetoothError> {
        if !self.state.is_booted() {
            return Err(BluetoothError::InvalidState);
        }
        if size > APP_MTU {
            log::error!("Too big echo packet requested on API: {size}");
            return Err(BluetoothError::MessageTooLong);
        }
        let message = std::iter::repeat_n(character, size).collect();
        let response = send_protocol_msg_wrapper!(
            self,
            Bluetooth::Echo(message),
            Bluetooth::EchoResponse(response) => Ok(response)
        )?;
        if response.len() != size || !response.iter().all(|c| *c == character) {
            log::error!("Response was invalid ({size} x 0x{character:02x}): {response:02x?}");
            return Err(BluetoothError::SpiProtocolError);
        }
        Ok(())
    }

    pub fn poll(&mut self) {
        match self.state {
            State::Booting => {
                if self.wait_for_state(
                    host_protocol::State::FirmwareUpgrade,
                    BOOT_TIME_GET_STATE_TIMEOUT,
                    BOOT_TIME_GET_STATE_RETRIES,
                ) {
                    let upd_img = self.firmware.img.clone();
                    if !upd_img.is_empty() && self.does_ble_firmware_need_update() {
                        if self.erase_firmware().is_err() {
                            self.set_state(State::Unknown);
                            return;
                        }
                        if self.update_firmware(&upd_img).is_err() {
                            self.set_state(State::Unknown);
                            return;
                        }

                        // Update the version info to reflect the new firmware version
                        if let Some(version_info) = self.version_info.as_mut() {
                            version_info.firmware_version = Some(self.firmware.ver.to_string());
                        }
                    }
                    self.read_version_info();
                    if !self.challenge_secret.sent
                        && self
                            .set_challenge_secret()
                            .inspect_err(|e| log::error!("Error setting challenge secret ({e:?})"))
                            .is_ok()
                    {
                        self.challenge_secret.sent = true;
                        self.security.set_bluetooth_challenge_secret_sent();
                    }
                    self.is_challenge_ok =
                        self.challenge().inspect_err(|e| log::error!("Error challenge ({e:?})")).is_ok();
                    self.boot_firmware();
                }
            }

            State::StartingFirmware => {
                if self.wait_for_state(
                    host_protocol::State::Disabled,
                    START_FIRMWARE_GET_STATE_TIMEOUT,
                    START_FIRMWARE_TIME_GET_STATE_RETRIES,
                ) {
                    log::info!("Firmware started");
                    self.set_state(State::Disabled);
                    if !self.device_id_sent {
                        match self.get_device_id() {
                            Ok(device_id) => {
                                log::debug!("First boot, send device id ({device_id:02x?}) to security");
                                self.security.set_bluetooth_device_id(device_id);
                                self.device_id_sent = true;
                            }
                            Err(_) => return,
                        }
                    }
                    if self.enable_after_boot {
                        if let Err(e) = self.enable() {
                            log::error!("Chip could not be enabled ({e:?}).");
                            self.reset();
                        }
                    }
                    if !self.device_name.is_empty() {
                        if let Err(e) = self.send_device_name() {
                            log::error!("Could not set device name ({e:?}).");
                            self.reset();
                        }
                    }
                }
            }

            State::WaitingForConnection | State::Connected { .. } => {
                if let Err(e) = self.recv_messages() {
                    log::error!("Error receiving packets ({e:?}).");
                    self.reset();
                };
                if self.challenge_last_check.elapsed().as_secs() > BT_CHALLENGE_PERIOD_SECS {
                    self.is_challenge_ok =
                        self.challenge().inspect_err(|e| log::error!("Error challenge ({e:?})")).is_ok();
                }
                self.stats.print();
                self.refresh_connection().ok();
            }
            State::Disabled => {
                if self.challenge_last_check.elapsed().as_secs() > BT_CHALLENGE_PERIOD_SECS {
                    self.is_challenge_ok =
                        self.challenge().inspect_err(|e| log::error!("Error challenge ({e:?})")).is_ok();
                }
            }
            State::Unknown => {
                log::warn!("Got Poll in Unknown state.")
            }
        }
    }

    pub fn get_version_info(&mut self) -> Option<BleVersionInfo> { self.version_info.clone() }
}

impl Stats {
    fn print(&mut self) {
        let stats_new_time = Instant::now();
        let elapsed = stats_new_time.duration_since(self.since);
        if elapsed > Duration::from_millis(STATS_PRINT_PERIOD_MS) {
            let elapsed = elapsed.as_secs_f32();
            if self.rx_packets != 0 || self.tx_packets != 0 {
                log::info!(
                    "BT throughtput  RX: {:.1}kBps, {:.1}pps  TX: {:.1}kBps, {:.1}pps",
                    self.rx_size as f32 / 1000.0 / elapsed,
                    self.rx_packets as f32 / elapsed,
                    self.tx_size as f32 / 1000.0 / elapsed,
                    self.tx_packets as f32 / elapsed,
                )
            }
            self.tx_size = 0;
            self.rx_size = 0;
            self.tx_packets = 0;
            self.rx_packets = 0;
            self.since = stats_new_time;
        }
    }
}

impl server::ScalarEventHandler<gpio::IrqMessage> for BluetoothServer {
    fn handle(&mut self, _msg: gpio::IrqMessage, _pid: NonZeroU8, _context: &mut ServerContext<Self>) {
        log::trace!("Got GPIO low");
        if self.state.is_enabled() {
            // Only try receiving stuff if we are in a state to do so
            // Otherwise consider this GPIO edge a spurious one.
            self.poll();
        }
    }
}

impl server::ArchiveEventHandler<DeviceName> for BluetoothServer {
    fn handle(
        &mut self,
        msg: server::Owned<DeviceName>,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.device_name = msg.0.to_string();
        if self.state.is_booted() {
            if let Err(e) = self.send_device_name() {
                log::error!("Could not set device name ({e:?}).");
                self.reset();
            }
        }
    }
}

fn to_adv_chan(chans: AdvChannel) -> AdvChan { AdvChan::from_bits_truncate(chans.bits()) }
