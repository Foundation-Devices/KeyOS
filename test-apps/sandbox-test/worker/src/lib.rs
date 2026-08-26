// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{thread::sleep, time::Duration};

use xous::{MemoryAddress, MemoryFlags, MemoryRange, CID, SID};

pub const RUNNER_SID: SID = SID::from_u32(0x1234, 0x5678, 0x9abc, 0xdef0);
pub const TEST_SID: SID = SID::from_u32(0xa, 0xb, 0xc, 0xd);

pub const TEST_PATTERN: u32 = 0x1337abcd;

pub struct Test {
    pub name: &'static str,
    pub crash: bool,
    pub worker_fn: fn(SID),
    pub runner_fn: fn(CID),
}

#[cfg(keyos)]
pub const TESTS: &[Test] = &[
    Test { name: "Smoke test", worker_fn: |_| {}, runner_fn: |_| {}, crash: false },
    Test { name: "Panic test", worker_fn: |_| panic!("Panic test"), runner_fn: |_| {}, crash: true },
    Test {
        name: "Map memory",
        crash: false,
        worker_fn: |_| {
            check_alloc(None, None);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map and populate",
        crash: false,
        worker_fn: |_| {
            let range = xous::map_memory(None, None, 0x1000, MemoryFlags::W | MemoryFlags::POPULATE).unwrap();
            if range.as_slice::<u32>().iter().any(|d| *d != 0) {
                panic!("Page was not zeroed");
            }
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map SRAM",
        crash: true,
        worker_fn: |_| {
            check_alloc(MemoryAddress::new(0x00200000), None);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map SECURAM",
        crash: true,
        worker_fn: |_| {
            check_alloc(MemoryAddress::new(0xF8044000), None);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map UART0",
        crash: true,
        worker_fn: |_| {
            check_alloc(MemoryAddress::new(0xF801C000), None);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map DDR directly",
        crash: false,
        worker_fn: |_| {
            check_alloc(MemoryAddress::new(0x27000000), None);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map EDDR directly",
        crash: false,
        worker_fn: |_| {
            check_alloc(MemoryAddress::new(0x47001000), None);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map DDR with unmap",
        crash: false,
        worker_fn: |_| {
            let range = check_alloc(MemoryAddress::new(0x27002000), None);
            xous::unmap_memory(range).unwrap();
            // Wait for the zeroer
            sleep(Duration::from_millis(10));
            check_alloc(MemoryAddress::new(0x27002000), None);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map DDR twice",
        crash: true,
        worker_fn: |_| {
            check_alloc(MemoryAddress::new(0x27003000), None);
            check_alloc(MemoryAddress::new(0x27003000), None);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map EDDR twice",
        crash: true,
        worker_fn: |_| {
            check_alloc(MemoryAddress::new(0x47004000), None);
            check_alloc(MemoryAddress::new(0x47004000), None);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map DDR + EDDR to same address",
        crash: true,
        worker_fn: |_| {
            check_alloc(MemoryAddress::new(0x27005000), None);
            xous::map_memory(
                MemoryAddress::new(0x47005000),
                None,
                0x1000,
                MemoryFlags::W | MemoryFlags::PLAINTEXT,
            )
            .unwrap();
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map DDR + EDDR to same address 2",
        crash: true,
        worker_fn: |_| {
            check_alloc(MemoryAddress::new(0x27006000), None);
            check_alloc(MemoryAddress::new(0x47006000), None);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map DDR + EDDR to same address 3",
        crash: true,
        worker_fn: |_| {
            check_alloc(MemoryAddress::new(0x47007000), None);
            check_alloc(MemoryAddress::new(0x27007000), None);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map memory used by other PID",
        crash: true,
        worker_fn: |sid| {
            // Sync up with runner
            xous::receive_message(sid).unwrap();
            check_alloc(MemoryAddress::new(0x27008000), None);
        },
        runner_fn: |cid| {
            let range = check_alloc(MemoryAddress::new(0x27008000), None);
            xous::send_message(cid, xous::Message::Scalar(Default::default())).unwrap();
            sleep(std::time::Duration::from_millis(50));
            xous::unmap_memory(range).unwrap();
        },
    },
    Test {
        name: "Map memory used by other PID with unmap",
        crash: false,
        worker_fn: |sid| {
            // Sync up with runner
            xous::receive_message(sid).unwrap();
            check_alloc(MemoryAddress::new(0x27009000), None);
        },
        runner_fn: |cid| {
            let range = check_alloc(MemoryAddress::new(0x27009000), None);
            xous::unmap_memory(range).unwrap();
            xous::send_message(cid, xous::Message::Scalar(Default::default())).unwrap();
        },
    },
    Test {
        name: "Map to valid virtual address",
        crash: false,
        worker_fn: |_| {
            check_alloc(None, MemoryAddress::new(0x0200a000));
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map to invalid virtual address",
        crash: true,
        worker_fn: |_| {
            check_alloc(None, MemoryAddress::new(0x7000b000));
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map to invalid virtual address 2",
        crash: true,
        worker_fn: |_| {
            check_alloc(None, MemoryAddress::new(0xFF808000));
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Read kernel code",
        crash: true,
        worker_fn: |_| {
            let ptr = 0xFFD00000 as *const usize;
            let val = unsafe { *ptr };
            println!("{val}");
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Read kernel memory",
        crash: true,
        worker_fn: |_| {
            let ptr = 0xFF808000 as *const usize;
            let val = unsafe { *ptr };
            println!("{val}");
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Read thread context",
        crash: true,
        worker_fn: |_| {
            let ptr = 0x70000000 as *const usize;
            let val = unsafe { *ptr };
            println!("{val}");
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Invalidate kernel memory",
        crash: true,
        worker_fn: |_| {
            xous::flush_cache(
                unsafe { MemoryRange::new(0xFF808000, 0x1000).unwrap() },
                xous::CacheOperation::Invalidate,
            )
            .unwrap()
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Invalidate thread context",
        crash: true,
        worker_fn: |_| {
            xous::flush_cache(
                unsafe { MemoryRange::new(0x70000000, 0x1000).unwrap() },
                xous::CacheOperation::Invalidate,
            )
            .unwrap()
        },
        runner_fn: |_| {},
    },
    Test {
        // Just Invalidate may panic the worker just by corrupting the thread context, so the test may notp
        // even fail, but a simple Clean shouln't be allowed either.
        name: "Clean thread context",
        crash: true,
        worker_fn: |_| {
            xous::flush_cache(
                unsafe { MemoryRange::new(0x70000000, 0x1000).unwrap() },
                xous::CacheOperation::Clean,
            )
            .unwrap()
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Crash without returning blocking scalar",
        crash: true,
        worker_fn: |sid| {
            let _envelope = xous::receive_message(sid).unwrap();
            panic!("Whoops");
        },
        runner_fn: |cid| {
            let blocking_result = xous::send_message(cid, xous::Message::BlockingScalar(Default::default()));
            assert!(blocking_result.is_err());
        },
    },
    Test {
        name: "Crash while blocking scalar is served",
        crash: true,
        worker_fn: |_| {
            let worker_cid = xous::connect(TEST_SID).unwrap();
            std::thread::spawn(|| {
                sleep(Duration::from_millis(20));
                panic!("Whoops")
            });
            xous::send_message(worker_cid, xous::Message::BlockingScalar(Default::default())).unwrap();
            sleep(Duration::from_millis(100));
        },
        runner_fn: |_| {
            let runner_sid = xous::create_server_with_sid(TEST_SID, 0..0xff).unwrap();
            {
                let msg = xous::receive_message(runner_sid).unwrap();
                assert!(msg.body.is_scalar() && msg.body.is_blocking());
                sleep(Duration::from_millis(50));
                xous::return_scalar(msg.sender, 0).unwrap();
            }
            xous::destroy_server(runner_sid).unwrap();
        },
    },
    Test {
        name: "Crash before blocking scalar is served",
        crash: true,
        worker_fn: |_| {
            let worker_cid = xous::connect(TEST_SID).unwrap();
            std::thread::spawn(|| {
                sleep(Duration::from_millis(20));
                panic!("Whoops")
            });
            xous::send_message(worker_cid, xous::Message::BlockingScalar(Default::default())).unwrap();
            sleep(Duration::from_millis(100));
        },
        runner_fn: |_| {
            let runner_sid = xous::create_server_with_sid(TEST_SID, 0..0xff).unwrap();
            {
                sleep(Duration::from_millis(50));
                let msg = xous::receive_message(runner_sid).unwrap();
                assert!(msg.body.is_scalar() && msg.body.is_blocking());
                xous::return_scalar(msg.sender, 0).unwrap();
            }
            xous::destroy_server(runner_sid).unwrap();
        },
    },
    Test {
        name: "Write LendMut memory",
        crash: false,
        worker_fn: |sid| {
            let mut envelope = xous::receive_message(sid).unwrap();
            let mem_range = envelope.body.memory_message_mut().unwrap().buf.as_slice_mut::<u32>();
            if mem_range.iter().any(|d| *d != TEST_PATTERN) {
                panic!("Lent page content was wrong")
            }
            mem_range.fill(0x1f2f3f4f);
            // message returned when the envelope goes out of scope.
        },
        runner_fn: |cid| {
            let range = check_alloc(None, None);
            xous::send_message(cid, xous::Message::new_lend_mut(0, range, None, None)).unwrap();
            if range.as_slice::<u32>().iter().any(|d| *d != 0x1f2f3f4f) {
                panic!("Returned page content was wrong");
            }
        },
    },
    Test {
        name: "Write Lend memory",
        crash: true,
        worker_fn: |sid| {
            let mut envelope = xous::receive_message(sid).unwrap();
            let mem_range = match &mut envelope.body {
                xous::Message::Borrow(memory_message) => memory_message.buf.as_slice_mut::<u32>(),
                _ => {
                    println!("Wrong message type");
                    // Fail the test by not crashing
                    return;
                }
            };
            mem_range[0] = 0;
            // message returned when the envelope goes out of scope.
        },
        runner_fn: |cid| {
            let range = check_alloc(None, None);
            xous::send_message(cid, xous::Message::new_lend(0, range, None, None)).ok();
            if range.as_slice::<u32>().iter().any(|d| *d != TEST_PATTERN) {
                panic!("Returned page content was wrong");
            }
        },
    },
    Test {
        name: "Read Lend after return",
        crash: true,
        worker_fn: |sid| {
            let mem_range;
            {
                let envelope = xous::receive_message(sid).unwrap();
                mem_range = envelope.body.memory_message().unwrap().buf;
                println!("Lent page contents while in scope: {}", mem_range.as_slice::<u32>()[0]);
                // message returned when the envelope goes out of scope.
            }
            println!("Lent page contents: {}", mem_range.as_slice::<u32>()[0]);
        },
        runner_fn: |cid| {
            let range = check_alloc(None, None);
            xous::send_message(cid, xous::Message::new_lend(0, range, None, None)).unwrap();
        },
    },
    Test {
        name: "Write LendMut after return",
        crash: true,
        worker_fn: |sid| {
            let mut mem_range;
            {
                let mut envelope = xous::receive_message(sid).unwrap();
                mem_range = envelope.body.memory_message_mut().unwrap().buf;
                println!("Lent page contents while in scope: {}", mem_range.as_slice::<u32>()[0]);
                // message returned when the envelope goes out of scope.
            }
            mem_range.as_slice_mut::<u32>()[0] = 0;
        },
        runner_fn: |cid| {
            let range = check_alloc(None, None);
            xous::send_message(cid, xous::Message::new_lend_mut(0, range, None, None)).unwrap();
        },
    },
    Test {
        name: "Use after send (lend)",
        crash: true,
        worker_fn: |_| {
            let buf = check_alloc(None, None);
            let worker_cid = xous::connect(TEST_SID).unwrap();
            let crashing_thread = xous::create_thread_1(
                |ptr| {
                    let ptr = ptr as *mut u32;
                    for _ in 0..5 {
                        unsafe { *ptr += 1 };
                        println!("Ptr val: {:08x}", unsafe { *ptr });
                        sleep(Duration::from_millis(10));
                    }
                },
                buf.as_ptr() as _,
            )
            .unwrap();
            sleep(Duration::from_millis(20));
            xous::send_message(worker_cid, xous::Message::new_lend(0, buf, None, None)).unwrap();
            xous::wait_thread(crashing_thread).ok();
        },
        runner_fn: |_| {
            let runner_sid = xous::create_server_with_sid(TEST_SID, 0..0xff).unwrap();
            {
                let _msg = xous::receive_message(runner_sid).unwrap();
                sleep(Duration::from_millis(50));
            }
            xous::destroy_server(runner_sid).unwrap();
        },
    },
    Test {
        name: "Use after send (lendmut)",
        crash: true,
        worker_fn: |_| {
            let buf = check_alloc(None, None);
            let worker_cid = xous::connect(TEST_SID).unwrap();
            let crashing_thread = xous::create_thread_1(
                |ptr| {
                    let ptr = ptr as *const u32;
                    for _ in 0..5 {
                        println!("Ptr val: {:08x}", unsafe { *ptr });
                        sleep(Duration::from_millis(10));
                    }
                },
                buf.as_ptr() as _,
            )
            .unwrap();
            sleep(Duration::from_millis(20));
            xous::send_message(worker_cid, xous::Message::new_lend_mut(0, buf, None, None)).unwrap();
            xous::wait_thread(crashing_thread).ok();
        },
        runner_fn: |_| {
            let runner_sid = xous::create_server_with_sid(TEST_SID, 0..0xff).unwrap();
            {
                let _msg = xous::receive_message(runner_sid).unwrap();
                sleep(Duration::from_millis(50));
            }
            xous::destroy_server(runner_sid).unwrap();
        },
    },
    Test {
        name: "Use after send (move)",
        crash: true,
        worker_fn: |_| {
            let buf = check_alloc(None, None);
            let worker_cid = xous::connect(TEST_SID).unwrap();
            let crashing_thread = xous::create_thread_1(
                |ptr| {
                    let ptr = ptr as *const u32;
                    for _ in 0..5 {
                        println!("Ptr val: {:08x}", unsafe { *ptr });
                        sleep(Duration::from_millis(10));
                    }
                },
                buf.as_ptr() as _,
            )
            .unwrap();
            sleep(Duration::from_millis(20));
            xous::send_message(worker_cid, xous::Message::new_move(0, buf, None, None)).unwrap();
            xous::wait_thread(crashing_thread).ok();
        },
        runner_fn: |_| {
            let runner_sid = xous::create_server_with_sid(TEST_SID, 0..0xff).unwrap();
            {
                let _msg = xous::receive_message(runner_sid).unwrap();
                sleep(Duration::from_millis(50));
            }
            xous::destroy_server(runner_sid).unwrap();
        },
    },
    Test {
        name: "Map a lot of memory but don't use all",
        crash: false,
        worker_fn: |_| {
            let mut range = xous::map_memory(None, None, 128 * 1024 * 1024, MemoryFlags::W).unwrap();
            range.as_slice_mut::<u8>()[0] = 1;
            range.as_slice_mut::<u8>()[127 * 1024 * 1024] = 1;
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Use too much memory",
        crash: true,
        worker_fn: |_| {
            let mut range = xous::map_memory(None, None, 128 * 1024 * 1024, MemoryFlags::W).unwrap();
            range.as_slice_mut::<u32>().fill(1);
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Use ok amount of heap",
        crash: false,
        worker_fn: |_| {
            let mut result = Vec::new();
            for _ in 0..15 {
                result.push(vec![0u8; 1024 * 1024]);
            }
            println!("Len: {}", result.len());
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map and immediate invalidate",
        crash: false,
        worker_fn: |_| {
            let range = xous::map_memory(None, None, 0x1000, MemoryFlags::W).unwrap();
            println!("End of range: {}", range.as_slice::<u8>()[0xfff]);
            xous::flush_cache(range, xous::CacheOperation::Invalidate).unwrap();
            for (i, d) in range.as_slice::<u32>().iter().enumerate() {
                if *d != 0 {
                    panic!("Page was not zeroed (@{i:04x}={d:08X})");
                }
            }
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Map, populate and invalidate",
        crash: false,
        worker_fn: |_| {
            let range = xous::map_memory(None, None, 0x1000, MemoryFlags::W | MemoryFlags::POPULATE).unwrap();
            xous::flush_cache(range, xous::CacheOperation::Invalidate).unwrap();
            if range.as_slice::<u32>().iter().any(|d| *d != 0) {
                panic!("Page was not zeroed");
            }
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Repeated connect returns one CID",
        crash: false,
        worker_fn: |sid| {
            let first = xous::connect(sid).unwrap();
            let second = xous::connect(sid).unwrap();
            if first != second {
                panic!("Connecting twice gave {first} and {second}");
            }
            xous::disconnect(first).unwrap();
            xous::disconnect(second).unwrap();
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Disconnected CID is not handed out again",
        crash: false,
        worker_fn: |sid| {
            let first = xous::connect(sid).unwrap();
            xous::disconnect(first).unwrap();
            let second = xous::connect(sid).unwrap();
            if first == second {
                panic!("Disconnected CID {first} was handed out again");
            }
            xous::disconnect(second).unwrap();
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Stale CID is rejected",
        crash: false,
        worker_fn: |sid| {
            let stale = xous::connect(sid).unwrap();
            xous::disconnect(stale).unwrap();
            let live = xous::connect(sid).unwrap();
            let msg = xous::Message::Scalar(xous::ScalarMessage::default());
            if xous::try_send_message(stale, msg).is_ok() {
                panic!("Stale CID {stale} still sent, aliasing {live}");
            }
            xous::disconnect(live).unwrap();
        },
        runner_fn: |_| {},
    },
    Test {
        name: "Forged CID generation is rejected",
        crash: false,
        worker_fn: |sid| {
            let cid = xous::connect(sid).unwrap();
            // The low byte of a CID is the slot, so this keeps the slot and forges the generation.
            let forged = cid ^ 0x100;
            let msg = xous::Message::Scalar(xous::ScalarMessage::default());
            if xous::try_send_message(forged, msg).is_ok() {
                panic!("Forged CID {forged} was accepted for the slot of {cid}");
            }
            xous::disconnect(cid).unwrap();
        },
        runner_fn: |_| {},
    },
    Test { name: "Ending Smoke test", worker_fn: |_| {}, runner_fn: |_| {}, crash: false },
];

fn check_alloc(phys: Option<MemoryAddress>, virt: Option<MemoryAddress>) -> MemoryRange {
    let mut range = xous::map_memory(phys, virt, 0x1000, MemoryFlags::W).unwrap();
    for (i, d) in range.as_slice::<u32>().iter().enumerate() {
        if *d != 0 {
            panic!("Page was not zeroed (@{i:04x}={d:08X})");
        }
    }
    range.as_slice_mut::<u32>().fill(TEST_PATTERN);
    range
}

#[cfg(not(keyos))]
pub const TESTS: &[Test] = &[];
