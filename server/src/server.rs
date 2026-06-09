// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::{MessageDef, MessageHandler};

/// A server.
///
/// The KeyOS operating system uses the `xous` kernel, which is a message-passing
/// microkernel. The messages are passed between "servers," which represent communication
/// endpoints.
///
/// This trait, and it's companion, ServerMessages, provide an idiomatic way for KeyOS to implement the
/// servers that it needs.
/// This trait contains optional hooks, but can be left empty, the ServerMessages trait contains the actual
/// server definition.
pub trait Server: ServerMessages {
    /// Hook to do stuff with the context once the server is started.
    fn on_start(&mut self, context: &mut ServerContext<Self>) { let _ = context; }
}

/// This trait contains the actual message definitions that the server can handle.
/// It should not be used manually, but via the `#[derive(server::Server)]` macro.
pub trait ServerMessages: Sized {
    /// The name of the server, as registered in the nameserver.
    /// If empty, a random SID will be assigned and no nameserver registration happens.
    const NAME: &'static str;

    /// Define the messages that are handled by this server. See [`MessageDef`] and the
    /// related functions.
    fn messages() -> &'static [MessageDef<Self>];
}

/// Handle representing the running server instance
#[derive(Debug)]
pub struct ServerContext<S> {
    pub sid: xous::SID,
    pub shutdown: bool,
    // Vec instead of Map, because for a small number of message handlers, a linear
    // search is way faster.
    pub(crate) handlers: Vec<(xous::MessageId, MessageHandler<S>)>,
}

impl<S> ServerContext<S> {
    /// Shut down the server.
    pub fn shutdown(&mut self) { self.shutdown = true; }

    pub fn sid(&self) -> xous::SID { self.sid }

    /// Create a context from a manually created server. Use carefully.
    pub fn from_raw_sid(sid: xous::SID) -> Self { Self { sid, shutdown: false, handlers: Vec::new() } }
}

impl<S: Server> ServerContext<S> {
    pub(crate) fn remove_handler(&mut self, msg_id: xous::MessageId) {
        let idx = self.handlers.iter().position(|(id, _)| *id == msg_id).unwrap();
        self.handlers.swap_remove(idx);
    }
}

/// Start a server in a background thread and open a connection to it.
pub fn listen_and_connect<S: Server + Send + 'static>(mut server: S, pid: xous::PID) -> xous::CID {
    let sid = create_sid(S::NAME);
    std::thread::spawn(move || main_loop(&mut server, sid));
    xous::connect_for_process(pid, sid).unwrap()
}

/// Start the main loop of the server, where messages will be handled as they come in.
pub fn listen<S: Server + 'static>(mut server: S) {
    let sid = create_sid(S::NAME);
    main_loop(&mut server, sid);
}

pub fn listen_with<S: Server + 'static>(make_server: impl FnOnce(xous::SID) -> S) {
    let sid = create_sid(S::NAME);
    let mut server = make_server(sid);
    main_loop(&mut server, sid);
}

fn main_loop<S: Server + 'static>(server: &mut S, sid: xous::SID) {
    // Create a lookup table for message handlers. The size is arbitrary and can be changed at
    // any point.
    let mut lut = [None; 128];
    for (id, handle) in S::messages() {
        if *id > lut.len() {
            panic!("message ID too large, either change the ID or increase LUT size");
        }
        if lut[*id].is_some() {
            panic!("message ID {id} registered twice.");
        }
        lut[*id] = Some(handle);
    }
    let lut = lut;
    let mut context = ServerContext::<S>::from_raw_sid(sid);
    server.on_start(&mut context);
    while !context.shutdown {
        let msg = xous::receive_message(sid).unwrap();
        match lut.get(msg.id()) {
            Some(Some(handle)) => {
                handle(server, msg, &mut context);
            }
            Some(None) => {
                log::error!("Unexpected message for \"{}\" with message ID {}", S::NAME, msg.id())
            }
            None => {
                if let Some((_, handler)) = context.handlers.iter().find(|s| s.0 == msg.id()) {
                    handler(server, msg, &mut context);
                } else {
                    log::warn!("spurious event message for \"{}\": ID={}", S::NAME, msg.id());
                }
            }
        }
    }
    xous::destroy_server(sid).unwrap();
}

pub fn create_sid(name: &str) -> xous::SID {
    let sid = xous::create_server().unwrap();
    if name.is_empty() {
        log::info!("Starting anonymous server with sid={sid:?}");
    } else {
        log::info!("Starting server name={name:?}");
        // Register the server with the xous names server.
        let names = xous_names::XousNames::new().unwrap();
        names.register_name(sid, name).unwrap();
        // Keep this connection open so xous-names keeps the registration alive.
        std::mem::forget(names);
    }
    sid
}
