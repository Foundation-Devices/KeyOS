// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::any::type_name;

use whence::WhenceExt;

use crate::XousValidator;
use crate::{Error, Owned, Server, ServerContext};

/// A [`Archive`] message handler.
pub trait ArchiveHandler<M>
where
    M: Archive,
    Self: Server,
{
    fn handle(&mut self, msg: Owned<M>, sender: xous::PID, context: &mut ServerContext<Self>);
}

/// A message which can be serialized and deserialized using rkyv, with no response.
pub trait Archive: crate::ArchiveCodec + crate::MessageId + 'static {}

/// Message handler, used by ServerMessages::messages()
pub fn handle_archive_message<M, S>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) where
    M: Archive,
    S: ArchiveHandler<M>,
    <M as rkyv::Archive>::Archived: for<'a> rkyv::bytecheck::CheckBytes<XousValidator<'a>>,
{
    let pid = raw.sender.pid().unwrap();
    if let Err(e) = try_handle_archive_message(pid, handler, raw, context) {
        log::warn!("Archive handle error (PID {pid}) for {}: {e}", type_name::<M>());
    }
}

fn try_handle_archive_message<M, S>(
    pid: xous::PID,
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) -> whence::Result<(), Error>
where
    M: Archive,
    S: ArchiveHandler<M>,
    <M as rkyv::Archive>::Archived: for<'a> rkyv::bytecheck::CheckBytes<XousValidator<'a>>,
{
    let msg = Owned::new_move(raw).whence()?;
    handler.handle(msg, pid, context);
    Ok(())
}

/// Send a [`Archive`] message.
/// Blocks if the queue is full.
/// Cannot be used from an IRQ context.
pub fn send_archive<M>(cid: xous::CID, msg: M) -> Result<(), xous::Error>
where
    M: Archive,
{
    crate::Buffer::into_buf(&msg).map_err(|_| xous::Error::InternalError)?.send(cid, M::ID as u32)?;
    Ok(())
}

/// Try to send a [`Archive`] message.
/// Returns an error if the queue is full
/// Can be used from an IRQ context.
pub fn send_archive_nowait<M>(cid: xous::CID, msg: M) -> Result<(), xous::Error>
where
    M: Archive,
{
    let buf = crate::Buffer::into_buf(&msg).map_err(|_| xous::Error::InternalError)?;
    buf.send_nowait(cid, M::ID as u32)?;
    Ok(())
}
