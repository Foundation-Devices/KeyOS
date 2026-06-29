// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use whence::WhenceExt;

use crate::{Server, ServerContext, SimpleMemoryMessage};

/// A [`Move`] message handler.
pub trait MoveHandler<M: Move>
where
    Self: Server,
{
    const LEAK_MESSAGE: bool;
    fn handle(&mut self, msg: M, sender: xous::PID, context: &mut ServerContext<Self>);
}

/// A message which is simply a to-be-sent memory range
pub trait Move: crate::MessageId + From<SimpleMemoryMessage> + Into<SimpleMemoryMessage> {}

/// Message handler, used by ServerMessages::messages()
pub fn handle_move<M: Move, S: MoveHandler<M>>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) {
    let sender = raw.sender.pid().unwrap();
    let msg = if S::LEAK_MESSAGE {
        let message = raw.take_message();
        let xous::Message::Move(mem) = message else {
            log::warn!("invalid message: {message:?}");
            return;
        };
        M::from(SimpleMemoryMessage::from(&mem))
    } else {
        let xous::Message::Move(mem) = &raw.body else {
            log::warn!("invalid message: {raw:?}");
            return;
        };
        M::from(SimpleMemoryMessage::from(mem))
    };

    handler.handle(msg, sender, context);
}

/// Send a [`Move`] message (panics on failure)
pub fn send_move<M: Move>(cid: xous::CID, msg: M) { try_send_move(cid, msg).unwrap(); }

/// Send a [`Move`] message (fallible)
pub fn try_send_move<M: Move>(cid: xous::CID, msg: M) -> whence::Result<(), xous::Error> {
    let msg: SimpleMemoryMessage = msg.into();
    xous::send_message(
        cid,
        xous::Message::Move(xous::MemoryMessage {
            id: M::ID,
            buf: msg.buf,
            offset: xous::MemoryAddress::new(msg.arg1),
            valid: xous::MemoryAddress::new(msg.arg2),
        }),
    )
    .whence()?;
    Ok(())
}

/// Try sending a [`Move`] message. Does not block if the message queue is full.
/// Can be used in an IRQ handler.
pub fn send_move_nowait<M>(cid: xous::CID, msg: M) -> whence::Result<(), xous::Error>
where
    M: Move,
{
    let msg: SimpleMemoryMessage = msg.into();
    xous::try_send_message(
        cid,
        xous::Message::Move(xous::MemoryMessage {
            id: M::ID,
            buf: msg.buf,
            offset: xous::MemoryAddress::new(msg.arg1),
            valid: xous::MemoryAddress::new(msg.arg2),
        }),
    )
    .whence()?;
    Ok(())
}

/// Deserialize the payload of an incoming `Move` message.
#[inline]
pub(crate) fn decode_move<T>(raw: &xous::MessageEnvelope) -> core::result::Result<T, rkyv::rancor::Error>
where
    T: rkyv::Archive,
    T::Archived: rkyv::Portable
        + for<'a> rkyv::bytecheck::CheckBytes<crate::XousValidator<'a>>
        + rkyv::Deserialize<T, crate::XousDeserializer>,
{
    match &raw.body {
        xous::Message::Move(mem) => crate::Buffer::deserialize(mem),
        _ => rkyv::rancor::fail!(crate::WrongMessageTypeError),
    }
}
