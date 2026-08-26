// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use whence::WhenceExt;

use crate::ServerContext;

/// A message that is known to be handled by a server. This is the type returned
/// by the server message registration helpers.
pub type MessageDef<S> = (xous::MessageId, MessageHandler<S>);

pub(crate) type MessageHandler<S> = fn(&mut S, xous::MessageEnvelope, &mut ServerContext<S>);

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct AsyncMessageInit<T> {
    pub cid: xous::CID,
    pub msg_id: xous::MessageId,
    pub msg: T,
}

impl<T> AsyncMessageInit<T>
where
    T: crate::BlockingArchive,
{
    #[inline]
    pub fn send_blocking_archive(self, cid: xous::CID) -> whence::Result<(), crate::Error> {
        let buf = crate::Buffer::into_buf(&self).whence()?;
        buf.send(cid, T::ID as u32).whence()?;
        Ok(())
    }
}

impl<T> AsyncMessageInit<T>
where
    T: crate::BlockingScalar,
{
    #[inline]
    pub fn send_scalar(self, cid: xous::CID) -> whence::Result<(), crate::Error> {
        let msg_init: AsyncMessageInit<[u32; 4]> =
            AsyncMessageInit { cid: self.cid, msg_id: self.msg_id, msg: self.msg.as_scalar() };
        let buf = crate::Buffer::into_buf(&msg_init).whence()?;
        buf.send(cid, T::ID as u32).whence()?;
        Ok(())
    }
}

pub(crate) fn check_caller_cid(cid: xous::CID, sender: xous::PID) -> whence::Result<(), crate::Error> {
    if xous::get_remote_pid(cid) != Ok(sender) {
        return Err(xous::Error::AccessDenied).whence();
    }
    Ok(())
}

pub trait MessageId {
    /// unique message identifier
    const ID: xous::MessageId;
    /// target server name
    const SERVER: &'static str;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WrongMessageTypeError;

impl std::fmt::Display for WrongMessageTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "wrong message type") }
}

impl std::error::Error for WrongMessageTypeError {}
