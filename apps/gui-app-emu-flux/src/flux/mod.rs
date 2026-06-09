// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU32, Ordering},
        LazyLock, RwLock,
    },
    time::{Duration, Instant},
};

use server::{
    BlockingArchive, BlockingArchiveHandler, BlockingScalar, BlockingScalarHandler, LendMut, LendMutHandler,
    MessageId as _, ScalarHandler, Server,
};

#[cfg(keyos)]
legacy_hid::use_api!();

use gui_app_emu_flux::{
    keys,
    messages::{RecvSeph, SendSeph, SvcCall, SyscallBuffer},
    syscall_id::*,
};

pub mod display;

#[cfg(not(keyos))]
mod hosted;
#[cfg(not(keyos))]
use hosted::{draw_image, draw_image_rle, draw_rect};

#[cfg(keyos)]
mod atsama5d2;
#[cfg(keyos)]
use atsama5d2::{draw_image, draw_image_rle, draw_rect};

const CX_APILEVEL: u32 = 12;

static TRY_CONTEXT: AtomicU32 = AtomicU32::new(0);
static SEPH_FIFO: LazyLock<RwLock<VecDeque<Vec<u8>>>> = LazyLock::new(|| RwLock::new(VecDeque::new()));
/// Last channel ID seen from the host (for framing outgoing responses).
/// Written by the inbound-APDU subscriber, read by the `Rapdu` handler.
#[cfg(keyos)]
static LAST_CHANNEL_ID: AtomicU32 = AtomicU32::new(0x0101);

/// Fixed test seed for hosted emulator builds (SHA-256 of "test seed").
#[cfg(not(keyos))]
const HOSTED_TEST_APP_SEED: [u8; 32] = [
    0x9f, 0x86, 0xd0, 0x81, 0x88, 0x4c, 0x7d, 0x65, 0x9a, 0x2f, 0xea, 0xa0, 0xc5, 0x5a, 0xd0, 0x15, 0xa3,
    0xbf, 0x4f, 0x1b, 0x2b, 0x0b, 0x82, 0x2c, 0xd1, 0x5d, 0x6c, 0x15, 0xb0, 0xf0, 0x0a, 0x08,
];

#[cfg(keyos)]
fn default_app_seed() -> Vec<u8> { Vec::new() }

#[cfg(not(keyos))]
fn default_app_seed() -> Vec<u8> { HOSTED_TEST_APP_SEED.to_vec() }

/// App seed for key derivation.
///
/// Hardware builds intentionally have no default seed; `main` must install a
/// 32-byte AppSeed from the security API or a 64-byte BIP39 seed from manual
/// entry before child apps can derive keys. Hosted emulator builds keep a fixed
/// deterministic test seed for convenience.
static APP_SEED: LazyLock<RwLock<Vec<u8>>> = LazyLock::new(|| RwLock::new(default_app_seed()));

/// Set the app seed for key derivation.
/// Accepts 32 bytes (derived AppSeed) or 64 bytes (BIP39 seed from a manual mnemonic entry).
pub fn set_app_seed(seed: Vec<u8>) {
    if let Ok(mut app_seed) = APP_SEED.write() {
        log::debug!("App seed updated ({} bytes)", seed.len());
        *app_seed = seed;
    }
}

/// Get the current app seed.
fn get_app_seed() -> Result<Vec<u8>, keys::KeyError> {
    let seed = APP_SEED.read().map(|s| s.clone()).map_err(|_| keys::KeyError::LockPoisoned)?;
    if seed.is_empty() {
        log::error!("App seed requested before configuration");
        return Err(keys::KeyError::SeedNotAvailable);
    }
    Ok(seed)
}

/// Wrap a reassembled inbound HID APDU as a SEPH `CapduEvent` and push it
/// into the in-process FIFO that running Flux child apps drain via
/// `RecvSeph`. Also stashes the channel id for the next outgoing `Rapdu`.
///
/// On hosted builds the FIFO and channel-id atomic don't carry traffic, so
/// this is a no-op there.
#[cfg(keyos)]
pub fn push_incoming_apdu(channel_id: u16, apdu: &[u8]) {
    LAST_CHANNEL_ID.store(channel_id as u32, Ordering::Relaxed);
    let apdu_len = apdu.len() as u16;
    let mut pkt = vec![SephTag::CapduEvent.into()];
    pkt.extend_from_slice(&apdu_len.to_be_bytes());
    pkt.extend_from_slice(apdu);
    match SEPH_FIFO.write() {
        Ok(mut fifo) => {
            fifo.push_back(pkt);
            log::debug!("CapduEvent pushed to SEPH_FIFO (depth={})", fifo.len());
        }
        Err(e) => log::error!("Failed to write CapduEvent to SEPH_FIFO: {e:?}"),
    }
}

/// Generate a random u32 from the platform RNG.
fn generate_random_u32() -> u32 {
    let mut bytes = [0u8; 4];
    if let Err(e) = getrandom::getrandom(&mut bytes) {
        log::error!("generate_random_u32: getrandom failed: {e:?}");
        return 0;
    }
    u32::from_le_bytes(bytes)
}

#[derive(Debug, Clone, PartialEq)]
enum SephTag {
    FingerEventTouch,
    FingerEventRelease,
    ButtonPushEvent,
    FingerEvent,
    DisplayProcessedEvent,
    TickerEvent,
    CapduEvent,
    Mcu,
    TagBleSend,
    TagBleRadioPower,
    SePowerOff,
    UsbConfig,
    UsbEpPrepare,
    NfcRapdu,
    NfcPower,
    RequestStatus,
    Rapdu,
    PlayTune,
    DbgScreenDisplayStatus,
    PrintcStatus,
    GeneralStatus,
    ScreenDisplayStatus,
    PrintfStatus,
    ScreenDisplayRawStatus,
    BaglDrawRect,
    BaglDrawBitmap,
    NbglDrawRect,
    NbglRefresh,
    NbglDrawLine,
    NbglDrawImage,
    NbglDrawImageFile,
    NbglDrawImageRle,
    NbglDrawText,
}

impl From<u8> for SephTag {
    fn from(value: u8) -> Self {
        match value {
            0x01 => Self::FingerEventTouch,
            0x02 => Self::FingerEventRelease,
            0x05 => Self::ButtonPushEvent,
            0x0c => Self::FingerEvent,
            0x0d => Self::DisplayProcessedEvent,
            0x0e => Self::TickerEvent,
            0x16 => Self::CapduEvent,
            0x31 => Self::Mcu,
            0x38 => Self::TagBleSend,
            0x44 => Self::TagBleRadioPower,
            0x46 => Self::SePowerOff,
            0x4f => Self::UsbConfig,
            0x50 => Self::UsbEpPrepare,
            0x4a => Self::NfcRapdu,
            0x34 => Self::NfcPower,
            0x52 => Self::RequestStatus,
            0x53 => Self::Rapdu,
            0x56 => Self::PlayTune,
            0x5e => Self::DbgScreenDisplayStatus,
            0x5f => Self::PrintcStatus,
            0x60 => Self::GeneralStatus,
            0x65 => Self::ScreenDisplayStatus,
            0x66 => Self::PrintfStatus,
            0x69 => Self::ScreenDisplayRawStatus,
            0xf1 => Self::BaglDrawRect,
            0xf2 => Self::BaglDrawBitmap,
            0xfa => Self::NbglDrawRect,
            0xfb => Self::NbglRefresh,
            0xfc => Self::NbglDrawLine,
            0xfd => Self::NbglDrawImage,
            0xfe => Self::NbglDrawImageFile,
            0xff => Self::NbglDrawImageRle,
            0xf9 => Self::NbglDrawText,
            v => {
                log::error!("Unknown SEPH tag: 0x{v:02x}, ignoring");
                Self::GeneralStatus // safe no-op fallback
            }
        }
    }
}

impl From<SephTag> for u8 {
    fn from(value: SephTag) -> Self {
        match value {
            SephTag::FingerEventTouch => 0x01,
            SephTag::FingerEventRelease => 0x02,
            SephTag::ButtonPushEvent => 0x05,
            SephTag::FingerEvent => 0x0c,
            SephTag::DisplayProcessedEvent => 0x0d,
            SephTag::TickerEvent => 0x0e,
            SephTag::CapduEvent => 0x16,
            SephTag::Mcu => 0x31,
            SephTag::TagBleSend => 0x38,
            SephTag::TagBleRadioPower => 0x44,
            SephTag::SePowerOff => 0x46,
            SephTag::UsbConfig => 0x4f,
            SephTag::UsbEpPrepare => 0x50,
            SephTag::NfcRapdu => 0x4a,
            SephTag::NfcPower => 0x34,
            SephTag::RequestStatus => 0x52,
            SephTag::Rapdu => 0x53,
            SephTag::PlayTune => 0x56,
            SephTag::DbgScreenDisplayStatus => 0x5e,
            SephTag::PrintcStatus => 0x5f,
            SephTag::GeneralStatus => 0x60,
            SephTag::ScreenDisplayStatus => 0x65,
            SephTag::PrintfStatus => 0x66,
            SephTag::ScreenDisplayRawStatus => 0x69,
            SephTag::BaglDrawRect => 0xf1,
            SephTag::BaglDrawBitmap => 0xf2,
            SephTag::NbglDrawRect => 0xfa,
            SephTag::NbglRefresh => 0xfb,
            SephTag::NbglDrawLine => 0xfc,
            SephTag::NbglDrawImage => 0xfd,
            SephTag::NbglDrawImageFile => 0xfe,
            SephTag::NbglDrawImageRle => 0xff,
            SephTag::NbglDrawText => 0xf9,
        }
    }
}

#[derive(Debug, Clone)]
struct SephPacket<'a> {
    tag: SephTag,
    data: &'a [u8],
}

impl<'a> SephPacket<'a> {
    fn parse(raw: &'a [u8]) -> Option<Self> {
        if raw.len() < 3 {
            log::error!("SephPacket: too short ({} bytes, need >= 3): {:02x?}", raw.len(), raw);
            return None;
        }
        let tag = SephTag::from(raw[0]);
        let len = u16::from_be_bytes([raw[1], raw[2]]) as usize;
        let data = &raw[3..];
        if data.len() != len {
            log::warn!(
                "SephPacket: length mismatch tag={:?} header_len={} actual_len={}, raw={:02x?}",
                tag,
                len,
                data.len(),
                &raw[..raw.len().min(20)]
            );
            // Use the smaller of the two to avoid out-of-bounds
            let usable = data.len().min(len);
            return Some(Self { tag, data: &data[..usable] });
        }
        Some(Self { tag, data })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NbglArea {
    x0: u16,
    y0: u16,
    width: u16,
    height: u16,
    color: u8,
    bpp: u8,
}

impl From<&[u8]> for NbglArea {
    fn from(raw: &[u8]) -> Self {
        assert!(raw.len() >= 10);
        Self {
            x0: u16::from_be_bytes(raw[0..2].try_into().unwrap()),
            y0: u16::from_be_bytes(raw[2..4].try_into().unwrap()),
            width: u16::from_be_bytes(raw[4..6].try_into().unwrap()),
            height: u16::from_be_bytes(raw[6..8].try_into().unwrap()),
            color: raw[8],
            bpp: raw[9],
        }
    }
}

fn nbgl_bpp_to_read_bpp(bpp: u8) -> u8 {
    match bpp {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 0,
    }
}

/// Minimum interval between synthetic ticker events (matches real hardware).
const TICKER_INTERVAL: Duration = Duration::from_millis(100);

/// System event: a client process disconnected from the Flux server.
///
/// Registered via `xous::register_system_event_handler(SystemEvent::Disconnected, ...)`.
/// The kernel delivers the connection ID of the disconnected process.
#[derive(Debug, server::Message)]
struct AppDisconnected(xous::CID);

#[derive(server::Server)]
#[name = "os/gui-app-emu-flux"]
pub struct FluxServer {
    last_ticker: Instant,
    #[cfg(keyos)]
    legacy_hid: LegacyHidApi,
}

impl Default for FluxServer {
    fn default() -> Self {
        log::debug!("Initializing Flux");

        Self {
            last_ticker: Instant::now(),
            #[cfg(keyos)]
            legacy_hid: LegacyHidApi::default(),
        }
    }
}

impl Server for FluxServer {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        xous::register_system_event_handler(
            xous::SystemEvent::Disconnected,
            context.sid(),
            AppDisconnected::ID,
        )
        .expect("Failed to register disconnected handler");
    }
}

impl ScalarHandler<AppDisconnected> for FluxServer {
    fn handle(
        &mut self,
        _msg: AppDisconnected,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        log::info!("Flux app disconnected, resetting display");
        display::reset();
    }
}

impl BlockingScalarHandler<SvcCall> for FluxServer {
    fn handle(
        &mut self,
        msg: SvcCall,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SvcCall as BlockingScalar>::Response {
        match msg.0 {
            // --- Existing syscalls ---
            SYSCALL_GET_API_LEVEL_ID => CX_APILEVEL,
            SYSCALL_TRY_CONTEXT_GET_ID => TRY_CONTEXT.load(Ordering::Relaxed),
            SYSCALL_TRY_CONTEXT_SET_ID => TRY_CONTEXT.swap(msg.1, Ordering::Relaxed),
            SYSCALL_IO_SEPH_IS_STATUS_SENT_ID => 1, // always true

            // --- Process control ---
            SYSCALL_OS_SCHED_EXIT_ID => {
                let status = msg.1;
                log::info!("os_sched_exit called with status: {}", status);
                0
            }

            // --- Library calls (stubs for now) ---
            SYSCALL_OS_LIB_CALL_ID => {
                log::debug!("os_lib_call: app_id={}", msg.1);
                0
            }
            SYSCALL_OS_LIB_END_ID => {
                log::debug!("os_lib_end called");
                0
            }

            // --- OS info syscalls ---
            SYSCALL_OS_FLAGS_ID => 0,
            SYSCALL_OS_SEPH_FEATURES_ID => 0,
            SYSCALL_OS_PERSO_ISONBOARDED_ID => 1,
            SYSCALL_OS_GLOBAL_PIN_IS_VALIDATED_ID => 1,

            // --- Random number generation (scalar) ---
            SYSCALL_CX_GET_RANDOM_BYTES_ID | SYSCALL_CX_TRNG_GET_RANDOM_DATA_ID => {
                let random = generate_random_u32();
                log::debug!("cx_rng_u32: returning 0x{:08x}", random);
                random
            }

            syscall => {
                log::warn!("Unimplemented scalar syscall 0x{:08X}", syscall);
                0
            }
        }
    }
}

/// LendMut handler for syscalls that need buffer access.
///
/// Currently handles:
/// - CRC32 (cx_crc_hw): input data in buffer, CRC32 result written back
/// - BIP32 derivation (os_perso_derive_node_bip32): path in buffer, key+chaincode written back
///
/// Other crypto operations (hash, ECDSA, HMAC, etc.) are handled locally in the
/// app process via `#[no_mangle]` function overrides in `crypto.rs`.
impl LendMutHandler<SyscallBuffer> for FluxServer {
    fn handle(
        &mut self,
        mut msg: SyscallBuffer,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SyscallBuffer as LendMut>::Response {
        let buf = msg.buf.as_slice_mut::<u8>();
        let syscall_id = msg.syscall_id;
        let arg = msg.arg;

        log::trace!("SyscallBuffer: id=0x{:08X}, arg={}, buf_len={}", syscall_id, arg, buf.len());

        match syscall_id {
            // --- Hash operations ---
            // cx_hash_no_throw: buf contains data, arg contains flags
            // Returns digest length on CX_LAST, 0 otherwise
            SYSCALL_CX_CRC_HW_ID => {
                // CRC32: buf contains data, return updated CRC
                let data_len = arg as usize;
                if data_len > buf.len() {
                    return usize::MAX; // Error: invalid length
                }
                let mut hasher = crc32fast::Hasher::new();
                hasher.update(&buf[..data_len]);
                let crc = hasher.finalize();
                buf[..4].copy_from_slice(&crc.to_le_bytes());
                4 // Output length
            }

            // --- BIP32 key derivation ---
            // os_perso_derive_node_bip32: buf contains path (array of u32), arg contains path_len
            // Output: private_key (32 bytes) + chain_code (32 bytes) written to buf
            SYSCALL_OS_PERSO_DERIVE_NODE_BIP32_ID => {
                let path_len = arg as usize;
                if path_len * 4 > buf.len() || buf.len() < 64 {
                    log::warn!("BIP32: invalid buffer size");
                    return usize::MAX;
                }

                // Read path from buffer (array of u32, big-endian)
                let mut path = Vec::with_capacity(path_len);
                for i in 0..path_len {
                    let offset = i * 4;
                    let element = u32::from_be_bytes(buf[offset..offset + 4].try_into().unwrap());
                    path.push(element);
                }

                log::debug!("BIP32 derivation path: {:08X?}", path);

                let app_seed = match get_app_seed() {
                    Ok(seed) => seed,
                    Err(e) => {
                        log::warn!("BIP32 derivation unavailable: {:?}", e);
                        return usize::MAX;
                    }
                };
                match keys::derive_bip32(&app_seed, &path, keys::curves::CX_CURVE_SECP256K1) {
                    Ok((private_key, chain_code)) => {
                        // Write output: private_key (32) + chain_code (32)
                        buf[..32].copy_from_slice(&private_key);
                        buf[32..64].copy_from_slice(&chain_code);
                        0 // Success
                    }
                    Err(e) => {
                        log::warn!("BIP32 derivation failed: {:?}", e);
                        usize::MAX
                    }
                }
            }

            // --- SLIP-10 / BIP32 key derivation with mode + curve ---
            // os_perso_derive_node_with_seed_key: mode, curve, and path_len packed in arg
            //   arg bits [31:24] = mode (0=BIP32, 1=SLIP10_ED25519)
            //   arg bits [23:16] = curve
            //   arg bits [15:0]  = path_len
            // Buffer input: path elements (4 bytes each, BE)
            // Buffer output: private_key (32) + chain_code (32)
            SYSCALL_OS_PERSO_DERIVE_NODE_WITH_SEED_KEY_ID => {
                let mode = (arg >> 24) & 0xFF;
                let curve = ((arg >> 16) & 0xFF) as u8;
                let path_len = (arg & 0xFFFF) as usize;

                if path_len * 4 > buf.len() || buf.len() < 64 {
                    log::warn!("derive_node_with_seed_key: invalid buffer size");
                    return usize::MAX;
                }

                // Read path from buffer (array of u32, big-endian)
                let mut path = Vec::with_capacity(path_len);
                for i in 0..path_len {
                    let offset = i * 4;
                    let element = u32::from_be_bytes(buf[offset..offset + 4].try_into().unwrap());
                    path.push(element);
                }

                log::debug!(
                    "derive_node_with_seed_key: mode={}, curve=0x{:02x}, path={:08X?}",
                    mode,
                    curve,
                    path
                );

                let app_seed = match get_app_seed() {
                    Ok(seed) => seed,
                    Err(e) => {
                        log::warn!("derive_node_with_seed_key: seed unavailable: {:?}", e);
                        return usize::MAX;
                    }
                };
                let result = match mode {
                    1 => {
                        // HDW_ED25519_SLIP10: use SLIP-10 derivation
                        keys::derive_slip10_ed25519(&app_seed, &path)
                    }
                    0 => {
                        // HDW_NORMAL: use BIP32 derivation
                        keys::derive_bip32(&app_seed, &path, curve)
                    }
                    _ => {
                        log::warn!("derive_node_with_seed_key: unsupported mode {}", mode);
                        Err(keys::KeyError::DerivationFailed)
                    }
                };

                match result {
                    Ok((private_key, chain_code)) => {
                        buf[..32].copy_from_slice(&private_key);
                        buf[32..64].copy_from_slice(&chain_code);
                        0 // Success
                    }
                    Err(e) => {
                        log::warn!("derive_node_with_seed_key failed: {:?}", e);
                        usize::MAX
                    }
                }
            }

            syscall => {
                log::warn!("Unimplemented buffer syscall 0x{:08X}", syscall);
                usize::MAX // Error indicator
            }
        }
    }
}

impl server::ArchiveHandler<SendSeph> for FluxServer {
    fn handle(
        &mut self,
        msg: server::Owned<SendSeph>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let Ok(msg) = msg.deserialize() else { return };
        let Some(pkt) = SephPacket::parse(msg.0.as_slice()) else {
            return;
        };
        log::trace!("SendSeph: tag={:?} ({} bytes payload)", pkt.tag, pkt.data.len());
        match pkt.tag {
            SephTag::GeneralStatus => {
                if pkt.data == [0x00, 0x00] {
                    // SDK signals "ready for next event". Push a TickerEvent only if
                    // the FIFO is empty AND enough time has elapsed since the last tick
                    // (100ms, matching real hardware). This prevents a tight
                    // busy-loop that starves other KeyOS processes.
                    if let Ok(fifo) = SEPH_FIFO.read() {
                        if fifo.is_empty() && self.last_ticker.elapsed() >= TICKER_INTERVAL {
                            drop(fifo);
                            if let Ok(mut fifo) = SEPH_FIFO.write() {
                                fifo.push_back(vec![SephTag::TickerEvent.into(), 0, 0]);
                                self.last_ticker = Instant::now();
                            }
                        }
                    }
                } else {
                    log::error!("General Status: {pkt:02x?}");
                }
            }
            SephTag::ScreenDisplayStatus | SephTag::DbgScreenDisplayStatus | SephTag::BaglDrawRect => {
                log::trace!("Screen Display Status: {:02x?}", pkt.data);
                if pkt.tag != SephTag::BaglDrawRect {
                    let mut fifo = SEPH_FIFO.write().unwrap();
                    fifo.push_back(vec![SephTag::DisplayProcessedEvent.into(), 0, 0]);
                }
            }
            SephTag::ScreenDisplayRawStatus | SephTag::BaglDrawBitmap => {
                log::trace!("Screen Display Raw Status: {:02x?}", pkt.data);
                if pkt.tag != SephTag::BaglDrawBitmap {
                    let mut fifo = SEPH_FIFO.write().unwrap();
                    fifo.push_back(vec![SephTag::DisplayProcessedEvent.into(), 0, 0]);
                }
            }
            SephTag::PrintcStatus | SephTag::PrintfStatus => {
                log::info!("Printc Status: {}", String::from_utf8_lossy(pkt.data));
            }
            SephTag::Rapdu => {
                log::trace!("Rapdu ({} bytes): {:02x?}", pkt.data.len(), pkt.data);
                #[cfg(keyos)]
                {
                    let channel_id = LAST_CHANNEL_ID.load(Ordering::Relaxed) as u16;
                    log::trace!("Rapdu: forwarding via legacy-hid channel_id=0x{channel_id:04x}");
                    if let Err(e) = self.legacy_hid.write_apdu(channel_id, pkt.data.to_vec()) {
                        log::error!("Rapdu: legacy-hid write failed: {e:?}");
                    }
                }
            }
            SephTag::UsbConfig => {
                log::trace!("UsbConfig: {:02x?}", pkt.data);
            }
            SephTag::UsbEpPrepare => {
                // The SDK's USB stack occasionally writes raw bytes directly to the
                // HID/WebUSB IN endpoints, bypassing the SEPH Rapdu wrapper. With
                // the HID endpoint now owned by `legacy-hid` we no longer expose
                // a direct write path; route HID-channel traffic through the same
                // `WriteHidApdu` IPC, and ignore WebUSB (currently disabled).
                log::trace!("UsbEpPrepare: {:02x?}", pkt.data);
                let ep = pkt.data[0];
                #[cfg(keyos)]
                let data_len = pkt.data[2] as usize;
                match ep {
                    0x82 => {
                        #[cfg(keyos)]
                        {
                            let channel_id = LAST_CHANNEL_ID.load(Ordering::Relaxed) as u16;
                            let payload = pkt.data[3..3 + data_len].to_vec();
                            if let Err(e) = self.legacy_hid.write_apdu(channel_id, payload) {
                                log::error!("UsbEpPrepare(HID): legacy-hid write failed: {e:?}");
                            }
                        }
                    }
                    0x83 => log::trace!("UsbEpPrepare(WebUSB): ignored — WebUSB not enabled"),
                    _ => log::error!("Unknown endpoint: {ep}"),
                }
            }
            SephTag::PlayTune => {
                log::trace!("PlayTune: {:02x?}", pkt.data);
            }
            SephTag::NbglDrawRect => {
                log::trace!("NbglDrawRect: {:02x?}", pkt.data);
                let area = NbglArea::from(pkt.data);
                draw_rect(area.x0, area.y0, area.width, area.height, (area.color as u32) * 0x555555);
                if let Ok(mut fifo) = SEPH_FIFO.write() {
                    fifo.push_back(vec![SephTag::DisplayProcessedEvent.into(), 0, 0]);
                }
            }
            SephTag::NbglRefresh => {
                log::trace!("NbglRefresh");
                display::refresh();
                if let Ok(mut fifo) = SEPH_FIFO.write() {
                    fifo.push_back(vec![SephTag::DisplayProcessedEvent.into(), 0, 0]);
                }
            }
            SephTag::NbglDrawLine => {
                log::trace!("NbglDrawLine: {:02x?}", pkt.data);
                let area = NbglArea::from(pkt.data);
                let line_color = (pkt.data[pkt.data.len() - 1] as u32) * 0x555555;
                // Fill the area with lineColor (dotStartIdx ignored — solid line).
                draw_rect(area.x0, area.y0, area.width, area.height, line_color);
                if let Ok(mut fifo) = SEPH_FIFO.write() {
                    fifo.push_back(vec![SephTag::DisplayProcessedEvent.into(), 0, 0]);
                }
            }
            SephTag::NbglDrawImage => {
                log::trace!("NbglDrawImage: {:02x?}", pkt.data);
                let area = NbglArea::from(pkt.data);
                let bpp = nbgl_bpp_to_read_bpp(area.bpp);
                let bit_size = area.width as usize * area.height as usize * bpp as usize;
                let buffer_size = bit_size.div_ceil(8);
                let buffer = &pkt.data[10..10 + buffer_size];
                let transformation = pkt.data[10 + buffer_size];
                let color_map = pkt.data[10 + buffer_size + 1];
                draw_image(area, bpp, transformation, buffer, color_map);
                if let Ok(mut fifo) = SEPH_FIFO.write() {
                    fifo.push_back(vec![SephTag::DisplayProcessedEvent.into(), 0, 0]);
                }
            }
            SephTag::NbglDrawImageFile => {
                // Payload: area(10) + color_map(1) + image_file_header(8) + compressed/raw data
                // Header: [width_lo, width_hi, height_lo, height_hi, (bpp_fmt<<4)|compression, len_0, len_1,
                // len_2] Compression: 0=None, 1=Gzlib, 2=Rle
                if pkt.data.len() < 19 {
                    log::warn!("NbglDrawImageFile: payload too short ({} bytes)", pkt.data.len());
                    return;
                }
                let area = NbglArea::from(pkt.data);
                let color_map = pkt.data[10];
                let file_hdr = &pkt.data[11..];
                let _img_w = file_hdr[0] as u16 | ((file_hdr[1] as u16) << 8);
                let _img_h = file_hdr[2] as u16 | ((file_hdr[3] as u16) << 8);
                let bpp_comp = file_hdr[4];
                let bpp_fmt = (bpp_comp >> 4) & 0x0F; // 0=1bpp, 1=2bpp, 2=4bpp
                let compression = bpp_comp & 0x0F;
                let data_len =
                    file_hdr[5] as usize | ((file_hdr[6] as usize) << 8) | ((file_hdr[7] as usize) << 16);
                let bpp = match bpp_fmt {
                    0 => 1u8,
                    1 => 2,
                    2 => 4,
                    _ => {
                        log::warn!("NbglDrawImageFile: unsupported bpp_fmt={bpp_fmt}");
                        return;
                    }
                };
                let pixel_data = &file_hdr[8..8 + data_len.min(file_hdr.len() - 8)];
                log::trace!(
                    "NbglDrawImageFile: {}x{} bpp={} compression={} color_map={} data_len={}",
                    area.width,
                    area.height,
                    bpp,
                    compression,
                    color_map,
                    data_len
                );
                match compression {
                    0 => {
                        // NoCompression: raw column-first bitmap data
                        draw_image(area, bpp, 0, pixel_data, color_map)
                    }
                    1 => {
                        // Gzlib: chunked gzip compression.
                        // Format: [chunk_len_lo, chunk_len_hi, <gzip_data>] repeated.
                        // Each chunk decompresses to up to 2048 bytes of pixel data.
                        let mut decompressed = Vec::new();
                        let mut offset = 0;
                        let mut ok = true;
                        while offset + 2 <= pixel_data.len() {
                            let chunk_len =
                                pixel_data[offset] as usize | ((pixel_data[offset + 1] as usize) << 8);
                            offset += 2;
                            if offset + chunk_len > pixel_data.len() {
                                log::warn!(
                                    "NbglDrawImageFile: gzlib chunk overflows (offset={offset}, chunk_len={chunk_len}, total={})",
                                    pixel_data.len()
                                );
                                ok = false;
                                break;
                            }
                            let chunk = &pixel_data[offset..offset + chunk_len];
                            offset += chunk_len;
                            // Gzip data: skip the gzip header to get the raw deflate stream.
                            // gzip header is at least 10 bytes: [1f 8b 08 ...], deflate data
                            // starts after the header, and there's an 8-byte trailer (crc32 + size).
                            if chunk.len() < 18 || chunk[0] != 0x1f || chunk[1] != 0x8b {
                                log::warn!("NbglDrawImageFile: invalid gzip header in chunk");
                                ok = false;
                                break;
                            }
                            // Find end of gzip header (skip FEXTRA, FNAME, FCOMMENT, FHCRC)
                            let flg = chunk[3];
                            let mut hdr_end = 10;
                            if flg & 0x04 != 0 {
                                // FEXTRA
                                if hdr_end + 2 > chunk.len() {
                                    ok = false;
                                    break;
                                }
                                let xlen = chunk[hdr_end] as usize | ((chunk[hdr_end + 1] as usize) << 8);
                                hdr_end += 2 + xlen;
                            }
                            if flg & 0x08 != 0 {
                                // FNAME
                                while hdr_end < chunk.len() && chunk[hdr_end] != 0 {
                                    hdr_end += 1;
                                }
                                hdr_end += 1;
                            }
                            if flg & 0x10 != 0 {
                                // FCOMMENT
                                while hdr_end < chunk.len() && chunk[hdr_end] != 0 {
                                    hdr_end += 1;
                                }
                                hdr_end += 1;
                            }
                            if flg & 0x02 != 0 {
                                // FHCRC
                                hdr_end += 2;
                            }
                            // Deflate data is between header and 8-byte trailer
                            let deflate_end = chunk.len().saturating_sub(8);
                            if hdr_end >= deflate_end {
                                ok = false;
                                break;
                            }
                            let deflate_data = &chunk[hdr_end..deflate_end];
                            match miniz_oxide::inflate::decompress_to_vec(deflate_data) {
                                Ok(chunk_decompressed) => {
                                    decompressed.extend_from_slice(&chunk_decompressed);
                                }
                                Err(e) => {
                                    log::warn!("NbglDrawImageFile: deflate decompression failed: {:?}", e);
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        if ok && !decompressed.is_empty() {
                            log::trace!(
                                "NbglDrawImageFile: gzlib decompressed {} -> {} bytes",
                                pixel_data.len(),
                                decompressed.len()
                            );
                            draw_image(area, bpp, 0, &decompressed, color_map);
                        } else if !ok {
                            log::warn!(
                                "NbglDrawImageFile: gzlib decompression failed (data_len={})",
                                pixel_data.len()
                            );
                        }
                    }
                    2 => {
                        // RLE compressed
                        draw_image_rle(area, bpp, color_map, pixel_data, 0);
                    }
                    _ => {
                        log::warn!("NbglDrawImageFile: unsupported compression={compression}");
                    }
                }
                if let Ok(mut fifo) = SEPH_FIFO.write() {
                    fifo.push_back(vec![SephTag::DisplayProcessedEvent.into(), 0, 0]);
                }
            }
            SephTag::NbglDrawImageRle => {
                if pkt.data.len() < 12 {
                    log::warn!("NbglDrawImageRle: payload too short ({} bytes)", pkt.data.len());
                    return;
                }
                let area = NbglArea::from(pkt.data);
                let nb_skipped_bytes = pkt.data[10];
                let fore_color = pkt.data[11];
                let bpp = nbgl_bpp_to_read_bpp(area.bpp);
                let rle_data = &pkt.data[12..];
                log::trace!(
                    "NbglDrawImageRle: {}x{} at ({},{}), bpp={}, fore={}, skip={}, rle_len={}",
                    area.width,
                    area.height,
                    area.x0,
                    area.y0,
                    bpp,
                    fore_color,
                    nb_skipped_bytes,
                    rle_data.len()
                );
                draw_image_rle(area, bpp, fore_color, rle_data, nb_skipped_bytes);
                if let Ok(mut fifo) = SEPH_FIFO.write() {
                    fifo.push_back(vec![SephTag::DisplayProcessedEvent.into(), 0, 0]);
                }
            }
            SephTag::NbglDrawText => {
                // Packet: [area:10B BE][fontId:1B][fontColor:1B][textLen:2B BE][text:N bytes]
                if pkt.data.len() < 14 {
                    log::warn!("NbglDrawText: payload too short ({} bytes)", pkt.data.len());
                    return;
                }
                let area = NbglArea::from(pkt.data);
                let font_id = pkt.data[10];
                let font_color = pkt.data[11];
                let text_len = u16::from_be_bytes([pkt.data[12], pkt.data[13]]) as usize;
                let text_bytes = &pkt.data[14..14 + text_len.min(pkt.data.len() - 14)];
                let text = String::from_utf8_lossy(text_bytes);
                log::trace!(
                    "NbglDrawText: '{}' font={} color={} at ({},{}) {}x{}",
                    text,
                    font_id,
                    font_color,
                    area.x0,
                    area.y0,
                    area.width,
                    area.height
                );
                display::draw_text_native(area, font_id, font_color, &text);
                if let Ok(mut fifo) = SEPH_FIFO.write() {
                    fifo.push_back(vec![SephTag::DisplayProcessedEvent.into(), 0, 0]);
                }
            }
            _ => {
                log::warn!("Unmanaged SEPH pkt: {pkt:02x?}");
            }
        }
    }
}

impl BlockingArchiveHandler<RecvSeph> for FluxServer {
    fn handle(
        &mut self,
        msg: RecvSeph,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <RecvSeph as BlockingArchive>::Response {
        let mut fifo = SEPH_FIFO.write().unwrap();
        if let Some(pkt) = fifo.pop_front() {
            if pkt.len() <= msg.0 {
                log::trace!(
                    "RecvSeph: delivering {} bytes: {:02x?} (fifo_remaining={})",
                    pkt.len(),
                    pkt,
                    fifo.len()
                );
                return Some(pkt);
            }
            log::warn!("RecvSeph: packet too large ({} > maxlen={}), dropping", pkt.len(), msg.0);
        } else if self.last_ticker.elapsed() >= TICKER_INTERVAL {
            // FIFO empty and ticker interval elapsed — synthesize a TickerEvent.
            // This runs during polling so the app doesn't have to wait for
            // the next GeneralStatus to get a tick.
            self.last_ticker = Instant::now();
            let pkt = vec![SephTag::TickerEvent.into(), 0, 0];
            if pkt.len() <= msg.0 {
                return Some(pkt);
            }
        }
        None
    }
}
