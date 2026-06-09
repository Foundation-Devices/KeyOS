// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::AtomicBool;

use server::{
    BlockingScalar, BlockingScalarHandler, CheckedConn, LendMutHandler, ScalarHandler, Server,
    SimpleMemoryMessage,
};
use xous::{unmap_memory, yield_slice, MemoryFlags, MemoryRange};

#[cfg(keyos)]
power_manager::use_api!();

#[allow(dead_code)]
#[repr(align(16))]
struct U8_16([u8; 16]);

pub fn main() -> () {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    // Let the system stabilize
    std::thread::sleep(std::time::Duration::from_secs(1));

    // Make sure we aren't doing any DMAs in the background
    #[cfg(keyos)]
    {
        let power_manager = PowerManagerApi::default();
        power_manager.disable_peripheral(atsama5d27::pmc::PeripheralId::Lcdc).unwrap();
        power_manager.disable_peripheral(atsama5d27::pmc::PeripheralId::Sdmmc0).unwrap();
    }

    measure_memory_speed();

    measure_syscall_speed();

    measure_context_switches();

    measure_message_ipc();

    measure_sleep_accuracy();

    log::info!("== Done ==");
}

fn allocate(size: usize, flags: MemoryFlags) -> MemoryRange {
    let mut range = xous::map_memory(None, None, size, flags).unwrap();
    // Needed to actually allocate the pages
    range.as_slice_mut().fill(0);
    range
}

fn measure_memory_speed() {
    fn mem_measurement<const C: usize>(src: *const u8, dst: *mut u8, reps: usize, name: &str) {
        let start = std::time::Instant::now();
        for _ in 0..(reps / 16) {
            unsafe {
                // This gets auto-unrolled by the compiler
                for _ in 0..16 {
                    std::ptr::copy_nonoverlapping(src as *const [U8_16; C], dst as *mut [U8_16; C], 1);
                }
            }
        }
        let elapsed = start.elapsed().as_millis();
        let mbps = (C as f64 * 16.0 * reps as f64) / (1024.0 * 1024.0) / (elapsed as f64 / 1000.0);
        let label = format!("repeated {name} memcpy({} bytes)", C * 16);
        log::info!("{label:<40}: {mbps:.2} MB/s");
    }

    log::info!("== Memory bandwidth ==");

    let normal_src = allocate(1024 * 1024, MemoryFlags::W | MemoryFlags::PLAINTEXT);
    let normal_dst = allocate(1024 * 1024, MemoryFlags::W | MemoryFlags::PLAINTEXT);

    let uncached_src = allocate(1024 * 1024, MemoryFlags::W | MemoryFlags::DEV | MemoryFlags::NO_CACHE);
    let uncached_dst = allocate(1024 * 1024, MemoryFlags::W | MemoryFlags::DEV | MemoryFlags::NO_CACHE);
    let encrypted_src = allocate(1024 * 1024, MemoryFlags::W);
    let encrypted_dst = allocate(1024 * 1024, MemoryFlags::W);

    const ONE_K: usize = 1024 / 16;
    const SIXTY_FOUR_K: usize = ONE_K * 64;
    const ONE_M: usize = ONE_K * 1024;

    mem_measurement::<ONE_M>(normal_src.as_ptr(), normal_dst.as_mut_ptr(), 8 * 16, "normal");
    mem_measurement::<ONE_M>(uncached_src.as_ptr(), uncached_dst.as_mut_ptr(), 16, "uncached");
    mem_measurement::<ONE_M>(encrypted_src.as_ptr(), encrypted_dst.as_mut_ptr(), 16, "encrypted");
    mem_measurement::<ONE_M>(normal_src.as_ptr(), encrypted_dst.as_mut_ptr(), 16, "norm2encrypted");
    mem_measurement::<ONE_M>(encrypted_src.as_ptr(), normal_dst.as_mut_ptr(), 16, "encrypted2norm");

    mem_measurement::<1>(normal_src.as_ptr(), normal_dst.as_mut_ptr(), 10000000, "normal");
    mem_measurement::<2>(normal_src.as_ptr(), normal_dst.as_mut_ptr(), 10000000, "normal");
    mem_measurement::<4>(normal_src.as_ptr(), normal_dst.as_mut_ptr(), 10000000, "normal");
    mem_measurement::<SIXTY_FOUR_K>(normal_src.as_ptr(), normal_dst.as_mut_ptr(), 10000, "normal");

    mem_measurement::<1>(encrypted_src.as_ptr(), encrypted_dst.as_mut_ptr(), 10000000, "encrypted");
    mem_measurement::<2>(encrypted_src.as_ptr(), encrypted_dst.as_mut_ptr(), 10000000, "encrypted");
    mem_measurement::<4>(encrypted_src.as_ptr(), encrypted_dst.as_mut_ptr(), 10000000, "encrypted");
    mem_measurement::<SIXTY_FOUR_K>(encrypted_src.as_ptr(), encrypted_dst.as_mut_ptr(), 10000, "encrypted");

    mem_measurement::<1>(encrypted_src.as_ptr(), normal_dst.as_mut_ptr(), 10000000, "encrypted2norm");
    mem_measurement::<2>(encrypted_src.as_ptr(), normal_dst.as_mut_ptr(), 10000000, "encrypted2norm");
    mem_measurement::<4>(encrypted_src.as_ptr(), normal_dst.as_mut_ptr(), 10000000, "encrypted2norm");
    mem_measurement::<SIXTY_FOUR_K>(encrypted_src.as_ptr(), normal_dst.as_mut_ptr(), 10000, "encrypted2norm");

    mem_measurement::<1>(normal_src.as_ptr(), encrypted_dst.as_mut_ptr(), 10000000, "norm2encrypted");
    mem_measurement::<2>(normal_src.as_ptr(), encrypted_dst.as_mut_ptr(), 10000000, "norm2encrypted");
    mem_measurement::<4>(normal_src.as_ptr(), encrypted_dst.as_mut_ptr(), 10000000, "norm2encrypted");
    mem_measurement::<SIXTY_FOUR_K>(normal_src.as_ptr(), encrypted_dst.as_mut_ptr(), 10000, "norm2encrypted");

    unmap_memory(normal_src).unwrap();
    unmap_memory(normal_dst).unwrap();
    unmap_memory(uncached_src).unwrap();
    unmap_memory(uncached_dst).unwrap();
    unmap_memory(encrypted_src).unwrap();
    unmap_memory(encrypted_dst).unwrap();
}

fn measure(f: impl Fn(), reps: usize, name: &str) {
    let start = std::time::Instant::now();
    for _ in 0..reps {
        #[allow(clippy::unit_arg)]
        std::hint::black_box(f());
    }
    let elapsed = start.elapsed().as_millis();
    let f_per_sec = ((reps as f64) / (elapsed as f64 / 1000.0)) as usize;
    log::info!("{name}: {f_per_sec}/s");
}

fn measure_syscall_speed() {
    log::info!("== Syscall speed ==");

    measure(
        || {
            xous::current_pid().unwrap();
        },
        400000,
        "get_pid                               ",
    );

    let start = std::time::Instant::now();
    measure(
        || {
            let _ = start.elapsed().as_millis();
        },
        15000,
        "Instant::elapsed()                    ",
    );
    measure(
        || {
            let mut buf = [0; 1024];
            getrandom::getrandom(&mut buf).ok();
        },
        100,
        "getrandom(1 kB)                       ",
    );

    measure(
        || {
            let range = xous::map_memory(None, None, 0x1000, MemoryFlags::W).unwrap();
            xous::unmap_memory(range).unwrap();
        },
        100000,
        "map(1 page) + unmap                   ",
    );
    measure(
        || {
            let range = xous::map_memory(None, None, 128 * 0x1000, MemoryFlags::W).unwrap();
            xous::unmap_memory(range).unwrap();
        },
        5000,
        "map(128 pages) + unmap                ",
    );

    measure(
        || {
            let mut range = xous::map_memory(None, None, 0x1000, MemoryFlags::W).unwrap();
            for b in range.as_slice_mut::<u8>().iter_mut().skip(0x1000) {
                *b = 1;
            }
            xous::unmap_memory(range).unwrap();
        },
        100000,
        "map(1 page) + W + unmap               ",
    );
    measure(
        || {
            let mut range = xous::map_memory(None, None, 128 * 0x1000, MemoryFlags::W).unwrap();
            for b in range.as_slice_mut::<u8>().iter_mut().skip(0x1000) {
                *b = 1;
            }
            xous::unmap_memory(range).unwrap();
        },
        200,
        "map(128 pages) + W + unmap            ",
    );

    measure(
        || {
            let range =
                xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W | xous::MemoryFlags::POPULATE)
                    .unwrap();
            xous::unmap_memory(range).unwrap();
        },
        20000,
        "map(1 page) + populate + unmap        ",
    );
    measure(
        || {
            let range = xous::map_memory(
                None,
                None,
                128 * 0x1000,
                xous::MemoryFlags::W | xous::MemoryFlags::POPULATE,
            )
            .unwrap();
            xous::unmap_memory(range).unwrap();
        },
        200,
        "map(128 pages) + populate + unmap     ",
    );

    measure(
        || {
            let mut range =
                xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W | xous::MemoryFlags::POPULATE)
                    .unwrap();
            for b in range.as_slice_mut::<u8>().iter_mut().skip(0x1000) {
                *b = 1;
            }
            xous::unmap_memory(range).unwrap();
        },
        20000,
        "map(1 page) + populate + W + unmap    ",
    );
    measure(
        || {
            let mut range = xous::map_memory(
                None,
                None,
                128 * 0x1000,
                xous::MemoryFlags::W | xous::MemoryFlags::POPULATE,
            )
            .unwrap();
            for b in range.as_slice_mut::<u8>().iter_mut().skip(0x1000) {
                *b = 1;
            }
            xous::unmap_memory(range).unwrap();
        },
        200,
        "map(128 pages) + populate + W + unmap ",
    );
}

#[derive(server::Server)]
#[name = "test/benchmark"]
struct TestServer;

#[derive(Debug, Default, Clone, server::Permissions)]
#[server_name = "test/benchmark"]
#[all_permissions]
struct TestServerPermissions;

#[derive(Debug, server::Message)]
#[response(())]
struct Ping;
#[derive(Debug, server::Message)]
struct AsyncPing;
#[derive(Debug, server::Message)]
#[response(())]
struct MemPing(pub MemoryRange);
#[derive(Debug, server::Message)]
struct Shutdown;

impl From<SimpleMemoryMessage> for MemPing {
    fn from(value: SimpleMemoryMessage) -> Self { Self(value.buf) }
}

impl From<MemPing> for SimpleMemoryMessage {
    fn from(value: MemPing) -> Self { SimpleMemoryMessage { buf: value.0, arg1: 0, arg2: 0 } }
}

impl Server for TestServer {}

impl BlockingScalarHandler<Ping> for TestServer {
    fn handle(
        &mut self,
        _msg: Ping,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <Ping as BlockingScalar>::Response {
    }
}

impl ScalarHandler<AsyncPing> for TestServer {
    fn handle(&mut self, _msg: AsyncPing, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {}
}

impl LendMutHandler<MemPing> for TestServer {
    fn handle(&mut self, mut msg: MemPing, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        for b in msg.0.as_slice_mut::<u8>().iter_mut().step_by(0x1000) {
            *b += 1;
        }
    }
}

impl ScalarHandler<Shutdown> for TestServer {
    fn handle(&mut self, _msg: Shutdown, _sender: xous::PID, context: &mut server::ServerContext<Self>) {
        context.shutdown();
    }
}

fn measure_message_ipc() {
    let conn: CheckedConn<TestServerPermissions> =
        server::listen_and_connect(TestServer, xous::current_pid().unwrap()).into();
    log::info!("== IPC ==");
    let buf_1 = allocate(0x1000, MemoryFlags::W);
    let buf_128 = allocate(128 * 0x1000, MemoryFlags::W);
    let buf_512 = allocate(512 * 0x1000, MemoryFlags::W);

    measure(|| conn.try_send_blocking_scalar(Ping).unwrap(), 100000, "Blocking scalar    ");
    measure(
        || {
            conn.try_send_scalar(AsyncPing).unwrap();
        },
        100000,
        "Non-blocking scalar    ",
    );
    measure(
        || {
            conn.lend_mut(MemPing(buf_1));
        },
        100000,
        "LendMut(1 page)    ",
    );
    measure(
        || {
            conn.lend_mut(MemPing(buf_128));
        },
        2000,
        "LendMut(128 pages) ",
    );
    measure(
        || {
            conn.lend_mut(MemPing(buf_512));
        },
        1000,
        "LendMut(512 pages) ",
    );

    conn.try_send_scalar(Shutdown).unwrap();
    unmap_memory(buf_1).unwrap();
    unmap_memory(buf_128).unwrap();
    unmap_memory(buf_512).unwrap();
}

fn measure_sleep_accuracy() {
    log::info!("== Sleep accuracy ==");
    for ms in &[100, 50, 10, 5, 2] {
        let iterations = 500 / ms;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            std::thread::sleep(std::time::Duration::from_millis(*ms));
        }
        let interval = start.elapsed().as_secs_f32() * 1000.0 / (iterations as f32);
        log::info!("Sleep({ms}) average actual sleep: {interval:.2}ms");
    }
}

fn measure_context_switches() {
    log::info!("== Context switch speed ==");

    static SHUTDOWN: AtomicBool = AtomicBool::new(false);

    let thread = std::thread::spawn(|| {
        while !SHUTDOWN.load(std::sync::atomic::Ordering::SeqCst) {
            yield_slice();
        }
    });
    measure(|| yield_slice(), 200000, "context switches (back and forth)");
    SHUTDOWN.store(true, std::sync::atomic::Ordering::SeqCst);
    thread.join().ok();
}
