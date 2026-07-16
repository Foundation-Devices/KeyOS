// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! scalar message for server IPC

use std::any::type_name;

use rkyv::rancor::{self, Source as _};
use whence::WhenceExt;

use crate::{utils, AsyncMessageInit, Error, Server, ServerContext, WrongMessageTypeError};

// ==================== core ====================

/// stack allocated message that expects a response
pub trait BlockingScalar
where
    Self: ScalarCodec,
    Self: crate::MessageId,
{
    /// response type for this message
    type Response: ScalarCodec;
}

/// encoding requirements for scalar messages
pub trait ScalarCodec
where
    Self: FromScalar<4> + AsScalar<4>,
{
}

impl<T> ScalarCodec for T where T: FromScalar<4> + AsScalar<4> {}

// ==================== handler traits ====================

/// handle scalar messages synchronously
pub trait BlockingScalarHandler<M>
where
    M: BlockingScalar,
    Self: Server,
{
    /// process message and return response immediately
    fn handle(&mut self, msg: M, sender: xous::PID, context: &mut ServerContext<Self>) -> M::Response;
}

/// handle scalar messages asynchronously (can defer response)
pub trait BlockingScalarAsyncHandler<M>
where
    M: BlockingScalar,
    Self: Server,
{
    /// process message, response can be sent later
    fn handle(&mut self, request: BlockingScalarRequest<M>, context: &mut ServerContext<Self>);
    /// default response if handler drops without responding
    fn default_response() -> M::Response;
}

// auto-convert sync handlers to async
impl<T, M> BlockingScalarAsyncHandler<M> for T
where
    M: BlockingScalar,
    T: BlockingScalarHandler<M>,
{
    fn handle(&mut self, request: BlockingScalarRequest<M>, context: &mut ServerContext<Self>) {
        let BlockingScalarRequest { message, response: request } = request;
        let response = <Self as BlockingScalarHandler<M>>::handle(self, message, request.pid(), context);
        if let Err(e) = request.respond(response) {
            log::warn!("failed to respond scalar {e:?}");
        }
    }

    fn default_response() -> <M as BlockingScalar>::Response {
        unreachable!("default value not required in sync handler")
    }
}

/// handle async responses from other servers
pub trait BlockingScalarResponseHandler<R>
where
    Self: Server,
    R: ScalarCodec,
{
    /// process received async response
    fn handle_response(&mut self, response: R, sender: xous::PID, context: &mut ServerContext<Self>);
}

// ==================== types ====================

/// scalar request with deferred response capability
#[derive(Debug)]
pub struct BlockingScalarRequest<M: BlockingScalar> {
    pub message: M,
    pub response: BlockingScalarResponse<M::Response>,
}

/// deferred response that sends default on drop if not used
#[derive(Debug)]
pub struct BlockingScalarResponse<R: ScalarCodec> {
    responder: Option<Responder>,
    pid: xous::PID,
    default: fn() -> R,
    response: Option<R>,
}

impl<R: ScalarCodec> BlockingScalarResponse<R> {
    /// get sender's process ID
    pub fn pid(&self) -> xous::PID { self.pid }

    /// send response
    pub fn respond(mut self, response: R) -> whence::Result<(), xous::Error> {
        let responder = self.responder.take().unwrap();
        responder.respond(response)
    }

    /// set response
    /// will be sent on drop if [`Self::response`] is not called
    pub fn set_response(&mut self, response: R) { self.response = Some(response) }
}

// auto-send default response on drop
impl<R: ScalarCodec> Drop for BlockingScalarResponse<R> {
    fn drop(&mut self) {
        if let Some(responder) = self.responder.take() {
            let default = self.response.take().unwrap_or_else(self.default);
            responder.respond(default).ok();
        }
    }
}

// ==================== API ====================

/// send scalar message and block for response
pub fn send_blocking_scalar<M>(cid: xous::CID, msg: M) -> M::Response
where
    M: BlockingScalar,
{
    try_send_blocking_scalar(cid, msg).unwrap()
}

/// send scalar message, returns error instead of panic
pub fn try_send_blocking_scalar<M>(cid: xous::CID, msg: M) -> whence::Result<M::Response, xous::Error>
where
    M: BlockingScalar,
{
    let msg = xous::Message::BlockingScalar(utils::scalar_to_message(&msg, M::ID));
    let result = xous::send_message(cid, msg).whence()?;
    match result {
        xous::Result::Scalar5(arg1, arg2, arg3, arg4, _) => {
            Ok(M::Response::from_scalar([arg1 as u32, arg2 as u32, arg3 as u32, arg4 as u32]))
        }
        unexpected => {
            log::error!(
                "unexpected result for message {} (ID {}): {:?}",
                type_name::<M>(),
                M::ID,
                unexpected
            );
            Err(xous::Error::InternalError).whence()?
        }
    }
}

/// send scalar message without blocking
/// returns the [`xous::MessageId`] used for the reply
pub fn send_scalar_async<M>(cid: xous::CID, msg: M, sid: xous::SID) -> xous::MessageId
where
    M: BlockingScalar,
{
    try_send_scalar_async(cid, msg, sid).unwrap()
}

/// send async scalar message, returns error instead of panic
/// returns the [`xous::MessageId`] used for the reply
pub fn try_send_scalar_async<M>(
    cid: xous::CID,
    msg: M,
    sid: xous::SID,
) -> whence::Result<xous::MessageId, crate::Error>
where
    M: BlockingScalar,
{
    let msg_id = crate::next_dynamic_message_id();
    let pid = xous::get_remote_pid(cid).whence()?;
    let cid_remote = xous::connect_for_process(pid, sid).whence()?;
    xous::allow_messages_on_connection(pid, cid_remote, msg_id..(msg_id + 1)).whence()?;
    AsyncMessageInit { cid: cid_remote, msg_id, msg }.send_scalar(cid)?;
    Ok(msg_id)
}

/// Message handler, used by ServerMessages::messages()
pub fn handle_blocking_scalar_message<M, S>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) where
    M: BlockingScalar,
    S: BlockingScalarAsyncHandler<M>,
{
    let pid = raw.sender.pid().unwrap();
    if let Err(e) = try_handle_blocking_scalar_message(pid, handler, raw, context) {
        log::warn!("blocking scalar handle error (PID {pid}) for {}: {e}", type_name::<M>());
    }
}

fn try_handle_blocking_scalar_message<M, S>(
    pid: xous::PID,
    handler: &mut S,
    mut raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) -> whence::Result<(), Error>
where
    M: BlockingScalar,
    S: BlockingScalarAsyncHandler<M>,
{
    match &mut raw.body {
        xous::Message::BlockingScalar(scalar) => {
            // sync case - extract message and create request
            let message = utils::scalar_from_message::<M>(scalar);
            let request = BlockingScalarResponse {
                responder: Some(Responder::Sync(raw)),
                pid,
                default: S::default_response,
                response: None,
            };
            let request = BlockingScalarRequest { message, response: request };
            handler.handle(request, context);
            Ok(())
        }
        xous::Message::Move(mem) => {
            // async case - extract async wrapper
            let buf = unsafe { xous_ipc::Buffer::from_memory_message(mem) };
            let init: AsyncMessageInit<[u32; 4]> = buf.to_original().whence()?;
            let AsyncMessageInit { cid, msg_id, msg } = init;
            let request = BlockingScalarResponse {
                responder: Some(Responder::Async { cid, msg_id }),
                pid,
                default: S::default_response,
                response: None,
            };
            let request = BlockingScalarRequest { message: M::from_scalar(msg), response: request };
            handler.handle(request, context);
            Ok(())
        }
        _ => Err(rancor::Error::new(WrongMessageTypeError)).whence(),
    }
}

/// decode async response from raw envelope
pub fn decode_scalar_async_response<R>(raw: xous::MessageEnvelope) -> R
where
    R: ScalarCodec,
{
    try_decode_scalar_async_response(raw).unwrap()
}

/// try to decode async response from raw envelope, returns error instead of panic
pub fn try_decode_scalar_async_response<R>(mut raw: xous::MessageEnvelope) -> whence::Result<R, crate::Error>
where
    R: ScalarCodec,
{
    let scalar = utils::extract_scalar_message(&mut raw).whence()?;
    Ok(R::from_scalar(scalar))
}

// ==================== fire-and-forget ====================

/// stack allocated message with no response
pub trait Scalar: ScalarCodec + crate::MessageId {
    fn to_message(&self) -> xous::ScalarMessage
    where
        Self: Sized,
    {
        utils::scalar_to_message(self, Self::ID)
    }
}

/// handle fire-and-forget scalar messages
pub trait ScalarHandler<M>
where
    M: Scalar,
    Self: Server,
{
    /// process message, no response expected
    fn handle(&mut self, msg: M, sender: xous::PID, context: &mut ServerContext<Self>);
}

/// Send a [`Scalar`] message. Blocks if queues are full.
///
/// Warning: Cannot be used in an IRQ handle
pub fn send_scalar<M>(cid: xous::CID, msg: M)
where
    M: Scalar,
{
    try_send_scalar(cid, msg).unwrap()
}

/// Send a [`Scalar`] message. Blocks if queues are full.
///
/// Warning: Cannot be used in an IRQ handle
pub fn try_send_scalar<M>(cid: xous::CID, msg: M) -> whence::Result<(), xous::Error>
where
    M: Scalar,
{
    let msg = xous::Message::Scalar(msg.to_message());
    xous::send_message(cid, msg).whence()?;
    Ok(())
}

/// Try sending a [`Scalar`] message, return error if the syscall queue is full.
/// Can be used in an IRQ handler.
pub fn send_scalar_nowait<M>(cid: xous::CID, msg: M) -> whence::Result<(), xous::Error>
where
    M: Scalar,
{
    let msg = xous::Message::Scalar(msg.to_message());
    xous::try_send_message(cid, msg).whence()?;
    Ok(())
}

/// Message handler, used by ServerMessages::messages()
pub fn handle_scalar_message<M, S>(
    handler: &mut S,
    mut raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) where
    M: Scalar,
    S: ScalarHandler<M>,
{
    let pid = raw.sender.pid().unwrap();

    match &mut raw.body {
        xous::Message::Scalar(scalar) => {
            let message = utils::scalar_from_message(scalar);
            handler.handle(message, pid, context);
        }
        _ => {
            log::error!("invalid Scalar message {} (ID {}) from PID {pid}: {raw:?}", type_name::<M>(), M::ID,);
        }
    }
}

// ==================== internal ====================

// internal: handle async responses
pub(crate) fn scalar_async_response_handler<M, S>(
    handler: &mut S,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<S>,
) where
    M: BlockingScalar,
    S: BlockingScalarResponseHandler<M::Response>,
{
    let msg_id = raw.id();
    let sender = raw.sender.pid().unwrap();

    match try_decode_scalar_async_response(raw) {
        Ok(response) => {
            handler.handle_response(response, sender, context);
        }
        Err(e) => log::warn!("invalid async scalar response {e}"),
    }

    context.remove_handler(msg_id);
}

#[derive(Debug)]
enum Responder {
    /// response returned via return_scalar5 (blocking call)
    Sync(xous::MessageEnvelope),
    /// response sent as new scalar message (async call)
    Async { cid: xous::CID, msg_id: xous::MessageId },
}

impl Responder {
    fn respond<R>(self, response: R) -> whence::Result<(), xous::Error>
    where
        R: ScalarCodec,
    {
        match self {
            Responder::Sync(envelope) => {
                let [arg1, arg2, arg3, arg4] = response.as_scalar().map(|a| a as usize);
                xous::return_scalar5(envelope.sender, arg1, arg2, arg3, arg4, 0).whence()
            }
            Responder::Async { cid, msg_id } => {
                let _disconnect = defer::defer(|| {
                    xous::disconnect(cid).ok();
                });
                let msg = utils::scalar_to_message(&response, msg_id);
                xous::try_send_message(cid, xous::Message::Scalar(msg)).whence()?;
                Ok(())
            }
        }
    }
}

// ==================== codec ====================

pub use codec::*;

mod codec {
    use xous::MemoryRange;

    // ==================== trait definitions ====================

    pub trait FromScalar<const N: usize> {
        fn from_scalar(value: [u32; N]) -> Self;
    }

    pub trait AsScalar<const N: usize> {
        fn as_scalar(&self) -> [u32; N];
    }

    // ==================== blanket impls ====================

    impl<T: FromScalar<3>> FromScalar<4> for T {
        fn from_scalar(value: [u32; 4]) -> Self { Self::from_scalar([value[0], value[1], value[2]]) }
    }

    impl<T: AsScalar<3>> AsScalar<4> for T {
        fn as_scalar(&self) -> [u32; 4] {
            let s = Self::as_scalar(self);
            [s[0], s[1], s[2], 0]
        }
    }

    impl<T: FromScalar<2>> FromScalar<3> for T {
        fn from_scalar(value: [u32; 3]) -> Self { Self::from_scalar([value[0], value[1]]) }
    }

    impl<T: AsScalar<2>> AsScalar<3> for T {
        fn as_scalar(&self) -> [u32; 3] {
            let s = Self::as_scalar(self);
            [s[0], s[1], 0]
        }
    }

    impl<T: FromScalar<1>> FromScalar<2> for T {
        fn from_scalar(value: [u32; 2]) -> Self { Self::from_scalar([value[0]]) }
    }

    impl<T: AsScalar<1>> AsScalar<2> for T {
        fn as_scalar(&self) -> [u32; 2] {
            let s = Self::as_scalar(self);
            [s[0], 0]
        }
    }

    // ==================== primitive types ====================

    impl FromScalar<1> for () {
        fn from_scalar(_value: [u32; 1]) -> Self {}
    }

    impl AsScalar<1> for () {
        fn as_scalar(&self) -> [u32; 1] { [0] }
    }

    impl FromScalar<1> for usize {
        fn from_scalar(value: [u32; 1]) -> Self { value[0] as usize }
    }

    impl AsScalar<1> for usize {
        fn as_scalar(&self) -> [u32; 1] { [*self as u32] }
    }

    impl FromScalar<1> for i32 {
        fn from_scalar(value: [u32; 1]) -> Self { value[0] as i32 }
    }

    impl AsScalar<1> for i32 {
        fn as_scalar(&self) -> [u32; 1] { [*self as u32] }
    }

    impl FromScalar<1> for u8 {
        fn from_scalar(value: [u32; 1]) -> Self { value[0] as u8 }
    }

    impl AsScalar<1> for u8 {
        fn as_scalar(&self) -> [u32; 1] { [*self as u32] }
    }

    impl FromScalar<1> for u16 {
        fn from_scalar(value: [u32; 1]) -> Self { value[0] as u16 }
    }

    impl AsScalar<1> for u16 {
        fn as_scalar(&self) -> [u32; 1] { [*self as u32] }
    }

    impl FromScalar<1> for u32 {
        fn from_scalar(value: [u32; 1]) -> Self { value[0] }
    }

    impl AsScalar<1> for u32 {
        fn as_scalar(&self) -> [u32; 1] { [*self] }
    }

    impl FromScalar<2> for u64 {
        fn from_scalar(value: [u32; 2]) -> Self { (value[0] as u64) | ((value[1] as u64) << 32) }
    }

    impl AsScalar<2> for u64 {
        fn as_scalar(&self) -> [u32; 2] { [*self as u32, (*self >> 32) as u32] }
    }

    impl FromScalar<1> for bool {
        fn from_scalar(value: [u32; 1]) -> Self { value[0] != 0 }
    }

    impl AsScalar<1> for bool {
        fn as_scalar(&self) -> [u32; 1] { [if *self { 1 } else { 0 }] }
    }

    // ==================== xous types ====================

    impl FromScalar<1> for xous::PID {
        fn from_scalar(value: [u32; 1]) -> Self { xous::PID::new(value[0].try_into().unwrap()).unwrap() }
    }

    impl AsScalar<1> for xous::PID {
        fn as_scalar(&self) -> [u32; 1] { [self.get() as u32] }
    }

    impl AsScalar<4> for xous::SID {
        fn as_scalar(&self) -> [u32; 4] {
            let s = self.to_u32();
            [s.0, s.1, s.2, s.3]
        }
    }

    impl FromScalar<4> for xous::SID {
        fn from_scalar(value: [u32; 4]) -> Self { Self::from_u32(value[0], value[1], value[2], value[3]) }
    }

    impl FromScalar<4> for xous::AppId {
        fn from_scalar(value: [u32; 4]) -> Self { value.into() }
    }

    impl AsScalar<4> for xous::AppId {
        fn as_scalar(&self) -> [u32; 4] { self.into() }
    }

    // ==================== memory range (platform specific) ====================

    #[cfg(not(keyos))]
    impl AsScalar<3> for MemoryRange {
        fn as_scalar(&self) -> [u32; 3] {
            let ptr = self.as_ptr() as usize;
            [ptr as _, (ptr >> 32) as _, self.len() as _]
        }
    }

    #[cfg(not(keyos))]
    impl FromScalar<3> for MemoryRange {
        fn from_scalar(value: [u32; 3]) -> Self {
            let ptr = (value[0] as usize) | ((value[1] as usize) << 32);
            unsafe { MemoryRange::new(ptr, value[2] as _).expect("valid memory range") }
        }
    }

    #[cfg(keyos)]
    impl AsScalar<2> for MemoryRange {
        fn as_scalar(&self) -> [u32; 2] { [self.as_ptr() as _, self.len() as _] }
    }

    #[cfg(keyos)]
    impl FromScalar<2> for MemoryRange {
        fn from_scalar(value: [u32; 2]) -> Self {
            unsafe { MemoryRange::new(value[0] as _, value[1] as _).expect("valid memory range") }
        }
    }

    // ==================== complex types ====================

    impl<T: FromScalar<3>> FromScalar<4> for Option<T> {
        fn from_scalar(value: [u32; 4]) -> Self {
            if value[0] == 1 {
                Some(T::from_scalar([value[1], value[2], value[3]]))
            } else {
                None
            }
        }
    }

    impl<T: AsScalar<3>> AsScalar<4> for Option<T> {
        fn as_scalar(&self) -> [u32; 4] {
            match self {
                Some(value) => {
                    let s = value.as_scalar();
                    [1, s[0], s[1], s[2]]
                }
                None => [0, 0, 0, 0],
            }
        }
    }

    impl<T: FromScalar<3>, E: FromScalar<3>> FromScalar<4> for Result<T, E> {
        fn from_scalar(value: [u32; 4]) -> Self {
            if value[0] == 1 {
                Ok(T::from_scalar([value[1], value[2], value[3]]))
            } else {
                Err(E::from_scalar([value[1], value[2], value[3]]))
            }
        }
    }

    impl<T: AsScalar<3>, E: AsScalar<3>> AsScalar<4> for Result<T, E> {
        fn as_scalar(&self) -> [u32; 4] {
            match self {
                Ok(value) => {
                    let s = value.as_scalar();
                    [1, s[0], s[1], s[2]]
                }
                Err(err) => {
                    let s = err.as_scalar();
                    [0, s[0], s[1], s[2]]
                }
            }
        }
    }
}
