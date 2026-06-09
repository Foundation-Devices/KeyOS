// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::messages::*;
use server::BlockingScalarHandler;

use crate::{Error, Location, Server};

macro_rules! impl_access_handler {
    ($name:ident, $field:ident, $location:ident) => {
        impl BlockingScalarHandler<$name> for Server {
            fn handle(
                &mut self,
                _msg: $name,
                sender: server::xous::PID,
                _context: &mut server::ServerContext<Self>,
            ) {
                // NOTE: Actual access control is implemented on an app->messageId level
                self.$field.insert((sender, Location::$location));
            }
        }
    };
}

impl_access_handler!(GetUsbReadAccess, read_access, Usb);
impl_access_handler!(GetUsbWriteAccess, write_access, Usb);
impl_access_handler!(GetBootReadAccess, read_access, Boot);
impl_access_handler!(GetBootWriteAccess, write_access, Boot);
impl_access_handler!(GetUserReadAccess, read_access, User);
impl_access_handler!(GetUserWriteAccess, write_access, User);
impl_access_handler!(GetSystemReadAccess, read_access, System);
impl_access_handler!(GetSystemWriteAccess, write_access, System);
impl_access_handler!(GetSystemAppDataReadAccess, read_access, SystemAppData);
impl_access_handler!(GetSystemAppDataWriteAccess, write_access, SystemAppData);
impl_access_handler!(GetEncryptedRootReadAccess, read_access, EncryptedRoot);
impl_access_handler!(GetEncryptedRootWriteAccess, write_access, EncryptedRoot);
impl_access_handler!(GetAirlockReadAccess, read_access, Airlock);
impl_access_handler!(GetAirlockWriteAccess, write_access, Airlock);

impl Server {
    pub fn check_read_access(&self, pid: server::xous::PID, location: Location) -> Result<(), Error> {
        match location {
            Location::CommonAssets | Location::AppData | Location::AppResources => Ok(()),
            _ => {
                if self.read_access.contains(&(pid, location)) {
                    Ok(())
                } else {
                    Err(Error::AccessDenied)
                }
            }
        }
    }

    pub fn check_write_access(&self, pid: server::xous::PID, location: Location) -> Result<(), Error> {
        match location {
            Location::AppData => Ok(()),
            Location::AppResources => Err(Error::InvalidOperation),
            _ => {
                if self.write_access.contains(&(pid, location)) {
                    Ok(())
                } else {
                    Err(Error::InvalidOperation)
                }
            }
        }
    }
}
