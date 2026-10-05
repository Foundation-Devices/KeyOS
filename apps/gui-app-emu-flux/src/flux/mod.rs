// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::{BTreeMap, VecDeque},
    io::Read,
    sync::{LazyLock, RwLock},
    time::{Duration, Instant},
};

use flate2::read::GzDecoder;
use server::{
    BlockingArchive, BlockingArchiveHandler, BlockingScalar, BlockingScalarHandler, CheckedConn, LendMut,
    LendMutHandler, MessageId as _, ScalarHandler, Server, WithAllPermissions,
};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[cfg(keyos)]
legacy_hid::use_api!();

use gui_app_emu_flux::{
    keys,
    messages::{RecvSeph, RegisterChild, SendSeph, SyscallBuffer},
    syscall_id::*,
};

pub mod display;

use display::{draw_image, draw_image_rle, draw_rect};

/// Upper bound on a BIP32 derivation path length. Real wallet paths are short
/// (at most ~10 levels); this bounds the buffer read and allocation against a
/// hostile `arg`, which on the 32-bit target could otherwise overflow `len * 4`.
const MAX_BIP32_PATH_LEN: usize = 16;

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
    log::debug!("App seed updated ({} bytes)", seed.len());
    // Dropping the previous `StoredSeed` scrubs the old bytes it held.
    *APP_SEED.write().unwrap() = StoredSeed(seed);
}

/// Clear the app seed. Key derivation then fails until a seed is set again, so a
/// deleted seed cannot keep signing in the same session.
pub fn clear_app_seed() {
    // Dropping the previous `StoredSeed` scrubs its bytes; no manual zeroize needed.
    *APP_SEED.write().unwrap() = StoredSeed::default();
    log::debug!("App seed cleared");
}

/// A `Zeroizing` copy of the current app seed, if one is set. Every derivation clones the seed
/// out of `APP_SEED`; wrapping the clone scrubs each per-call copy on drop, so a
/// signing session can't leave recoverable 64-byte seed copies on the heap.
fn get_app_seed() -> Option<Zeroizing<Vec<u8>>> {
    let seed = Zeroizing::new(APP_SEED.read().unwrap().0.clone());
    if seed.is_empty() {
        log::error!("App seed requested before configuration");
        return None;
    }
    Some(seed)
}

/// A SEPH packet: the tag, the payload's big-endian length, then the payload.
fn seph_packet(tag: SephTag, payload: &[u8]) -> Vec<u8> {
    let mut packet = vec![tag.into()];
    packet.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    packet.extend_from_slice(payload);
    packet
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

/// Kernel-delivered when a client disconnects or exits; the scalar payload is unused because the
/// message's sender is the departed pid.
#[derive(Debug, server::Message)]
struct ClientDisconnected(xous::CID);

/// Launch a Flux child, or find the one already running, answering its PID.
#[derive(Debug, server::Message)]
#[response(Result<xous::PID, app_manager::AppManagerError>)]
pub struct LaunchChild(pub xous::AppId);

/// Queue a touch on the display for the children. A release dragged off the display keeps its
/// negative coordinates, which the children take as cancelling the press.
#[derive(Debug, server::Message)]
pub struct Touch {
    pub x: i16,
    pub y: i16,
    pub pressed: bool,
}

impl server::AsScalar<3> for Touch {
    fn as_scalar(&self) -> [u32; 3] { [self.x as u32, self.y as u32, self.pressed.into()] }
}

impl server::FromScalar<3> for Touch {
    fn from_scalar([x, y, pressed]: [u32; 3]) -> Self {
        Self { x: x as i16, y: y as i16, pressed: pressed != 0 }
    }
}

/// The emulator's connection to its own server, which the kernel never permission-checks.
#[derive(Default, Clone)]
pub struct FluxServerPermissions;

impl server::CheckedPermissions for FluxServerPermissions {
    const NAME: &str = gui_app_emu_flux::SERVER_NAME;
}

pub type FluxConn = CheckedConn<WithAllPermissions<FluxServerPermissions>>;

#[derive(server::Server)]
#[name = "os/gui-app-emu-flux"]
pub struct FluxServer {
    last_ticker: Instant,
    app_manager: crate::AppManagerApi,
    /// SEPH events for the children, oldest first, each host command with the HID channel its
    /// response goes out on.
    events: VecDeque<(Vec<u8>, Option<u16>)>,
    /// The channel of each command delivered and not yet answered, oldest first.
    response_channels: VecDeque<u16>,
    /// The channel of the last host command, for a response with none pending.
    #[cfg(keyos)]
    last_channel_id: u16,
    /// The BIP32 roots each child registered, by PID. Every child derives from one shared Legacy
    /// wallet, so without this bound an app answers requests for the accounts of every other coin
    /// on that wallet (SFT-8141); the roots are each upstream app's own `PATH_APP_LOAD_PARAMS`,
    /// which its OS enforces the same way. A child gets nothing from the emulator before it
    /// registers and can register only once, so its roots are fixed before it handles any input.
    /// An empty entry belongs to a process that dropped its connection while alive: it counts as
    /// unregistered and can't register again.
    derivation_roots: BTreeMap<xous::PID, Vec<Vec<u32>>>,
    #[cfg(keyos)]
    legacy_hid: LegacyHidApi,
}

impl Default for FluxServer {
    fn default() -> Self {
        log::debug!("Initializing Flux");

        Self {
            last_ticker: Instant::now(),
            app_manager: crate::AppManagerApi::default(),
            events: VecDeque::new(),
            response_channels: VecDeque::new(),
            #[cfg(keyos)]
            last_channel_id: 0x0101,
            derivation_roots: BTreeMap::new(),
            #[cfg(keyos)]
            legacy_hid: LegacyHidApi::default(),
        }
    }
}

impl FluxServer {
    /// Whether `pid` registered and still holds its roots.
    fn is_registered(&self, pid: xous::PID) -> bool {
        self.derivation_roots.get(&pid).is_some_and(|roots| !roots.is_empty())
    }

    /// Whether `path` extends one of the roots `sender` registered.
    fn derivation_allowed(&self, sender: xous::PID, path: &[u32]) -> bool {
        let allowed = self
            .derivation_roots
            .get(&sender)
            .is_some_and(|roots| roots.iter().any(|root| path.starts_with(root)));
        if !allowed {
            log::warn!("Derivation denied for PID {sender:?}: {path:08X?} extends no registered root");
        }
        allowed
    }

    /// Queue an event for the children.
    fn push_event(&mut self, tag: SephTag, payload: &[u8]) {
        self.events.push_back((seph_packet(tag, payload), None));
    }

    /// Send `data` to the host on the channel of the command it answers.
    #[cfg(keyos)]
    fn respond(&mut self, data: Vec<u8>) {
        let channel_id = self.response_channels.pop_front().unwrap_or(self.last_channel_id);
        if let Err(e) = self.legacy_hid.write_apdu(channel_id, data) {
            log::error!("Response on channel 0x{channel_id:04x} failed: {e:?}");
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
        xous::register_server_event_handler(
            xous::ServerEvent::Disconnected,
            context.sid(),
            ClientDisconnected::ID,
        )
        .expect("Failed to register client disconnected handler");
        #[cfg(keyos)]
        CheckedConn::<legacy_hid_permissions::LegacyHidPermissions>::default()
            .subscribe_archive_infallible(legacy_hid::messages::SubscribeIncomingApdu, context);
    }
}

impl ScalarHandler<ClientDisconnected> for FluxServer {
    fn handle(
        &mut self,
        _msg: ClientDisconnected,
        pid: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        // A process still alive here only dropped its connection. Its roots are emptied rather
        // than removed, or it could register wider ones on a new connection.
        // XXX: A PID a new child took before this is handled looks alive too, and that child is
        // refused registration. Only children launched outside the emulator hit this, as
        // LaunchChild removes the roots at launch. A disconnect the kernel drops on a full queue
        // leaves the dead child's queued input for the next one, and its roots for an outside
        // child to use without registering.
        if matches!(xous::get_app_id(pid), Ok(Some(_))) {
            if let Some(roots) = self.derivation_roots.get_mut(&pid) {
                roots.clear();
            }
        } else if self.derivation_roots.remove(&pid).is_some() {
            // The dead child won't drain its input, and the next one mustn't pick it up.
            self.events.clear();
            self.response_channels.clear();
        }
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

impl BlockingScalarHandler<LaunchChild> for FluxServer {
    fn handle(
        &mut self,
        LaunchChild(app_id): LaunchChild,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <LaunchChild as BlockingScalar>::Response {
        // A running child keeps its roots, so any child the launch below returns is new, even one
        // launched elsewhere.
        if let Ok(Some(pid)) = xous::app_id_to_pid(&app_id) {
            return Ok(pid);
        }
        let pid = self.app_manager.launch_app_blocking(&app_id)?;
        // In case the ClientDisconnected handler kept the roots of a dead child on this PID.
        self.derivation_roots.remove(&pid);
        Ok(pid)
    }
}

impl BlockingArchiveHandler<RegisterChild> for FluxServer {
    fn handle(
        &mut self,
        msg: RegisterChild,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <RegisterChild as BlockingArchive>::Response {
        let roots = msg.derivation_roots;
        // An empty root is a prefix of every path, so it would allow everything, and no roots at
        // all would leave the child unregistered.
        if roots.is_empty() || roots.iter().any(|root| !(1..=MAX_BIP32_PATH_LEN).contains(&root.len())) {
            log::warn!("Registration from PID {sender:?} refused: malformed roots {roots:08X?}");
            return false;
        }
        if self.derivation_roots.contains_key(&sender) {
            log::warn!("Registration from PID {sender:?} refused: already registered");
            return false;
        }
        log::debug!("PID {sender:?} registered derivation roots {roots:08X?}");
        self.derivation_roots.insert(sender, roots);
        true
    }
}

impl ScalarHandler<Touch> for FluxServer {
    fn handle(
        &mut self,
        Touch { x, y, pressed }: Touch,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let [x_hi, x_lo] = x.to_be_bytes();
        let [y_hi, y_lo] = y.to_be_bytes();
        self.push_event(SephTag::FingerEvent, &[pressed.into(), x_hi, x_lo, y_hi, y_lo]);
    }
}

#[cfg(keyos)]
impl server::ArchiveEventHandler<legacy_hid::messages::IncomingApdu> for FluxServer {
    fn handle(
        &mut self,
        msg: server::Owned<legacy_hid::messages::IncomingApdu>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let Ok(legacy_hid::messages::IncomingApdu { channel_id, data }) = msg.deserialize() else {
            return;
        };
        self.last_channel_id = channel_id;

        // Delivered but unanswered commands count too, or a child that drains without replying
        // could grow the queue without bound.
        let in_flight = self.events.iter().filter(|(_, channel)| channel.is_some()).count()
            + self.response_channels.len();
        let child_running = self.derivation_roots.values().any(|roots| !roots.is_empty());
        // Answered "not supported" rather than queued: with no child running it would be stale
        // input for the next one.
        if !child_running || in_flight >= MAX_QUEUED_APDUS {
            log::debug!("Rejecting APDU {:02x?}: no child running or queue full", &data[..data.len().min(2)]);
            if let Err(e) = self.legacy_hid.write_apdu(channel_id, vec![0x6d, 0x00]) {
                log::error!("Rejection on channel 0x{channel_id:04x} failed: {e:?}");
            }
            return;
        }
        self.events.push_back((seph_packet(SephTag::CapduEvent, &data), Some(channel_id)));
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
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SyscallBuffer as LendMut>::Response {
        if !self.is_registered(sender) {
            log::warn!("SyscallBuffer from unregistered PID {sender:?} refused");
            return usize::MAX;
        }

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

                if !self.derivation_allowed(sender, &path) {
                    return usize::MAX;
                }

                let Some(app_seed) = get_app_seed() else {
                    return usize::MAX;
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

                if !self.derivation_allowed(sender, &path) {
                    return usize::MAX;
                }

                let Some(app_seed) = get_app_seed() else {
                    return usize::MAX;
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
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if !self.is_registered(sender) {
            log::debug!("SendSeph from unregistered PID {sender:?} dropped");
            return;
        }

        let Ok(msg) = msg.deserialize() else { return };
        let Some(pkt) = SephPacket::parse(msg.0.as_slice()) else {
            return;
        };
        log::trace!("SendSeph: tag={:?} ({} bytes payload)", pkt.tag, pkt.data.len());
        match pkt.tag {
            SephTag::GeneralStatus => {
                // [0, 0] is the SDK's "ready for next event", which RecvSeph answers with a tick.
                if pkt.data != [0x00, 0x00] {
                    log::error!("Unexpected General Status ({} bytes)", pkt.data.len());
                    log::trace!("General Status: {pkt:02x?}");
                }
            }
            SephTag::ScreenDisplayStatus | SephTag::DbgScreenDisplayStatus | SephTag::BaglDrawRect => {
                log::trace!("Screen Display Status: {:02x?}", pkt.data);
                if pkt.tag != SephTag::BaglDrawRect {
                    self.push_event(SephTag::DisplayProcessedEvent, &[]);
                }
            }
            SephTag::ScreenDisplayRawStatus | SephTag::BaglDrawBitmap => {
                log::trace!("Screen Display Raw Status: {:02x?}", pkt.data);
                if pkt.tag != SephTag::BaglDrawBitmap {
                    self.push_event(SephTag::DisplayProcessedEvent, &[]);
                }
            }
            SephTag::PrintcStatus | SephTag::PrintfStatus => {
                log::info!("Printc Status: {}", String::from_utf8_lossy(pkt.data));
            }
            SephTag::Rapdu => {
                log::trace!("Rapdu ({} bytes): {:02x?}", pkt.data.len(), pkt.data);
                #[cfg(keyos)]
                self.respond(pkt.data.to_vec());
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
                            self.respond(payload.to_vec());
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
                self.push_event(SephTag::DisplayProcessedEvent, &[]);
            }
            SephTag::NbglRefresh => {
                log::trace!("NbglRefresh");
                display::refresh();
                self.push_event(SephTag::DisplayProcessedEvent, &[]);
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
                self.push_event(SephTag::DisplayProcessedEvent, &[]);
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
                self.push_event(SephTag::DisplayProcessedEvent, &[]);
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
                self.push_event(SephTag::DisplayProcessedEvent, &[]);
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
                self.push_event(SephTag::DisplayProcessedEvent, &[]);
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
                self.push_event(SephTag::DisplayProcessedEvent, &[]);
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
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <RecvSeph as BlockingArchive>::Response {
        if !self.is_registered(sender) {
            return None;
        }

        let (packet, channel) = match self.events.pop_front() {
            Some(event) => event,
            // At most every TICKER_INTERVAL, or a child polling an empty queue busy-loops.
            None if self.last_ticker.elapsed() >= TICKER_INTERVAL => {
                self.last_ticker = Instant::now();
                (seph_packet(SephTag::TickerEvent, &[]), None)
            }
            None => return None,
        };
        if packet.len() > msg.0 {
            log::warn!("RecvSeph: packet too large ({} > maxlen={}), dropping", packet.len(), msg.0);
            return None;
        }
        log::trace!("RecvSeph: delivering {} bytes: {packet:02x?}", packet.len());
        self.response_channels.extend(channel);
        Some(packet)
    }
}
