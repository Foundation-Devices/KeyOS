// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use sandbox_test_worker::{RUNNER_SID, TESTS};

pub fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    log::info!("Started");

    let server = xous::create_server().unwrap();
    xous::allow_messages_on_server(server, 0..0xff).unwrap();

    // Creating a server at a known address is privileged, so the runner owns the fixed one
    // and we hand it our random address instead.
    let (a0, a1, a2, a3) = server.to_u32();
    let runner = xous::connect(RUNNER_SID).unwrap();
    xous::send_message(
        runner,
        xous::Message::new_scalar(0, a0 as usize, a1 as usize, a2 as usize, a3 as usize),
    )
    .unwrap();

    let step = xous::receive_message(server).unwrap().body.id();
    let test = &TESTS[step];
    log::info!("Executing test: {}", test.name);
    (test.worker_fn)(server);
    log::info!("Exiting");
}
