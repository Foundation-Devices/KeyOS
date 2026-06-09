// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{any::type_name, marker::PhantomData};

use rkyv::bytecheck::CheckBytes;
use whence::WhenceExt;
use xous_ipc::{XousDeserializer, XousValidator};

use crate::{utils, Error, EventSubscriptionMessage, ScalarCodec, Server, ServerContext};

/// Handle for a single event subscriber
pub struct ScalarEventSubscriber<M>
where
    M: ScalarEvent,
{
    pid: xous::PID,
    cid: xous::CID,
    msg_id: xous::MessageId,
    cancel_msg_id: xous::MessageId,
    _phantom: PhantomData<M>,
}

impl<M> core::fmt::Debug for ScalarEventSubscriber<M>
where
    M: ScalarEvent,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScalarEventSubscriber").field("pid", &self.pid).finish()
    }
}

impl<M> ScalarEventSubscriber<M>
where
    M: ScalarEvent,
{
    /// Send the event to the subscriber.
    /// Can be used in an IRQ handler context.
    pub fn send(&self, msg: &M) -> Result<xous::Result, xous::Error> {
        let msg = xous::Message::Scalar(utils::scalar_to_message(msg, self.msg_id));
        xous::try_send_message(self.cid, msg)
    }

    pub fn pid(&self) -> xous::PID { self.pid }

    pub fn cid(&self) -> xous::CID { self.cid }
}

impl<M> Drop for ScalarEventSubscriber<M>
where
    M: ScalarEvent,
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

/// A message which can be serialized and deserialized using scalar encoding.
pub trait ScalarEvent: ScalarCodec {}

impl<M> ScalarEvent for M where M: ScalarCodec {}

pub trait ScalarSubscription
where
    Self: crate::MessageId + crate::ArchiveCodec,
    <Self::Error as rkyv::Archive>::Archived:
        rkyv::Deserialize<Self::Error, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
    <Result<(), Self::Error> as rkyv::Archive>::Archived:
        rkyv::Deserialize<Result<(), Self::Error>, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    type Event: ScalarEvent;
    type Error: super::SubscriptionError;
}

/// A [`ScalarSubscription`] handler.
pub trait ScalarEventSubscriptionHandler<M>
where
    Self: Server,
    M: ScalarSubscription,
{
    /// Handle the subscription.
    ///
    /// The `subscriber` parameter can be used to store the subscriber info and send events to them
    /// later. Once their subscription is not used, the object can be dropped.
    fn handle(
        &mut self,
        msg: M,
        subscriber: ScalarEventSubscriber<M::Event>,
        context: &mut ServerContext<Self>,
    ) -> Result<(), M::Error>;
}

/// Handler for an incoming [`ScalarEvent`]
pub trait ScalarEventHandler<M>
where
    Self: Server,
    M: ScalarEvent,
{
    fn handle(&mut self, msg: M, sender: xous::PID, context: &mut ServerContext<Self>);
}

/// Message handler, used by ServerMessages::messages()
pub fn handle_scalar_subscription<M, S>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) where
    M: ScalarSubscription + 'static,
    S: ScalarEventSubscriptionHandler<M>,
    <M as rkyv::Archive>::Archived:
        rkyv::Deserialize<M, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    let pid = raw.sender.pid().unwrap();
    if let Err(e) = try_handle_scalar_subscription(pid, handler, raw, context) {
        log::warn!("archive sub handle error (PID {pid}) for {}: {e}", type_name::<M>());
    }
}

fn try_handle_scalar_subscription<M, S>(
    pid: xous::PID,
    handler: &mut S,
    mut raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) -> whence::Result<(), Error>
where
    M: ScalarSubscription + 'static,
    S: ScalarEventSubscriptionHandler<M>,
    <M as rkyv::Archive>::Archived:
        rkyv::Deserialize<M, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    let mut buf = utils::extract_borrow_mut_message(&mut raw).whence()?;
    let msg: EventSubscriptionMessage<M> = buf.to_original().whence()?;
    let res = handler.handle(
        msg.msg,
        ScalarEventSubscriber::<M::Event> {
            pid,
            msg_id: msg.msg_id,
            cancel_msg_id: msg.cancel_msg_id,
            cid: msg.cid,
            _phantom: PhantomData,
        },
        context,
    );
    buf.replace(&res).whence()?;
    Ok(())
}

pub fn decode_scalar_event<M>(raw: xous::MessageEnvelope) -> M
where
    M: ScalarEvent,
{
    try_decode_scalar_event(raw).unwrap()
}

pub fn try_decode_scalar_event<M>(mut raw: xous::MessageEnvelope) -> whence::Result<M, crate::Error>
where
    M: ScalarEvent,
{
    let scalar = utils::extract_scalar_message(&mut raw).whence()?;
    Ok(M::from_scalar(scalar))
}

pub(crate) fn scalar_event_handler<M, S>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) where
    M: ScalarEvent,
    S: ScalarEventHandler<M>,
{
    let sender = raw.sender.pid().unwrap();
    let msg = decode_scalar_event::<M>(raw);
    handler.handle(msg, sender, context);
}

/// Subscribe to a [`ScalarEvent`] event.
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
pub fn subscribe_scalar<M>(cid: xous::CID, msg: M, sid: xous::SID) -> Result<(usize, usize), M::Error>
where
    M: ScalarSubscription + 'static,
{
    try_subscribe_scalar(cid, msg, sid).unwrap()
}

pub fn try_subscribe_scalar<M>(
    cid: xous::CID,
    msg: M,
    sid: xous::SID,
) -> whence::Result<Result<(usize, usize), M::Error>, crate::Error>
where
    M: ScalarSubscription + 'static,
{
    let msg_id = crate::next_dynamic_message_id();
    let cancel_msg_id = crate::next_dynamic_message_id();
    let pid = xous::get_remote_pid(cid).whence()?;
    let cid_remote = xous::connect_for_process(pid, sid).whence()?;
    xous::allow_messages_on_connection(pid, cid_remote, msg_id..(cancel_msg_id + 1)).whence()?;
    let msg = EventSubscriptionMessage { cid: cid_remote, msg_id, cancel_msg_id, msg };
    let result = msg.send_scalar(cid)?;
    Ok(result.map(|_| (msg_id, cancel_msg_id)))
}

/// A list of scalar event subscribers.
pub struct ScalarSubList<T: ScalarCodec> {
    inner: Vec<ScalarEventSubscriber<T>>,
}

impl<T: ScalarCodec> Default for ScalarSubList<T> {
    fn default() -> Self { Self { inner: Default::default() } }
}

impl<T: ScalarCodec> ScalarSubList<T> {
    pub fn push(&mut self, sub: ScalarEventSubscriber<T>) { self.inner.push(sub); }

    pub fn send(&mut self, msg: &T) { self.inner.retain(|sub| sub.send(msg).is_ok()) }

    pub fn send_nowait(&mut self, msg: &T) {
        self.inner.retain(|sub| match sub.send(msg) {
            Ok(_) => true,
            Err(xous::Error::ServerQueueFull) => {
                log::warn!("scalar event send_nowait error for pid {} {}", sub.pid(), type_name::<T>());
                true
            }
            Err(_) => false,
        })
    }

    pub fn remove_cid(&mut self, cid: xous::CID) { self.inner.retain(|s| s.cid() != cid) }
}
