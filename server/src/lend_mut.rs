// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::{Server, ServerContext};

/// A [`LendMut`] message handler.
pub trait LendMutHandler<M: LendMut>
where
    Self: Server,
{
    fn handle(&mut self, msg: M, sender: xous::PID, context: &mut ServerContext<Self>) -> M::Response;
}

/// A message which is simply some mutably borrowed memory.
pub trait LendMut: crate::MessageId + From<SimpleMemoryMessage> + Into<SimpleMemoryMessage> {
    type Response: LendMutResponse;
}

pub trait LendMutResponse {
    fn from_usize_pair(arg1: usize, arg2: usize) -> Self;
    fn to_usize_pair(self) -> (usize, usize);
}

impl LendMutResponse for () {
    fn from_usize_pair(_arg1: usize, _arg2: usize) {}

    fn to_usize_pair(self) -> (usize, usize) { (0, 0) }
}

impl LendMutResponse for usize {
    fn from_usize_pair(arg1: usize, _arg2: usize) -> usize { arg1 }

    fn to_usize_pair(self) -> (usize, usize) { (self, 0) }
}

impl<T1: From<usize> + Into<usize>, T2: From<usize> + Into<usize>> LendMutResponse for Result<T1, T2> {
    fn from_usize_pair(arg1: usize, arg2: usize) -> Self {
        if arg1 == 0 {
            Ok(arg2.into())
        } else {
            Err(arg2.into())
        }
    }

    fn to_usize_pair(self) -> (usize, usize) {
        match self {
            Ok(o) => (0, o.into()),
            Err(e) => (1, e.into()),
        }
    }
}

/// An easier to use version of [`xous::MemoryMessage`]
pub struct SimpleMemoryMessage {
    /// The offset of the buffer.  This address will get transformed when the
    /// message is moved between processes.
    pub buf: xous::MemoryRange,
    pub arg1: usize,
    pub arg2: usize,
}

impl From<&xous::MemoryMessage> for SimpleMemoryMessage {
    fn from(value: &xous::MemoryMessage) -> Self {
        Self {
            buf: value.buf,
            arg1: value.offset.map(|v| v.get()).unwrap_or_default(),
            arg2: value.valid.map(|v| v.get()).unwrap_or_default(),
        }
    }
}

/// Message handler, used by ServerMessages::messages()
pub fn handle_lend_mut<M: LendMut, S: LendMutHandler<M>>(
    handler: &mut S,
    mut raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) {
    let xous::Message::MutableBorrow(mem) = &mut raw.body else {
        log::warn!("invalid message: {raw:?}");
        return;
    };

    let msg = M::from(SimpleMemoryMessage::from(&*mem));
    let (arg1, arg2) = handler.handle(msg, raw.sender.pid().unwrap(), context).to_usize_pair();
    mem.offset = xous::MemoryAddress::new(arg1);
    mem.valid = xous::MemoryAddress::new(arg2);
}

/// Send a [`LendMut`] message.
pub fn lend_mut<M: LendMut>(cid: xous::CID, msg: M) -> M::Response {
    try_lend_mut(cid, msg).expect("lend_mut: send_message failed")
}

/// Send a [`LendMut`] message, returning the transport error instead of panicking
/// when it cannot be delivered (e.g. the server is gone).
pub fn try_lend_mut<M: LendMut>(cid: xous::CID, msg: M) -> Result<M::Response, xous::Error> {
    let msg: SimpleMemoryMessage = msg.into();
    let result = xous::send_message(
        cid,
        xous::Message::MutableBorrow(xous::MemoryMessage {
            id: M::ID,
            buf: msg.buf,
            offset: xous::MemoryAddress::new(msg.arg1),
            valid: xous::MemoryAddress::new(msg.arg2),
        }),
    )?;
    match result {
        xous::Result::MemoryReturned(_range, arg1, arg2) => Ok(M::Response::from_usize_pair(
            arg1.map(|v| v.get()).unwrap_or_default(),
            arg2.map(|v| v.get()).unwrap_or_default(),
        )),
        other => panic!("Unexpected return from send_message: {other:?}"),
    }
}

/// The `MutableBorrow` payload of an incoming message, for reading then replying in place.
#[inline]
pub(crate) fn borrow_mut(
    raw: &mut xous::MessageEnvelope,
) -> core::result::Result<&mut xous::MemoryMessage, rkyv::rancor::Error> {
    match &mut raw.body {
        xous::Message::MutableBorrow(mem) => Ok(mem),
        _ => rkyv::rancor::fail!(crate::WrongMessageTypeError),
    }
}
