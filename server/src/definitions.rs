// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use whence::WhenceExt;

use crate::ServerContext;

/// A message that is known to be handled by the server [`S`]. This is the type of
/// the [`scalar_message`], [`archive_message`], and other functions.
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
        let buf = xous_ipc::Buffer::into_buf(&self).whence()?;
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
        let buf = xous_ipc::Buffer::into_buf(&msg_init).whence()?;
        buf.send(cid, T::ID as u32).whence()?;
        Ok(())
    }
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
