// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod error;
pub mod messages;

#[macro_export]
macro_rules! use_api {
    () => {
        mod keycard_permissions {
            use keycard::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/keycard"]
            pub struct KeycardPermissions;
        }
    };
}
