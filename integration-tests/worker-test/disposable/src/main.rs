// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{xous, BlockingScalarHandler, MessageDef, MessageId, Server, ServerContext, ServerMessages};
use worker_test_disposable::{HoldDisposableScalar, ScalarEcho, ShutdownDisposable};

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    let sid = server::create_sid("test/disposable");
    let mut server = DisposableServer;
    let mut context = server::ServerContext::from_raw_sid(sid);
    loop {
        let msg = xous::receive_message(sid).unwrap();
        match msg.id() {
            ScalarEcho::ID => {
                server::handle_blocking_scalar_message::<ScalarEcho, _>(&mut server, msg, &mut context)
            }
            HoldDisposableScalar::ID => {
                // use raw xous api to avoid the `server` crate's default response on drop
                log::info!("holding disposable scalar request");
            }
            ShutdownDisposable::ID => break,
            id => log::warn!("spurious disposable server message {id}"),
        }
    }
    xous::destroy_server(sid).unwrap();
}

struct DisposableServer;

impl ServerMessages for DisposableServer {
    const NAME: &'static str = "";

    fn messages() -> &'static [MessageDef<Self>] { &[] }
}

impl Server for DisposableServer {}

impl BlockingScalarHandler<ScalarEcho> for DisposableServer {
    fn handle(&mut self, msg: ScalarEcho, _: xous::PID, _: &mut ServerContext<Self>) -> u32 { msg.0 }
}
