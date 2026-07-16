// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{CheckedConn, CheckedPermissions, MessageAllowed};

use crate::messages::*;

#[macro_export]
macro_rules! use_api {
    () => {
        mod ctap_permissions {
            use ctap_hid::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/ctap-hid"]
            pub struct CtapHidPermissions;
        }
        type CtapHidApi = ctap_hid::api::CtapHidApi<ctap_permissions::CtapHidPermissions>;
    };
}

#[derive(Default)]
pub struct CtapHidApi<P: CheckedPermissions>(CheckedConn<P>);

impl<P: CheckedPermissions> CtapHidApi<P> {
    pub fn process_hid_packet(&mut self, pkt: &[u8])
    where
        P: MessageAllowed<ProcessHidPacket>,
    {
        self.0.send_blocking_archive(ProcessHidPacket(pkt.to_vec()))
    }

    /* For Tests only */

    #[cfg(feature = "test-app")]
    pub fn register_simu_usb_receiver<S>(
        &mut self,
        simu_usb_receiver: S,
    ) -> Result<(), crate::error::CtapHidError>
    where
        S: server::Server + server::BlockingArchiveHandler<SimuUsbReceiveCallback> + Send + 'static,
        P: MessageAllowed<RegisterSimuUsbReceiver>,
    {
        use server::MessageId;
        let pid = self.0.get_remote_pid();
        let cid = server::listen_and_connect(simu_usb_receiver, pid);
        xous::allow_messages_on_connection(
            pid,
            cid,
            SimuUsbReceiveCallback::ID..(SimuUsbReceiveCallback::ID + 1),
        )?;
        self.0.try_send_blocking_scalar(RegisterSimuUsbReceiver(cid))?
    }
}
