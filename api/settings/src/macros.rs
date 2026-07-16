// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! API-side macros for generating message types and client methods.
//!
//! These macros generate:
//! - Message type definitions (Get/Set/Subscribe)
//! - Client API methods on [`crate::SettingsApi`]

/// global scalar setting
macro_rules! global_scalar {
    (
        $(#[doc = $doc:expr])*
        $storage:ident,
        $(#[$meta:meta])*
        $vis:vis $kind:ident $name:ident $($body:tt)*
    ) => {
        type_def_scalar! {
            $(#[doc = $doc])*
            $(#[$meta])*
            $vis $kind $name $($body)*
        }
        message_api_scalar!($name, $storage);
    };
}

///  global archive setting
macro_rules! global_archive {
    (
        $(#[doc = $doc:expr])*
        $storage:ident,
        $(#[$meta:meta])*
        $vis:vis $kind:ident $name:ident $($body:tt)*
    ) => {
        type_def_archive! {
            $(#[doc = $doc])*
            $(#[$meta])*
            $vis $kind $name $($body)*
        }
        message_api_archive!($name, $storage);
    };
}

macro_rules! type_def_impl {
    // enum
    ($(#[$attr:meta])* $vis:vis enum $name:ident { $($body:tt)* }) => {
        $(#[$attr])*
        $vis enum $name { $($body)* }
    };
    // tuple struct
    ($(#[$attr:meta])* $vis:vis struct $name:ident(pub $inner:ty);) => {
        $(#[$attr])*
        $vis struct $name(pub $inner);

        impl From<$inner> for $name {
            fn from(value: $inner) -> Self {
                $name(value)
            }
        }

        impl From<$name> for $inner {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
    // named struct
    ($(#[$attr:meta])* $vis:vis struct $name:ident { $($body:tt)* }) => {
        $(#[$attr])*
        $vis struct $name { $($body)* }
    };
}

macro_rules! type_def_scalar {
    ($($tt:tt)*) => {
        type_def_impl! {
            #[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
            $($tt)*
        }
    };
}

macro_rules! type_def_archive {
    ($($tt:tt)*) => {
        type_def_impl! {
            #[derive(Debug, Clone, PartialEq, Eq, Hash, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, serde::Serialize, serde::Deserialize)]
            $($tt)*
        }
    };
}

macro_rules! message_api_scalar {
    ($ty:ident, $storage:ident) => {
        paste::paste! {
            #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash, server::Message)]
            #[response(response_type!($ty, $storage))]
            pub struct [<Get $ty>];

            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, server::Message)]
            pub struct [<Set $ty>](pub $ty);

            #[derive(
                Debug,
                Clone,
                Copy,
                Default,
                PartialEq,
                Eq,
                Hash,
                server::Message,
                rkyv::Archive,
                rkyv::Serialize,
                rkyv::Deserialize,
            )]
            #[event($ty)]
            pub struct [<Subscribe $ty>];

            impl<P: server::CheckedPermissions> crate::SettingsApi<P> {
                #[doc = "Subscribes to the [`settings::global::" $ty "`] setting"]
                pub fn [<server_subscribe_$ty:snake>]<S>(&self, context: &mut server::ServerContext<S>)
                where
                    S: server::Server + server::ScalarEventHandler<$ty>,
                    P: server::MessageAllowed<[<Subscribe $ty>]>,
                {
                    self.conn.subscribe_scalar_infallible([<Subscribe $ty>], context)
                }

                #[doc = "Gets the value of the [`settings::global::" $ty "`] setting"]
                pub fn [<get_$ty:snake>](&self) -> response_type!($ty, $storage)
                where
                    P: server::MessageAllowed<[<Get $ty>]>,
                {
                    self.conn.send_blocking_scalar([<Get $ty>]::default())
                }

                #[doc = "Sets the value of the [`settings::global::" $ty "`] setting"]
                pub fn [<set_$ty:snake>](&self, value: impl Into<$ty>)
                where
                    P: server::MessageAllowed<[<Set $ty>]>,
                {
                    self.conn.send_scalar([<Set $ty>](value.into()))
                }
            }
        }
    };
}

macro_rules! message_api_archive {
    ($ty:ident, $storage:ident) => {
        paste::paste! {
            #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
            #[response(response_type!($ty, $storage))]
            pub struct [<Get $ty>];

            #[derive(Debug, Clone, PartialEq, Eq, Hash, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
            pub struct [<Set $ty>](pub $ty);

            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
            #[event($ty)]
            pub struct [<Subscribe $ty>];

            impl<P: server::CheckedPermissions> crate::SettingsApi<P> {
                #[doc = "Subscribes to the [`settings::global::" $ty "`] setting"]
                pub fn [<server_subscribe_$ty:snake>]<S>(&self, context: &mut server::ServerContext<S>)
                where
                    S: server::Server + server::ArchiveEventHandler<$ty>,
                    P: server::MessageAllowed<[<Subscribe $ty>]>,
                {
                    self.conn.subscribe_archive_infallible([<Subscribe $ty>], context)
                }

                #[doc = "Gets the value of the [`settings::global::" $ty "`] setting"]
                pub fn [<get_$ty:snake>](&self) -> response_type!($ty, $storage)
                where
                    P: server::MessageAllowed<[<Get $ty>]>,
                {
                    self.conn.send_blocking_archive([<Get $ty>]::default())
                }

                #[doc = "Sets the value of the [`settings::global::" $ty "`] setting"]
                pub fn [<set_$ty:snake>](&self, value: impl Into<$ty>)
                where
                    P: server::MessageAllowed<[<Set $ty>]>,
                {
                    self.conn.send_archive([<Set $ty>](value.into()))
                }
            }
        }
    };
}

macro_rules! response_type {
    ($ty:ty, system) => { $ty };
    ($ty:ty, encrypted) => { Option<$ty> };
}

/// create modules with santizied exports for api
macro_rules! create_modules {
    ($($ty:ident),* $(,)?) => {
        pub mod messages {
            paste::paste! {
                pub use super::{
                    $(
                        [<Get $ty>],
                        [<Set $ty>],
                        [<Subscribe $ty>],
                    )*
                };
            }
        }

        pub mod inner {
            pub use super::{
                $($ty,)*
            };
        }
    };
}

pub(crate) use create_modules;
pub(crate) use global_archive;
pub(crate) use global_scalar;
pub(crate) use message_api_archive;
pub(crate) use message_api_scalar;
pub(crate) use response_type;
pub(crate) use type_def_archive;
pub(crate) use type_def_impl;
pub(crate) use type_def_scalar;
