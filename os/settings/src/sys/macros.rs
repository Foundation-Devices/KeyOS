// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Server-side macros for generating handler implementations.
//!
//! This module contains macros that generate handler boilerplate for settings:
//!
//! - [`archive_global_handler!`]:
//!   - Generates handler implementations for archive-based settings
//!
//! - [`scalar_global_handler!`]:
//!   - Generates handler implementations for scalar-based settings
//!
//! - [`settings_registry!`]:
//!   - Generates over-arching structs for all global settings
//!   - SystemSettings and EncryptedSettings structs
//!   - GlobalSubscriptions struct for managing subscriptions

macro_rules! settings_registry {
    (
        system: [
            $($sys_ty:ident : $sys_kind:ident),* $(,)?
        ],
        encrypted: [
            $($enc_ty:ident : $enc_kind:ident),* $(,)?
        ] $(,)?
    ) => {
        paste::paste! {
            // Generate handler implementations for all settings
            $(
                handler_macro!($sys_kind, $sys_ty, system);
            )*
            $(
                handler_macro!($enc_kind, $enc_ty, encrypted);
            )*

            #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
            pub struct SystemSettings {
                $(
                    #[serde(default = "crate::sys::LoadDefault::load_default")]
                    pub [<$sys_ty:snake>]: settings::global::$sys_ty,
                )*
            }

            impl Default for SystemSettings {
                fn default() -> Self {
                    Self {
                        $(
                            [<$sys_ty:snake>]: settings::global::$sys_ty::load_default(),
                        )*
                    }
                }
            }

            #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
            pub struct EncryptedSettings {
                $(
                    #[serde(default = "crate::sys::LoadDefault::load_default")]
                    pub [<$enc_ty:snake>]: settings::global::$enc_ty,
                )*
            }

            impl Default for EncryptedSettings {
                fn default() -> Self {
                    Self {
                        $(
                            [<$enc_ty:snake>]: settings::global::$enc_ty::load_default(),
                        )*
                    }
                }
            }

            #[derive(Default)]
            pub(crate) struct GlobalSubscriptions {
                $(
                    pub [<$sys_ty:snake>]: Vec<macros::subscriber_type!($sys_kind, settings::global::$sys_ty)>,
                )*
                $(
                    pub [<$enc_ty:snake>]: Vec<macros::subscriber_type!($enc_kind, settings::global::$enc_ty)>,
                )*
            }

            impl GlobalSubscriptions {
                #[allow(unused)]
                pub fn remove_process(&mut self, pid: xous::PID) {
                    $(
                        self.[<$sys_ty:snake>].retain(|s| s.pid() != pid);
                    )*
                    $(
                        self.[<$enc_ty:snake>].retain(|s| s.pid() != pid);
                    )*
                }

                pub fn remove_cid(&mut self, cid: xous::CID) {
                    $(
                        self.[<$sys_ty:snake>].retain(|s| s.cid() != cid);
                    )*
                    $(
                        self.[<$enc_ty:snake>].retain(|s| s.cid() != cid);
                    )*
                }

                pub fn notify_encrypted_subscribers(&mut self, settings: &EncryptedSettings) {
                    $(
                        let value = settings.[<$enc_ty:snake>].clone();
                        self.[<$enc_ty:snake>].retain(|subscriber| subscriber.send(&value).is_ok());
                    )*
                }
            }
        }
    };
}

macro_rules! archive_global_handler {
    ($ty:ident, $storage:ident) => {
        paste::paste! {
            impl server::BlockingArchiveHandler<settings::messages::[<Get $ty>]> for crate::Server {
                fn handle(
                    &mut self,
                    _msg: settings::messages::[<Get $ty>],
                    _sender: xous::PID,
                    _context: &mut server::ServerContext<Self>,
                ) -> <settings::messages::[<Get $ty>] as server::BlockingArchive>::Response {
                    get_value!($ty, self, $storage)
                }
            }

            impl server::ArchiveHandler<settings::messages::[<Set $ty>]> for crate::Server {
                fn handle(
                    &mut self,
                    msg: server::Owned<settings::messages::[<Set $ty>]>,
                    _sender: xous::PID,
                    _context: &mut server::ServerContext<Self>,
                )  {
                    let Ok(msg) = msg.deserialize() else { return };
                    set_value!($ty, self, msg, $storage);
                }
            }

            impl server::ArchiveEventSubscriptionHandler<settings::messages::[<Subscribe $ty>]> for crate::Server {
                fn handle(
                    &mut self,
                    _msg: settings::messages::[<Subscribe $ty>],
                    subscriber: server::ArchiveEventSubscriber<settings::global::$ty>,
                    _context: &mut server::ServerContext<Self>,
                ) -> Result<(), server::Infallible> {
                    subscribe_value!($ty, self, subscriber, $storage);
                    Ok(())
                }
            }
        }
    };
}

macro_rules! scalar_global_handler {
    ($ty:ident, $storage:ident) => {
        paste::paste! {
            impl server::BlockingScalarHandler<settings::messages::[<Get $ty>]> for crate::Server {
                fn handle(
                    &mut self,
                    _msg: settings::messages::[<Get $ty>],
                    _sender: xous::PID,
                    _context: &mut server::ServerContext<Self>,
                ) -> <settings::messages::[<Get $ty>] as server::BlockingScalar>::Response {
                    get_value!($ty, self, $storage)
                }
            }

            impl server::ScalarHandler<settings::messages::[<Set $ty>]> for crate::Server {
                fn handle(
                    &mut self,
                    msg: settings::messages::[<Set $ty>],
                    _sender: xous::PID,
                    _context: &mut server::ServerContext<Self>,
                )  {
                    set_value!($ty, self, msg, $storage);
                }
            }

            impl server::ScalarEventSubscriptionHandler<settings::messages::[<Subscribe $ty>]> for crate::Server {
                fn handle(
                    &mut self,
                    _msg: settings::messages::[<Subscribe $ty>],
                    subscriber: server::ScalarEventSubscriber<settings::global::$ty>,
                    _context: &mut server::ServerContext<Self>,
                ) -> Result<(), server::Infallible> {
                    subscribe_value!($ty, self, subscriber, $storage);
                    Ok(())
                }
            }
        }
    };
}

macro_rules! handler_macro {
    (scalar, $ty:ident, $storage:ident) => {
        scalar_global_handler!($ty, $storage);
    };
    (archive, $ty:ident, $storage:ident) => {
        archive_global_handler!($ty, $storage);
    };
}

macro_rules! subscriber_type {
    (scalar, $ty:ty) => { server::ScalarEventSubscriber<$ty> };
    (archive, $ty:ty) => { server::ArchiveEventSubscriber<$ty> };
}

macro_rules! get_value {
    ($ty:ident, $self:expr,system) => {
        paste::paste! {{
            let global = $self.store.get_system();
            let value = global.[<$ty:snake>].clone();
            value
        }}
    };
    ($ty:ident, $self:expr,encrypted) => {
        paste::paste! {{
            let global = $self.store.get_encrypted();
            let value = global.map(|g| g.[<$ty:snake>].clone());
            value
        }}
    };
}

macro_rules! set_value_inner {
    ($ty:ident, $self:expr, $value:expr, $global:expr) => {
        paste::paste! {{
            $self.subscriptions.[<$ty:snake>].retain(|subscriber| {
                match subscriber.send(&$value) {
                    Ok(_) => true,
                    Err(e) => {
                        log::error!("Failed to send value to subscriber: {e:?}");
                        false
                    }
                }
            });
            $global.[<$ty:snake>] = $value;
        }}
    };
}

macro_rules! set_value {
    ($ty:ident, $self:expr, $msg:expr,system) => {
        paste::paste! {{
            let value = $msg.0;
            let mut global = $self.store.get_system();
            set_value_inner!($ty, $self, value, global);
        }}
    };
    ($ty:ident, $self:expr, $msg:expr,encrypted) => {
        paste::paste! {{
            let value = $msg.0;
            if let Some(mut global) = $self.store.get_encrypted() {
                set_value_inner!($ty, $self, value, global);
            } else {
                log::warn!("global setting set called without encrypted partition being mounted");
            }
        }}
    };
}

macro_rules! subscribe_value {
    ($ty:ident, $self:expr, $subscriber:expr,system) => {
        paste::paste! {{
            let global = $self.store.get_system();
            let value = &global.[<$ty:snake>];
            if $subscriber.send(&value).is_ok() {
                $self.subscriptions.[<$ty:snake>].push($subscriber);
            }
        }}
    };
    ($ty:ident, $self:expr, $subscriber:expr,encrypted) => {
        paste::paste! {{
            if let Some(global) = $self.store.get_encrypted() {
                let value = &global.[<$ty:snake>];
                if $subscriber.send(value).is_ok() {
                    $self.subscriptions.[<$ty:snake>].push($subscriber);
                }
            } else {
                // Store subscriber to be notified when partition mounts
                $self.subscriptions.[<$ty:snake>].push($subscriber);
            }
        }}
    };
}

pub(crate) use archive_global_handler;
pub(crate) use get_value;
pub(crate) use handler_macro;
pub(crate) use scalar_global_handler;
pub(crate) use set_value;
pub(crate) use set_value_inner;
pub(crate) use settings_registry;
pub(crate) use subscribe_value;
pub(crate) use subscriber_type;
