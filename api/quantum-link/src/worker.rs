// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{future::Future, marker::PhantomData};

use rkyv::bytecheck::CheckBytes;
use server::{CheckedPermissions, MessageAllowed, XousDeserializer, XousValidator};
use worker::{StreamWatch, WorkerHandle};

use crate::{messages::SubscribeConnectionStatus, ConnectionStatus};

/// reactive handle to latest QuantumLink status
#[derive(Clone)]
pub struct QlStatus<P> {
    watch: StreamWatch<ConnectionStatus>,
    worker: WorkerHandle,
    _phantom: PhantomData<fn() -> P>,
}

impl<P> QlStatus<P>
where
    P: CheckedPermissions + 'static,
{
    pub fn new(worker: WorkerHandle) -> Self
    where
        P: MessageAllowed<SubscribeConnectionStatus> + 'static,
    {
        let sub = worker.subscribe_scalar::<P, _>(SubscribeConnectionStatus);
        let initial = ConnectionStatus { bt_connected: false, ql_paired: false, live: false };
        let watch = worker.watch_stream(sub, initial);
        Self { watch, worker, _phantom: Default::default() }
    }

    /// wait until bluetooth is connected, device is paired, and connection is confirmed as live
    pub async fn ready(&self) {
        self.watch.wait_until(|status| status.bt_connected && status.ql_paired && status.live).await
    }

    /// wait until bluetooth is connected
    pub async fn bt_ready(&self) { self.watch.wait_until(|status| status.bt_connected).await }

    /// check if fully connected (BT + paired)
    pub fn is_connected(&self) -> bool {
        let status = self.watch.borrow();
        status.bt_connected && status.ql_paired
    }

    // send a ql archive, after waiting for a connection
    pub fn send_ql_archive<M>(&self, msg: M) -> impl Future<Output = M::Response>
    where
        P: server::MessageAllowed<M>,
        M: server::BlockingArchive + Send + 'static,
        M::Response: Send,
    {
        let this = self.clone();
        async move {
            this.ready().await;
            this.worker.async_archive::<P, _>(msg).await
        }
    }

    // retry publishing the message indefinitely
    pub fn send_ql_archive_retry<M, T, E>(
        &self,
        msg: M,
        mut error: impl FnMut(E) + Send + 'static,
    ) -> impl Future<Output = T>
    where
        P: server::CheckedPermissions + server::MessageAllowed<M>,
        M: server::BlockingArchive<Response = Result<T, E>> + Send + Clone + 'static,
        M::Response: Send,
        T: server::ArchiveCodec + Send + 'static,
        <T as rkyv::Archive>::Archived:
            rkyv::Deserialize<T, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
        E: server::ArchiveCodec + Send + 'static,
        <E as rkyv::Archive>::Archived:
            rkyv::Deserialize<E, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
    {
        let this = self.clone();
        async move {
            loop {
                this.ready().await;
                match this.worker.async_archive::<P, _>(msg.clone()).await {
                    Ok(value) => {
                        return value;
                    }
                    Err(e) => {
                        error(e);
                    }
                }
            }
        }
    }

    pub fn into_inner(self) -> StreamWatch<ConnectionStatus> { self.watch }
}

impl<P> std::ops::Deref for QlStatus<P> {
    type Target = StreamWatch<ConnectionStatus>;

    fn deref(&self) -> &Self::Target { &self.watch }
}

impl<P> std::ops::DerefMut for QlStatus<P> {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.watch }
}
