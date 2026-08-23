// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::ops::Deref;

pub trait MessageAllowed<Message> {}

pub struct GetThing;

pub trait LocalTrait<P> {
    /// Documented so rustdoc wraps this required method in a toggle.
    fn local(&self)
    where
        P: MessageAllowed<GetThing>;

    fn undocumented(&self)
    where
        P: MessageAllowed<GetThing>;

    /// Documented so rustdoc wraps this provided method in a toggle.
    fn provided(&self)
    where
        P: MessageAllowed<GetThing>,
    {
    }
}

pub trait PlainTrait {
    fn plain(&self);
}

pub struct Inner;

impl Inner {
    pub fn inherited(&self) {}
}

pub struct Api<P> {
    inner: Inner,
    permissions: P,
}

impl<P> Api<P> {
    /// Documented so rustdoc wraps this method in a toggle.
    pub fn read(&self)
    where
        P: MessageAllowed<GetThing>,
    {
    }
}

impl<P> LocalTrait<P> for Api<P> {
    fn local(&self)
    where
        P: MessageAllowed<GetThing>,
    {
    }

    fn undocumented(&self)
    where
        P: MessageAllowed<GetThing>,
    {
    }
}

impl<P> PlainTrait for Api<P> {
    fn plain(&self) {}
}

impl<P> Deref for Api<P> {
    type Target = Inner;

    fn deref(&self) -> &Self::Target { &self.inner }
}

impl<P> Drop for Api<P> {
    fn drop(&mut self) { let _ = &self.permissions; }
}

pub struct ExternalDeref(String);

impl Deref for ExternalDeref {
    type Target = str;

    fn deref(&self) -> &Self::Target { &self.0 }
}

pub fn read_free<P>()
where
    P: MessageAllowed<GetThing>,
{
}
