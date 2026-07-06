use std::collections::HashMap;
use std::convert::TryInto;
use std::io::{Read, Write};
use std::mem::size_of;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Condvar, LazyLock, Mutex};

use crate::{Result, SysCall, SysCallResult, TID};

const MEMORY_LIMIT: usize = 1024 * 1024 * 1024;
const MEMORY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

mod mem;
pub use mem::*;

mod threading;
pub use threading::*;

mod process;
pub use process::*;

pub mod irq {
    #[derive(Debug, Clone, Copy)]
    pub enum IrqNumber {}
    impl TryFrom<usize> for IrqNumber {
        type Error = crate::Error;

        fn try_from(_value: usize) -> Result<Self, Self::Error> { unimplemented!() }
    }
}

static NETWORK_CONNECT_ADDRESS: LazyLock<SocketAddr> = LazyLock::new(|| {
    std::env::var("XOUS_SERVER")
        .map(|s| {
            s.to_socket_addrs()
                .expect("invalid server address")
                .next()
                .expect("unable to resolve server address")
        })
        .unwrap_or_else(|_| SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0))
});

pub(crate) static PROCESS_KEY: LazyLock<crate::AppId> = LazyLock::new(|| {
    // piggy back a seed initialization on top of the PROCESS_KEY initialization.
    let seed = match std::env::var("XOUS_SEED") {
        Ok(s) => s.parse::<u64>().unwrap_or(0u64),
        Err(_) => 0u64,
    };
    crate::TESTING_RNG_SEED.store(seed, core::sync::atomic::Ordering::SeqCst);

    // resume with PROCESS_KEY initialization
    std::env::var("XOUS_PROCESS_KEY").map(|s| s.as_str().into()).unwrap_or(crate::AppId([0u8; 16]))
});

static SERVER_CONNECTION: LazyLock<ServerConnection> = LazyLock::new(|| {
    spawn_memory_monitor();

    // By the time we get our process' key, it should not be zero
    assert_ne!(&PROCESS_KEY.0, &[0u8; 16]);

    // Note: &* is required due to how `LazyLock` works behind the scenes:
    // https://doc.rust-lang.org/beta/std/sync/struct.LazyLock.html
    let mut conn = TcpStream::connect(&*NETWORK_CONNECT_ADDRESS).expect("unable to connect to Xous kernel");

    // Disable Nagel's algorithm, since we're running locally and managing buffers ourselves
    conn.set_nodelay(true).unwrap();

    // Send key to authenticate us as a known process
    conn.write_all(&PROCESS_KEY.0).unwrap();
    conn.flush().unwrap();

    // Read the 8-bit process ID and verify it matches what we were told.
    let mut pid = [0u8];
    conn.read_exact(&mut pid).unwrap();
    assert_eq!(pid[0], PROCESS_ID.get(), "process ID mismatch");

    let call_mem_tracker = Arc::new(Mutex::new(HashMap::new()));
    // let call_tracker = Arc::new(Mutex::new(HashMap::new()));
    // let response_tracker = Arc::new(Mutex::new(HashMap::new()));
    let mailbox = Arc::new((Mutex::new(HashMap::new()), Condvar::new()));

    let mut reader_conn = conn.try_clone().unwrap();
    let reader_call_mem_tracker = call_mem_tracker.clone();
    // let reader_call_tracker = call_tracker.clone();
    // let reader_response_tracker = response_tracker.clone();
    let reader_mailbox = mailbox.clone();
    let _network_watcher = std::thread::spawn(move || {
        loop {
            let (tid, response) = read_next_syscall_result(&mut reader_conn, &reader_call_mem_tracker);
            // assert!(reader_call_tracker.lock().unwrap().remove(&tid).is_some());
            // assert!(reader_response_tracker.lock().unwrap().insert(tid, ()).is_none());
            let (lock, cv) = &*reader_mailbox;
            let existing = lock.lock().unwrap().insert(tid, response);
            assert!(existing.is_none(), "got two responses for the same thread");
            cv.notify_all();
        }
    });

    ServerConnection {
        send: Arc::new(Mutex::new(conn)),
        mailbox,
        call_mem_tracker,
        // call_tracker,
        // response_tracker,
    }
});

/// Spawn a thread that terminates this process once its resident set passes
/// `MEMORY_LIMIT`, so a hosted leak takes down only the leaker instead of
/// starving the host.
pub fn spawn_memory_monitor() {
    std::thread::Builder::new()
        .name("memory monitor".to_owned())
        .spawn(|| loop {
            if let Some(usage) = memory_stats::memory_stats() {
                if usage.physical_mem > MEMORY_LIMIT {
                    eprintln!(
                        "memory monitor: process {} RSS {} bytes exceeded {} -- exiting",
                        std::process::id(),
                        usage.physical_mem,
                        MEMORY_LIMIT
                    );
                    // Exit, not abort, which dumps core on many systems.
                    std::process::exit(101);
                }
            }
            std::thread::sleep(MEMORY_POLL_INTERVAL);
        })
        .expect("couldn't spawn memory monitor");
}

/// The ID of the current process
static PROCESS_ID: LazyLock<PID> = LazyLock::new(|| {
    use std::str::FromStr;
    PID::from_str(&std::env::var("XOUS_PID").expect("missing environment variable XOUS_PID"))
        .expect("XOUS_PID environment variable was not valid")
});

/// The network address to connect to when making a kernel call
pub static CHILD_PROCESS_ADDRESS: LazyLock<Arc<Mutex<SocketAddr>>> =
    LazyLock::new(|| Arc::new(Mutex::new(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0))));

pub fn set_xous_address(new_address: SocketAddr) { *CHILD_PROCESS_ADDRESS.lock().unwrap() = new_address; }

pub fn set_thread_id(new_tid: TID) { THREAD_ID.with(|tid| *tid.borrow_mut() = Some(new_tid)); }

#[derive(PartialEq)]
enum CallMemoryKind {
    Borrow,
    MutableBorrow,
    Move,
    BlockingMove,
    ReturnMemory,
}

#[derive(Clone)]
struct ServerConnection {
    send: Arc<Mutex<TcpStream>>,
    mailbox: Arc<(Mutex<HashMap<TID, Result>>, Condvar)>,
    call_mem_tracker: Arc<Mutex<HashMap<TID, (crate::MemoryRange, CallMemoryKind)>>>,
    // call_tracker: Arc<Mutex<HashMap<TID, ()>>>,
    // response_tracker: Arc<Mutex<HashMap<TID, ()>>>,
}

/// Perform a synchronous syscall to the kernel.
pub fn syscall(call: SysCall) -> SysCallResult {
    if PROCESS_KEY.0 == [0u8; 16] {
        return Err(crate::Error::ServerNotFound);
    }

    let tid = thread_id();

    // If this call has memory attached to it, save that memory information
    // in a call tracker. That way we'll know how much data to read when
    // the kernel response comes back.
    if let Some(range) = call.memory() {
        let kind = if call.is_borrow() {
            CallMemoryKind::Borrow
        } else if call.is_mutableborrow() {
            CallMemoryKind::MutableBorrow
        } else if call.is_move() {
            CallMemoryKind::Move
        } else if call.is_blocking_move() {
            CallMemoryKind::BlockingMove
        } else if call.is_return_memory() {
            CallMemoryKind::ReturnMemory
        } else {
            panic!("call had memory, but was unrecognized")
        };

        SERVER_CONNECTION.call_mem_tracker.lock().unwrap().insert(tid, (range, kind));
    }

    // let start_time = std::time::Instant::now();
    loop {
        // assert!(SERVER_CONNECTION
        //     .call_tracker
        //     .lock()
        //     .unwrap()
        //     .insert(tid, ())
        //     .is_none());
        send_syscall(&call);

        let result = match read_syscall_result(tid) {
            Result::Error(e) => Some(Err(e)),
            Result::RetryCall => None,
            other => Some(Ok(other)),
        };
        // assert!(SERVER_CONNECTION
        //     .response_tracker
        //     .lock()
        //     .unwrap()
        //     .remove(&tid)
        //     .is_some());
        if let Some(val) = result {
            // println!("[{:2}:{:2}] Syscall took {:7} usec: {:x?}", PROCESS_ID.get(), tid,
            // start_time.elapsed().as_micros(), call);
            return val;
        }

        // If the syscall would block, give it 5ms and retry the call.
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn read_next_syscall_result(
    stream: &mut TcpStream,
    call_mem_tracker: &Arc<Mutex<HashMap<TID, (crate::MemoryRange, CallMemoryKind)>>>,
) -> (TID, Result) {
    // This thread_id doesn't exist in the mailbox, so read additional data.
    let mut pkt = [0usize; 8];
    let mut raw_bytes = [0u8; size_of::<usize>() * 9];
    if let Err(e) = stream.read_exact(&mut raw_bytes) {
        eprintln!("Server shut down: {}", e);
        std::process::exit(0);
    }

    let mut raw_bytes_chunks = raw_bytes.chunks(size_of::<usize>());

    // Read the Thread ID, which comes across first, followed by the 8 words of
    // the message data.
    let msg_thread_id = TID::from_le_bytes(raw_bytes_chunks.next().unwrap().try_into().unwrap());
    for (pkt_word, word) in pkt.iter_mut().zip(raw_bytes_chunks) {
        *pkt_word = usize::from_le_bytes(word.try_into().unwrap());
    }

    // Reconstitute the response from the args.
    let mut response = Result::from_args(pkt);

    // The syscall MAY contain memory, however the call was unsuccessful and should
    // be retried. Examples of this are SendMessage when there is no server available
    // to handle the message.
    if response == Result::RetryCall {
        return (msg_thread_id, response);
    }

    // If control flow got here, then we have a valid syscall.

    // If the client is passing us memory, read bytes from the data, then
    // remap the addresses to function in our address space.
    if let Result::MessageEnvelope(msg) = &mut response {
        match &mut msg.body {
            crate::Message::Move(ref mut memory_message)
            | crate::Message::Borrow(ref mut memory_message)
            | crate::Message::MutableBorrow(ref mut memory_message)
            | crate::Message::BlockingMove(ref mut memory_message) => {
                memory_message.buf =
                    mem::alloc_message_buf(memory_message.buf.len()).expect("couldn't allocate range");
                if let Err(e) = stream.read_exact(memory_message.buf.as_slice_mut()) {
                    eprintln!("Server shut down: {}", e);
                    std::process::exit(0);
                }
            }
            _ => (),
        }
    }

    // If the original call contained memory, then the server will send a copy of the
    // buffer back to us. Ensure the memory we get back is correct.
    if let Some((mem, kind)) = call_mem_tracker.lock().unwrap().remove(&msg_thread_id) {
        if response == Result::RetryCall {
        } else if kind == CallMemoryKind::BlockingMove {
            if let Result::MemoryReturned(ref mut range, _, _) = response {
                // The server moved a (possibly resized) buffer back; take it and free
                // the one we sent.
                let mut buf = mem::alloc_message_buf(range.len()).expect("couldn't allocate range");
                if let Err(e) = stream.read_exact(buf.as_slice_mut()) {
                    eprintln!("Server shut down: {}", e);
                    std::process::exit(0);
                }
                *range = buf;
                mem::free_message_buf(mem).unwrap();
            }
            // Otherwise the server died mid-call: our buffer never moved, so keep it for
            // the Buffer to drop. The kernel sends no bytes back, so read nothing.
        } else if (kind == CallMemoryKind::Borrow || kind == CallMemoryKind::MutableBorrow)
            && matches!(&response, Result::MemoryReturned(..))
        {
            // Read the buffer back from the remote host. On an error the kernel sends no
            // bytes (the server is gone), so there is nothing to read.
            use core::slice;
            let mut data = unsafe { slice::from_raw_parts_mut(mem.as_mut_ptr(), mem.len()) };

            // If it's a Borrow, verify the contents haven't changed by saving the previous
            // buffer in a Vec called `previous_data`.
            let previous_data = match kind {
                CallMemoryKind::Borrow => Some(data.to_vec()),
                _ => None,
            };

            // Read the incoming data from the network.
            if let Err(e) = stream.read_exact(&mut data) {
                eprintln!("Server shut down: {}", e);
                std::process::exit(0);
            }

            // If it is an immutable borrow, verify the contents haven't changed somehow
            if let Some(previous_data) = previous_data {
                if data != previous_data.as_slice() {
                    println!("Data: {:x?}", data);
                    println!("Previous data: {:x?}", previous_data);
                    panic!("Data changed during borrow!");
                }
                // assert_eq!(
                //     data,
                //     previous_data.as_slice(),
                //     "Data changed. Was: {:x?}, Now: {:x?}",
                //     data,
                //     previous_data
                // );
            }

            // The returned range is not valid here, so hand back the lent buffer.
            if let Result::MemoryReturned(ref mut range, _, _) = response {
                *range = mem;
            }
        }

        if kind == CallMemoryKind::Move {
            // In a hosted environment, the message contents are leaked when
            // it gets converted into a MemoryMessage. Now that the call is
            // complete, free the memory.
            mem::free_message_buf(mem).unwrap();
        }

        // If we're returning memory to the Server, then free it here
        if kind == CallMemoryKind::ReturnMemory {
            mem::free_message_buf(mem).unwrap();
        }
    }
    (msg_thread_id, response)
}

/// Read a response from the kernel
fn read_syscall_result(thread_id: TID) -> Result {
    // Check to see if this thread id has an entry in the mailbox already.
    // This will block until the hashmap is free.
    let (lock, cv) = &*SERVER_CONNECTION.mailbox;
    let mut mailbox = lock.lock().unwrap();
    loop {
        if let Some(entry) = mailbox.remove(&thread_id) {
            return entry;
        }

        mailbox = cv.wait(mailbox).unwrap();
    }
}

fn send_syscall(call: &crate::SysCall) {
    let tid = thread_id();

    send_syscall_from_tid(call, tid)
}

fn send_syscall_from_tid(call: &crate::SysCall, tid: TID) {
    let args = call.as_args();

    // Send the packet to the server
    let mut capacity = args.len() * core::mem::size_of::<usize>() + core::mem::size_of_val(&tid);

    // If there's memory attached to this packet, add that on to the
    // number of bytes.
    if let Some(mem) = call.memory() {
        capacity += mem.len();
    }

    let mut pkt = Vec::with_capacity(capacity);

    // 1. Add in the Thread ID
    pkt.extend_from_slice(&tid.to_le_bytes());

    // 2. Add in each of the args
    for word in &args {
        pkt.extend_from_slice(&word.to_le_bytes());
    }

    // 3. (Optional) add in memory data, if present
    if let Some(memory) = call.memory() {
        use core::slice;
        // Unsafety: As long as `memory` is a valid pointer, this is safe.
        // The call to `MemoryRange::new()` is unsafe, and requires that
        // the memory pointed to is always valid.
        let data: &[u8] = unsafe { slice::from_raw_parts(memory.as_ptr(), memory.len()) };
        pkt.extend_from_slice(data);
    }

    let mut stream = SERVER_CONNECTION.send.lock().unwrap();
    if let Err(e) = stream.write_all(&pkt) {
        eprintln!("Server shut down: {}", e);
        std::process::exit(0);
    }
}
