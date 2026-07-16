// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! archive message for server IPC

use std::any::type_name;

use rkyv::{
    bytecheck::CheckBytes,
    rancor::{self, Source as _},
};
use whence::WhenceExt;
use xous_ipc::{SizeOfSerializer, XousDeserializer, XousSerializer, XousValidator};

use crate::{utils, AsyncMessageInit, Error, Server, ServerContext, WrongMessageTypeError};

// ==================== core ====================

/// heap allocated message that expects a response
pub trait BlockingArchive
where
    Self: ArchiveCodec,
    Self: crate::MessageId,
    <Self::Response as rkyv::Archive>::Archived:
        rkyv::Deserialize<Self::Response, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    /// response type for this message
    type Response: ArchiveCodec;
}

/// serialization requirements for archive messages
pub trait ArchiveCodec
where
    Self: Sized
        + rkyv::Archive
        + for<'a, 'b> rkyv::Serialize<XousSerializer<'a, 'b>>
        + for<'a> rkyv::Serialize<SizeOfSerializer<'a>>,
    Self::Archived: rkyv::Portable,
{
}

impl<T> ArchiveCodec for T
where
    T: Sized
        + rkyv::Archive
        + for<'a, 'b> rkyv::Serialize<XousSerializer<'a, 'b>>
        + for<'a> rkyv::Serialize<SizeOfSerializer<'a>>,
    T::Archived: rkyv::Portable,
{
}

// ==================== handler traits ====================

/// handle archive messages synchronously
pub trait BlockingArchiveHandler<M>
where
    M: BlockingArchive,
    Self: Server,
{
    /// process message and return response immediately
    fn handle(&mut self, msg: M, sender: xous::PID, context: &mut ServerContext<Self>) -> M::Response;
}

/// handle archive messages asynchronously (can defer response)
pub trait BlockingArchiveAsyncHandler<M>
where
    M: BlockingArchive,
    Self: Server,
{
    /// process message, response can be sent later
    fn handle(&mut self, request: ArchiveRequest<M>, context: &mut ServerContext<Self>);
    /// default response if handler drops without responding
    fn default_response() -> M::Response;
}

// auto-convert sync handlers to async
impl<T, M> BlockingArchiveAsyncHandler<M> for T
where
    M: BlockingArchive,
    T: BlockingArchiveHandler<M>,
{
    fn handle(&mut self, request: ArchiveRequest<M>, context: &mut ServerContext<Self>) {
        let ArchiveRequest { message, response: request } = request;
        let response = <Self as BlockingArchiveHandler<M>>::handle(self, message, request.pid(), context);
        if let Err(e) = request.respond(response) {
            log::warn!("failed to respond archive {e:?}");
        }
    }

    fn default_response() -> <M as BlockingArchive>::Response {
        unreachable!("default value not required in sync handler")
    }
}

/// handle async responses from other servers
pub trait ArchiveResponseHandler<R>
where
    Self: Server,
    R: ArchiveCodec,
{
    /// process received async response
    fn handle_response(&mut self, response: R, sender: xous::PID, context: &mut ServerContext<Self>);
}

// ==================== types ====================

/// archive request with deferred response capability
#[derive(Debug)]
pub struct ArchiveRequest<M: BlockingArchive> {
    pub message: M,
    pub response: ArchiveResponse<M::Response>,
}

/// deferred response that sends default on drop if not used
#[derive(Debug)]
pub struct ArchiveResponse<R: ArchiveCodec> {
    responder: Option<Responder>,
    pid: xous::PID,
    default: fn() -> R,
}

impl<R: ArchiveCodec> ArchiveResponse<R> {
    /// get sender's process ID
    pub fn pid(&self) -> xous::PID { self.pid }

    /// send response
    pub fn respond(mut self, response: R) -> whence::Result<(), Error> {
        let responder = self.responder.take().unwrap();
        responder.respond(&response)
    }

    /// override default response function
    /// will be sent on drop if [`Self::response`] is not called
    pub fn set_response(&mut self, f: fn() -> R) { self.default = f; }
}

// auto-send default response on drop
impl<R: ArchiveCodec> Drop for ArchiveResponse<R> {
    fn drop(&mut self) {
        if let Some(responder) = self.responder.take() {
            let default = (self.default)();
            responder.respond(&default).ok();
        }
    }
}

// ==================== API ====================

/// send archive message and block for response
pub fn send_blocking_archive<M>(cid: xous::CID, msg: M) -> M::Response
where
    M: BlockingArchive,
{
    try_send_blocking_archive(cid, msg).unwrap()
}

/// send archive message, returns error instead of panic
pub fn try_send_blocking_archive<M>(cid: xous::CID, msg: M) -> whence::Result<M::Response, Error>
where
    M: BlockingArchive,
{
    let mut buf = xous_ipc::Buffer::into_buf(&msg).whence()?;
    buf.lend_mut(cid, M::ID as u32).whence()?;
    buf.to_original().whence()
}

/// send archive message reusing existing buffer
pub fn send_blocking_archive_buf<M>(cid: xous::CID, buf: &mut xous_ipc::Buffer, msg: M) -> M::Response
where
    M: BlockingArchive,
{
    try_send_blocking_archive_buf(cid, buf, msg).unwrap()
}

pub fn try_send_blocking_archive_buf<M>(
    cid: xous::CID,
    buf: &mut xous_ipc::Buffer,
    msg: M,
) -> whence::Result<M::Response, Error>
where
    M: BlockingArchive,
{
    buf.replace(&msg).whence()?;
    buf.lend_mut(cid, M::ID as u32).whence()?;
    buf.to_original().whence()
}

/// send archive message without blocking
/// returns the [`xous::MessageId`] used for the reply
pub fn send_blocking_archive_async<M>(cid: xous::CID, msg: M, sid: xous::SID) -> xous::MessageId
where
    M: BlockingArchive,
{
    try_send_blocking_archive_async(cid, msg, sid).unwrap()
}

/// send async archive message, returns error instead of panic
/// returns the [`xous::MessageId`] used for the reply
pub fn try_send_blocking_archive_async<M>(
    cid: xous::CID,
    msg: M,
    sid: xous::SID,
) -> whence::Result<xous::MessageId, Error>
where
    M: BlockingArchive,
{
    let msg_id = crate::next_dynamic_message_id();
    let pid = xous::get_remote_pid(cid).whence()?;
    let cid_remote = xous::connect_for_process(pid, sid).whence()?;
    xous::allow_messages_on_connection(pid, cid_remote, msg_id..(msg_id + 1)).whence()?;
    let msg = AsyncMessageInit { cid: cid_remote, msg_id, msg };
    msg.send_blocking_archive(cid)?;
    Ok(msg_id)
}

/// Message handler, used by ServerMessages::messages()
pub fn handle_blocking_archive_message<M, S>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) where
    M: BlockingArchive,
    S: BlockingArchiveAsyncHandler<M>,
    <M as rkyv::Archive>::Archived:
        rkyv::Deserialize<M, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    let pid = raw.sender.pid().unwrap();
    if let Err(e) = try_handle_blocking_archive_message(handler, raw, context) {
        log::warn!("BlockingArchive handle error (PID {pid}) for {}: {e}", type_name::<M>());
    }
}

fn try_handle_blocking_archive_message<M, S>(
    handler: &mut S,
    mut raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) -> whence::Result<(), Error>
where
    M: BlockingArchive,
    S: BlockingArchiveAsyncHandler<M>,
    <M as rkyv::Archive>::Archived:
        rkyv::Deserialize<M, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    let pid = raw.sender.pid().unwrap();

    match &mut raw.body {
        xous::Message::MutableBorrow(mem) => {
            // sync case - extract message directly (no AsyncMessageInit wrapper)
            let message: M = {
                let buf = unsafe { xous_ipc::Buffer::from_memory_message_mut(mem) };
                buf.to_original::<M>().whence()?
            };
            let request =
                ArchiveResponse { responder: Some(Responder::Sync(raw)), pid, default: S::default_response };
            let request = ArchiveRequest { message, response: request };
            handler.handle(request, context);
            Ok(())
        }
        xous::Message::Move(mem) => {
            // async case - extract async wrapper
            let buf = unsafe { xous_ipc::Buffer::from_memory_message(mem) };
            let init: AsyncMessageInit<M> = buf.to_original().whence()?;
            let request = ArchiveResponse {
                responder: Some(Responder::Async { cid: init.cid, msg_id: init.msg_id }),
                pid,
                default: S::default_response,
            };
            let request = ArchiveRequest { message: init.msg, response: request };
            handler.handle(request, context);
            Ok(())
        }
        _ => Err(rancor::Error::new(WrongMessageTypeError)).whence(),
    }
}
/// decode async response from raw envelope
pub fn decode_archive_async_response<M>(raw: xous::MessageEnvelope) -> M
where
    M: ArchiveCodec,
    <M as rkyv::Archive>::Archived:
        rkyv::Deserialize<M, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    try_decode_archive_async_response(raw).unwrap()
}

pub fn try_decode_archive_async_response<M>(mut raw: xous::MessageEnvelope) -> whence::Result<M, Error>
where
    M: ArchiveCodec,
    <M as rkyv::Archive>::Archived:
        rkyv::Deserialize<M, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
    let buf = utils::extract_move_message(&mut raw).whence()?;
    Ok(buf.to_original::<M>().whence()?)
}

// ==================== internal ====================

// internal: handle async responses
pub(crate) fn archive_async_response_handler<M, S>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) where
    M: BlockingArchive,
    S: ArchiveResponseHandler<M::Response>,
{
    let msg_id = raw.id();
    let sender = raw.sender.pid().unwrap();

    match try_decode_archive_async_response(raw) {
        Ok(response) => {
            handler.handle_response(response, sender, context);
        }
        Err(e) => log::warn!("invalid async message response {e}"),
    }

    context.remove_handler(msg_id);
}

#[derive(Debug)]
enum Responder {
    /// response goes back in same buffer (blocking call)
    Sync(xous::MessageEnvelope),
    /// response sent in new buffer (async call)
    Async { cid: xous::CID, msg_id: xous::MessageId },
}

impl Responder {
    fn respond<R>(self, response: &R) -> whence::Result<(), Error>
    where
        R: ArchiveCodec,
    {
        match self {
            Responder::Sync(mut envelope) => {
                let mut buf = Self::unwrap_buffer(&mut envelope);
                buf.replace(response).whence()?;
                Ok(())
            }
            Responder::Async { cid, msg_id } => {
                let _disconnect = defer::defer(|| {
                    xous::disconnect(cid).ok();
                });
                xous_ipc::Buffer::into_buf(response).whence()?.send_nowait(cid, msg_id as u32).whence()?;
                Ok(())
            }
        }
    }

    fn unwrap_buffer(envelope: &mut xous::MessageEnvelope) -> xous_ipc::Buffer<'_> {
        unsafe { xous_ipc::Buffer::from_memory_message_mut(envelope.body.memory_message_mut().unwrap()) }
    }
}
