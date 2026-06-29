// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
pub use archive::*;
pub use scalar::*;

mod archive;
mod scalar;

use rkyv::bytecheck::CheckBytes;
use whence::WhenceExt;

use crate::{Error, WrongMessageTypeError};
use crate::{XousDeserializer, XousValidator};

pub trait SubscriptionError
where
    Self: crate::ArchiveCodec,
    <Result<(), Self> as rkyv::Archive>::Archived:
        rkyv::Deserialize<Result<(), Self>, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
}

impl<T> SubscriptionError for T
where
    T: crate::ArchiveCodec,

    <Result<(), T> as rkyv::Archive>::Archived:
        rkyv::Deserialize<Result<(), T>, XousDeserializer> + for<'a> CheckBytes<XousValidator<'a>>,
{
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct EventSubscriptionMessage<T> {
    pub cid: xous::CID,
    pub msg_id: xous::MessageId,
    pub cancel_msg_id: xous::MessageId,
    pub msg: T,
}

impl<T> EventSubscriptionMessage<T>
where
    T: scalar::ScalarSubscription,
{
    #[inline]
    pub fn send_scalar(self, cid: xous::CID) -> whence::Result<Result<(), T::Error>, Error> {
        let mut buf = crate::Buffer::into_buf(&self).whence()?;
        buf.lend_mut(cid, T::ID as u32).whence()?;
        buf.to_original::<Result<(), T::Error>>().whence()
    }
}

impl<T> EventSubscriptionMessage<T>
where
    T: archive::ArchiveSubscription,
{
    #[inline]
    pub fn send_archive(self, cid: xous::CID) -> whence::Result<Result<(), T::Error>, Error> {
        let mut buf = crate::Buffer::into_buf(&self).whence()?;
        buf.lend_mut(cid, T::ID as u32).whence()?;
        buf.to_original::<Result<(), T::Error>>().whence()
    }
}

fn cancellation_message(msg_id: xous::MessageId, cancel_msg_id: xous::MessageId) -> xous::Message {
    xous::Message::new_scalar(cancel_msg_id, msg_id, cancel_msg_id, 0, 0)
}

pub fn extract_cancellation_message(
    msg: &xous::Message,
) -> Result<(xous::MessageId, xous::MessageId), Error> {
    match msg {
        xous::Message::Scalar(scalar) => Ok((scalar.arg1 as usize, scalar.arg2 as usize)),
        _ => {
            let err: rkyv::rancor::Error = rkyv::rancor::Source::new(WrongMessageTypeError);
            Err(err.into())
        }
    }
}

pub use infallible::Infallible;

mod infallible {
    /// An error type that can never be constructed, similar to std::convert::Infallible
    /// but with rkyv serialization support.
    #[derive(Debug, Clone, Copy)]
    pub enum Infallible {}

    impl rkyv::Archive for Infallible {
        type Archived = ArchivedInfallible;
        type Resolver = ();

        fn resolve(&self, _resolver: Self::Resolver, _out: rkyv::Place<Self::Archived>) { match *self {} }
    }

    #[derive(Debug)]
    pub enum ArchivedInfallible {}

    unsafe impl rkyv::Portable for ArchivedInfallible {}

    unsafe impl<C: rkyv::rancor::Fallible + ?Sized> rkyv::bytecheck::CheckBytes<C> for ArchivedInfallible {
        unsafe fn check_bytes(_value: *const Self, _context: &mut C) -> Result<(), C::Error> { Ok(()) }
    }

    impl<S: rkyv::rancor::Fallible + ?Sized> rkyv::Serialize<S> for Infallible {
        fn serialize(&self, _serializer: &mut S) -> Result<Self::Resolver, S::Error> { match *self {} }
    }

    impl<D: rkyv::rancor::Fallible + ?Sized> rkyv::Deserialize<Infallible, D> for ArchivedInfallible {
        fn deserialize(&self, _deserializer: &mut D) -> Result<Infallible, D::Error> { match *self {} }
    }
}

#[test]
fn cancellation_msg() {
    let msg = cancellation_message(1, 2);
    assert_eq!(extract_cancellation_message(&msg).unwrap(), (1, 2));
}
