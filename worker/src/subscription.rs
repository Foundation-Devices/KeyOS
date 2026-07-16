// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use server::xous;

use crate::implementation::CancelOnDrop;

pin_project_lite::pin_project! {
    /// Stream of decoded subscription events
    pub struct Subscription<M> {
        #[pin]
        pub(crate) rx: async_channel::Receiver<xous::MessageEnvelope>,
        pub(crate) decode: fn(xous::MessageEnvelope) -> M,
        pub(crate) cancel: CancelOnDrop,
    }
}

impl<M> Subscription<M> {
    pub async fn next(&mut self) -> Option<M> {
        decode_next(self.rx.recv().await.ok(), self.decode, &mut self.cancel)
    }
}

impl<M> futures_lite::Stream for Subscription<M> {
    type Item = M;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        futures_lite::Stream::poll_next(this.rx, cx)
            .map(|envelope| decode_next(envelope, *this.decode, this.cancel))
    }
}

fn decode_next<M>(
    envelope: Option<xous::MessageEnvelope>,
    decode: fn(xous::MessageEnvelope) -> M,
    cancel: &mut CancelOnDrop,
) -> Option<M> {
    envelope.map_or_else(
        || {
            cancel.disarm();
            None
        },
        |envelope| Some(decode(envelope)),
    )
}
