// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(keyos)]
use std::sync::atomic::{AtomicU32, Ordering};
use std::{
    collections::VecDeque,
    io::Read,
    sync::{LazyLock, RwLock},
    time::{Duration, Instant},
};

use flate2::read::GzDecoder;
use server::{
    BlockingArchive, BlockingArchiveHandler, LendMut, LendMutHandler, MessageId as _, ScalarHandler, Server,
};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[cfg(keyos)]
legacy_hid::use_api!();

use gui_app_emu_flux::{
    keys,
    messages::{RecvSeph, SendSeph, SyscallBuffer},
    syscall_id::*,
};

pub mod display;

use display::{draw_image, draw_image_rle, draw_rect};

/// Upper bound on a BIP32 derivation path length. Real wallet paths are short
/// (at most ~10 levels); this bounds the buffer read and allocation against a
/// hostile `arg`, which on the 32-bit target could otherwise overflow `len * 4`.
const MAX_BIP32_PATH_LEN: usize = 16;

static SEPH_FIFO: LazyLock<RwLock<VecDeque<Vec<u8>>>> = LazyLock::new(|| RwLock::new(VecDeque::new()));
/// Last channel ID seen from the host, used as a fallback when CHANNEL_FIFO is
/// empty. Written by the inbound-APDU subscriber.
#[cfg(keyos)]
static LAST_CHANNEL_ID: AtomicU32 = AtomicU32::new(0x0101);
/// HID channel for each CapduEvent still queued in SEPH_FIFO (not yet pulled by
/// the child), in enqueue order and kept in step with those CapduEvents. When the
/// child pulls a command its channel moves to CHANNEL_FIFO; when an oversized
/// command is dropped its channel is popped from here, so a drop only discards
/// that command's own channel instead of an earlier in-flight one.
#[cfg(keyos)]
static QUEUED_CHANNELS: LazyLock<RwLock<VecDeque<u16>>> = LazyLock::new(|| RwLock::new(VecDeque::new()));
/// HID channel for each command delivered to the child and still awaiting its
/// Rapdu, in delivery order, so a response is framed on the channel of the command
/// it answers rather than the most recent one seen.
#[cfg(keyos)]
static CHANNEL_FIFO: LazyLock<RwLock<VecDeque<u16>>> = LazyLock::new(|| RwLock::new(VecDeque::new()));
/// Cap on unanswered host commands queued for the child. The child answers one
/// at a time, so this sits well above any legitimate depth; it stops a flooding
/// host from growing the FIFO until the session runs out of memory.
#[cfg(keyos)]
const MAX_QUEUED_APDUS: usize = 16;

/// Fixed test seed for hosted emulator builds (SHA-256 of "test seed").
#[cfg(not(keyos))]
const HOSTED_TEST_APP_SEED: [u8; 32] = [
    0x9f, 0x86, 0xd0, 0x81, 0x88, 0x4c, 0x7d, 0x65, 0x9a, 0x2f, 0xea, 0xa0, 0xc5, 0x5a, 0xd0, 0x15, 0xa3,
    0xbf, 0x4f, 0x1b, 0x2b, 0x0b, 0x82, 0x2c, 0xd1, 0x5d, 0x6c, 0x15, 0xb0, 0xf0, 0x0a, 0x08,
];

/// The emulator's active key-derivation seed: a 32-byte AppSeed or a 64-byte BIP39 seed.
///
/// A newtype with `ZeroizeOnDrop` so replacing or clearing the stored seed scrubs the old
/// bytes, rather than leaving a freed heap buffer behind for a manual `zeroize()` to catch.
#[derive(Clone, Default, ZeroizeOnDrop)]
struct StoredSeed(Vec<u8>);

#[cfg(keyos)]
fn default_app_seed() -> StoredSeed { StoredSeed::default() }

#[cfg(not(keyos))]
fn default_app_seed() -> StoredSeed { StoredSeed(HOSTED_TEST_APP_SEED.to_vec()) }

/// App seed for key derivation.
///
/// Hardware builds intentionally have no default seed; `main` must install a
/// 32-byte AppSeed from the security API or a 64-byte BIP39 seed from manual
/// entry before child apps can derive keys. Hosted emulator builds keep a fixed
/// deterministic test seed for convenience.
static APP_SEED: LazyLock<RwLock<StoredSeed>> = LazyLock::new(|| RwLock::new(default_app_seed()));

/// Set the app seed for key derivation.
/// Accepts 32 bytes (derived AppSeed) or 64 bytes (BIP39 seed from a manual mnemonic entry).
pub fn set_app_seed(seed: Vec<u8>) {
    if let Ok(mut app_seed) = APP_SEED.write() {
        log::debug!("App seed updated ({} bytes)", seed.len());
        // Dropping the previous `StoredSeed` scrubs the old bytes it held.
        *app_seed = StoredSeed(seed);
    }
}

/// Clear the app seed. Key derivation then fails until a seed is set again, so a
/// deleted seed cannot keep signing in the same session.
pub fn clear_app_seed() {
    if let Ok(mut app_seed) = APP_SEED.write() {
        // Dropping the previous `StoredSeed` scrubs its bytes; no manual zeroize needed.
        *app_seed = StoredSeed::default();
        log::debug!("App seed cleared");
    }
}

/// A `Zeroizing` copy of the current app seed. Every derivation clones the seed
/// out of `APP_SEED`; wrapping the clone scrubs each per-call copy on drop, so a
/// signing session can't leave recoverable 64-byte seed copies on the heap.
fn get_app_seed() -> Result<Zeroizing<Vec<u8>>, keys::KeyError> {
    let seed =
        Zeroizing::new(APP_SEED.read().map(|s| s.0.clone()).map_err(|_| keys::KeyError::LockPoisoned)?);
    if seed.is_empty() {
        log::error!("App seed requested before configuration");
        return Err(keys::KeyError::SeedNotAvailable);
    }
    Ok(seed)
}

/// Drop any input queued for a Flux child that has gone away: the SEPH events it never
/// drained, plus their queued and pending-response channels. Without this the next child
/// to launch would pick up the dead child's stale commands.
#[cfg(keyos)]
pub fn clear_fifos() {
    if let Ok(mut fifo) = SEPH_FIFO.write() {
        fifo.clear();
    }
    if let Ok(mut channels) = QUEUED_CHANNELS.write() {
        channels.clear();
    }
    if let Ok(mut channels) = CHANNEL_FIFO.write() {
        channels.clear();
    }
}

/// Wrap a reassembled inbound HID APDU as a SEPH `CapduEvent` and push it into
/// the in-process FIFO that running Flux child apps drain via `RecvSeph`. Also
/// stashes the channel id for the next outgoing `Rapdu`.
///
/// `child_running` is the caller's authoritative "is one of ours on screen?" state (the
/// emulator's running-children map). Returns a ready-made "not supported" reply when no
/// child is running, which the caller writes straight back to the host. Otherwise the child
/// handles the APDU, including GET_APP_AND_VERSION, which it now answers itself.
#[cfg(keyos)]
pub fn push_incoming_apdu(channel_id: u16, apdu: &[u8], child_running: bool) -> Option<Vec<u8>> {
    LAST_CHANNEL_ID.store(channel_id as u32, Ordering::Relaxed);

    // No child is running: reject app commands with "not supported" rather than
    // queue them for the next child to pick up as stale input.
    if !child_running {
        log::debug!("No Flux child running, rejecting APDU {:02x?}", &apdu[..apdu.len().min(2)]);
        return Some(vec![0x6d, 0x00]);
    }

    // Bound the in-flight command depth so a host flooding faster than the child
    // drains can't grow the FIFOs until they exhaust memory. Count both queued
    // commands and those already delivered but not yet answered (CHANNEL_FIFO): a
    // child that drains without replying would otherwise leave the queued side
    // below the cap while CHANNEL_FIFO grows unbounded. A poisoned lock counts as full.
    let queued = QUEUED_CHANNELS.read().map(|c| c.len()).unwrap_or(usize::MAX);
    let pending = CHANNEL_FIFO.read().map(|c| c.len()).unwrap_or(usize::MAX);
    if queued.saturating_add(pending) >= MAX_QUEUED_APDUS {
        log::warn!("Flux command queue full; rejecting APDU {:02x?} as busy", &apdu[..apdu.len().min(2)]);
        return Some(vec![0x6d, 0x00]);
    }

    let apdu_len = apdu.len() as u16;
    let mut pkt = vec![SephTag::CapduEvent.into()];
    pkt.extend_from_slice(&apdu_len.to_be_bytes());
    pkt.extend_from_slice(apdu);
    match SEPH_FIFO.write() {
        Ok(mut fifo) => {
            fifo.push_back(pkt);
            log::debug!("CapduEvent pushed to SEPH_FIFO (depth={})", fifo.len());
            // Remember this command's channel so its response frames correctly
            // even if another command arrives before the child replies. It rides
            // in QUEUED_CHANNELS alongside the CapduEvent until the child pulls it.
            if let Ok(mut channels) = QUEUED_CHANNELS.write() {
                channels.push_back(channel_id);
            }
        }
        Err(e) => log::error!("Failed to write CapduEvent to SEPH_FIFO: {e:?}"),
    }
    None
}

/// The HID channel for the next outgoing response: the oldest delivered command
/// still awaiting its Rapdu, or the last channel seen if none is pending.
#[cfg(keyos)]
fn next_response_channel() -> u16 {
    CHANNEL_FIFO
        .write()
        .ok()
        .and_then(|mut channels| channels.pop_front())
        .unwrap_or_else(|| LAST_CHANNEL_ID.load(Ordering::Relaxed) as u16)
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
        let len = u16::from_be_bytes([raw[1], raw[2]]) as usize;
        let data = &raw[3..];
        // Senders size the packet from this same header, so a buffer shorter than it claims is
        // one they already clamped and reported. Take what is there.
        Some(Self { tag: SephTag::from(raw[0]), data: &data[..data.len().min(len)] })
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

impl NbglArea {
    /// Size of the area header that opens every NBGL draw packet.
    const HEADER_LEN: usize = 10;

    /// Parse the area header, or `None` if the payload is too short to hold one.
    ///
    /// Draw packets come from a Flux child whose behaviour the host steers over USB, so a
    /// short or inconsistent one must not take the emulator down with it.
    fn parse(raw: &[u8]) -> Option<Self> {
        let header: &[u8; Self::HEADER_LEN] = raw.get(..Self::HEADER_LEN)?.try_into().ok()?;
        Some(Self {
            x0: u16::from_be_bytes([header[0], header[1]]),
            y0: u16::from_be_bytes([header[2], header[3]]),
            width: u16::from_be_bytes([header[4], header[5]]),
            height: u16::from_be_bytes([header[6], header[7]]),
            color: header[8],
            bpp: header[9],
        })
    }
}

/// The largest image a Flux draw may cover: the whole emulator screen. Draw
/// dimensions come off the wire, and the RLE and compressed paths carry no
/// payload big enough to bound them, so an unchecked width * height could make a
/// renderer allocate until it OOMs the Legacy session.
const MAX_IMAGE_PIXELS: usize = display::DISPLAY_WIDTH * display::DISPLAY_HEIGHT;

/// Unpacked byte size of an image with these dimensions and bit depth, or None
/// if it overflows or covers more than the screen. Callers reject the draw on
/// None rather than let the renderer allocate an unbounded buffer.
fn checked_image_size(area: &NbglArea, bpp: u8) -> Option<usize> {
    let pixels = (area.width as usize).checked_mul(area.height as usize)?;
    if pixels > MAX_IMAGE_PIXELS {
        return None;
    }
    pixels.checked_mul(bpp as usize).map(|bits| bits.div_ceil(8))
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
                if data_len > buf.len() || buf.len() < 4 {
                    return usize::MAX; // Error: invalid length or no room for the CRC
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
                if path_len > MAX_BIP32_PATH_LEN
                    || path_len.checked_mul(4).map_or(true, |n| n > buf.len())
                    || buf.len() < 64
                {
                    log::warn!("BIP32: invalid buffer size or path length");
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
                    Ok((mut private_key, mut chain_code)) => {
                        // Write output: private_key (32) + chain_code (32)
                        buf[..32].copy_from_slice(&private_key);
                        buf[32..64].copy_from_slice(&chain_code);
                        // Scrub the per-address secret copies from the server's stack.
                        private_key.zeroize();
                        chain_code.zeroize();
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

                if path_len > MAX_BIP32_PATH_LEN
                    || path_len.checked_mul(4).map_or(true, |n| n > buf.len())
                    || buf.len() < 64
                {
                    log::warn!("derive_node_with_seed_key: invalid buffer size or path length");
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
                    Ok((mut private_key, mut chain_code)) => {
                        buf[..32].copy_from_slice(&private_key);
                        buf[32..64].copy_from_slice(&chain_code);
                        // Scrub the per-address secret copies from the server's stack.
                        private_key.zeroize();
                        chain_code.zeroize();
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
                    log::error!("Unexpected General Status ({} bytes)", pkt.data.len());
                    log::trace!("General Status: {pkt:02x?}");
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
                    let channel_id = next_response_channel();
                    log::debug!("Rapdu: forwarding via legacy-hid channel_id=0x{channel_id:04x}");
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
                // Packet: [ep:1][?:1][len:1][payload:len]
                let Some(&ep) = pkt.data.first() else {
                    log::warn!("UsbEpPrepare: empty payload");
                    return;
                };
                match ep {
                    0x82 => {
                        #[cfg(keyos)]
                        {
                            let payload = pkt.data.get(2).and_then(|&len| pkt.data.get(3..3 + len as usize));
                            let Some(payload) = payload else {
                                log::warn!(
                                    "UsbEpPrepare(HID): {}-byte payload is short or truncated",
                                    pkt.data.len()
                                );
                                return;
                            };
                            let channel_id = next_response_channel();
                            if let Err(e) = self.legacy_hid.write_apdu(channel_id, payload.to_vec()) {
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
                let Some(area) = NbglArea::parse(pkt.data) else {
                    log::warn!("NbglDrawRect: payload too short ({} bytes)", pkt.data.len());
                    return;
                };
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
                let Some(area) = NbglArea::parse(pkt.data) else {
                    log::warn!("NbglDrawLine: payload too short ({} bytes)", pkt.data.len());
                    return;
                };
                // parse() accepted a header, so there is always a trailing byte to read.
                let line_color = (pkt.data.last().copied().unwrap_or_default() as u32) * 0x555555;
                // Fill the area with lineColor (dotStartIdx ignored — solid line).
                draw_rect(area.x0, area.y0, area.width, area.height, line_color);
                if let Ok(mut fifo) = SEPH_FIFO.write() {
                    fifo.push_back(vec![SephTag::DisplayProcessedEvent.into(), 0, 0]);
                }
            }
            SephTag::NbglDrawImage => {
                log::trace!("NbglDrawImage: {:02x?}", pkt.data);
                let Some(area) = NbglArea::parse(pkt.data) else {
                    log::warn!("NbglDrawImage: payload too short ({} bytes)", pkt.data.len());
                    return;
                };
                let bpp = nbgl_bpp_to_read_bpp(area.bpp);
                // Cap the dimensions at the screen before sizing the payload, so a
                // large area with a matching packed payload can't make draw_image
                // allocate width * height beyond the framebuffer.
                if checked_image_size(&area, bpp).is_none() {
                    log::warn!("NbglDrawImage: {}x{} exceeds the screen bound", area.width, area.height);
                    return;
                }
                // usize is 32-bit on the device, and the dimensions come off the wire, so
                // width * height * bpp has to be checked rather than left to wrap.
                let Some(bit_size) = (area.width as usize)
                    .checked_mul(area.height as usize)
                    .and_then(|bits| bits.checked_mul(bpp as usize))
                else {
                    log::warn!("NbglDrawImage: {}x{} at bpp={bpp} overflows", area.width, area.height);
                    return;
                };
                let buffer_size = bit_size.div_ceil(8);
                // Header, then the pixel buffer, then the transformation and color_map trailer.
                let Some(packet_len) = buffer_size.checked_add(NbglArea::HEADER_LEN + 2) else {
                    log::warn!("NbglDrawImage: buffer of {buffer_size} bytes overflows the payload");
                    return;
                };
                if pkt.data.len() < packet_len {
                    log::warn!(
                        "NbglDrawImage: {}-byte payload holds no {buffer_size}-byte buffer",
                        pkt.data.len()
                    );
                    return;
                }
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
                let Some(area) = NbglArea::parse(pkt.data) else {
                    log::warn!("NbglDrawImageFile: payload too short ({} bytes)", pkt.data.len());
                    return;
                };
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
                // Dimensions come off the wire and the compressed paths don't have
                // the payload to bound them; reject before a renderer allocates
                // width * height.
                let Some(expected_size) = checked_image_size(&area, bpp) else {
                    log::warn!("NbglDrawImageFile: {}x{} exceeds the screen bound", area.width, area.height);
                    return;
                };
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
                            // Each chunk is a self-contained gzip member. Cap the reader one
                            // byte past the screen-sized image budget so a malformed chunk can't
                            // inflate unbounded; the accumulated-size check below rejects overflow.
                            let mut chunk_decompressed = Vec::new();
                            let mut decoder = GzDecoder::new(chunk).take(expected_size as u64 + 1);
                            if let Err(e) = decoder.read_to_end(&mut chunk_decompressed) {
                                log::warn!("NbglDrawImageFile: gzip decompression failed: {e:?}");
                                ok = false;
                                break;
                            }
                            decompressed.extend_from_slice(&chunk_decompressed);
                            if decompressed.len() > expected_size {
                                log::warn!(
                                    "NbglDrawImageFile: gzlib output exceeds {expected_size} bytes, rejecting"
                                );
                                ok = false;
                                break;
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
                let Some(area) = NbglArea::parse(pkt.data) else {
                    log::warn!("NbglDrawImageRle: payload too short ({} bytes)", pkt.data.len());
                    return;
                };
                let nb_skipped_bytes = pkt.data[10];
                let fore_color = pkt.data[11];
                let bpp = nbgl_bpp_to_read_bpp(area.bpp);
                // draw_image_rle allocates width * height pixels, and the RLE
                // payload doesn't bound them; reject an oversized area.
                if checked_image_size(&area, bpp).is_none() {
                    log::warn!("NbglDrawImageRle: {}x{} exceeds the screen bound", area.width, area.height);
                    return;
                }
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
                let Some(area) = NbglArea::parse(pkt.data) else {
                    log::warn!("NbglDrawText: payload too short ({} bytes)", pkt.data.len());
                    return;
                };
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
                log::warn!("Unmanaged SEPH pkt: tag={:?}", pkt.tag);
                log::trace!("Unmanaged SEPH pkt: {pkt:02x?}");
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
                // A delivered CapduEvent's channel moves from the queued list to the
                // pending-response list, so its Rapdu frames on this command's channel
                // and a later dropped command can't disturb it.
                #[cfg(keyos)]
                if pkt.first().copied() == Some(SephTag::CapduEvent.into()) {
                    if let (Ok(mut queued), Ok(mut pending)) = (QUEUED_CHANNELS.write(), CHANNEL_FIFO.write())
                    {
                        if let Some(channel) = queued.pop_front() {
                            pending.push_back(channel);
                        }
                    }
                }
                return Some(pkt);
            }
            log::warn!("RecvSeph: packet too large ({} > maxlen={}), dropping", pkt.len(), msg.0);
            // A dropped CapduEvent never reaches the child, so no Rapdu will consume
            // its channel; pop this command's own queued channel (at the front of
            // QUEUED_CHANNELS, in step with SEPH_FIFO) rather than the front of the
            // pending-response list, which may belong to an earlier in-flight command.
            #[cfg(keyos)]
            if pkt.first().copied() == Some(SephTag::CapduEvent.into()) {
                if let Ok(mut channels) = QUEUED_CHANNELS.write() {
                    channels.pop_front();
                }
            }
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
