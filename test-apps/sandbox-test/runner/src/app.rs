// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use sandbox_test_worker::{RUNNER_SID, TESTS};

// Cargo doesn't seem to set any useful env vars for us, so we include the file with a hardcoded path.
const WORKER_ELF: &[u8] =
    include_bytes!("../../../../target/armv7a-unknown-xous-elf/release/sandbox-test-worker.strip");

pub fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    xous::set_thread_priority(xous::ThreadPriority::AppHigh0).unwrap();

    let cth_server = xous::create_server().unwrap();
    let rendezvous = xous::create_server_with_sid(RUNNER_SID, 0..1).unwrap();

    xous::register_system_event_handler(xous::SystemEvent::ChildTerminated, cth_server, 0).unwrap();

    // Wait for the system to stabilize
    std::thread::sleep(std::time::Duration::from_millis(500));

    for (step, test) in TESTS.iter().enumerate() {
        let mut memory =
            xous::map_memory(None, None, WORKER_ELF.len().next_multiple_of(0x1000), xous::MemoryFlags::W)
                .unwrap();

        memory.as_slice_mut()[..WORKER_ELF.len()].copy_from_slice(WORKER_ELF);
        let args = xous::ProcessArgs::new([1, 2, 3, 4].into(), "sandbox-test-worker", memory);

        println!();
        println!("=== {} ===", test.name);
        xous::create_process(args).unwrap();
        let announcement = xous::receive_message(rendezvous).unwrap();
        let sid = announcement.body.scalar_message().expect("worker announces its server as a scalar");
        let worker_sid =
            xous::SID::from_u32(sid.arg1 as u32, sid.arg2 as u32, sid.arg3 as u32, sid.arg4 as u32);
        let cid = xous::connect(worker_sid).unwrap();
        xous::send_message(
            cid,
            xous::Message::Scalar(xous::ScalarMessage { id: step, ..Default::default() }),
        )
        .unwrap();

        (test.runner_fn)(cid);

        xous::disconnect(cid).unwrap();

        let termination_msg = xous::receive_message(cth_server).unwrap();
        let crashed = termination_msg.body.scalar_message().unwrap().arg1 != 0;
        if crashed {
            if !test.crash {
                println!("=== FAIL: '{}' crashed when it shouldn't have ===", test.name);
                return;
            }
        } else if test.crash {
            println!("=== FAIL: '{}' did not crash when it should have ===", test.name);
            return;
        }
    }

    println!();
    println!("=== SUCCESS ===");
}
