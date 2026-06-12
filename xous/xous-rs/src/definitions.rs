use core::convert::TryInto;
use core::num::{NonZeroU8, NonZeroUsize};
#[cfg(not(keyos))]
use core::sync::atomic::AtomicU64;

pub type MemoryAddress = NonZeroUsize;
pub type MemorySize = NonZeroUsize;

pub type PID = NonZeroU8;

/// Page size used by both the kernel and userspace memory management.
pub const PAGE_SIZE: usize = 4096;

// Secretly, you can change this by setting the XOUS_SEED environment variable.
// I don't lke environment variables because where do you document features like this?
// But, this was the most expedient way to get all the threads in Hosted mode to pick up a seed.
// The code that reads the varable this is all the way over in xous-rs\src\arch\hosted\mod.rs#29, and
// it's glommed onto some other static process initialization code because I don't fully understand
// what's going on over there.
#[cfg(not(keyos))]
pub static TESTING_RNG_SEED: AtomicU64 = AtomicU64::new(0);

pub mod memoryflags;
pub use memoryflags::*;

pub mod messages;
pub use messages::*;

use crate::arch::ProcessStartup;

/// Server ID
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct SID([u32; 4]);
impl SID {
    #[inline]
    pub fn from_bytes(b: &[u8]) -> Option<SID> {
        if b.len() > 16 {
            None
        } else {
            let mut sid = [0; 4];
            let mut byte_iter = b.chunks_exact(4);
            if let Some(val) = byte_iter.next() {
                sid[0] = u32::from_le_bytes(val.try_into().ok()?);
            }
            if let Some(val) = byte_iter.next() {
                sid[1] = u32::from_le_bytes(val.try_into().ok()?);
            }
            if let Some(val) = byte_iter.next() {
                sid[2] = u32::from_le_bytes(val.try_into().ok()?);
            }
            if let Some(val) = byte_iter.next() {
                sid[3] = u32::from_le_bytes(val.try_into().ok()?);
            }
            Some(SID(sid))
        }
    }

    #[inline]
    pub const fn from_u32(a0: u32, a1: u32, a2: u32, a3: u32) -> SID { SID([a0, a1, a2, a3]) }

    #[inline]
    pub const fn from_array(a: [u32; 4]) -> SID { SID(a) }

    #[inline]
    pub const fn to_u32(&self) -> (u32, u32, u32, u32) { (self.0[0], self.0[1], self.0[2], self.0[3]) }

    #[inline]
    pub const fn to_array(&self) -> [u32; 4] { self.0 }

    #[inline]
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut result = [0; 16];
        for (src, dst) in self.0.iter().zip(result.chunks_exact_mut(4)) {
            dst.copy_from_slice(&src.to_le_bytes());
        }
        result
    }

    #[inline]
    pub const fn quick_hash(&self) -> u32 {
        // This is only used to filter threads that need to be woken up, not meant to be a perfect hash.
        // If we wake up a thread unnecessarily, it will just go back to sleep.
        self.0[0] ^ self.0[1] ^ self.0[2] ^ self.0[3]
    }
}

impl core::str::FromStr for SID {
    type Err = ();

    fn from_str(s: &str) -> core::result::Result<SID, ()> { Self::from_bytes(s.as_bytes()).ok_or(()) }
}

impl From<[u32; 4]> for SID {
    fn from(src: [u32; 4]) -> Self { Self::from_u32(src[0], src[1], src[2], src[3]) }
}

impl From<&[u32; 4]> for SID {
    fn from(src: &[u32; 4]) -> Self { Self::from_array(*src) }
}

impl From<SID> for [u32; 4] {
    fn from(s: SID) -> [u32; 4] { s.0 }
}

/// Connection ID
pub type CID = u32;

/// Thread ID
pub type TID = usize;

/// Equivalent to a RISC-V Hart ID
pub type CpuID = usize;

#[derive(Debug, PartialEq, Copy, Clone)]
pub struct MemoryRange {
    pub(crate) addr: MemoryAddress,
    pub(crate) size: MemorySize,
}

pub fn pid_from_usize(src: usize) -> core::result::Result<PID, Error> {
    if src > u8::MAX as _ {
        return Err(Error::InvalidPID);
    }
    PID::new(src as u8).ok_or(Error::InvalidPID)
}

#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NoError = 0,
    BadAlignment = 1,
    BadAddress = 2,
    OutOfMemory = 3,
    MemoryInUse = 4,
    InterruptNotFound = 5,
    InterruptInUse = 6,
    InvalidString = 7,
    ServerExists = 8,
    ServerNotFound = 9,
    ProcessNotFound = 10,
    ProcessNotChild = 11,
    ProcessTerminated = 12,
    Timeout = 13,
    InternalError = 14,
    ServerQueueFull = 15,
    ThreadNotAvailable = 16,
    UnhandledSyscall = 17,
    InvalidSyscall = 18,
    ShareViolation = 19,
    InvalidThread = 20,
    InvalidPID = 21,
    UnknownError = 22,
    AccessDenied = 23,
    Again = 24,
    DoubleFree = 25,
    DebugInProgress = 26,
    InvalidLimit = 27,
    ParseError = 28,

    /// The provided physical memory address is not suitable for encryption while the encryption is
    /// requested.
    InvalidPhysicalAddress = 29,

    InvalidArguments = 30,

    /// A fixed-size kernel table (e.g. the per-process connection map, the PID
    /// table, the server table) had no free slot. Distinct from `OutOfMemory`,
    /// which is reserved for actual physical-memory exhaustion.
    KernelTableFull = 31,
}

impl Error {
    #[inline]
    pub fn from_usize(arg: usize) -> Self {
        use crate::Error::*;
        match arg {
            0 => NoError,
            1 => BadAlignment,
            2 => BadAddress,
            3 => OutOfMemory,
            4 => MemoryInUse,
            5 => InterruptNotFound,
            6 => InterruptInUse,
            7 => InvalidString,
            8 => ServerExists,
            9 => ServerNotFound,
            10 => ProcessNotFound,
            11 => ProcessNotChild,
            12 => ProcessTerminated,
            13 => Timeout,
            14 => InternalError,
            15 => ServerQueueFull,
            16 => ThreadNotAvailable,
            17 => UnhandledSyscall,
            18 => InvalidSyscall,
            19 => ShareViolation,
            20 => InvalidThread,
            21 => InvalidPID,
            23 => AccessDenied,
            24 => Again,
            25 => DoubleFree,
            26 => DebugInProgress,
            27 => InvalidLimit,
            28 => ParseError,
            29 => InvalidPhysicalAddress,
            30 => InvalidArguments,
            31 => KernelTableFull,
            _ => UnknownError,
        }
    }

    #[inline]
    pub fn to_usize(&self) -> usize {
        if *self == Self::UnknownError {
            usize::MAX
        } else {
            *self as usize
        }
    }
}

impl MemoryRange {
    /// # Safety
    ///
    /// This allows for creating a `MemoryRange` from any arbitrary pointer,
    /// so it is imperative that this only be used to point to valid ranges.
    #[inline]
    pub unsafe fn new(addr: usize, size: usize) -> core::result::Result<MemoryRange, Error> {
        Ok(MemoryRange {
            addr: MemoryAddress::new(addr).ok_or(Error::BadAddress)?,
            size: MemorySize::new(size).ok_or(Error::BadAddress)?,
        })
    }

    #[inline]
    pub fn from_parts(addr: MemoryAddress, size: MemorySize) -> MemoryRange { MemoryRange { addr, size } }

    #[inline]
    pub fn len(&self) -> usize { self.size.get() }

    #[inline]
    pub fn is_empty(&self) -> bool { self.size.get() > 0 }

    #[inline]
    pub fn as_ptr(&self) -> *const u8 { self.addr.get() as *const u8 }

    #[inline]
    pub fn as_mut_ptr(&self) -> *mut u8 { self.addr.get() as *mut u8 }

    /// Return this memory as a slice of values. The resulting slice
    /// will cover the maximum number of elements given the size of `T`.
    /// For example, if the allocation is 4096 bytes, then the resulting
    /// `&[u8]` would have 4096 elements, `&[u16]` would have 2048, and
    /// `&[u32]` would have 1024. Values are rounded down.
    #[inline]
    pub fn as_slice<T>(&self) -> &[T] {
        // This is safe because the pointer and length are guaranteed to
        // be valid, as long as the user hasn't already called `as_ptr()`
        // and done something unsound with the resulting pointer.
        unsafe {
            core::slice::from_raw_parts(self.as_ptr() as *const T, self.len() / core::mem::size_of::<T>())
        }
    }

    /// Return this memory as a slice of mutable values. The resulting slice
    /// will cover the maximum number of elements given the size of `T`.
    /// For example, if the allocation is 4096 bytes, then the resulting
    /// `&[u8]` would have 4096 elements, `&[u16]` would have 2048, and
    /// `&[u32]` would have 1024. Values are rounded down.
    #[inline]
    pub fn as_slice_mut<T>(&mut self) -> &mut [T] {
        // This is safe because the pointer and length are guaranteed to
        // be valid, as long as the user hasn't already called `as_ptr()`
        // and done something unsound with the resulting pointer.
        unsafe {
            core::slice::from_raw_parts_mut(
                self.as_mut_ptr() as *mut T,
                self.len() / core::mem::size_of::<T>(),
            )
        }
    }

    #[inline]
    pub fn subrange(&self, offset: usize, len: usize) -> Option<Self> {
        if offset.checked_add(len)? > self.size.get() {
            return None;
        }

        Some(Self { addr: self.addr.checked_add(offset)?, size: MemorySize::new(len)? })
    }
}

#[repr(C)]
#[derive(Debug, PartialEq)]
pub enum Result {
    // 0
    Ok,
    // 1
    Error(Error),

    // 2
    MemoryAddress(MemoryAddress),

    // 3
    MemoryRange(MemoryRange),

    // 4
    ReadyThreads(
        usize, /* count */
        usize,
        /* pid0 */ usize, /* context0 */
        usize,
        /* pid1 */ usize, /* context1 */
        usize,
        /* pid2 */ usize, /* context2 */
    ),

    // 5
    ResumeProcess,

    // 6
    ServerID(SID),

    // 7
    ConnectionID(CID),

    // 8
    NewServerID(SID),

    // 9
    MessageEnvelope(MessageEnvelope),

    // 10
    ThreadID(TID),

    // 11
    ProcessID(PID),

    /// 12: The requested system call is unimplemented
    Unimplemented,

    /// 13: Unlucky result, do not use.
    _Reserved,

    /// 14: A scalar with one value
    Scalar1(usize),

    /// 15: A scalar with two values
    Scalar2(usize, usize),

    /// 16: The syscall should be attempted again. Only useed in hosted mode.
    /// On actual hardware, we put PC back to the SWI instruction automatically,
    /// and never return this.
    RetryCall,

    /// The message was successful but no value was returned.
    None,

    /// Memory was returned, and more information is available. The range is where the
    /// buffer now lives in the sender: the same range it lent for a borrow, or the
    /// (possibly resized) range moved back for a `BlockingMove`.
    MemoryReturned(MemoryRange, Option<MemorySize> /* offset */, Option<MemorySize> /* valid */),

    /// Returned when a process has started. This describes the new process to
    /// the caller.
    NewProcess(ProcessStartup),

    /// 20: A scalar with five values
    Scalar5(usize, usize, usize, usize, usize),

    // 21: A message is returned as part of `send_message()` when the result is blocking
    Message(Message),

    UnknownResult(usize, usize, usize, usize, usize, usize, usize),
}

impl Result {
    fn add_opcode(opcode: usize, args: [usize; 7]) -> [usize; 8] {
        [opcode, args[0], args[1], args[2], args[3], args[4], args[5], args[6]]
    }

    #[inline]
    pub fn to_args(&self) -> [usize; 8] {
        match self {
            Result::Ok => [0, 0, 0, 0, 0, 0, 0, 0],
            Result::Error(e) => [1, e.to_usize(), 0, 0, 0, 0, 0, 0],
            Result::MemoryAddress(s) => [2, s.get(), 0, 0, 0, 0, 0, 0],
            Result::MemoryRange(r) => [3, r.addr.get(), r.size.get(), 0, 0, 0, 0, 0],
            Result::ReadyThreads(count, pid0, ctx0, pid1, ctx1, pid2, ctx2) => {
                [4, *count, *pid0, *ctx0, *pid1, *ctx1, *pid2, *ctx2]
            }
            Result::ResumeProcess => [5, 0, 0, 0, 0, 0, 0, 0],
            Result::ServerID(sid) => {
                let s = sid.to_u32();
                [6, s.0 as _, s.1 as _, s.2 as _, s.3 as _, 0, 0, 0]
            }
            Result::ConnectionID(cid) => [7, *cid as usize, 0, 0, 0, 0, 0, 0],
            Result::MessageEnvelope(me) => {
                let me_enc = me.to_usize();
                [9, me_enc[0], me_enc[1], me_enc[2], me_enc[3], me_enc[4], me_enc[5], me_enc[6]]
            }
            Result::ThreadID(ctx) => [10, *ctx, 0, 0, 0, 0, 0, 0],
            Result::ProcessID(pid) => [11, pid.get() as _, 0, 0, 0, 0, 0, 0],
            Result::Unimplemented => [21, 0, 0, 0, 0, 0, 0, 0],
            Result::_Reserved => [13, 0, 0, 0, 0, 0, 0, 0],
            Result::Scalar1(a) => [14, *a, 0, 0, 0, 0, 0, 0],
            Result::Scalar2(a, b) => [15, *a, *b, 0, 0, 0, 0, 0],
            Result::NewServerID(sid) => {
                let s = sid.to_u32();
                [8, s.0 as _, s.1 as _, s.2 as _, s.3 as _, 0, 0, 0]
            }
            Result::RetryCall => [16, 0, 0, 0, 0, 0, 0, 0],
            Result::None => [17, 0, 0, 0, 0, 0, 0, 0],
            Result::MemoryReturned(range, offset, valid) => [
                18,
                offset.map(|o| o.get()).unwrap_or_default(),
                valid.map(|v| v.get()).unwrap_or_default(),
                range.addr.get(),
                range.size.get(),
                0,
                0,
                0,
            ],
            Result::NewProcess(p) => Self::add_opcode(19, p.into()),
            Result::Scalar5(a, b, c, d, e) => [20, *a, *b, *c, *d, *e, 0, 0],
            Result::Message(message) => {
                let encoded = message.to_usize();
                [21, encoded[0], encoded[1], encoded[2], encoded[3], encoded[4], encoded[5], 0]
            }
            Result::UnknownResult(arg1, arg2, arg3, arg4, arg5, arg6, arg7) => {
                [usize::MAX, *arg1, *arg2, *arg3, *arg4, *arg5, *arg6, *arg7]
            }
        }
    }

    #[inline]
    pub fn from_args(src: [usize; 8]) -> Self {
        match src[0] {
            0 => Result::Ok,
            1 => Result::Error(Error::from_usize(src[1])),
            2 => match MemoryAddress::new(src[1]) {
                None => Result::Error(Error::InternalError),
                Some(s) => Result::MemoryAddress(s),
            },
            3 => {
                let addr = match MemoryAddress::new(src[1]) {
                    None => return Result::Error(Error::InternalError),
                    Some(s) => s,
                };
                let size = match MemorySize::new(src[2]) {
                    None => return Result::Error(Error::InternalError),
                    Some(s) => s,
                };

                Result::MemoryRange(MemoryRange { addr, size })
            }
            4 => Result::ReadyThreads(src[1], src[2], src[3], src[4], src[5], src[6], src[7]),
            5 => Result::ResumeProcess,
            6 => Result::ServerID(SID::from_u32(src[1] as _, src[2] as _, src[3] as _, src[4] as _)),
            7 => Result::ConnectionID(src[1] as CID),
            8 => Result::NewServerID(SID::from_u32(src[1] as _, src[2] as _, src[3] as _, src[4] as _)),
            9 => {
                let sender = src[1];
                let message = match src[2] {
                    0 => match MemoryMessage::from_usize(src[3], src[4], src[5], src[6], src[7]) {
                        None => return Result::Error(Error::InternalError),
                        Some(s) => Message::MutableBorrow(s),
                    },
                    1 => match MemoryMessage::from_usize(src[3], src[4], src[5], src[6], src[7]) {
                        None => return Result::Error(Error::InternalError),
                        Some(s) => Message::Borrow(s),
                    },
                    2 => match MemoryMessage::from_usize(src[3], src[4], src[5], src[6], src[7]) {
                        None => return Result::Error(Error::InternalError),
                        Some(s) => Message::Move(s),
                    },
                    3 => Message::Scalar(ScalarMessage::from_usize(src[3], src[4], src[5], src[6], src[7])),
                    4 => Message::BlockingScalar(ScalarMessage::from_usize(
                        src[3], src[4], src[5], src[6], src[7],
                    )),
                    5 => match MemoryMessage::from_usize(src[3], src[4], src[5], src[6], src[7]) {
                        None => return Result::Error(Error::InternalError),
                        Some(s) => Message::BlockingMove(s),
                    },
                    _ => return Result::Error(Error::InternalError),
                };
                Result::MessageEnvelope(MessageEnvelope {
                    sender: MessageSender::from_usize(sender),
                    body: message,
                })
            }
            10 => Result::ThreadID(src[1] as TID),
            11 => Result::ProcessID(PID::new(src[1] as _).unwrap()),
            12 => Result::Unimplemented,
            13 => Result::_Reserved,
            14 => Result::Scalar1(src[1]),
            15 => Result::Scalar2(src[1], src[2]),
            16 => Result::RetryCall,
            17 => Result::None,
            18 => {
                let addr = match MemoryAddress::new(src[3]) {
                    None => return Result::Error(Error::InternalError),
                    Some(s) => s,
                };
                let size = match MemorySize::new(src[4]) {
                    None => return Result::Error(Error::InternalError),
                    Some(s) => s,
                };
                Result::MemoryReturned(
                    MemoryRange { addr, size },
                    MemorySize::new(src[1]),
                    MemorySize::new(src[2]),
                )
            }
            19 => Result::NewProcess(src.into()),
            20 => Result::Scalar5(src[1], src[2], src[3], src[4], src[5]),
            21 => Result::Message(match src[1] {
                0 => match MemoryMessage::from_usize(src[2], src[3], src[4], src[5], src[6]) {
                    None => return Result::Error(Error::InternalError),
                    Some(s) => Message::MutableBorrow(s),
                },
                1 => match MemoryMessage::from_usize(src[2], src[3], src[4], src[5], src[6]) {
                    None => return Result::Error(Error::InternalError),
                    Some(s) => Message::Borrow(s),
                },
                2 => match MemoryMessage::from_usize(src[2], src[3], src[4], src[5], src[6]) {
                    None => return Result::Error(Error::InternalError),
                    Some(s) => Message::Move(s),
                },
                3 => Message::Scalar(ScalarMessage::from_usize(src[2], src[3], src[4], src[5], src[6])),
                4 => {
                    Message::BlockingScalar(ScalarMessage::from_usize(src[2], src[3], src[4], src[5], src[6]))
                }
                5 => match MemoryMessage::from_usize(src[2], src[3], src[4], src[5], src[6]) {
                    None => return Result::Error(Error::InternalError),
                    Some(s) => Message::BlockingMove(s),
                },
                _ => return Result::Error(Error::InternalError),
            }),
            _ => Result::UnknownResult(src[0], src[1], src[2], src[3], src[4], src[5], src[6]),
        }
    }

    /// If the Result has memory attached to it, return the memory
    #[inline]
    pub fn memory(&self) -> Option<&MemoryRange> {
        match self {
            Result::MessageEnvelope(msg) => msg.body.memory(),
            Result::Message(msg) => msg.memory(),
            _ => None,
        }
    }
}

impl From<Error> for Result {
    fn from(e: Error) -> Self { Result::Error(e) }
}

pub type SysCallRequest = core::result::Result<crate::syscall::SysCall, Error>;
pub type SysCallResult = core::result::Result<Result, Error>;

#[macro_export]
macro_rules! msg_scalar_unpack {
    // the args are `tt` so that you can specify _ as the arg
    ($msg:ident, $arg1:tt, $arg2:tt, $arg3:tt, $arg4:tt, $body:block) => {{
        if let xous::Message::Scalar(xous::ScalarMessage {
            id: _,
            arg1: $arg1,
            arg2: $arg2,
            arg3: $arg3,
            arg4: $arg4,
        }) = $msg.body
        {
            $body
        } else {
            log::error!("message expansion failed in msg_scalar_unpack macro")
        }
    }};
}

#[macro_export]
macro_rules! msg_blocking_scalar_unpack {
    // the args are `tt` so that you can specify _ as the arg
    ($msg:ident, $arg1:tt, $arg2:tt, $arg3:tt, $arg4:tt, $body:block) => {{
        if let xous::Message::BlockingScalar(xous::ScalarMessage {
            id: _,
            arg1: $arg1,
            arg2: $arg2,
            arg3: $arg3,
            arg4: $arg4,
        }) = $msg.body
        {
            $body
        } else {
            log::error!("message expansion failed in msg_scalar_unpack macro")
        }
    }};
}

pub const APP_ID_SIZE: usize = 16;

/// A unique identifier for an app running in the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppId(pub [u8; APP_ID_SIZE]);

impl AppId {
    pub fn truncate(bytes: &[u8]) -> Self {
        let mut app_id = [0; APP_ID_SIZE];
        app_id.iter_mut().zip(bytes.iter()).for_each(|(a, b)| *a = *b);
        AppId(app_id)
    }
}

impl From<&AppId> for [u32; 4] {
    fn from(app_id: &AppId) -> [u32; 4] {
        [
            u32::from_le_bytes(app_id.0[0..4].try_into().unwrap()),
            u32::from_le_bytes(app_id.0[4..8].try_into().unwrap()),
            u32::from_le_bytes(app_id.0[8..12].try_into().unwrap()),
            u32::from_le_bytes(app_id.0[12..16].try_into().unwrap()),
        ]
    }
}

impl From<[u32; 4]> for AppId {
    fn from(app_id: [u32; 4]) -> AppId {
        let mut bytes = [0; APP_ID_SIZE];
        bytes[0..4].copy_from_slice(&app_id[0].to_le_bytes());
        bytes[4..8].copy_from_slice(&app_id[1].to_le_bytes());
        bytes[8..12].copy_from_slice(&app_id[2].to_le_bytes());
        bytes[12..16].copy_from_slice(&app_id[3].to_le_bytes());
        AppId(bytes)
    }
}

impl From<[u8; APP_ID_SIZE]> for AppId {
    fn from(app_id: [u8; APP_ID_SIZE]) -> AppId { AppId(app_id) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheOperation {
    /// Flush cache entries into main memory, i.e. make sure writes are
    /// visible by peripherals.
    Clean = 0,
    /// Remove cache entries, i.e. make sure writes by peripherals are
    /// visible by the CPU. If the cache was dirty (there was data not yet
    /// written to RAM), the writes will be lost
    Invalidate = 1,
    /// Both Clean and Invalidate, i.e. write to main memory, but also make
    /// sure they are re-read from there the next time they are read.
    /// Can be used as a micro-optimization to free up cache lines that will
    /// surely not be used.
    CleanAndInvalidate = 2,
}

impl From<usize> for CacheOperation {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::Clean,
            1 => Self::Invalidate,
            _ => Self::CleanAndInvalidate,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DramIdleMode {
    /// DRAM is needed even when the CPU is idle. Use when DMA is in progress
    KeepClocked = 0,
    /// DRAM can be set to low power mode (i.e. inaccessible) when the CPU is idle.
    LowPower = 1,
}

impl From<usize> for DramIdleMode {
    fn from(value: usize) -> Self {
        match value {
            1 => Self::LowPower,
            _ => Self::KeepClocked,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemEvent {
    /// A process launched by the current one has exited
    /// Sender => PID of the child
    /// arg1 => The exit code
    ChildTerminated = 0,
    /// A connection was closed from the other side (the server was destroyed).
    /// When a process with active servers exits, this event is sent before the ChildTerminated event.
    ///
    /// Sender => The PID of the process owning the server
    /// arg1 => CID
    Disconnected = 1,
    /// Sent each time the number of free pages goes below LOW_MEMORY_THRESHOLD.
    LowFreeMemory = 2,
}

pub const NUM_SYSTEM_EVENTS: usize = 3;

impl From<usize> for SystemEvent {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::ChildTerminated,
            1 => Self::Disconnected,
            2 => Self::LowFreeMemory,
            _ => Self::ChildTerminated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerEvent {
    /// A new process has connected.
    /// Sender => PID of the process making the connection. Not necessarily the same as the one that will use
    /// the connection.
    NewConnection = 0,
    /// A process has disconnected.
    /// Sender => PID of the process
    Disconnected = 1,
}

pub const NUM_SERVER_EVENTS: usize = 2;

impl From<usize> for ServerEvent {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::NewConnection,
            1 => Self::Disconnected,
            _ => Self::NewConnection,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemStat {
    /// Number of bytes of free memory
    FreeMemory = 0,
    /// 1 if the system is low on memory, 0 if it has enough
    IsSystemLowOnMemory = 1,
    // TODO: CPU usage (SFT-4868)
}

impl From<usize> for SystemStat {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::FreeMemory,
            1 => Self::IsSystemLowOnMemory,
            _ => Self::FreeMemory,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ThreadPriority {
    Idle = 0,
    AppBackground0 = 1,
    AppBackground1 = 2,
    AppDefault = 3,
    AppHigh0 = 4,
    AppHigh1 = 5,
    System0 = 6,
    System1 = 7,
    System2 = 8,
    System3 = 9,
    System4 = 10,
    System5 = 11,
    System6 = 12,
    System7 = 13,
    System8 = 14,
    Highest = 15,
}

impl From<usize> for ThreadPriority {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::Idle,
            1 => Self::AppBackground0,
            2 => Self::AppBackground1,
            3 => Self::AppDefault,
            4 => Self::AppHigh0,
            5 => Self::AppHigh1,
            6 => Self::System0,
            7 => Self::System1,
            8 => Self::System2,
            9 => Self::System3,
            10 => Self::System4,
            11 => Self::System5,
            12 => Self::System6,
            13 => Self::System7,
            14 => Self::System8,
            15 => Self::Highest,
            _ => Self::AppDefault,
        }
    }
}
