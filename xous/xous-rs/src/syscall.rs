use core::convert::{TryFrom, TryInto};
#[cfg(keyos)]
use core::sync::atomic::AtomicUsize;

#[cfg(keyos)]
use crate::pid_from_usize;
use crate::{
    definitions::MessageId, AppId, CacheOperation, DramIdleMode, Error, MemoryAddress, MemoryFlags,
    MemoryMessage, MemoryRange, MemorySize, Message, MessageEnvelope, MessageSender, ProcessArgs,
    ProcessInit, Result, ScalarMessage, ServerEvent, SysCallResult, SystemEvent, SystemStat, ThreadInit,
    ThreadPriority, CID, PID, SID, TID,
};

#[derive(Debug, PartialEq)]
pub enum SysCall {
    /// Allocates pages of memory, equal to a total of `size` bytes.  A physical
    /// address may be specified, which can be used to allocate regions such as
    /// memory-mapped I/O.
    ///
    /// If a virtual address is specified, then the returned pages are located
    /// at that address.  Otherwise, they are located at the Default offset.
    ///
    /// # Returns
    ///
    /// * **MemoryRange**: A memory range containing zeroed bytes.
    ///
    /// # Errors
    ///
    /// * **BadAlignment**: Either the physical or virtual addresses aren't page-aligned, or the size isn't a
    ///   multiple of the page width.
    /// * **OutOfMemory**: A contiguous chunk of memory couldn't be found, or the system's memory size has
    ///   been exceeded.
    MapMemory(
        Option<MemoryAddress>, /* phys */
        Option<MemoryAddress>, /* virt */
        MemorySize,            /* region size */
        MemoryFlags,           /* flags */
    ),

    /// Release the memory back to the operating system.
    ///
    /// # Errors
    ///
    /// * **BadAlignment**: The memory range was not page-aligned
    /// * **BadAddress**: A page in the range was not mapped
    UnmapMemory(MemoryRange),

    /// Set the specified flags on the virtual address range. This can be used
    /// to REMOVE flags on a memory region, for example to mark it as no-execute
    /// after writing program data.
    ///
    /// If `PID` is `None`, then modifies this process. Note that it is not legal
    /// to modify the memory range of another process that has been started already.
    ///
    /// # Returns
    ///
    /// * **Ok**: The call completed successfully
    ///
    /// # Errors
    ///
    /// * **ProcessNotChild**: The given PID is not a child of the current process.
    /// * **MemoryInUse**: The given PID has already been started, and it is not legal to modify memory flags
    ///   anymore.
    UpdateMemoryFlags(
        MemoryRange, /* range of memory to update flags for */
        MemoryFlags, /* new flags */
        Option<PID>, /* if present, indicates the process to modify */
    ),

    /// Return execution to the kernel. This function may return at any time, including immediately.
    ///
    /// # Returns
    ///
    /// * **Ok**: The call completed successfully
    ///
    /// # Errors
    ///
    /// This syscall will never return an error.
    Yield,

    /// Set the priority of the current thread.
    SetThreadPriority(ThreadPriority),

    /// This thread will now wait for a message with the given server ID. You
    /// can set up a pool by having multiple threads call `ReceiveMessage` with
    /// the same SID.
    ///
    /// # Returns
    ///
    /// * **MessageEnvelope**: A valid message from the queue
    ///
    /// # Errors
    ///
    /// * **ServerNotFound**: The given SID is not active or has terminated
    /// * **ProcessNotFound**: The parent process terminated when we were getting ready to block. This is an
    ///   internal error.
    /// * **BlockedProcess**: When running in Hosted mode, this indicates that this thread is blocking.
    ReceiveMessage(SID),

    /// If a message is available for the specified server, return that message
    /// and resume execution. If no message is available, return `Result::None`
    /// immediately without blocking.
    ///
    /// # Returns
    ///
    /// * **Message**: A valid message from the queue
    /// * **None**: Indicates that no message was in the queue
    ///
    /// # Errors
    ///
    /// * **ServerNotFound**: The given SID is not active or has terminated
    /// * **ProcessNotFound**: The parent process terminated when we were getting ready to block. This is an
    ///   internal error.
    TryReceiveMessage(SID),

    /// Claims an interrupt and unmasks it immediately.  The provided function
    /// will be called from within an interrupt context, but using the ordinary
    /// privilege level of the process.
    ///
    /// # Returns
    ///
    /// * **Ok**: The interrupt has been mapped to this process
    ///
    /// # Errors
    ///
    /// * **InterruptNotFound**: The specified interrupt isn't valid on this system
    /// * **InterruptInUse**: The specified interrupt has already been claimed
    ClaimInterrupt(
        usize,                 /* IRQ number */
        MemoryAddress,         /* function pointer */
        Option<MemoryAddress>, /* argument */
    ),

    /// Returns the interrupt back to the operating system and masks it again.
    /// This function is implicitly called when a process exits.
    ///
    /// # Errors
    ///
    /// * **InterruptNotFound**: The specified interrupt doesn't exist, or isn't assigned to this process.
    FreeInterrupt(usize /* IRQ number */),

    /// If the value at the specified address is the same as the provided expected value,
    /// block the thread until woken up by FutexWake. Used to implement user-space mutexes.
    /// The check and the block is totally ordered with other futex operations on the same address.
    /// There's no need to initialize or destroy futexes, they are not stored in kernel space, only
    /// as part of the thread state while blocking.
    /// The expected value is used to prevent missed wakes, where the thread is preempted
    /// between checking the lock flag and actually calling this syscall.
    ///
    /// # Errors
    /// * **BadAddress**: The address was not in the address space of the process
    /// * **BadAlignment**: The address was not aligned to the size of `usize`
    /// * **Again**: The value was not the one provided.
    #[cfg(keyos)]
    FutexWait(usize /* Address */, usize /* expected value */),

    /// Wake up at most N threads blocking on a futex at the address.
    #[cfg(keyos)]
    FutexWake(usize /* Address */, usize /* number of threads to wake */),

    /// Create a new Server with a specified address
    ///
    /// This will return a 128-bit Server ID that can be used to send messages
    /// to this server, as well as a connection ID.  This connection ID will be
    /// unique per process, while the server ID is available globally.
    ///
    /// # Returns
    ///
    /// * **NewServerID(sid)**: The specified SID
    ///
    /// # Errors
    ///
    /// * **KernelTableFull**: The server table was full and a new server couldn't be created.
    /// * **ServerExists**: The server hash is already in use.
    CreateServerWithAddress(
        SID,                         /* server hash */
        core::ops::Range<MessageId>, /* Initial globally allowed messages */
    ),

    /// Connect to a server.   This turns a 128-bit Server ID into a 32-bit
    /// Connection ID. Blocks until the server is available.
    /// Returns the same CID if the same server is requested multiple times,
    /// in this case it adds to an internal refcount.
    ///
    /// # Returns
    ///
    /// * **ConnectionID(cid)**: The new connection ID for communicating with the server.
    ///
    /// # Errors
    ///
    /// None
    Connect(SID /* server id */),

    /// Try to connect to a server.   This turns a 128-bit Server ID into a 32-bit
    /// Connection ID.
    ///
    /// # Returns
    ///
    /// * **ConnectionID(cid)**: The new connection ID for communicating with the server.
    ///
    /// # Errors
    ///
    /// * **ServerNotFound**: The server could not be found.
    TryConnect(SID /* server id */),

    /// Send a message to a server (blocking until it's ready)
    ///
    /// # Returns
    ///
    /// * **Ok**: The Scalar / Send message was successfully sent
    /// * **Scalar1**: The Server returned a `Scalar1` value
    /// * **Scalar2**: The Server returned a `Scalar2` value
    /// * **Scalar5**: The Server returned a `Scalar5` value
    /// * **BlockedProcess**: In Hosted mode, the target process is now blocked
    /// * **Message**: For Scalar messages, this includes the args as returned by the server. For
    ///   MemoryMessages, this will include the Opcode, Offset, and Valid fields.
    ///
    /// # Errors
    ///
    /// * **ServerNotFound**: The server could not be found.
    /// * **ProcessNotFound**: Internal error -- the parent process couldn't be found when blocking
    SendMessage(CID, Message),

    /// Try to send a message to a server
    ///
    /// # Returns
    ///
    /// * **Ok**: The Scalar / Send message was successfully sent, or the Borrow has finished
    /// * **Scalar1**: The Server returned a `Scalar1` value
    /// * **Scalar2**: The Server returned a `Scalar2` value
    /// * **Scalar5**: The Server returned a `Scalar5` value
    /// * **BlockedProcess**: In Hosted mode, the target process is now blocked
    ///
    /// # Errors
    ///
    /// * **ServerNotFound**: The server could not be found.
    /// * **ServerQueueFull**: The server's mailbox is full
    /// * **ProcessNotFound**: Internal error -- the parent process couldn't be found when blocking
    TrySendMessage(CID, Message),

    /// Return a Borrowed memory region to the sender
    ReturnMemory(
        MessageSender,      /* source of this message */
        MemoryRange,        /* address of range */
        Option<MemorySize>, /* offset */
        Option<MemorySize>, /* valid */
    ),

    /// Return a scalar to the sender
    ReturnScalar1(MessageSender, usize),

    /// Return two scalars to the sender
    ReturnScalar2(MessageSender, usize, usize),

    /// Spawn a new thread
    CreateThread(ThreadInit),

    /// Create a new process, setting the current process as the parent ID.
    /// Starts the process immediately and returns a `ProcessStartup` value.
    CreateProcess(ProcessInit),

    /// Terminate the current process, closing all server connections.
    TerminateProcess(u32),

    /// Terminates a process with the given PID and exit code.
    TerminatePid(PID, u32),

    /// Shut down the entire system
    Shutdown(i32),

    /// Create a new Server
    ///
    /// This will return a 128-bit Server ID that can be used to send messages
    /// to this server. The returned Server ID is random.
    ///
    /// # Returns
    ///
    /// The SID, along with a Connection ID that can be used to immediately
    /// communicate with this process.
    ///
    /// # Errors
    ///
    /// * **KernelTableFull**: The server table was full and a new server couldn't be created.
    CreateServer,

    /// Returns a 128-bit server ID, but does not create the server itself.
    /// basically an API to access the TRNG inside the kernel.
    CreateServerId,

    /// Establish a connection in the given process to the given server. This
    /// call can be used by a nameserver to make server connections without
    /// disclosing SIDs.
    ConnectForProcess(PID, SID),

    // Get the Process ID of the other end of the connection
    GetRemoteProcessId(CID),

    /// Get the current Thread ID
    GetThreadId,

    /// Get the current Process ID
    GetProcessId,

    /// Destroys the given Server ID. All clients that are waiting will be woken
    /// up and will receive a `ServerNotFound` response.
    DestroyServer(SID),

    /// Release one ref from the CID. If called as many times as Donnect was called on the same SID,
    /// it frees the CID, which may be then reused.
    Disconnect(CID),

    /// Waits for a thread to finish, and returns the return value of that thread.
    JoinThread(TID),

    /// Returns the physical address corresponding to a virtual address, if such a mapping exists.
    ///
    /// ## Arguments
    ///     * **vaddr**: The virtual address to inspect
    ///
    /// ## Returns
    /// Returns a Scalar1 containing the physical address
    ///
    /// ## Errors
    ///     * **BadAddress**: The mapping does not exist
    #[cfg(keyos)]
    VirtToPhys(usize /* virtual address */),

    /// Return five scalars to the sender
    ReturnScalar5(MessageSender, usize, usize, usize, usize, usize),

    /// Returns the physical address corresponding to a virtual address for a given process, if such a
    /// mapping exists.
    ///
    /// ## Arguments
    ///     * **pid**: The PID
    ///     * **vaddr**: The virtual address to inspect
    ///
    /// ## Returns
    /// Returns a Scalar1 containing the physical address
    ///
    /// ## Errors
    ///     * **BadAddress**: The mapping does not exist
    #[cfg(keyos)]
    VirtToPhysPid(PID /* Process ID */, usize /* virtual address */),

    /// Hook for xous name server to fetch app ID from PID so that it can
    /// check if a process can bind to a name.
    GetAppId(PID),

    /// Allow incoming messages to the specified SID from any incoming connection.
    /// Can only be used on SIDs owned by the current process, unless generic syscall privilege is given.
    AllowMessagesSID(SID, core::ops::Range<MessageId>),

    /// Allow messages on the specified connection.
    /// Can only be used on connections to a server owned by the current process, unless generic syscall
    /// privilege is given.
    AllowMessagesCID(PID, CID, core::ops::Range<MessageId>),

    /// Requests the kernel to flush the virtual memory region from both L1 and L2 cache.
    ///
    /// ## Errors
    ///     * **BadAlignment**: The memory range isn't page-aligned.
    #[cfg(keyos)]
    FlushCache(MemoryRange, CacheOperation),

    /// Sets up kernel-level power management settings
    ///
    /// keep_dram_clocked: Even when idle, keep the DRAM clock on (needed when DMA is in progress)
    PowerManagement(DramIdleMode),

    /// Looks up for the PID of a given app ID if the app is currently running.
    AppIdToPid(AppId),

    /// Register a server to receive a notification message on various server-specific events
    /// Only one handler per event per servere can be set.
    ServerEventHandler(ServerEvent, SID, MessageId),

    /// Create a mirror of the memory of the current process in another process.
    ///
    /// ## Returns
    /// Returns a memory range of the mirror within the other process.
    ///
    /// ## Errors
    ///    * **ProcessNotFound**: The requested process does not exist.
    MirrorMemoryToPid(MemoryRange, PID),

    /// Run a debug command on the kernel and get the resulting string.
    /// To be used to implement low-level human readable debug outputs.
    /// The commands and string contents may change at any time.
    #[cfg(keyos)]
    DebugCommand(MemoryRange, u8),

    /// Get a system statistic (e.g. free memory available)
    GetSystemStats(SystemStat),

    /// Register a server to receive a notification message on various system events
    /// Only one handler per event per process can be set.
    SystemEventHandler(SystemEvent, SID, MessageId),

    /// Saves this PID's panic message to be shown when the process terminates
    AppendPanicMessage(usize, usize, usize, usize, usize, usize, usize),

    /// Gets the last panic message (including backtrace) from the kernel
    GetPanicMessage(MemoryRange),

    /// This syscall does not exist. It captures all possible
    /// arguments so detailed analysis can be performed.
    Invalid(usize, usize, usize, usize, usize, usize, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysCallNumber {
    MapMemory = 2,
    Yield = 3,
    SetThreadPriority = 4,
    ClaimInterrupt = 5,
    FreeInterrupt = 6,
    FutexWait = 7,
    FutexWake = 8,
    // 9 is unused
    // 10 is unused
    // 11 is unused
    UpdateMemoryFlags = 12,
    // 13 is unused
    CreateServerWithAddress = 14,
    ReceiveMessage = 15,
    SendMessage = 16,
    Connect = 17,
    CreateThread = 18,
    UnmapMemory = 19,
    ReturnMemory = 20,
    CreateProcess = 21,
    TerminateProcess = 22,
    Shutdown = 23,
    TrySendMessage = 24,
    TryConnect = 25,
    ReturnScalar1 = 26,
    ReturnScalar2 = 27,
    TryReceiveMessage = 28,
    CreateServer = 29,
    ConnectForProcess = 30,
    CreateServerId = 31,
    GetThreadId = 32,
    GetProcessId = 33,
    DestroyServer = 34,
    Disconnect = 35,
    JoinThread = 36,
    GetRemoteProcessId = 37,
    // 38 is unused
    VirtToPhys = 39,
    ReturnScalar5 = 40,
    // 41 is unused
    VirtToPhysPid = 42,
    GetAppId = 43,
    AllowMessagesSID = 44,
    AllowMessagesCID = 45,
    InvalidateCache = 46,
    PowerManagement = 47,
    AppIdToPid = 48,
    ServerEventHandler = 49,
    MirrorMemoryToPid = 50,
    DebugCommand = 51,
    GetSystemStats = 52,
    TerminatePid = 53,
    SystemEventHandler = 54,
    AppendPanicMessage = 55,
    GetPanicMessage = 56,

    Invalid,
}

impl SysCallNumber {
    #[inline(always)]
    pub fn from(val: usize) -> SysCallNumber {
        use SysCallNumber::*;
        match val {
            2 => MapMemory,
            3 => Yield,
            4 => SetThreadPriority,
            5 => ClaimInterrupt,
            6 => FreeInterrupt,
            7 => FutexWait,
            8 => FutexWake,
            // 9 is unused
            // 10 is unused
            // 11 is unused
            12 => UpdateMemoryFlags,
            14 => CreateServerWithAddress,
            15 => ReceiveMessage,
            16 => SendMessage,
            17 => Connect,
            18 => CreateThread,
            19 => UnmapMemory,
            20 => ReturnMemory,
            21 => CreateProcess,
            22 => TerminateProcess,
            23 => Shutdown,
            24 => TrySendMessage,
            25 => TryConnect,
            26 => ReturnScalar1,
            27 => ReturnScalar2,
            28 => TryReceiveMessage,
            29 => CreateServer,
            30 => ConnectForProcess,
            31 => CreateServerId,
            32 => GetThreadId,
            33 => GetProcessId,
            34 => DestroyServer,
            35 => Disconnect,
            36 => JoinThread,
            37 => GetRemoteProcessId,
            // 38 is unused
            39 => VirtToPhys,
            40 => ReturnScalar5,
            // 41 is unused
            42 => VirtToPhysPid,
            43 => GetAppId,
            44 => AllowMessagesSID,
            45 => AllowMessagesCID,
            46 => InvalidateCache,
            47 => PowerManagement,
            48 => AppIdToPid,
            49 => ServerEventHandler,
            50 => MirrorMemoryToPid,
            51 => DebugCommand,
            52 => GetSystemStats,
            53 => TerminatePid,
            54 => SystemEventHandler,
            55 => AppendPanicMessage,
            56 => GetPanicMessage,
            _ => Invalid,
        }
    }
}

impl SysCall {
    fn add_opcode(opcode: SysCallNumber, args: [usize; 7]) -> [usize; 8] {
        [opcode as usize, args[0], args[1], args[2], args[3], args[4], args[5], args[6]]
    }

    /// Convert the SysCall into an array of eight `usize` elements,
    /// suitable for passing to the kernel.
    #[inline(always)]
    pub fn as_args(&self) -> [usize; 8] {
        use core::mem;
        assert!(
            mem::size_of::<SysCall>() == mem::size_of::<usize>() * 8,
            "SysCall is not the expected size (expected {}, got {})",
            mem::size_of::<usize>() * 8,
            mem::size_of::<SysCall>()
        );

        match self {
            SysCall::MapMemory(a1, a2, a3, a4) => [
                SysCallNumber::MapMemory as usize,
                a1.map(|x| x.get()).unwrap_or_default(),
                a2.map(|x| x.get()).unwrap_or_default(),
                a3.get(),
                a4.bits(),
                0,
                0,
                0,
            ],
            SysCall::UnmapMemory(range) => {
                [SysCallNumber::UnmapMemory as usize, range.as_ptr() as usize, range.len(), 0, 0, 0, 0, 0]
            }
            SysCall::Yield => [SysCallNumber::Yield as usize, 0, 0, 0, 0, 0, 0, 0],
            SysCall::SetThreadPriority(priority) => {
                [SysCallNumber::SetThreadPriority as usize, *priority as usize, 0, 0, 0, 0, 0, 0]
            }

            SysCall::ReceiveMessage(sid) => {
                let s = sid.to_u32();
                [SysCallNumber::ReceiveMessage as usize, s.0 as _, s.1 as _, s.2 as _, s.3 as _, 0, 0, 0]
            }
            SysCall::TryReceiveMessage(sid) => {
                let s = sid.to_u32();
                [SysCallNumber::TryReceiveMessage as usize, s.0 as _, s.1 as _, s.2 as _, s.3 as _, 0, 0, 0]
            }
            SysCall::ConnectForProcess(pid, sid) => {
                let s = sid.to_u32();
                [
                    SysCallNumber::ConnectForProcess as usize,
                    pid.get() as _,
                    s.0 as _,
                    s.1 as _,
                    s.2 as _,
                    s.3 as _,
                    0,
                    0,
                ]
            }
            SysCall::CreateServerId => [SysCallNumber::CreateServerId as usize, 0, 0, 0, 0, 0, 0, 0],
            SysCall::ClaimInterrupt(a1, a2, a3) => [
                SysCallNumber::ClaimInterrupt as usize,
                *a1,
                a2.get(),
                a3.map(|x| x.get()).unwrap_or_default(),
                0,
                0,
                0,
                0,
            ],
            SysCall::FreeInterrupt(a1) => [SysCallNumber::FreeInterrupt as usize, *a1, 0, 0, 0, 0, 0, 0],
            #[cfg(keyos)]
            SysCall::FutexWait(addr, val) => [SysCallNumber::FutexWait as usize, *addr, *val, 0, 0, 0, 0, 0],
            #[cfg(keyos)]
            SysCall::FutexWake(addr, n) => [SysCallNumber::FutexWake as usize, *addr, *n, 0, 0, 0, 0, 0],
            SysCall::UpdateMemoryFlags(a1, a2, a3) => [
                SysCallNumber::UpdateMemoryFlags as usize,
                a1.as_mut_ptr() as usize,
                a1.len(),
                a2.bits(),
                a3.map(|m| m.get() as usize).unwrap_or(0),
                0,
                0,
                0,
            ],
            SysCall::CreateServerWithAddress(sid, messages) => {
                let s = sid.to_u32();
                [
                    SysCallNumber::CreateServerWithAddress as usize,
                    s.0 as _,
                    s.1 as _,
                    s.2 as _,
                    s.3 as _,
                    messages.start,
                    messages.end,
                    0,
                ]
            }
            SysCall::CreateServer => [SysCallNumber::CreateServer as usize, 0, 0, 0, 0, 0, 0, 0],
            SysCall::Connect(sid) => {
                let s = sid.to_u32();
                [SysCallNumber::Connect as usize, s.0 as _, s.1 as _, s.2 as _, s.3 as _, 0, 0, 0]
            }
            SysCall::SendMessage(a1, ref a2) => match a2 {
                Message::MutableBorrow(mm)
                | Message::Borrow(mm)
                | Message::Move(mm)
                | Message::BlockingMove(mm) => [
                    SysCallNumber::SendMessage as usize,
                    *a1 as usize,
                    a2.message_type(),
                    mm.id,
                    mm.buf.as_ptr() as usize,
                    mm.buf.len(),
                    mm.offset.map(|x| x.get()).unwrap_or(0),
                    mm.valid.map(|x| x.get()).unwrap_or(0),
                ],
                Message::Scalar(sc) | Message::BlockingScalar(sc) => [
                    SysCallNumber::SendMessage as usize,
                    *a1 as usize,
                    a2.message_type(),
                    sc.id,
                    sc.arg1,
                    sc.arg2,
                    sc.arg3,
                    sc.arg4,
                ],
            },
            SysCall::ReturnMemory(sender, buf, offset, valid) => [
                SysCallNumber::ReturnMemory as usize,
                sender.to_usize(),
                buf.as_ptr() as usize,
                buf.len(),
                offset.map(|o| o.get()).unwrap_or_default(),
                valid.map(|v| v.get()).unwrap_or_default(),
                0,
                0,
            ],
            SysCall::CreateThread(init) => {
                crate::arch::thread_to_args(SysCallNumber::CreateThread as usize, init)
            }
            SysCall::CreateProcess(init) => Self::add_opcode(SysCallNumber::CreateProcess, init.into()),
            SysCall::TerminateProcess(exit_code) => {
                [SysCallNumber::TerminateProcess as usize, *exit_code as usize, 0, 0, 0, 0, 0, 0]
            }
            SysCall::Shutdown(exit_code) => {
                [SysCallNumber::Shutdown as usize, *exit_code as usize, 0, 0, 0, 0, 0, 0]
            }
            SysCall::TryConnect(sid) => {
                let s = sid.to_u32();
                [SysCallNumber::TryConnect as usize, s.0 as _, s.1 as _, s.2 as _, s.3 as _, 0, 0, 0]
            }
            SysCall::TrySendMessage(a1, ref a2) => match a2 {
                Message::MutableBorrow(mm)
                | Message::Borrow(mm)
                | Message::Move(mm)
                | Message::BlockingMove(mm) => [
                    SysCallNumber::TrySendMessage as usize,
                    *a1 as usize,
                    a2.message_type(),
                    mm.id,
                    mm.buf.as_ptr() as usize,
                    mm.buf.len(),
                    mm.offset.map(|x| x.get()).unwrap_or(0),
                    mm.valid.map(|x| x.get()).unwrap_or(0),
                ],
                Message::Scalar(sc) | Message::BlockingScalar(sc) => [
                    SysCallNumber::TrySendMessage as usize,
                    *a1 as usize,
                    a2.message_type(),
                    sc.id,
                    sc.arg1,
                    sc.arg2,
                    sc.arg3,
                    sc.arg4,
                ],
            },
            SysCall::ReturnScalar1(sender, arg1) => {
                [SysCallNumber::ReturnScalar1 as usize, sender.to_usize(), *arg1, 0, 0, 0, 0, 0]
            }
            SysCall::ReturnScalar2(sender, arg1, arg2) => {
                [SysCallNumber::ReturnScalar2 as usize, sender.to_usize(), *arg1, *arg2, 0, 0, 0, 0]
            }
            SysCall::GetThreadId => [SysCallNumber::GetThreadId as usize, 0, 0, 0, 0, 0, 0, 0],
            SysCall::GetProcessId => [SysCallNumber::GetProcessId as usize, 0, 0, 0, 0, 0, 0, 0],
            SysCall::DestroyServer(sid) => {
                let s = sid.to_u32();
                [SysCallNumber::DestroyServer as usize, s.0 as _, s.1 as _, s.2 as _, s.3 as _, 0, 0, 0]
            }
            SysCall::Disconnect(cid) => [SysCallNumber::Disconnect as usize, *cid as usize, 0, 0, 0, 0, 0, 0],
            SysCall::JoinThread(tid) => [SysCallNumber::JoinThread as usize, *tid, 0, 0, 0, 0, 0, 0],
            SysCall::GetRemoteProcessId(cid) => {
                [SysCallNumber::GetRemoteProcessId as usize, *cid as usize, 0, 0, 0, 0, 0, 0]
            }

            #[cfg(keyos)]
            SysCall::VirtToPhys(vaddr) => [SysCallNumber::VirtToPhys as usize, *vaddr, 0, 0, 0, 0, 0, 0],
            SysCall::ReturnScalar5(sender, arg1, arg2, arg3, arg4, arg5) => [
                SysCallNumber::ReturnScalar5 as usize,
                sender.to_usize(),
                *arg1,
                *arg2,
                *arg3,
                *arg4,
                *arg5,
                0,
            ],
            #[cfg(keyos)]
            SysCall::VirtToPhysPid(pid, vaddr) => {
                [SysCallNumber::VirtToPhysPid as usize, pid.get() as usize, *vaddr, 0, 0, 0, 0, 0]
            }
            SysCall::GetAppId(pid) => {
                [SysCallNumber::GetAppId as usize, pid.get() as usize, 0, 0, 0, 0, 0, 0]
            }
            SysCall::ServerEventHandler(event, sid, id) => {
                let s = sid.to_u32();
                [
                    SysCallNumber::ServerEventHandler as usize,
                    *event as usize,
                    s.0 as _,
                    s.1 as _,
                    s.2 as _,
                    s.3 as _,
                    *id as _,
                    0,
                ]
            }
            SysCall::AllowMessagesSID(sid, messages) => {
                let s = sid.to_u32();
                [
                    SysCallNumber::AllowMessagesSID as usize,
                    s.0 as usize,
                    s.1 as usize,
                    s.2 as usize,
                    s.3 as usize,
                    messages.start,
                    messages.end,
                    0,
                ]
            }
            SysCall::AllowMessagesCID(pid, cid, messages) => [
                SysCallNumber::AllowMessagesCID as usize,
                pid.get() as usize,
                *cid as usize,
                messages.start,
                messages.end,
                0,
                0,
                0,
            ],
            #[cfg(keyos)]
            SysCall::FlushCache(mem, op) => [
                SysCallNumber::InvalidateCache as usize,
                mem.as_ptr() as usize,
                mem.len(),
                *op as usize,
                0,
                0,
                0,
                0,
            ],
            SysCall::PowerManagement(dram) => {
                [SysCallNumber::PowerManagement as usize, *dram as usize, 0, 0, 0, 0, 0, 0]
            }
            SysCall::AppIdToPid(app_id) => {
                let app_id_words: [u32; 4] = app_id.into();

                [
                    SysCallNumber::AppIdToPid as usize,
                    app_id_words[0] as usize,
                    app_id_words[1] as usize,
                    app_id_words[2] as usize,
                    app_id_words[3] as usize,
                    0,
                    0,
                    0,
                ]
            }
            SysCall::MirrorMemoryToPid(mem, pid) => [
                SysCallNumber::MirrorMemoryToPid as usize,
                mem.as_ptr() as usize,
                mem.len(),
                pid.get() as usize,
                0,
                0,
                0,
                0,
            ],

            #[cfg(keyos)]
            SysCall::DebugCommand(buffer, cmd) => [
                SysCallNumber::DebugCommand as usize,
                buffer.as_ptr() as usize,
                buffer.len(),
                *cmd as usize,
                0,
                0,
                0,
                0,
            ],

            SysCall::GetSystemStats(stat) => {
                [SysCallNumber::GetSystemStats as usize, *stat as usize, 0, 0, 0, 0, 0, 0]
            }

            SysCall::SystemEventHandler(event, sid, id) => {
                let s = sid.to_u32();
                [
                    SysCallNumber::SystemEventHandler as usize,
                    *event as usize,
                    s.0 as _,
                    s.1 as _,
                    s.2 as _,
                    s.3 as _,
                    *id as _,
                    0,
                ]
            }

            SysCall::AppendPanicMessage(len, a2, a3, a4, a5, a6, a7) => {
                [SysCallNumber::AppendPanicMessage as usize, *len, *a2, *a3, *a4, *a5, *a6, *a7]
            }

            SysCall::GetPanicMessage(buf) => {
                [SysCallNumber::GetPanicMessage as usize, buf.as_ptr() as usize, buf.len(), 0, 0, 0, 0, 0]
            }

            SysCall::TerminatePid(pid, exit_code) => {
                [SysCallNumber::TerminatePid as usize, pid.get() as usize, *exit_code as usize, 0, 0, 0, 0, 0]
            }

            SysCall::Invalid(a1, a2, a3, a4, a5, a6, a7) => {
                [SysCallNumber::Invalid as usize, *a1, *a2, *a3, *a4, *a5, *a6, *a7]
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub fn from_args(
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
        a6: usize,
        a7: usize,
    ) -> core::result::Result<Self, Error> {
        Ok(match SysCallNumber::from(a0) {
            SysCallNumber::MapMemory => SysCall::MapMemory(
                MemoryAddress::new(a1),
                MemoryAddress::new(a2),
                MemoryAddress::new(a3).ok_or(Error::InvalidSyscall)?,
                crate::MemoryFlags::from_bits(a4),
            ),
            SysCallNumber::UnmapMemory => {
                SysCall::UnmapMemory(unsafe { MemoryRange::new(a1, a2).or(Err(Error::InvalidSyscall)) }?)
            }
            SysCallNumber::Yield => SysCall::Yield,
            SysCallNumber::SetThreadPriority => SysCall::SetThreadPriority(a1.into()),
            SysCallNumber::ReceiveMessage => {
                SysCall::ReceiveMessage(SID::from_u32(a1 as _, a2 as _, a3 as _, a4 as _))
            }
            SysCallNumber::TryReceiveMessage => {
                SysCall::TryReceiveMessage(SID::from_u32(a1 as _, a2 as _, a3 as _, a4 as _))
            }
            SysCallNumber::ClaimInterrupt => SysCall::ClaimInterrupt(
                a1,
                MemoryAddress::new(a2).ok_or(Error::InvalidSyscall)?,
                MemoryAddress::new(a3),
            ),
            SysCallNumber::FreeInterrupt => SysCall::FreeInterrupt(a1),
            #[cfg(keyos)]
            SysCallNumber::FutexWait => SysCall::FutexWait(a1, a2),
            #[cfg(keyos)]
            SysCallNumber::FutexWake => SysCall::FutexWake(a1, a2),
            SysCallNumber::UpdateMemoryFlags => SysCall::UpdateMemoryFlags(
                unsafe { MemoryRange::new(a1, a2) }?,
                MemoryFlags::from_bits(a3),
                PID::new(a4 as _),
            ),
            SysCallNumber::CreateServerWithAddress => {
                SysCall::CreateServerWithAddress(SID::from_u32(a1 as _, a2 as _, a3 as _, a4 as _), a5..a6)
            }
            SysCallNumber::CreateServer => SysCall::CreateServer,
            SysCallNumber::Connect => SysCall::Connect(SID::from_u32(a1 as _, a2 as _, a3 as _, a4 as _)),
            SysCallNumber::SendMessage => Message::try_from((a2, a3, a4, a5, a6, a7))
                .map(|m| SysCall::SendMessage(a1.try_into().unwrap(), m))
                .unwrap_or_else(|_| SysCall::Invalid(a1, a2, a3, a4, a5, a6, a7)),
            SysCallNumber::ReturnMemory => SysCall::ReturnMemory(
                MessageSender::from_usize(a1),
                unsafe { MemoryRange::new(a2, a3) }?,
                MemorySize::new(a4),
                MemorySize::new(a5),
            ),
            SysCallNumber::CreateThread => {
                SysCall::CreateThread(crate::arch::args_to_thread(a1, a2, a3, a4, a5, a6, a7)?)
            }
            SysCallNumber::CreateProcess => SysCall::CreateProcess([a1, a2, a3, a4, a5, a6, a7].try_into()?),
            SysCallNumber::TerminateProcess => SysCall::TerminateProcess(a1 as u32),
            SysCallNumber::Shutdown => SysCall::Shutdown(a1 as i32),
            SysCallNumber::TryConnect => {
                SysCall::TryConnect(SID::from_u32(a1 as _, a2 as _, a3 as _, a4 as _))
            }
            SysCallNumber::TrySendMessage => match a2 {
                1 => SysCall::TrySendMessage(
                    a1 as u32,
                    Message::MutableBorrow(MemoryMessage {
                        id: a3,
                        buf: unsafe { MemoryRange::new(a4, a5) }?,
                        offset: MemoryAddress::new(a6),
                        valid: MemorySize::new(a7),
                    }),
                ),
                2 => SysCall::TrySendMessage(
                    a1 as u32,
                    Message::Borrow(MemoryMessage {
                        id: a3,
                        buf: unsafe { MemoryRange::new(a4, a5) }?,
                        offset: MemoryAddress::new(a6),
                        valid: MemorySize::new(a7),
                    }),
                ),
                3 => SysCall::TrySendMessage(
                    a1 as u32,
                    Message::Move(MemoryMessage {
                        id: a3,
                        buf: unsafe { MemoryRange::new(a4, a5) }?,
                        offset: MemoryAddress::new(a6),
                        valid: MemorySize::new(a7),
                    }),
                ),
                4 => SysCall::TrySendMessage(
                    a1 as u32,
                    Message::Scalar(ScalarMessage { id: a3, arg1: a4, arg2: a5, arg3: a6, arg4: a7 }),
                ),
                5 => SysCall::TrySendMessage(
                    a1.try_into().unwrap(),
                    Message::BlockingScalar(ScalarMessage { id: a3, arg1: a4, arg2: a5, arg3: a6, arg4: a7 }),
                ),
                6 => SysCall::TrySendMessage(
                    a1 as u32,
                    Message::BlockingMove(MemoryMessage {
                        id: a3,
                        buf: unsafe { MemoryRange::new(a4, a5) }?,
                        offset: MemoryAddress::new(a6),
                        valid: MemorySize::new(a7),
                    }),
                ),
                _ => SysCall::Invalid(a1, a2, a3, a4, a5, a6, a7),
            },
            SysCallNumber::ReturnScalar1 => SysCall::ReturnScalar1(MessageSender::from_usize(a1), a2),
            SysCallNumber::ReturnScalar2 => SysCall::ReturnScalar2(MessageSender::from_usize(a1), a2, a3),
            SysCallNumber::ConnectForProcess => SysCall::ConnectForProcess(
                PID::new(a1 as _).ok_or(Error::InvalidSyscall)?,
                SID::from_u32(a2 as _, a3 as _, a4 as _, a5 as _),
            ),
            SysCallNumber::CreateServerId => SysCall::CreateServerId,
            SysCallNumber::GetThreadId => SysCall::GetThreadId,
            SysCallNumber::GetProcessId => SysCall::GetProcessId,
            SysCallNumber::DestroyServer => {
                SysCall::DestroyServer(SID::from_u32(a1 as _, a2 as _, a3 as _, a4 as _))
            }
            SysCallNumber::Disconnect => SysCall::Disconnect(a1 as _),
            SysCallNumber::JoinThread => SysCall::JoinThread(a1 as _),
            SysCallNumber::GetRemoteProcessId => SysCall::GetRemoteProcessId(a1 as _),
            #[cfg(keyos)]
            SysCallNumber::VirtToPhys => SysCall::VirtToPhys(a1 as _),
            #[cfg(keyos)]
            SysCallNumber::VirtToPhysPid => SysCall::VirtToPhysPid(pid_from_usize(a1)?, a2 as _),
            SysCallNumber::ReturnScalar5 => {
                SysCall::ReturnScalar5(MessageSender::from_usize(a1), a2, a3, a4, a5, a6)
            }
            SysCallNumber::GetAppId => SysCall::GetAppId(PID::new(a1 as _).ok_or(Error::InvalidSyscall)?),
            SysCallNumber::AllowMessagesSID => {
                SysCall::AllowMessagesSID(SID::from_u32(a1 as _, a2 as _, a3 as _, a4 as _), a5..a6)
            }
            SysCallNumber::AllowMessagesCID => {
                SysCall::AllowMessagesCID(PID::new(a1 as _).ok_or(Error::InvalidSyscall)?, a2 as CID, a3..a4)
            }
            #[cfg(keyos)]
            SysCallNumber::InvalidateCache => {
                SysCall::FlushCache(unsafe { MemoryRange::new(a1, a2) }?, a3.into())
            }
            SysCallNumber::PowerManagement => SysCall::PowerManagement(a1.into()),
            SysCallNumber::AppIdToPid => {
                SysCall::AppIdToPid(AppId::from([a1 as u32, a2 as u32, a3 as u32, a4 as u32]))
            }
            SysCallNumber::ServerEventHandler => SysCall::ServerEventHandler(
                a1.into(),
                SID::from_u32(a2 as _, a3 as _, a4 as _, a5 as _),
                a6 as _,
            ),
            SysCallNumber::MirrorMemoryToPid => SysCall::MirrorMemoryToPid(
                unsafe { MemoryRange::new(a1, a2) }?,
                PID::new(a3 as _).ok_or(Error::InvalidSyscall)?,
            ),

            #[cfg(keyos)]
            SysCallNumber::DebugCommand => {
                SysCall::DebugCommand(unsafe { MemoryRange::new(a1, a2)? }, a3 as u8)
            }
            SysCallNumber::GetSystemStats => SysCall::GetSystemStats(SystemStat::from(a1)),

            SysCallNumber::SystemEventHandler => SysCall::SystemEventHandler(
                a1.into(),
                SID::from_u32(a2 as _, a3 as _, a4 as _, a5 as _),
                a6 as _,
            ),
            SysCallNumber::AppendPanicMessage => SysCall::AppendPanicMessage(a1, a2, a3, a4, a5, a6, a7),

            SysCallNumber::GetPanicMessage => SysCall::GetPanicMessage(unsafe { MemoryRange::new(a1, a2)? }),

            SysCallNumber::TerminatePid => {
                SysCall::TerminatePid(PID::new(a1 as _).ok_or(Error::InvalidSyscall)?, a2 as u32)
            }

            #[cfg(not(keyos))]
            SysCallNumber::FutexWait
            | SysCallNumber::FutexWake
            | SysCallNumber::DebugCommand
            | SysCallNumber::VirtToPhys
            | SysCallNumber::VirtToPhysPid
            | SysCallNumber::InvalidateCache => SysCall::Invalid(a1, a2, a3, a4, a5, a6, a7),
            SysCallNumber::Invalid => SysCall::Invalid(a1, a2, a3, a4, a5, a6, a7),
        })
    }

    /// Returns `true` if the associated syscall is a message that has memory attached to it
    #[inline(always)]
    pub fn has_memory(&self) -> bool {
        match self {
            SysCall::TrySendMessage(_, msg) | SysCall::SendMessage(_, msg) => {
                matches!(
                    msg,
                    Message::Move(_)
                        | Message::Borrow(_)
                        | Message::MutableBorrow(_)
                        | Message::BlockingMove(_)
                )
            }
            SysCall::ReturnMemory(_, _, _, _) => true,
            _ => false,
        }
    }

    /// Returns `true` if the associated syscall is a message that is a Move
    #[inline(always)]
    pub fn is_move(&self) -> bool {
        match self {
            SysCall::TrySendMessage(_, msg) | SysCall::SendMessage(_, msg) => {
                matches!(msg, Message::Move(_))
            }
            _ => false,
        }
    }

    /// Returns `true` if the associated syscall is a message that is a BlockingMove
    #[inline(always)]
    pub fn is_blocking_move(&self) -> bool {
        match self {
            SysCall::TrySendMessage(_, msg) | SysCall::SendMessage(_, msg) => {
                matches!(msg, Message::BlockingMove(_))
            }
            _ => false,
        }
    }

    /// Returns `true` if the associated syscall is a message that is a Borrow
    #[inline(always)]
    pub fn is_borrow(&self) -> bool {
        match self {
            SysCall::TrySendMessage(_, msg) | SysCall::SendMessage(_, msg) => {
                matches!(msg, Message::Borrow(_))
            }
            _ => false,
        }
    }

    /// Returns `true` if the associated syscall is a message that is a MutableBorrow
    #[inline(always)]
    pub fn is_mutableborrow(&self) -> bool {
        match self {
            SysCall::TrySendMessage(_, msg) | SysCall::SendMessage(_, msg) => {
                matches!(msg, Message::MutableBorrow(_))
            }
            _ => false,
        }
    }

    /// Returns `true` if the associated syscall is returning memory
    #[inline(always)]
    pub fn is_return_memory(&self) -> bool { matches!(self, SysCall::ReturnMemory(..)) }

    /// If the syscall has memory attached to it, return the memory
    #[inline(always)]
    pub fn memory(&self) -> Option<MemoryRange> {
        match self {
            SysCall::TrySendMessage(_, msg) | SysCall::SendMessage(_, msg) => match msg {
                Message::Move(memory_message)
                | Message::Borrow(memory_message)
                | Message::MutableBorrow(memory_message)
                | Message::BlockingMove(memory_message) => Some(memory_message.buf),
                _ => None,
            },
            SysCall::ReturnMemory(_, range, _, _) => Some(*range),
            #[cfg(keyos)]
            SysCall::FlushCache(range, _) => Some(*range),
            _ => None,
        }
    }

    /// If the syscall has memory attached to it, replace the memory.
    ///
    /// # Safety
    ///
    /// This function is only safe to call to fixup the pointer, particularly
    /// when running in hosted mode. It should not be used for any other purpose.
    #[inline(always)]
    pub unsafe fn replace_memory(&mut self, new: MemoryRange) {
        match self {
            SysCall::TrySendMessage(_, msg) | SysCall::SendMessage(_, msg) => match msg {
                Message::Move(memory_message)
                | Message::Borrow(memory_message)
                | Message::MutableBorrow(memory_message)
                | Message::BlockingMove(memory_message) => memory_message.buf = new,
                _ => (),
            },
            SysCall::ReturnMemory(_, range, _, _) => *range = new,
            #[cfg(keyos)]
            SysCall::FlushCache(range, _) => *range = new,
            _ => (),
        }
    }

    /// Returns `true` if the given syscall may be called from an IRQ context
    #[inline(always)]
    pub fn can_call_from_interrupt(&self) -> bool {
        if let SysCall::TrySendMessage(_cid, msg) = self {
            return !msg.is_blocking();
        }
        matches!(
            self,
            SysCall::TryConnect(_)
                | SysCall::FreeInterrupt(_)
                | SysCall::ClaimInterrupt(_, _, _)
                | SysCall::TryReceiveMessage(_)
                | SysCall::ReturnScalar5(_, _, _, _, _, _)
                | SysCall::ReturnScalar2(_, _, _)
                | SysCall::ReturnScalar1(_, _)
                | SysCall::ReturnMemory(_, _, _, _)
                | SysCall::MapMemory(_, _, _, _)
                | SysCall::UnmapMemory(_)
        )
    }
}

/// Map the given physical address to the given virtual address.
/// The `size` field must be page-aligned.
#[cfg(keyos)]
#[inline]
pub fn map_memory(
    phys: Option<MemoryAddress>,
    virt: Option<MemoryAddress>,
    size: usize,
    flags: MemoryFlags,
) -> core::result::Result<MemoryRange, Error> {
    let result =
        rsyscall(SysCall::MapMemory(phys, virt, MemorySize::new(size).ok_or(Error::InvalidSyscall)?, flags))?;
    match result {
        Result::MemoryRange(range) => Ok(range),
        Result::Error(e) => Err(e),
        _ => Err(Error::InternalError),
    }
}

/// Hosted `map_memory` is a plain `alloc_zeroed`. The kernel emulator's `MapMemory`
/// handler is a no-op in hosted mode (its `map_page` returns `Ok(())` without backing
/// any storage), so going over TCP would just round-trip a useless syscall. Allocate
/// directly here.
#[cfg(not(keyos))]
#[inline]
pub fn map_memory(
    _phys: Option<MemoryAddress>,
    _virt: Option<MemoryAddress>,
    size: usize,
    _flags: MemoryFlags,
) -> core::result::Result<MemoryRange, Error> {
    use core::alloc::Layout;
    extern crate alloc;
    let layout =
        Layout::from_size_align(size, crate::PAGE_SIZE).map_err(|_| Error::InvalidSyscall)?.pad_to_align();
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        return Err(Error::OutOfMemory);
    }
    unsafe { MemoryRange::new(ptr as usize, size) }
}

/// Map the given physical address to the given virtual address.
/// The `size` field must be page-aligned.
#[cfg(keyos)]
#[inline]
pub fn unmap_memory(range: MemoryRange) -> core::result::Result<(), Error> {
    let result = rsyscall(SysCall::UnmapMemory(range))?;
    match result {
        crate::Result::Ok => Ok(()),
        Result::Error(e) => Err(e),
        _ => Err(Error::InternalError),
    }
}

/// Hosted `unmap_memory` mirrors the alloc above: free the memory directly without
/// going through the no-op kernel-emulator syscall.
#[cfg(not(keyos))]
#[inline]
pub fn unmap_memory(range: MemoryRange) -> core::result::Result<(), Error> {
    use core::alloc::Layout;
    extern crate alloc;
    let layout = Layout::from_size_align(range.len(), crate::PAGE_SIZE)
        .map_err(|_| Error::InvalidSyscall)?
        .pad_to_align();
    unsafe { alloc::alloc::dealloc(range.as_mut_ptr(), layout) };
    Ok(())
}

/// Update the permissions on the given memory range. Note that permissions may
/// only be stripped here -- they may never be added.
#[inline]
pub fn update_memory_flags(range: MemoryRange, flags: MemoryFlags) -> core::result::Result<Result, Error> {
    let result = rsyscall(SysCall::UpdateMemoryFlags(range, flags, None))?;
    if let Result::Ok = result {
        Ok(Result::Ok)
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Map the given physical address to the given virtual address.
/// The `size` field must be page-aligned.
#[inline]
pub fn return_memory(sender: MessageSender, mem: MemoryRange) -> core::result::Result<(), Error> {
    let result = rsyscall(SysCall::ReturnMemory(sender, mem, None, None))?;
    if let crate::Result::Ok = result {
        Ok(())
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Map the given physical address to the given virtual address.
/// The `size` field must be page-aligned.
#[inline]
pub fn return_memory_offset(
    sender: MessageSender,
    mem: MemoryRange,
    offset: Option<MemorySize>,
) -> core::result::Result<(), Error> {
    let result = rsyscall(SysCall::ReturnMemory(sender, mem, offset, None))?;
    if let crate::Result::Ok = result {
        Ok(())
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Map the given physical address to the given virtual address.
/// The `size` field must be page-aligned.
#[inline]
pub fn return_memory_offset_valid(
    sender: MessageSender,
    mem: MemoryRange,
    offset: Option<MemorySize>,
    valid: Option<MemorySize>,
) -> core::result::Result<(), Error> {
    let result = rsyscall(SysCall::ReturnMemory(sender, mem, offset, valid))?;
    if let crate::Result::Ok = result {
        Ok(())
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Map the given physical address to the given virtual address.
/// The `size` field must be page-aligned.
#[inline]
pub fn return_scalar(sender: MessageSender, val: usize) -> core::result::Result<(), Error> {
    let result = rsyscall(SysCall::ReturnScalar1(sender, val))?;
    if let crate::Result::Ok = result {
        Ok(())
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Map the given physical address to the given virtual address.
/// The `size` field must be page-aligned.
#[inline]
pub fn return_scalar2(sender: MessageSender, val1: usize, val2: usize) -> core::result::Result<(), Error> {
    let result = rsyscall(SysCall::ReturnScalar2(sender, val1, val2))?;
    if let crate::Result::Ok = result {
        Ok(())
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Return 5 scalars to the provided message.
#[inline]
pub fn return_scalar5(
    sender: MessageSender,
    val1: usize,
    val2: usize,
    val3: usize,
    val4: usize,
    val5: usize,
) -> core::result::Result<(), Error> {
    let result = rsyscall(SysCall::ReturnScalar5(sender, val1, val2, val3, val4, val5))?;
    if let crate::Result::Ok = result {
        Ok(())
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Claim a hardware interrupt for this process.
#[cfg(keyos)]
#[inline]
pub fn claim_interrupt(
    irq_no: crate::arch::irq::IrqNumber,
    callback: fn(irq_no: usize, arg: *mut usize),
    arg: *mut usize,
) -> core::result::Result<(), Error> {
    let result = rsyscall(SysCall::ClaimInterrupt(
        irq_no as usize,
        MemoryAddress::new(callback as *mut usize as usize).ok_or(Error::InvalidSyscall)?,
        MemoryAddress::new(arg as usize),
    ))?;
    if let crate::Result::Ok = result {
        Ok(())
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Wait on a futex. See [`SysCall::FutexWait`] for documentation.
#[cfg(keyos)]
#[inline]
pub fn futex_wait(futex: &AtomicUsize, expected_value: usize) -> core::result::Result<(), Error> {
    let result = rsyscall(SysCall::FutexWait(futex as *const _ as usize, expected_value))?;
    if let crate::Result::Ok = result {
        Ok(())
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Wake threads waiting on a futex. See [`SysCall::FutexWake`] for documentation.
#[cfg(keyos)]
#[inline]
pub fn futex_wake(futex: &AtomicUsize, n: usize) -> core::result::Result<(), Error> {
    let result = rsyscall(SysCall::FutexWake(futex as *const _ as usize, n))?;
    if let crate::Result::Ok = result {
        Ok(())
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Create a new server with the given SID.  This enables other processes to
/// connect to this server to send messages without connecting to the nameserver.
/// Message IDs that are in the range `initial_message_permissions` are allowed to
/// every connecting client. Use 0..0 to not allow any messages by default.
///
/// # Errors
///
/// * **OutOfMemory**: No more servers may be created because the server count limit has been reached, or the
///   system does not have enough memory for the backing store.
/// * **ServerExists**: A server has already registered with that name
/// * **InvalidString**: The name was not a valid UTF-8 string
#[inline]
pub fn create_server_with_sid(
    sid: SID,
    initial_message_permissions: core::ops::Range<MessageId>,
) -> core::result::Result<SID, Error> {
    let result = rsyscall(SysCall::CreateServerWithAddress(sid, initial_message_permissions))?;
    if let Result::NewServerID(sid) = result {
        Ok(sid)
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Create a new server with a random name.  This enables other processes to
/// connect to this server to send messages.  A random server ID is generated
/// by the kernel and returned to the caller. This address can then be registered
/// to a namserver.
///
/// # Errors
///
/// * **ServerNotFound**: No more servers may be created
/// * **OutOfMemory**: No more servers may be created because the server count limit has been reached, or the
///   system does not have enough memory for the backing store.
#[inline]
pub fn create_server() -> core::result::Result<SID, Error> {
    let result = rsyscall(SysCall::CreateServer)?;
    if let Result::NewServerID(sid) = result {
        Ok(sid)
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Fetch a random server ID from the kernel. This is used
/// exclusively by the name server and the suspend/resume server.  A random server ID is generated
/// by the kernel and returned to the caller. This address can then be registered
/// to a namserver by the caller in their memory space.
///
/// The implementation is just a call to the kernel-exclusive TRNG to fetch random numbers.
///
/// # Errors
#[inline]
pub fn create_server_id() -> core::result::Result<SID, Error> {
    let result = rsyscall(SysCall::CreateServerId)?;
    if let Result::ServerID(sid) = result {
        Ok(sid)
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Connect to a server with the given SID
#[inline]
pub fn connect(server: SID) -> core::result::Result<CID, Error> {
    let result = rsyscall(SysCall::Connect(server))?;
    if let Result::ConnectionID(cid) = result {
        Ok(cid)
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Connect to a server with the given SID
#[inline]
pub fn try_connect(server: SID) -> core::result::Result<CID, Error> {
    let result = rsyscall(SysCall::TryConnect(server))?;
    if let Result::ConnectionID(cid) = result {
        Ok(cid)
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Suspend the current process until a message is received.  This thread will
/// block until a message is received.
///
/// # Errors
#[inline]
pub fn receive_message(server: SID) -> core::result::Result<MessageEnvelope, Error> {
    let result = rsyscall(SysCall::ReceiveMessage(server)).expect("Couldn't call ReceiveMessage");
    if let Result::MessageEnvelope(envelope) = result {
        Ok(envelope)
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Retrieve a message from the message queue for the provided server. If no message
/// is available, returns `Ok(None)` without blocking
///
/// # Errors
#[inline]
pub fn try_receive_message(server: SID) -> core::result::Result<Option<MessageEnvelope>, Error> {
    let result = rsyscall(SysCall::TryReceiveMessage(server)).expect("Couldn't call ReceiveMessage");
    if let Result::MessageEnvelope(envelope) = result {
        Ok(Some(envelope))
    } else if result == Result::None {
        Ok(None)
    } else if let Result::Error(e) = result {
        Err(e)
    } else {
        Err(Error::InternalError)
    }
}

/// Send a message to a server.  Depending on the mesage type (move or borrow), it
/// will either block (borrow) or return immediately (move).
/// If the message type is `borrow`, then the memory addresses pointed to will be
/// unavailable to this process until this function returns.
///
/// # Errors
///
/// * **ServerNotFound**: The server does not exist so the connection is now invalid
/// * **BadAddress**: The client tried to pass a Memory message using an address it doesn't own
/// * **ServerQueueFull**: The queue in the server is full, and this call would block
/// * **Timeout**: The timeout limit has been reached
#[inline]
pub fn try_send_message(connection: CID, message: Message) -> core::result::Result<Result, Error> {
    let result = rsyscall(SysCall::TrySendMessage(connection, message));
    match result {
        Ok(Result::Ok) => Ok(Result::Ok),
        Ok(Result::Scalar1(a)) => Ok(Result::Scalar1(a)),
        Ok(Result::Scalar2(a, b)) => Ok(Result::Scalar2(a, b)),
        Ok(Result::Scalar5(a, b, c, d, e)) => Ok(Result::Scalar5(a, b, c, d, e)),
        Ok(Result::MemoryReturned(range, offset, valid)) => Ok(Result::MemoryReturned(range, offset, valid)),
        Ok(Result::MessageEnvelope(msg)) => Ok(Result::MessageEnvelope(msg)),
        Err(e) => Err(e),
        v => panic!("Unexpected return value: {:?}", v),
    }
}

/// Connect to a server on behalf of another process. This can be used by a name
/// resolution server to securely create connections without disclosing a SID.
///
/// # Errors
///
/// * **ServerNotFound**: The server does not exist so the connection is now invalid
/// * **BadAddress**: The client tried to pass a Memory message using an address it doesn't own
/// * **ServerQueueFull**: The queue in the server is full, and this call would block
/// * **Timeout**: The timeout limit has been reached
#[inline]
pub fn connect_for_process(pid: PID, sid: SID) -> core::result::Result<CID, Error> {
    let result = rsyscall(SysCall::ConnectForProcess(pid, sid));
    match result {
        Ok(Result::ConnectionID(cid)) => Ok(cid),
        Err(e) => Err(e),
        v => panic!("Unexpected return value: {:?}", v),
    }
}

/// Retrieves the process ID (PID) of a remote process identified by a connection ID (CID).
#[inline]
pub fn get_remote_pid(cid: CID) -> core::result::Result<PID, Error> {
    let result = rsyscall(SysCall::GetRemoteProcessId(cid));
    match result {
        Ok(Result::ProcessID(pid)) => Ok(pid),
        Err(e) => Err(e),
        v => panic!("Unexpected return value: {:?}", v),
    }
}

/// Send a message to a server.  Depending on the message type (move or borrow), it
/// will either block (borrow) or return immediately (move).
/// If the message type is `borrow`, then the memory addresses pointed to will be
/// unavailable to this process until this function returns.
///
/// If the server queue is full, this will block.
///
/// # Errors
///
/// * **ServerNotFound**: The server does not exist so the connection is now invalid
/// * **BadAddress**: The client tried to pass a Memory message using an address it doesn't own
/// * **Timeout**: The timeout limit has been reached
#[inline]
pub fn send_message(connection: CID, message: Message) -> core::result::Result<Result, Error> {
    let result = rsyscall(SysCall::SendMessage(connection, message));
    match result {
        Ok(Result::Ok) => Ok(Result::Ok),
        Ok(Result::Scalar1(a)) => Ok(Result::Scalar1(a)),
        Ok(Result::Scalar2(a, b)) => Ok(Result::Scalar2(a, b)),
        Ok(Result::Scalar5(a, b, c, d, e)) => Ok(Result::Scalar5(a, b, c, d, e)),
        Ok(Result::MemoryReturned(range, offset, valid)) => Ok(Result::MemoryReturned(range, offset, valid)),
        Err(e) => Err(e),
        v => panic!("Unexpected return value: {:?}", v),
    }
}

#[inline]
pub fn terminate_process(exit_code: u32) -> ! {
    rsyscall(SysCall::TerminateProcess(exit_code)).expect("terminate_process returned an error");
    panic!("process didn't terminate");
}

/// Terminates a process with the given PID.
#[inline]
pub fn terminate_pid(pid: PID, exit_code: u32) -> core::result::Result<(), Error> {
    let result = rsyscall(SysCall::TerminatePid(pid, exit_code));
    match result {
        Ok(Result::Ok) => Ok(()),
        Err(e) => Err(e),
        v => panic!("Unexpected return value: {:?}", v),
    }
}

/// Return execution to the kernel. This function may return at any time,
/// including immediately
#[inline]
pub fn yield_slice() { rsyscall(SysCall::Yield).ok(); }

/// Set the priority of the current thread.
/// Threads spawned from this thread will inherit this priority.
/// Keep in mind that high priority threads can starve lower-priority ones infinitely,
/// even if they are in different processes.
/// [`ThreadPriority::Idle`] is reserved for the kernel
/// [`ThreadPriority::System0`] and higher are privileged.
#[inline]
pub fn set_thread_priority(priority: ThreadPriority) -> core::result::Result<(), Error> {
    #[cfg(keyos)]
    let result = rsyscall(SysCall::SetThreadPriority(priority));
    #[cfg(not(keyos))]
    let _ = priority;
    #[cfg(not(keyos))]
    let result = Ok(Result::Ok);

    match result {
        Ok(Result::Ok) => Ok(()),
        Err(e) => Err(e),
        v => panic!("Unexpected return value: {:?}", v),
    }
}

#[deprecated(since = "0.2.0", note = "Please use create_thread_n() or create_thread()")]
#[inline]
pub fn create_thread_simple<T, U>(
    f: fn(T) -> U,
    arg: T,
) -> core::result::Result<crate::arch::WaitHandle<U>, Error>
where
    T: Send + 'static,
    U: Send + 'static,
{
    let thread_info = crate::arch::create_thread_simple_pre(&f, &arg)?;
    rsyscall(SysCall::CreateThread(thread_info)).and_then(|result| {
        if let Result::ThreadID(thread_id) = result {
            crate::arch::create_thread_simple_post(f, arg, thread_id)
        } else {
            Err(Error::InternalError)
        }
    })
}

#[inline]
pub fn create_thread_0<T>(f: fn() -> T) -> core::result::Result<crate::arch::WaitHandle<T>, Error>
where
    T: Send + 'static,
{
    let thread_info = crate::arch::create_thread_0_pre(&f)?;
    rsyscall(SysCall::CreateThread(thread_info)).and_then(|result| {
        if let Result::ThreadID(thread_id) = result {
            crate::arch::create_thread_0_post(f, thread_id)
        } else {
            Err(Error::InternalError)
        }
    })
}

#[inline]
pub fn create_thread_1<T>(
    f: fn(usize) -> T,
    arg1: usize,
) -> core::result::Result<crate::arch::WaitHandle<T>, Error>
where
    T: Send + 'static,
{
    let thread_info = crate::arch::create_thread_1_pre(&f, &arg1)?;
    rsyscall(SysCall::CreateThread(thread_info)).and_then(|result| {
        if let Result::ThreadID(thread_id) = result {
            crate::arch::create_thread_1_post(f, arg1, thread_id)
        } else {
            Err(Error::InternalError)
        }
    })
}

#[inline]
pub fn create_thread_2<T>(
    f: fn(usize, usize) -> T,
    arg1: usize,
    arg2: usize,
) -> core::result::Result<crate::arch::WaitHandle<T>, Error>
where
    T: Send + 'static,
{
    let thread_info = crate::arch::create_thread_2_pre(&f, &arg1, &arg2)?;
    rsyscall(SysCall::CreateThread(thread_info)).and_then(|result| {
        if let Result::ThreadID(thread_id) = result {
            crate::arch::create_thread_2_post(f, arg1, arg2, thread_id)
        } else {
            Err(Error::InternalError)
        }
    })
}

#[inline]
pub fn create_thread_3<T>(
    f: fn(usize, usize, usize) -> T,
    arg1: usize,
    arg2: usize,
    arg3: usize,
) -> core::result::Result<crate::arch::WaitHandle<T>, Error>
where
    T: Send + 'static,
{
    let thread_info = crate::arch::create_thread_3_pre(&f, &arg1, &arg2, &arg3)?;
    rsyscall(SysCall::CreateThread(thread_info)).and_then(|result| {
        if let Result::ThreadID(thread_id) = result {
            crate::arch::create_thread_3_post(f, arg1, arg2, arg3, thread_id)
        } else {
            Err(Error::InternalError)
        }
    })
}

#[inline]
pub fn create_thread_4<T>(
    f: fn(usize, usize, usize, usize) -> T,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
) -> core::result::Result<crate::arch::WaitHandle<T>, Error>
where
    T: Send + 'static,
{
    let thread_info = crate::arch::create_thread_4_pre(&f, &arg1, &arg2, &arg3, &arg4)?;
    rsyscall(SysCall::CreateThread(thread_info)).and_then(|result| {
        if let Result::ThreadID(thread_id) = result {
            crate::arch::create_thread_4_post(f, arg1, arg2, arg3, arg4, thread_id)
        } else {
            Err(Error::InternalError)
        }
    })
}

/// Create a new thread with the given closure.
#[inline]
pub fn create_thread<F, T>(f: F) -> core::result::Result<crate::arch::WaitHandle<T>, Error>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    let thread_info = crate::arch::create_thread_pre(&f)?;
    rsyscall(SysCall::CreateThread(thread_info)).and_then(|result| {
        if let Result::ThreadID(thread_id) = result {
            crate::arch::create_thread_post(f, thread_id)
        } else {
            Err(Error::InternalError)
        }
    })
}

/// Wait for a thread to finish. This is equivalent to `join_thread`
#[inline]
pub fn wait_thread<T>(joiner: crate::arch::WaitHandle<T>) -> SysCallResult {
    crate::arch::wait_thread(joiner)
}

#[inline]
pub fn create_process(args: ProcessArgs) -> core::result::Result<(PID, crate::arch::ProcessHandle), Error> {
    let process_init = crate::arch::create_process_pre(&args)?;
    rsyscall(SysCall::CreateProcess(process_init)).and_then(|result| {
        #[cfg(keyos)]
        {
            process_init.free_name_buf();
        }

        if let Result::NewProcess(startup) = result {
            crate::arch::create_process_post(args, process_init, startup)
        } else {
            Err(Error::InternalError)
        }
    })
}

/// Wait for a thread to finish
#[inline]
pub fn wait_process(joiner: crate::arch::ProcessHandle) -> SysCallResult { crate::arch::wait_process(joiner) }

/// Get the current process ID
#[inline]
pub fn current_pid() -> core::result::Result<PID, Error> {
    rsyscall(SysCall::GetProcessId).and_then(|result| {
        if let Result::ProcessID(pid) = result {
            Ok(pid)
        } else {
            Err(Error::InternalError)
        }
    })
}

/// Get the current thread ID
#[inline]
pub fn current_tid() -> core::result::Result<TID, Error> {
    rsyscall(SysCall::GetThreadId).and_then(|result| {
        if let Result::ThreadID(tid) = result {
            Ok(tid)
        } else {
            Err(Error::InternalError)
        }
    })
}

#[inline]
pub fn destroy_server(sid: SID) -> core::result::Result<(), Error> {
    rsyscall(SysCall::DestroyServer(sid)).and_then(|result| {
        if let Result::Ok = result {
            Ok(())
        } else {
            Err(Error::InternalError)
        }
    })
}

/// Release the CID once. If called as many times as connect() was, the CID is truly
/// freed.
#[inline]
pub fn disconnect(cid: CID) -> core::result::Result<(), Error> {
    rsyscall(SysCall::Disconnect(cid)).and_then(|result| {
        if let Result::Ok = result {
            Ok(())
        } else {
            Err(Error::InternalError)
        }
    })
}

/// Block the current thread and wait for the specified thread to
/// return. Returns the return value of the thread.
///
/// # Errors
///
/// * **ThreadNotAvailable**: The thread could not be found, or was not sleeping.
#[inline]
pub fn join_thread(tid: TID) -> core::result::Result<usize, Error> {
    rsyscall(SysCall::JoinThread(tid)).and_then(|result| {
        if let Result::Scalar1(val) = result {
            Ok(val)
        } else if let Result::Error(Error::ThreadNotAvailable) = result {
            Err(Error::ThreadNotAvailable)
        } else {
            Err(Error::InternalError)
        }
    })
}

/// Translate a virtual address to a physical address
#[cfg(keyos)]
#[inline]
pub fn virt_to_phys(va: usize) -> core::result::Result<usize, Error> {
    rsyscall(SysCall::VirtToPhys(va)).and_then(|result| {
        if let Result::Scalar1(pa) = result {
            Ok(pa)
        } else {
            Err(Error::BadAddress)
        }
    })
}

/// Translate a virtual address to a physical address for a given process
#[cfg(keyos)]
#[inline]
pub fn virt_to_phys_pid(pid: PID, va: usize) -> core::result::Result<usize, Error> {
    rsyscall(SysCall::VirtToPhysPid(pid, va)).and_then(|result| {
        if let Result::Scalar1(pa) = result {
            Ok(pa)
        } else {
            Err(Error::BadAddress)
        }
    })
}

#[inline]
pub fn get_app_id(pid: PID) -> core::result::Result<Option<AppId>, Error> {
    crate::arch::syscall(SysCall::GetAppId(pid)).and_then(|result| {
        if let Result::Scalar5(a, b, c, d, present) = result {
            let a = a as u32;
            let b = b as u32;
            let c = c as u32;
            let d = d as u32;
            if present == 0 {
                return Ok(None);
            }
            let mut result = [0; 16];
            result[0..4].copy_from_slice(&a.to_le_bytes());
            result[4..8].copy_from_slice(&b.to_le_bytes());
            result[8..12].copy_from_slice(&c.to_le_bytes());
            result[12..16].copy_from_slice(&d.to_le_bytes());
            Ok(Some(AppId(result)))
        } else {
            Err(Error::InternalError)
        }
    })
}

#[inline]
pub fn allow_messages_on_server(
    sid: SID,
    messages: core::ops::Range<MessageId>,
) -> core::result::Result<(), Error> {
    crate::arch::syscall(SysCall::AllowMessagesSID(sid, messages))?;
    Ok(())
}

#[inline]
pub fn allow_messages_on_connection(
    pid: PID,
    cid: CID,
    messages: core::ops::Range<MessageId>,
) -> core::result::Result<(), Error> {
    crate::arch::syscall(SysCall::AllowMessagesCID(pid, cid, messages))?;
    Ok(())
}

#[inline]
pub fn flush_cache(_mem: MemoryRange, _operation: CacheOperation) -> core::result::Result<(), Error> {
    #[cfg(keyos)]
    crate::arch::syscall(SysCall::FlushCache(_mem, _operation))?;
    Ok(())
}

#[inline]
pub fn set_power_management(dram: DramIdleMode) -> core::result::Result<(), Error> {
    crate::arch::syscall(SysCall::PowerManagement(dram))?;
    Ok(())
}

#[inline]
pub fn app_id_to_pid(app_id: &AppId) -> core::result::Result<Option<PID>, Error> {
    crate::arch::syscall(SysCall::AppIdToPid(*app_id)).and_then(|result| match result {
        Result::ProcessID(pid) => Ok(Some(pid)),
        Result::None => Ok(None),
        Result::Error(e) => Err(e),
        _ => Err(Error::InternalError),
    })
}

#[inline]
pub fn mirror_memory_to_pid(mem: MemoryRange, pid: PID) -> core::result::Result<MemoryRange, Error> {
    crate::arch::syscall(SysCall::MirrorMemoryToPid(mem, pid)).and_then(|result| match result {
        Result::MemoryRange(mem) => Ok(mem),
        Result::Error(e) => Err(e),
        _ => Err(Error::InternalError),
    })
}

#[cfg(keyos)]
#[inline]
pub fn debug_command(buffer: MemoryRange, cmd: u8) -> core::result::Result<usize, Error> {
    crate::arch::syscall(SysCall::DebugCommand(buffer, cmd)).and_then(|result| match result {
        Result::Scalar1(v) => Ok(v),
        Result::Error(e) => Err(e),
        _ => Err(Error::InternalError),
    })
}

#[inline]
pub fn get_system_stat(stat: SystemStat) -> core::result::Result<usize, Error> {
    crate::arch::syscall(SysCall::GetSystemStats(stat)).and_then(|result| match result {
        Result::Scalar1(v) => Ok(v),
        Result::Error(e) => Err(e),
        _ => Err(Error::InternalError),
    })
}

/// Registers a message ID that will be received when the specified event happens on the server.
/// See the docs of [`ServerEvent`] for the parameters of the sent message.
#[inline]
pub fn register_server_event_handler(
    event: ServerEvent,
    sid: SID,
    id: MessageId,
) -> core::result::Result<(), Error> {
    crate::arch::syscall(SysCall::ServerEventHandler(event, sid, id)).and_then(|result| match result {
        Result::Ok => Ok(()),
        Result::Error(e) => Err(e),
        _ => Err(Error::InternalError),
    })
}

/// Registers a SID that will receive a message when the specified event happens.
/// See the docs of [`SystemEvent`] for the parameters of the sent message.
#[inline]
pub fn register_system_event_handler(
    event: SystemEvent,
    sid: SID,
    id: MessageId,
) -> core::result::Result<(), Error> {
    crate::arch::syscall(SysCall::SystemEventHandler(event, sid, id)).and_then(|result| match result {
        Result::Ok => Ok(()),
        Result::Error(e) => Err(e),
        _ => Err(Error::InternalError),
    })
}

/// Appends the given bytes to a panic message buffer stored by the kernel.
///
/// *Note*: this function can only accept up to 24 bytes of data, to add more, call this function multiple
/// times.
#[inline]
pub fn append_panic_message(msg: &[u8]) -> core::result::Result<(), Error> {
    let msg_len = msg.len();

    // Pack message bytes into usize words
    let mut args = [0usize; 6];
    for (chunk, arg) in msg.chunks(core::mem::size_of::<usize>()).zip(args.iter_mut()) {
        let mut usize_chunk = [0u8; core::mem::size_of::<usize>()];
        usize_chunk[..chunk.len()].copy_from_slice(chunk);
        *arg = usize::from_le_bytes(usize_chunk);
    }

    crate::arch::syscall(SysCall::AppendPanicMessage(
        msg_len, args[0], args[1], args[2], args[3], args[4], args[5],
    ))
    .and_then(|result| match result {
        Result::Ok => Ok(()),
        Result::Error(e) => Err(e),
        _ => Err(Error::InternalError),
    })
}

/// Gets the last panic message (including backtrace) from the kernel
/// Returns (`PID`, `bytes_written`) on success
#[inline]
pub fn get_panic_message(buffer: MemoryRange) -> core::result::Result<(u8, usize), Error> {
    crate::arch::syscall(SysCall::GetPanicMessage(buffer)).and_then(|result| match result {
        Result::Scalar2(pid, bytes_written) => Ok((pid as u8, bytes_written)),
        Result::Error(e) => Err(e),
        _ => Err(Error::InternalError),
    })
}

/// Perform a raw syscall and return the result. This will transform
/// `xous::Result::Error(e)` into an `Err(e)`.
#[inline]
pub fn rsyscall(call: SysCall) -> SysCallResult { crate::arch::syscall(call) }

// /// This is dangerous, but fast.
// pub unsafe fn dangerous_syscall(call: SysCall) -> SyscallResult {
//     use core::mem::{transmute, MaybeUninit};
//     let mut ret = MaybeUninit::uninit().assume_init();
//     let presto = transmute::<_, (usize, usize, usize, usize, usize, usize, usize, usize)>(call);
//     _xous_syscall_rust(
//         presto.0, presto.1, presto.2, presto.3, presto.4, presto.5, presto.6, presto.7, &mut ret,
//     );
//     match ret {
//         Result::Error(e) => Err(e),
//         other => Ok(other),
//     }
// }
