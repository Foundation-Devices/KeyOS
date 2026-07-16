// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use crate::{
    archive_async_response_handler, archive_event_handler, lend_mut, scalar_async_response_handler,
    scalar_event_handler, send_archive, send_archive_nowait, send_blocking_archive,
    send_blocking_archive_async, send_blocking_archive_buf, send_blocking_scalar, send_move,
    send_move_nowait, send_scalar, send_scalar_async, send_scalar_nowait, subscribe_archive,
    subscribe_scalar, try_send_blocking_archive, try_send_blocking_archive_buf, try_send_blocking_scalar,
    try_send_move, try_send_scalar, try_send_scalar_async, Archive, ArchiveEventHandler,
    ArchiveResponseHandler, ArchiveSubscription, BlockingArchive, BlockingScalar,
    BlockingScalarResponseHandler, LendMut, Move, Scalar, ScalarEventHandler, ScalarSubscription, Server,
    ServerContext,
};

/// A connection to a running server.
#[derive(Clone)]
pub struct CheckedConn<T: CheckedPermissions> {
    cid: Arc<DisconnectOnDrop>,
    _phantom: core::marker::PhantomData<fn() -> T>,
}

pub trait CheckedPermissions: Clone + Default + 'static {
    const NAME: &str;
}

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

    /// Get a version of this connection that does not do any compile-time permission checking
    pub fn unchecked(&self) -> CheckedConn<WithAllPermissions<P>> {
        CheckedConn { cid: self.cid.clone(), _phantom: Default::default() }
    }

    // ==================== BlockingScalar Messages ====================

    /// Send a [`Scalar`] message.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn send_blocking_scalar<M>(&self, msg: M) -> M::Response
    where
        M: BlockingScalar,
        P: MessageAllowed<M>,
    {
        send_blocking_scalar(self.cid.0, msg)
    }

    /// Send a [`Scalar`] message, retaining the error.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn try_send_blocking_scalar<M>(&self, msg: M) -> Result<M::Response, xous::Error>
    where
        M: BlockingScalar,
        P: MessageAllowed<M>,
    {
        try_send_blocking_scalar(self.cid.0, msg).map_err(|e| e.into_inner())
    }

    /// Send a blocking scalar message asynchronously.
    pub fn send_scalar_async<M, SR>(&self, msg: M, context: &mut ServerContext<SR>)
    where
        M: BlockingScalar,
        P: MessageAllowed<M>,
        SR: BlockingScalarResponseHandler<M::Response>,
    {
        let msg_id = send_scalar_async(self.cid.0, msg, context.sid);
        context.handlers.push((msg_id, scalar_async_response_handler::<M, SR>));
    }

    /// Send a blocking scalar message asynchronously, retaining the error.
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

    /// Send a [`Scalar`] message, retaining the error. Blocks if the message queue is full.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn send_scalar<M>(&self, msg: M)
    where
        M: Scalar,
        P: MessageAllowed<M>,
    {
        send_scalar(self.cid.0, msg)
    }

    /// Send a [`Scalar`] message, retaining the error. Blocks if the message queue is full.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn try_send_scalar<M>(&self, msg: M) -> Result<(), xous::Error>
    where
        M: Scalar,
        P: MessageAllowed<M>,
    {
        try_send_scalar(self.cid.0, msg).map_err(|e| e.into_inner())
    }

    /// Send a [`Scalar`] message. Does not block if the message queue is full.
    /// Can be used in an IRQ handler context.
    pub fn send_scalar_nowait<M>(&self, msg: M) -> Result<(), xous::Error>
    where
        M: Scalar,
        P: MessageAllowed<M>,
    {
        send_scalar_nowait(self.cid.0, msg).map_err(|e| e.into_inner())
    }

    // ==================== Archive Messages ====================

    /// Send an [`BlockingArchive`] message and block for response.
    pub fn send_blocking_archive<M>(&self, msg: M) -> M::Response
    where
        M: BlockingArchive,
        P: MessageAllowed<M>,
    {
        send_blocking_archive(self.cid.0, msg)
    }

    /// Send an [`BlockingArchive`] message and block for response.
    /// Retains the error channel
    pub fn try_send_blocking_archive<M>(&self, msg: M) -> Result<M::Response, xous::Error>
    where
        M: BlockingArchive,
        P: MessageAllowed<M>,
    {
        try_send_blocking_archive(self.cid.0, msg).map_err(|e| e.into_inner().into_xous())
    }

    /// Send an [`BlockingArchive`] message but reuses the `Buffer`.
    pub fn send_blocking_archive_buf<M>(&self, buf: &mut xous_ipc::Buffer, msg: M) -> M::Response
    where
        M: BlockingArchive,
        P: MessageAllowed<M>,
    {
        send_blocking_archive_buf(self.cid.0, buf, msg)
    }

    /// Send an [`BlockingArchive`] message but reuses the `Buffer`.
    /// Preserves the error channel
    pub fn try_send_blocking_archive_buf<M>(
        &self,
        buf: &mut xous_ipc::Buffer,
        msg: M,
    ) -> Result<M::Response, xous::Error>
    where
        M: BlockingArchive,
        P: MessageAllowed<M>,
    {
        try_send_blocking_archive_buf(self.cid.0, buf, msg).map_err(|e| e.into_inner().into_xous())
    }

    /// Send an [`BlockingArchive`] message without blocking (for server-to-server async).
    pub fn send_blocking_archive_async<M, SR>(&self, msg: M, context: &mut ServerContext<SR>)
    where
        M: BlockingArchive,
        P: MessageAllowed<M>,
        SR: ArchiveResponseHandler<M::Response>,
    {
        let msg_id = send_blocking_archive_async(self.cid.0, msg, context.sid);
        context.handlers.push((msg_id, archive_async_response_handler::<M, SR>));
    }

    // ==================== Archive Messages (fire-and-forget) ====================

    /// Send a [`Archive`] message, retaining the error. Blocks if the message queue is full.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn try_send_archive<M>(&self, msg: M) -> Result<(), xous::Error>
    where
        M: Archive,
        P: MessageAllowed<M>,
    {
        send_archive(self.cid.0, msg)
    }

    /// Send a [`Archive`] message. Blocks if the message queue is full.
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

    /// Send a [`Archive`] message. Does not block if the message queue is full.
    /// Can be used in an IRQ handler context.
    pub fn send_archive_nowait<M>(&self, msg: M) -> Result<(), xous::Error>
    where
        M: Archive,
        P: MessageAllowed<M>,
    {
        send_archive_nowait(self.cid.0, msg)
    }

    // ==================== LendMut Messages ====================

    /// Send a [`LendMut`] message.
    pub fn lend_mut<M>(&self, msg: M) -> M::Response
    where
        M: LendMut,
        P: MessageAllowed<M>,
    {
        lend_mut(self.cid.0, msg)
    }

    // ==================== Move Messages ====================

    /// Send a [`Move`] message, retaining the error. Blocks if the message queue is full.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn send_move<M>(&self, msg: M)
    where
        M: Move,
        P: MessageAllowed<M>,
    {
        send_move(self.cid.0, msg)
    }

    /// Send a [`Move`] message, retaining the error. Blocks if the message queue is full.
    ///
    /// Warning: Cannot be used in an IRQ handler context.
    pub fn try_send_move<M>(&self, msg: M) -> Result<(), xous::Error>
    where
        M: Move,
        P: MessageAllowed<M>,
    {
        try_send_move(self.cid.0, msg).map_err(|e| e.into_inner())
    }

    /// Send a [`Move`] message. Does not block if the message queue is full.
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
