// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! The same debug protocol on the simulator, over a loopback socket.
//!
//! The hosted build has no USB, so the frames the device sends over bulk endpoints travel over TCP
//! here, delimited by the length prefix in `usb_debug_protocol::stream`. Everything above the
//! transport (command decoding, dispatch, log draining) is what runs on hardware.

use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::time::Duration;

use usb_debug_protocol::{stream, Response};

use crate::dispatch;

/// What the session writer receives: frames to forward, and the end of the session.
enum Msg {
    Frame(Response),
    /// The command thread saw the client leave; the session is over.
    Disconnected,
}

pub fn run() -> ! {
    let addr = stream::sim_addr();
    let (writer_tx, writer_rx) = sync_channel::<Msg>(crate::MAX_PENDING_LOGS);

    std::thread::spawn({
        let writer_tx = writer_tx.clone();
        move || log_drain_thread(writer_tx)
    });

    // A second simulator on the same address keeps running without a debug channel rather than
    // taking the whole system down over a dev-only convenience.
    let listener = match TcpListener::bind(&addr) {
        Ok(listener) => listener,
        Err(e) => {
            log::warn!("usb-debug: no debug channel, cannot listen on {addr}: {e}");
            park()
        }
    };
    log::info!("usb-debug: simulator debug channel on {addr}");

    // Accept runs on its own thread because a newcomer must be refused while this loop is busy
    // serving; accepted here instead, it would complete its handshake in the backlog and time
    // out, looking like an unresponsive device. The rendezvous handoff keeps the channel
    // exclusive, like the USB interface it mirrors: try_send only succeeds while this loop is
    // waiting for a client.
    let (conn_tx, conn_rx) = sync_channel::<TcpStream>(0);
    std::thread::spawn(move || accept_thread(listener, conn_tx));

    loop {
        let client = conn_rx.recv().expect("the accept thread never exits");
        serve(client, &writer_tx, &writer_rx);
    }
}

fn accept_thread(listener: TcpListener, conn_tx: SyncSender<TcpStream>) {
    loop {
        match listener.accept() {
            Ok((client, peer)) => {
                // Dropping the refused stream closes it, so the client fails its next read
                // rather than timing out.
                if conn_tx.try_send(client).is_err() {
                    log::warn!("usb-debug: refusing {peer}, the debug channel already has a client");
                }
            }
            Err(e) => log::warn!("usb-debug: accept failed: {e}"),
        }
    }
}

/// Run one client until it goes away. Serving one at a time matches the hardware transport, where
/// the debug interface is claimed exclusively.
fn serve(client: TcpStream, writer_tx: &SyncSender<Msg>, writer_rx: &Receiver<Msg>) {
    let Ok(reader) = client.try_clone() else {
        log::warn!("usb-debug: could not split the client connection");
        return;
    };

    let command_thread = std::thread::spawn({
        let writer_tx = writer_tx.clone();
        move || command_thread(reader, writer_tx)
    });

    let mut out = &client;
    let disconnected = loop {
        match writer_rx.recv() {
            Ok(Msg::Frame(response)) => {
                let (header, payload) = response.parts();
                if let Err(e) = stream::write_frame(&mut out, header, payload) {
                    log::info!("usb-debug: client write failed, dropping the connection: {e}");
                    break false;
                }
            }
            Ok(Msg::Disconnected) | Err(_) => break true,
        }
    };

    // The command thread blocks reading the socket, so close it before joining. After a write
    // failure, keep draining until its Disconnected arrives, or the join could deadlock against
    // a full channel.
    let _ = client.shutdown(Shutdown::Both);
    if !disconnected {
        while !matches!(writer_rx.recv(), Ok(Msg::Disconnected) | Err(_)) {}
    }
    let _ = command_thread.join();
}

/// Read command frames and dispatch them, exactly as the USB OUT drain does. Ends by reporting the
/// disconnect through the channel, so the writer needs no side signal to notice it.
fn command_thread(mut reader: TcpStream, writer_tx: SyncSender<Msg>) {
    let mut debug = dispatch::DebugProtocol::new();

    loop {
        match stream::read_frame(&mut reader) {
            Ok(Some(frame)) => debug.process(&frame, |response| {
                let _ = writer_tx.send(Msg::Frame(response));
            }),
            Ok(None) => break,
            Err(e) => {
                log::info!("usb-debug: client read failed, dropping the connection: {e}");
                break;
            }
        }
    }

    let _ = writer_tx.send(Msg::Disconnected);
}

/// Forward log records to the writer. Logs keep flowing with no client connected, so the bounded
/// channel throttles this thread instead of the connection.
fn log_drain_thread(writer_tx: SyncSender<Msg>) {
    let log_reader = log_server::reader::LogReader::default();
    let log_buffer =
        xous::map_memory(None, None, 0x4000, xous::MemoryFlags::W).expect("Could not allocate log buffer");

    loop {
        let len = log_reader.read(log_buffer);
        if len > 0
            && writer_tx.send(Msg::Frame(Response::Log(log_buffer.as_slice()[..len].to_vec()))).is_err()
        {
            break;
        }
    }
}

fn park() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
