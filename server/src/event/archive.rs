// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{any::type_name, marker::PhantomData};

use rkyv::bytecheck::CheckBytes;
use whence::WhenceExt;

use crate::{ArchiveCodec, Error, EventSubscriptionMessage, Owned, Server, ServerContext};
use crate::{XousDeserializer, XousValidator};

/// Handle for a single event subscriber
pub struct ArchiveEventSubscriber<M>
where
    M: ArchiveCodec,
{
    pid: xous::PID,
    cid: xous::CID,
    msg_id: xous::MessageId,
    cancel_msg_id: xous::MessageId,
    _phantom: PhantomData<M>,
}

impl<M> core::fmt::Debug for ArchiveEventSubscriber<M>
where
    M: ArchiveCodec,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveEventSubscriber").field("pid", &self.pid).finish()
    }
}

impl<M> ArchiveEventSubscriber<M>
where
    M: ArchiveCodec,
{
    /// Send the event to the subscriber.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn send(&self, msg: &M) -> Result<xous::Result, xous::Error> {
        crate::Buffer::into_buf(msg)
            .map_err(|_| xous::Error::InternalError)?
            .send(self.cid, self.msg_id as u32)
    }

    /// Send the event to the subscriber.
    pub fn send_nowait(&self, msg: &M) -> Result<xous::Result, xous::Error> {
        crate::Buffer::into_buf(msg)
            .map_err(|_| xous::Error::InternalError)?
            .send_nowait(self.cid, self.msg_id as u32)
    }

    pub fn pid(&self) -> xous::PID { self.pid }

    pub fn cid(&self) -> xous::CID { self.cid }
}

impl<M> Drop for ArchiveEventSubscriber<M>
where
    M: ArchiveCodec,
{
    fn drop(&mut self) {
        if let Err(e) =
            xous::send_message(self.cid, super::cancellation_message(self.msg_id, self.cancel_msg_id))
        {
            log::debug!("Error sending cancellation message {self:?}: {e:?}")
        }
        if let Err(e) = xous::disconnect(self.cid) {
            log::error!("Error disconnecting {self:?}: {e:?}")
        }
    }
}

/// A message which can be serialized and deserialized using rkyv.
pub trait ArchiveEvent: ArchiveCodec
where
    <Self as rkyv::Archive>::Archived: for<'a> CheckBytes<XousValidator<'a>>,
{
}

impl<M> ArchiveEvent for M
where
    M: ArchiveCodec,
    <M as rkyv::Archive>::Archived: for<'a> CheckBytes<XousValidator<'a>>,
{
}

pub trait ArchiveSubscription
where
    Self: crate::MessageId + ArchiveCodec,
    <Self::Event as rkyv::Archive>::Archived:
        rkyv::Deserialize<Self::Event, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
    <Self::Error as rkyv::Archive>::Archived:
        rkyv::Deserialize<Self::Error, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    type Event: ArchiveEvent;
    type Error: super::SubscriptionError;
}

pub trait ArchiveEventSubscriptionHandler<M>
where
    Self: Server,
    M: ArchiveSubscription,
{
    fn handle(
        &mut self,
        msg: M,
        subscriber: ArchiveEventSubscriber<M::Event>,
        context: &mut ServerContext<Self>,
    ) -> Result<(), M::Error>;
}

/// Handler for an incoming [`ArchiveEvent`]
pub trait ArchiveEventHandler<M>
where
    Self: Server,
    M: ArchiveEvent,
    <M as rkyv::Archive>::Archived: for<'a> CheckBytes<XousValidator<'a>>,
{
    fn handle(&mut self, msg: Owned<M>, sender: xous::PID, context: &mut ServerContext<Self>);
}

/// Message handler, used by ServerMessages::messages()
pub fn handle_archive_subscription<M, S>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) where
    M: ArchiveSubscription + 'static,
    S: ArchiveEventSubscriptionHandler<M>,
    <M as rkyv::Archive>::Archived:
        rkyv::Deserialize<M, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    let pid = raw.sender.pid().unwrap();
    if let Err(e) = try_handle_archive_subscription(pid, handler, raw, context) {
        log::warn!("archive sub handle error (PID {pid}) for {}: {e}", type_name::<M>());
    }
}

/// Message handler, used by ServerMessages::messages()
fn try_handle_archive_subscription<M, S>(
    pid: xous::PID,
    handler: &mut S,
    mut raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) -> whence::Result<(), Error>
where
    M: ArchiveSubscription + 'static,
    S: ArchiveEventSubscriptionHandler<M>,
    <M as rkyv::Archive>::Archived:
        rkyv::Deserialize<M, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    let mem = crate::lend_mut::borrow_mut(&mut raw).whence()?;
    let msg: EventSubscriptionMessage<M> = crate::Buffer::deserialize(mem).whence()?;
    let res = handler.handle(
        msg.msg,
        ArchiveEventSubscriber::<M::Event> {
            pid,
            msg_id: msg.msg_id,
            cancel_msg_id: msg.cancel_msg_id,
            cid: msg.cid,
            _phantom: PhantomData,
        },
        context,
    );
    crate::Buffer::reply(mem, &res).whence()
}

pub fn decode_archive_event<M>(raw: xous::MessageEnvelope) -> M
where
    M: ArchiveEvent,
    <M as rkyv::Archive>::Archived:
        rkyv::Deserialize<M, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    try_decode_archive_event(raw).unwrap()
}

pub fn try_decode_archive_event<M>(raw: xous::MessageEnvelope) -> whence::Result<M, Error>
where
    M: ArchiveEvent,
    <M as rkyv::Archive>::Archived:
        rkyv::Deserialize<M, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    crate::r#move::decode_move::<M>(&raw).whence()
}

pub(crate) fn archive_event_handler<M, S>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) where
    M: ArchiveEvent + 'static,
    S: ArchiveEventHandler<M>,
    <M as rkyv::Archive>::Archived: for<'a> CheckBytes<XousValidator<'a>>,
{
    if let Err(e) = try_archive_event_handler(handler, raw, context) {
        log::warn!("failed to handle archive event {e:?}")
    }
}

fn try_archive_event_handler<M, S>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) -> whence::Result<(), Error>
where
    M: ArchiveEvent + 'static,
    S: ArchiveEventHandler<M>,
    <M as rkyv::Archive>::Archived: for<'a> CheckBytes<XousValidator<'a>>,
{
    let sender = raw.sender.pid().unwrap();
    let msg = Owned::new_move(raw).whence()?;
    handler.handle(msg, sender, context);
    Ok(())
}

/// Subscribe to an [`ArchiveEvent`] event.
///
/// # Arguments
///
/// * `cid` - The connection ID to the event sending server.
/// * `sid` - The server ID of the event receiving server.
///
/// # Returns
///
/// A tuple containing two unique message IDs (to this process) for the incoming events:
/// - The first ID is for the event message.
/// - The second ID is for the cancellation message.
pub fn subscribe_archive<M>(cid: xous::CID, msg: M, sid: xous::SID) -> Result<(usize, usize), M::Error>
where
    M: ArchiveSubscription + 'static,
{
    try_subscribe_archive(cid, msg, sid).unwrap()
}

pub fn try_subscribe_archive<M>(
    cid: xous::CID,
    msg: M,
    sid: xous::SID,
) -> whence::Result<Result<(usize, usize), M::Error>, Error>
where
    M: ArchiveSubscription + 'static,
{
    let msg_id = crate::next_dynamic_message_id();
    let cancel_msg_id = crate::next_dynamic_message_id();
    let pid = xous::get_remote_pid(cid).whence()?;
    let cid_remote = xous::connect_for_process(pid, sid).whence()?;
    xous::allow_messages_on_connection(pid, cid_remote, msg_id..(cancel_msg_id + 1)).whence()?;
    let msg = EventSubscriptionMessage { cid: cid_remote, msg_id, cancel_msg_id, msg };
    let result = msg.send_archive(cid)?;
    Ok(result.map(|_| (msg_id, cancel_msg_id)))
}

#[derive(Debug)]
pub struct ArchiveSubList<T: ArchiveCodec> {
    inner: Vec<ArchiveEventSubscriber<T>>,
}

impl<T: ArchiveCodec> Default for ArchiveSubList<T> {
    fn default() -> Self { Self { inner: Default::default() } }
}

impl<T: ArchiveCodec> ArchiveSubList<T> {
    pub fn push(&mut self, sub: ArchiveEventSubscriber<T>) { self.inner.push(sub); }

    pub fn send(&mut self, msg: &T) { self.inner.retain(|sub| sub.send(msg).is_ok()) }

    pub fn send_nowait(&mut self, msg: &T) {
        self.inner.retain(|sub| match sub.send_nowait(msg) {
            Ok(_) => true,
            Err(xous::Error::ServerQueueFull) => {
                log::warn!("archive event send_nowait error for pid {} {}", sub.pid(), type_name::<T>());
                true
            }
            Err(_) => false,
        })
    }

    pub fn remove_cid(&mut self, cid: xous::CID) { self.inner.retain(|s| s.cid() != cid) }
}
