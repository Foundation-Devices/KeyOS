// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use xous::MessageSender;

use crate::{LendMut, LendMutResponse, Server, ServerContext, SimpleMemoryMessage};
/// An [`DeferredLendMut`] message handler that can defer the response
pub trait DeferredLendMutHandler<M>
where
    M: LendMut,
    Self: Server,
{
    fn handle(&mut self, msg: DeferredLendMut<M>, context: &mut ServerContext<Self>);
    fn default_response() -> M::Response;
}

/// An encapsulated blocking [`LendMut`] message, which can be responded to
/// later during execution. It can be stored, and will return the message when
/// dropped, or the [`DeferredLendMut::respond`] function is called.
#[derive(Debug)]
pub struct DeferredLendMut<M: LendMut> {
    sender: MessageSender,
    body: Option<M>,
    response: Option<M::Response>,
}

impl<M: LendMut> DeferredLendMut<M> {
    pub(crate) fn new(mut envelope: xous::MessageEnvelope, response: M::Response) -> Option<Self> {
        let sender = envelope.sender;
        let body = if let xous::Message::MutableBorrow(mem) = &mut envelope.body {
            M::from(SimpleMemoryMessage::from(&*mem))
        } else {
            log::warn!("invalid message: {envelope:?}");
            return None;
        };
        // In some feature combinations envelope does not do anything when dropped.
        #[allow(clippy::forget_non_drop)]
        core::mem::forget(envelope);
        Some(Self { sender, body: Some(body), response: Some(response) })
    }

    /// Returns the PID of the sender
    pub fn pid(&self) -> xous::PID { self.sender.pid().unwrap() }

    /// Get a reference to the parsed message body.
    pub fn body(&self) -> &M { self.body.as_ref().unwrap() }

    /// Get a mutable reference to the parsed message body.
    pub fn body_mut(&mut self) -> &mut M { self.body.as_mut().unwrap() }

    /// Get a mutable reference to the parsed message body.
    pub fn set_response(&mut self, response: M::Response) { self.response = Some(response) }
}

impl<M: LendMut> Drop for DeferredLendMut<M> {
    fn drop(&mut self) {
        let simple: SimpleMemoryMessage = self.body.take().unwrap().into();
        let (arg1, arg2) = self.response.take().unwrap().to_usize_pair();
        xous::syscall::return_memory_offset_valid(
            self.sender,
            simple.buf,
            xous::MemoryAddress::new(arg1),
            xous::MemoryAddress::new(arg2),
        )
        .expect("couldn't return memory")
    }
}

/// Message handler, used by ServerMessages::messages()
pub fn handle_deferred_lend_mut<M, S>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) where
    M: LendMut + 'static,
    S: Server + DeferredLendMutHandler<M>,
{
    let Some(deferred) = DeferredLendMut::new(raw, S::default_response()) else { return };
    handler.handle(deferred, context);
}
