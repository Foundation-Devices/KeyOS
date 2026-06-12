// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use crate::{
    archive_event_handler, lend_mut, scalar_async_response_handler, scalar_event_handler, send_archive,
    send_archive_nowait, send_blocking_archive, send_blocking_scalar, send_move, send_move_nowait,
    send_scalar, send_scalar_async, send_scalar_nowait, subscribe_archive, subscribe_scalar,
    try_send_blocking_archive, try_send_blocking_scalar, try_send_move, try_send_scalar,
    try_send_scalar_async, Archive, ArchiveEventHandler, ArchiveSubscription, BlockingArchive,
    BlockingScalar, BlockingScalarResponseHandler, LendMut, Move, Scalar, ScalarEventHandler,
    ScalarSubscription, Server, ServerContext,
};

/// A typed connection to a running KeyOS server.
///
/// `CheckedConn<P>` is the lower-level message sender used by the public API
/// crates. Most application code should call a crate-specific wrapper such as
/// `BluetoothApi::enable_ble` or `SettingsApi::get_locale` instead of sending
/// raw message values directly. When adding a wrapper method, choose the send
/// method that matches the message trait implemented by `#[derive(Message)]`:
///
/// | Message trait | Send method | Use when |
/// | --- | --- | --- |
/// | [`BlockingScalar`] | [`send_blocking_scalar`](Self::send_blocking_scalar) or [`try_send_blocking_scalar`](Self::try_send_blocking_scalar) | The message fits in scalar registers and returns a response. |
/// | [`Scalar`] | [`send_scalar`](Self::send_scalar), [`try_send_scalar`](Self::try_send_scalar), or [`send_scalar_nowait`](Self::send_scalar_nowait) | The message fits in scalar registers and has no response. |
/// | [`BlockingArchive`] | [`send_blocking_archive`](Self::send_blocking_archive) or [`try_send_blocking_archive`](Self::try_send_blocking_archive) | The message is serialized with `rkyv` and returns a response. |
/// | [`Archive`] | [`send_archive`](Self::send_archive), [`try_send_archive`](Self::try_send_archive), or [`send_archive_nowait`](Self::send_archive_nowait) | The message is serialized with `rkyv` and has no response. |
/// | [`LendMut`] | [`lend_mut`](Self::lend_mut) | The caller lends a mutable memory range to the server and waits for the server to finish with it. |
/// | [`Move`] | [`send_move`](Self::send_move), [`try_send_move`](Self::try_send_move), or [`send_move_nowait`](Self::send_move_nowait) | The caller transfers ownership of a memory range to the server. |
/// | [`ScalarSubscription`] / [`ArchiveSubscription`] | [`subscribe_scalar`](Self::subscribe_scalar) or [`subscribe_archive`](Self::subscribe_archive) | The caller registers its [`ServerContext`] to receive future events. |
///
/// For normal system API wrappers, prefer the infallible methods when the target
/// service is mandatory: delivery failure means the system service has crashed or
/// disconnected, and the device should reboot rather than route unreachable
/// error handling through every caller. Use `try_*` methods when the API
/// intentionally exposes optional service availability, caller-recoverable
/// transport failure, or explicit queue-full handling.
#[derive(Clone)]
pub struct CheckedConn<T: CheckedPermissions> {
    cid: Arc<DisconnectOnDrop>,
    _phantom: core::marker::PhantomData<fn() -> T>,
}

/// Marker trait for the server name and compile-time permissions attached to a
/// connection.
///
/// Client crates normally get an implementation from `#[derive(Permissions)]`
/// via the API crate's `use_api!` macro. The derived implementation also emits
/// [`MessageAllowed<M>`] implementations for every message granted to the
/// caller by the API manifest.
pub trait CheckedPermissions: Clone + Default + 'static {
    const NAME: &str;
}

/// Compile-time proof that permissions type `P` may send message `M`.
///
/// API wrapper methods express their permission needs with bounds like
/// `P: MessageAllowed<GetStatus>`. If a call fails to compile because this
/// bound is not satisfied, grant that message in the app's manifest. Custom
/// permission types only satisfy compile-time bounds; `xous-names` still
/// enforces the manifest at runtime. Hand-written permissions are only valid for
/// infrastructure paths that connect to their own server.
pub trait MessageAllowed<M> {}

impl<P: CheckedPermissions> std::fmt::Debug for CheckedConn<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckedConn")
            .field("M", &core::any::type_name::<P>())
            .field("CID", &self.cid.0)
            .finish()
    }
}

impl<P: CheckedPermissions> Default for CheckedConn<P> {
    fn default() -> Self {
        let names = xous_names::XousNames::new().unwrap();
        names.request_connection_blocking(P::NAME).unwrap().into()
    }
}

#[derive(Default, Clone)]
pub struct WithAllPermissions<P: CheckedPermissions> {
    _phantom: core::marker::PhantomData<fn() -> P>,
}

impl<P: CheckedPermissions> CheckedPermissions for WithAllPermissions<P> {
    const NAME: &str = P::NAME;
}

impl<P: CheckedPermissions, M> MessageAllowed<M> for WithAllPermissions<P> {}

#[derive(Debug, Default, Clone)]
pub struct AllPermissions;

impl CheckedPermissions for AllPermissions {
    const NAME: &str = "";
}

impl<T> MessageAllowed<T> for AllPermissions {}

impl<P: CheckedPermissions> CheckedConn<P> {
    // ==================== Utility Methods ====================

    /// Open a connection to the server based on the server name.
    pub fn try_connect() -> Option<Self> {
        let names = xous_names::XousNames::new().unwrap();
        names.request_connection(P::NAME).map(Into::into).ok()
    }

    pub fn try_connect_with_timeout(timeout: std::time::Duration) -> Option<Self> {
        let started = std::time::Instant::now();
        loop {
            if let Some(conn) = Self::try_connect() {
                return Some(conn);
            }

            if started.elapsed() >= timeout {
                return None;
            }

            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// Get the remote process ID.
    pub fn get_remote_pid(&self) -> xous::PID { xous::get_remote_pid(self.cid.0).unwrap() }

    /// Get a version of this connection that does not do compile-time
    /// permission checking.
    ///
    /// Use this only for infrastructure code that has already enforced
    /// permissions another way. Normal API wrappers should keep their
    /// `P: MessageAllowed<M>` bounds so missing permissions fail at compile
    /// time.
    pub fn unchecked(&self) -> CheckedConn<WithAllPermissions<P>> {
        CheckedConn { cid: self.cid.clone(), _phantom: Default::default() }
    }

    // ==================== BlockingScalar Messages ====================

    /// Send a [`BlockingScalar`] message and wait for its response.
    ///
    /// Panics if the message cannot be delivered.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn send_blocking_scalar<M>(&self, msg: M) -> M::Response
    where
        M: BlockingScalar,
        P: MessageAllowed<M>,
    {
        send_blocking_scalar(self.cid.0, msg)
    }

    /// Send a [`BlockingScalar`] message and wait for its response.
    ///
    /// Returns the underlying transport error if the message cannot be
    /// delivered.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn try_send_blocking_scalar<M>(&self, msg: M) -> Result<M::Response, xous::Error>
    where
        M: BlockingScalar,
        P: MessageAllowed<M>,
    {
        try_send_blocking_scalar(self.cid.0, msg).map_err(|e| e.into_inner())
    }

    /// Send a [`BlockingScalar`] message and handle its response later on the
    /// supplied [`ServerContext`].
    pub fn send_scalar_async<M, SR>(&self, msg: M, context: &mut ServerContext<SR>)
    where
        M: BlockingScalar,
        P: MessageAllowed<M>,
        SR: BlockingScalarResponseHandler<M::Response>,
    {
        let msg_id = send_scalar_async(self.cid.0, msg, context.sid);
        context.handlers.push((msg_id, scalar_async_response_handler::<M, SR>));
    }

    /// Send a [`BlockingScalar`] message asynchronously, returning the transport
    /// error if the message cannot be queued.
    pub fn try_send_scalar_async<M, SR>(
        &self,
        msg: M,
        context: &mut ServerContext<SR>,
    ) -> Result<(), xous::Error>
    where
        M: BlockingScalar,
        P: MessageAllowed<M>,
        SR: BlockingScalarResponseHandler<M::Response>,
    {
        let msg_id =
            try_send_scalar_async(self.cid.0, msg, context.sid).map_err(|e| e.into_inner().into_xous())?;
        context.handlers.push((msg_id, scalar_async_response_handler::<M, SR>));
        Ok(())
    }

    // ==================== Scalar Messages (fire-and-forget) ====================
    //

    /// Send a fire-and-forget [`Scalar`] message.
    ///
    /// Blocks if the message queue is full and panics if the message cannot be
    /// delivered.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn send_scalar<M>(&self, msg: M)
    where
        M: Scalar,
        P: MessageAllowed<M>,
    {
        send_scalar(self.cid.0, msg)
    }

    /// Send a fire-and-forget [`Scalar`] message.
    ///
    /// Blocks if the message queue is full and returns the delivery error if
    /// the message cannot be delivered.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn try_send_scalar<M>(&self, msg: M) -> Result<(), xous::Error>
    where
        M: Scalar,
        P: MessageAllowed<M>,
    {
        try_send_scalar(self.cid.0, msg).map_err(|e| e.into_inner())
    }

    /// Send a fire-and-forget [`Scalar`] message without waiting for queue
    /// space.
    ///
    /// Returns an error if the queue is full or delivery otherwise fails.
    /// Can be used in an IRQ handler context.
    pub fn send_scalar_nowait<M>(&self, msg: M) -> Result<(), xous::Error>
    where
        M: Scalar,
        P: MessageAllowed<M>,
    {
        send_scalar_nowait(self.cid.0, msg).map_err(|e| e.into_inner())
    }

    // ==================== Archive Messages ====================

    /// Send a [`BlockingArchive`] message and wait for its response.
    ///
    /// Panics if the message cannot be delivered.
    pub fn send_blocking_archive<M>(&self, msg: M) -> M::Response
    where
        M: BlockingArchive,
        P: MessageAllowed<M>,
    {
        send_blocking_archive(self.cid.0, msg)
    }

    /// Send a [`BlockingArchive`] message and wait for its response.
    ///
    /// Returns the underlying transport error if the message cannot be
    /// delivered or decoded.
    pub fn try_send_blocking_archive<M>(&self, msg: M) -> Result<M::Response, xous::Error>
    where
        M: BlockingArchive,
        P: MessageAllowed<M>,
    {
        try_send_blocking_archive(self.cid.0, msg).map_err(|e| e.into_inner().into_xous())
    }

    // ==================== Archive Messages (fire-and-forget) ====================

    /// Send a fire-and-forget [`Archive`] message.
    ///
    /// Blocks if the message queue is full and returns the delivery error if
    /// the message cannot be delivered.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn try_send_archive<M>(&self, msg: M) -> Result<(), xous::Error>
    where
        M: Archive,
        P: MessageAllowed<M>,
    {
        send_archive(self.cid.0, msg)
    }

    /// Send a fire-and-forget [`Archive`] message.
    ///
    /// Blocks if the message queue is full and panics if the message cannot be
    /// delivered.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    #[track_caller]
    pub fn send_archive<M>(&self, msg: M)
    where
        M: Archive,
        P: MessageAllowed<M>,
    {
        send_archive(self.cid.0, msg).unwrap()
    }

    /// Send a fire-and-forget [`Archive`] message without waiting for queue
    /// space.
    ///
    /// Returns an error if the queue is full or delivery otherwise fails.
    /// Can be used in an IRQ handler context.
    pub fn send_archive_nowait<M>(&self, msg: M) -> Result<(), xous::Error>
    where
        M: Archive,
        P: MessageAllowed<M>,
    {
        send_archive_nowait(self.cid.0, msg)
    }

    // ==================== LendMut Messages ====================

    /// Send a [`LendMut`] message and wait for the server to finish with the
    /// lent memory.
    ///
    /// Use this for APIs that pass a [`xous::MemoryRange`] to a server for
    /// in-place reads or writes.
    pub fn lend_mut<M>(&self, msg: M) -> M::Response
    where
        M: LendMut,
        P: MessageAllowed<M>,
    {
        lend_mut(self.cid.0, msg)
    }

    // ==================== Move Messages ====================

    /// Send a [`Move`] message, transferring ownership of its memory range.
    ///
    /// Blocks if the message queue is full and panics if the message cannot be
    /// delivered.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn send_move<M>(&self, msg: M)
    where
        M: Move,
        P: MessageAllowed<M>,
    {
        send_move(self.cid.0, msg)
    }

    /// Send a [`Move`] message, transferring ownership of its memory range.
    ///
    /// Blocks if the message queue is full and returns the delivery error if
    /// the message cannot be delivered.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn try_send_move<M>(&self, msg: M) -> Result<(), xous::Error>
    where
        M: Move,
        P: MessageAllowed<M>,
    {
        try_send_move(self.cid.0, msg).map_err(|e| e.into_inner())
    }

    /// Send a [`Move`] message without waiting for queue space.
    ///
    /// Returns an error if the queue is full or delivery otherwise fails.
    /// Can be used in an IRQ handler context.
    pub fn send_move_nowait<M>(&self, msg: M) -> Result<(), xous::Error>
    where
        M: Move,
        P: MessageAllowed<M>,
    {
        send_move_nowait(self.cid.0, msg).map_err(|e| e.into_inner())
    }

    // ==================== Subscriptions ====================

    /// Subscribe to archive events.
    pub fn subscribe_archive<M, SR>(&self, msg: M, context: &mut ServerContext<SR>) -> Result<(), M::Error>
    where
        M: ArchiveSubscription + 'static,
        P: MessageAllowed<M>,
        SR: ArchiveEventHandler<M::Event>,
    {
        match subscribe_archive::<M>(self.cid.0, msg, context.sid) {
            Ok((msg_id, cancel_msg_id)) => {
                context.handlers.push((msg_id, archive_event_handler::<M::Event, SR>));
                context.handlers.push((cancel_msg_id, cancellation_handler::<SR>));
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Subscribe to archive events (infallible version).
    pub fn subscribe_archive_infallible<M, SR>(&self, msg: M, context: &mut ServerContext<SR>)
    where
        M: ArchiveSubscription<Error = crate::Infallible> + 'static,
        P: MessageAllowed<M>,
        SR: ArchiveEventHandler<M::Event>,
    {
        self.subscribe_archive::<M, SR>(msg, context).unwrap()
    }

    /// Subscribe to scalar events.
    pub fn subscribe_scalar<M, SR>(&self, msg: M, context: &mut ServerContext<SR>) -> Result<(), M::Error>
    where
        M: ScalarSubscription + 'static,
        P: MessageAllowed<M>,
        SR: ScalarEventHandler<M::Event>,
    {
        match subscribe_scalar::<M>(self.cid.0, msg, context.sid) {
            Ok((msg_id, cancel_msg_id)) => {
                context.handlers.push((msg_id, scalar_event_handler::<M::Event, SR>));
                context.handlers.push((cancel_msg_id, cancellation_handler::<SR>));
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Subscribe to scalar events (infallible version).
    pub fn subscribe_scalar_infallible<M, SR>(&self, msg: M, context: &mut ServerContext<SR>)
    where
        M: ScalarSubscription<Error = crate::Infallible> + 'static,
        P: MessageAllowed<M>,
        SR: ScalarEventHandler<M::Event>,
    {
        self.subscribe_scalar::<M, SR>(msg, context).unwrap()
    }
}

impl<P: CheckedPermissions> From<xous::CID> for CheckedConn<P> {
    fn from(cid: xous::CID) -> Self {
        Self { cid: Arc::new(DisconnectOnDrop(cid)), _phantom: Default::default() }
    }
}

fn cancellation_handler<SR: Server>(
    _handler: &mut SR,
    raw: xous::MessageEnvelope,
    context: &mut ServerContext<SR>,
) {
    if let Ok((msg_id, cancel_msg_id)) = crate::event::extract_cancellation_message(&raw.body) {
        context.handlers.retain(|(id, _)| *id != msg_id && *id != cancel_msg_id);
    }
}

struct DisconnectOnDrop(xous::CID);

impl Drop for DisconnectOnDrop {
    fn drop(&mut self) {
        if let Err(e) = xous::disconnect(self.0) {
            log::error!("Disconnect failed: {e:?}");
        }
    }
}
