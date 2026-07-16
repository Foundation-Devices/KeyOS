// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[derive(server::Message)]
#[response(u32)]
pub struct ScalarEcho(pub u32);

#[derive(server::Message)]
#[response(u32)]
pub struct HoldDisposableScalar;

#[derive(server::Message)]
pub struct ShutdownDisposable;

#[derive(Debug, Default, Clone, server::Permissions)]
#[server_name = "test/disposable"]
#[all_permissions]
pub struct DisposablePermissions;

pub struct DisposableHandle(server::CheckedConn<DisposablePermissions>);

impl Default for DisposableHandle {
    fn default() -> Self { Self(Default::default()) }
}

impl Drop for DisposableHandle {
    fn drop(&mut self) { self.0.try_send_scalar(ShutdownDisposable).ok(); }
}
