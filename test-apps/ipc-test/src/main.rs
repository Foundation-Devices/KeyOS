// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::CheckedConn;

#[derive(server::Server)]
#[name = "test/buffer-server"]
struct BufferServer;

impl server::Server for BufferServer {}

#[derive(Debug, Default, Clone, server::Permissions)]
#[server_name = "test/buffer-server"]
#[all_permissions]
struct BufferServerPermissions;

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(StructWithVec)]
struct StructWithVec {
    a: usize,
    v: Vec<u8>,
    b: usize,
}

impl server::BlockingArchiveHandler<StructWithVec> for BufferServer {
    fn handle(
        &mut self,
        mut msg: StructWithVec,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <StructWithVec as server::BlockingArchive>::Response {
        log::info!("Got: {}, {:?} (last: {}), {}", msg.a, &msg.v[0..10], msg.v.last().unwrap(), msg.b);
        msg.a += 1;
        msg.b += 1;
        msg.v.remove(0);
        msg
    }
}

pub fn main() -> () {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    log::info!("Starting");

    let conn: CheckedConn<BufferServerPermissions> =
        server::listen_and_connect(BufferServer, xous::current_pid().unwrap()).into();
    let mut msg = StructWithVec { a: 123, v: (0..16000).map(|a| a as u8).collect(), b: 456 };
    msg.v.push(255);
    let result = conn.send_blocking_archive(msg);

    log::info!(
        "Result: {}, {:?} (last: {}), {}",
        result.a,
        &result.v[0..10],
        result.v.last().unwrap(),
        result.b
    );
}
