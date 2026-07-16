use crate::definitions::SysCallResult;
use crate::AppId;
use crate::MemoryAddress;
use crate::MemoryRange;
use crate::TID;

// Won't compile without this symbol defined
#[cfg(feature = "rustc-dep-of-std")]
#[no_mangle]
static __aeabi_unwind_cpp_pr1: usize = 0;

pub mod irq;
mod syscall;
use keyos::PAGE_SIZE;
pub use syscall::*;

pub const MAX_PROCESS_NAME_LEN: usize = 32;

pub type ProcessHandle = ();

/// ProcessArgs are the arguments that are created by the user. These
/// will be turned into `ProcessInit` by this library prior to sending
/// them into the kernel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessArgs<'a> {
    name: &'a str,
    elf: MemoryRange,
    app_id: AppId,
}

impl<'a> ProcessArgs<'a> {
    pub fn new(app_id: AppId, name: &'a str, elf: MemoryRange) -> ProcessArgs<'a> {
        ProcessArgs { name, elf, app_id }
    }
}

/// `ProcessInit` describes the values that are passed to the
/// kernel. This value will only be used internally inside
/// `xous-rs`, as well as inside the kernel itself.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ProcessInit {
    // 2,3 -- Text Start, Text Size
    pub elf: MemoryRange,
    pub name_addr: MemoryAddress,
    pub app_id: AppId,
}

impl ProcessInit {
    pub fn free_name_buf(&self) {
        // Free the process name buffer page allocated earlier
        let name_buf_range = unsafe { MemoryRange::new(self.name_addr.get(), 4096).expect("free name buf") };
        crate::unmap_memory(name_buf_range).expect("free name buf");
    }
}

impl From<&ProcessInit> for [usize; 7] {
    fn from(src: &ProcessInit) -> [usize; 7] {
        let app_id_words: [u32; 4] = (&src.app_id).into();
        [
            src.elf.addr.get(),
            src.elf.size.get(),
            src.name_addr.get(),
            app_id_words[0] as _,
            app_id_words[1] as _,
            app_id_words[2] as _,
            app_id_words[3] as _,
        ]
    }
}

impl TryFrom<[usize; 7]> for ProcessInit {
    type Error = crate::Error;

    fn try_from(src: [usize; 7]) -> Result<ProcessInit, Self::Error> {
        let app_id_words = [src[3] as u32, src[4] as u32, src[5] as u32, src[6] as u32];
        Ok(ProcessInit {
            elf: unsafe { MemoryRange::new(src[0], src[1]).or(Err(crate::Error::OutOfMemory))? },
            name_addr: MemoryAddress::new(src[2]).ok_or(crate::Error::OutOfMemory)?,
            app_id: app_id_words.into(),
        })
    }
}

/// When a new process is created, this platform-specific structure is returned.
#[derive(Debug, PartialEq)]
pub struct ProcessStartup {
    /// The process ID of the new process
    pid: crate::PID,
}

impl ProcessStartup {
    pub fn new(pid: crate::PID) -> Self { ProcessStartup { pid } }
}

impl From<&[usize; 7]> for ProcessStartup {
    fn from(src: &[usize; 7]) -> ProcessStartup {
        ProcessStartup { pid: crate::PID::new(src[0] as _).unwrap() }
    }
}

impl From<[usize; 8]> for ProcessStartup {
    fn from(src: [usize; 8]) -> ProcessStartup {
        let pid = match crate::PID::new(src[1] as _) {
            Some(o) => o,
            None => unsafe { crate::PID::new_unchecked(255) },
        };
        ProcessStartup { pid }
    }
}

impl From<&ProcessStartup> for [usize; 7] {
    fn from(src: &ProcessStartup) -> [usize; 7] { [src.pid.get() as _, 0, 0, 0, 0, 0, 0] }
}

pub fn wait_thread<T>(joiner: WaitHandle<T>) -> crate::SysCallResult {
    let call = crate::SysCall::JoinThread(joiner.tid);
    crate::syscall::rsyscall(call)
}

/// Convert the `ProcessArgs` structure passed by the user into a `ProcessInit`
/// structure suitable for consumption by the kernel.
pub fn create_process_pre(args: &ProcessArgs) -> core::result::Result<ProcessInit, crate::Error> {
    // Allocate a page for the buffer of the process name, it will get freed after the process is created
    let name_bytes = args.name.as_bytes();
    if name_bytes.len() > MAX_PROCESS_NAME_LEN {
        return Err(crate::Error::InvalidString);
    }
    let mut name_buf = crate::map_memory(None, None, 4096, crate::MemoryFlags::W)?;
    let name_buf = name_buf.as_slice_mut();
    name_buf[..name_bytes.len()].copy_from_slice(name_bytes);
    Ok(ProcessInit {
        elf: args.elf,
        name_addr: MemoryAddress::new(name_buf.as_ptr() as usize).ok_or(crate::Error::BadAddress)?,
        app_id: args.app_id,
    })
}

/// Any post-processing required to set up this process.
pub fn create_process_post(
    _args: ProcessArgs,
    _init: ProcessInit,
    startup: ProcessStartup,
) -> core::result::Result<(crate::PID, ProcessHandle), crate::Error> {
    Ok((startup.pid, ()))
}

pub struct WaitHandle<T> {
    #[allow(dead_code)]
    tid: TID,
    data: core::marker::PhantomData<T>,
}

pub fn wait_process(_joiner: crate::arch::ProcessHandle) -> SysCallResult {
    todo!();
}

pub fn create_thread_pre<F, T>(_f: &F) -> core::result::Result<ThreadInit, crate::Error>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    todo!()
}

pub fn create_thread_post<F, T>(_f: F, _thread_id: TID) -> core::result::Result<WaitHandle<T>, crate::Error>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    todo!()
}

pub fn create_thread_0_pre<U>(f: &fn() -> U) -> core::result::Result<ThreadInit, crate::Error>
where
    U: Send + 'static,
{
    let start = *f as usize;
    create_thread_n_pre(start, &0, &0, &0, &0)
}

pub fn create_thread_0_post<U>(
    f: fn() -> U,
    thread_id: TID,
) -> core::result::Result<WaitHandle<U>, crate::Error>
where
    U: Send + 'static,
{
    let start = f as usize;
    create_thread_n_post(start, 0, 0, 0, 0, thread_id)
}

pub fn create_thread_1_pre<U>(
    f: &fn(usize) -> U,
    arg1: &usize,
) -> core::result::Result<ThreadInit, crate::Error>
where
    U: Send + 'static,
{
    let start = *f as usize;
    create_thread_n_pre(start, arg1, &0, &0, &0)
}

pub fn create_thread_1_post<U>(
    f: fn(usize) -> U,
    arg1: usize,
    thread_id: TID,
) -> core::result::Result<WaitHandle<U>, crate::Error>
where
    U: Send + 'static,
{
    let start = f as usize;
    create_thread_n_post(start, arg1, 0, 0, 0, thread_id)
}

pub fn create_thread_2_pre<U>(
    f: &fn(usize, usize) -> U,
    arg1: &usize,
    arg2: &usize,
) -> core::result::Result<ThreadInit, crate::Error>
where
    U: Send + 'static,
{
    let start = *f as usize;
    create_thread_n_pre(start, arg1, arg2, &0, &0)
}

pub fn create_thread_2_post<U>(
    f: fn(usize, usize) -> U,
    arg1: usize,
    arg2: usize,
    thread_id: TID,
) -> core::result::Result<WaitHandle<U>, crate::Error>
where
    U: Send + 'static,
{
    let start = f as usize;
    create_thread_n_post(start, arg1, arg2, 0, 0, thread_id)
}

pub fn create_thread_3_pre<U>(
    f: &fn(usize, usize, usize) -> U,
    arg1: &usize,
    arg2: &usize,
    arg3: &usize,
) -> core::result::Result<ThreadInit, crate::Error>
where
    U: Send + 'static,
{
    let start = *f as usize;
    create_thread_n_pre(start, arg1, arg2, arg3, &0)
}

pub fn create_thread_3_post<U>(
    f: fn(usize, usize, usize) -> U,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    thread_id: TID,
) -> core::result::Result<WaitHandle<U>, crate::Error>
where
    U: Send + 'static,
{
    let start = f as usize;
    create_thread_n_post(start, arg1, arg2, arg3, 0, thread_id)
}

pub fn create_thread_4_pre<U>(
    f: &fn(usize, usize, usize, usize) -> U,
    arg1: &usize,
    arg2: &usize,
    arg3: &usize,
    arg4: &usize,
) -> core::result::Result<ThreadInit, crate::Error>
where
    U: Send + 'static,
{
    let start = *f as usize;
    create_thread_n_pre(start, arg1, arg2, arg3, arg4)
}

pub fn create_thread_4_post<U>(
    f: fn(usize, usize, usize, usize) -> U,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    thread_id: TID,
) -> core::result::Result<WaitHandle<U>, crate::Error>
where
    U: Send + 'static,
{
    let start = f as usize;
    create_thread_n_post(start, arg1, arg2, arg3, arg4, thread_id)
}

pub fn create_thread_simple_pre<T, U>(
    f: &fn(T) -> U,
    arg: &T,
) -> core::result::Result<ThreadInit, crate::Error>
where
    T: Send + 'static,
    U: Send + 'static,
{
    create_thread_n_pre(*f as usize, unsafe { core::mem::transmute::<&T, &usize>(arg) }, &0, &0, &0)
}

pub fn create_thread_simple_post<T, U>(
    f: fn(T) -> U,
    arg: T,
    thread_id: TID,
) -> core::result::Result<WaitHandle<U>, crate::Error>
where
    T: Send + 'static,
    U: Send + 'static,
{
    create_thread_n_post(
        f as usize,
        unsafe { core::mem::transmute::<&T, usize>(&arg) },
        0,
        0,
        0,
        thread_id,
    )
    // If we succeeded, the variable will be moved into the caller. Drop it from here.
    .map(|f| {
        core::mem::forget(arg);
        f
    })
}

pub fn create_thread_n_pre(
    start: usize,
    arg1: &usize,
    arg2: &usize,
    arg3: &usize,
    arg4: &usize,
) -> core::result::Result<ThreadInit, crate::Error> {
    let stack = crate::map_memory(None, None, 131_072, crate::MemoryFlags::W)?;
    Ok(ThreadInit::new(start, stack, *arg1, *arg2, *arg3, *arg4))
}

pub fn create_thread_n_post<U>(
    _f: usize,
    _arg1: usize,
    _arg2: usize,
    _arg3: usize,
    _arg4: usize,
    thread_id: TID,
) -> core::result::Result<WaitHandle<U>, crate::Error>
where
    U: Send + 'static,
{
    Ok(WaitHandle { tid: thread_id, data: core::marker::PhantomData })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThreadInit {
    /// Function pointer that accepts 0-4 arguments
    pub call: usize,
    pub stack: MemoryRange,
    pub arg1: usize,
    pub arg2: usize,
    pub arg3: usize,
    pub arg4: usize,
}

impl ThreadInit {
    pub fn new(call: usize, stack: MemoryRange, arg1: usize, arg2: usize, arg3: usize, arg4: usize) -> Self {
        ThreadInit { call, stack, arg1, arg2, arg3, arg4 }
    }
}

impl Default for ThreadInit {
    fn default() -> Self {
        ThreadInit {
            call: 1,
            stack: unsafe { MemoryRange::new(4, 4).unwrap() },
            arg1: 0,
            arg2: 0,
            arg3: 0,
            arg4: 0,
        }
    }
}

/// This code is executed inside the kernel. It takes the list of args
/// that were passed via registers and converts them into a `ThreadInit`
/// struct with enough information to start the new thread.
pub fn args_to_thread(
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
    a7: usize,
) -> core::result::Result<ThreadInit, crate::Error> {
    if a2 & (PAGE_SIZE - 1) != 0 || a3 & (PAGE_SIZE - 1) != 0 {
        return Err(crate::Error::BadAlignment);
    }
    Ok(ThreadInit {
        call: a1,
        stack: unsafe { MemoryRange::new(a2, a3).map_err(|_| crate::Error::InvalidSyscall) }?,
        arg1: a4,
        arg2: a5,
        arg3: a6,
        arg4: a7,
    })
}

pub fn thread_to_args(syscall: usize, init: &ThreadInit) -> [usize; 8] {
    [
        syscall,
        init.call,
        init.stack.as_ptr() as _,
        init.stack.len(),
        init.arg1,
        init.arg2,
        init.arg3,
        init.arg4,
    ]
}
