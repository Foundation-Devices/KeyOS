// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use power_manager::{AttachedState, Status};
use server::{CheckedConn, CheckedPermissions, MessageId as _, ScalarHandler, WithAllPermissions};
#[cfg(not(feature = "recovery-os"))]
use settings::global::{AirlockMode, DeviceName, UsbEnabled};
use usb::host::messages::SetEnabled;

#[cfg(not(feature = "recovery-os"))]
use crate::device::messages::SetDeviceEmulationEnabled;
use crate::device::messages::SetHostAttached;
use crate::host::messages::PortRole;
use crate::PowerManagerExtApi;

/// Long enough to sit out the port controller's soft reset and the CC debounce that
/// follows it, and for a peer offered the host role to toggle back as a source.
const DETACH_DEBOUNCE_MS: usize = 1000;

#[derive(Debug, server::Message, Clone)]
pub struct DetachDebounced;

#[cfg(not(feature = "recovery-os"))]
settings::use_api!();

#[derive(Default, server::Server)]
#[name = "os/usb-internal"]
pub struct SubscriptionServer {
    device: CheckedConn<WithAllPermissions<DevicePermissions>>,
    host: CheckedConn<WithAllPermissions<HostPermissions>>,
    power_manager_ext: PowerManagerExtApi,
    detach_callback: Option<xous_ticktimer::TicktimerCallback>,
    attached_state: AttachedState,
    #[cfg(not(feature = "recovery-os"))]
    settings: SettingsApi,
    #[cfg(not(feature = "recovery-os"))]
    airlock_is_rw: bool,
}

#[derive(Default, Clone)]
struct DevicePermissions;

impl CheckedPermissions for DevicePermissions {
    const NAME: &str = "os/usbdev";
}

#[derive(Default, Clone)]
struct HostPermissions;

impl CheckedPermissions for HostPermissions {
    const NAME: &str = "os/usb";
}

impl server::Server for SubscriptionServer {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        #[cfg(not(feature = "recovery-os"))]
        {
            self.settings.server_subscribe_device_name(context);
            self.settings.server_subscribe_usb_enabled(context);
            self.settings.server_subscribe_airlock_mode(context);
        }

        self.detach_callback = Some(
            xous_ticktimer::TicktimerCallback::new(context.sid()).expect("Could not connect to ticktimer"),
        );
        // Subscribing answers with the current status, which seeds the attached state.
        self.power_manager_ext.subscribe_status(context);

        #[cfg(feature = "recovery-os")]
        self.set_usb_host_enabled(true);
    }
}

impl SubscriptionServer {
    fn notify_port_role(&mut self) {
        // Only a host on the other end has any use for the device side.
        self.device.send_scalar(SetHostAttached(self.attached_state == AttachedState::Sink));
        self.host.send_scalar(PortRole(self.attached_state));
        #[cfg(not(feature = "recovery-os"))]
        if self.attached_state == AttachedState::None && self.airlock_is_rw {
            log::info!("Cable disconnected, setting airlock mode to ReadOnly");
            self.settings.set_airlock_mode(AirlockMode::ReadOnly);
        }
    }

    #[cfg(not(feature = "recovery-os"))]
    fn set_usb_enabled(&mut self, enabled: bool) {
        self.set_usb_device_enabled(enabled);
        self.set_usb_host_enabled(enabled);
    }

    #[cfg(not(feature = "recovery-os"))]
    fn set_usb_device_enabled(&mut self, enabled: bool) {
        self.device.send_scalar(SetDeviceEmulationEnabled(enabled));
    }

    fn set_usb_host_enabled(&mut self, enabled: bool) { self.host.send_scalar(SetEnabled(enabled)); }
}

#[cfg(not(feature = "recovery-os"))]
impl server::ArchiveEventHandler<DeviceName> for SubscriptionServer {
    fn handle(
        &mut self,
        msg: server::Owned<DeviceName>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let Ok(name) = msg.deserialize() else { return };
        log::debug!("received device name event {:?}", name.0);
        *crate::DEVICE_NAME.lock().unwrap_or_else(|e| e.into_inner()) = name.0;
    }
}

#[cfg(not(feature = "recovery-os"))]
impl server::ScalarEventHandler<UsbEnabled> for SubscriptionServer {
    fn handle(
        &mut self,
        UsbEnabled(enabled): UsbEnabled,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        log::debug!("received usb enabled event {enabled}");
        self.set_usb_enabled(enabled);
    }
}

#[cfg(not(feature = "recovery-os"))]
impl server::ScalarEventHandler<AirlockMode> for SubscriptionServer {
    fn handle(&mut self, msg: AirlockMode, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        self.airlock_is_rw = msg == AirlockMode::ReadWrite
    }
}

impl server::ScalarEventHandler<Status> for SubscriptionServer {
    fn handle(&mut self, msg: Status, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        if msg.attached_state == self.attached_state {
            return;
        }
        self.attached_state = msg.attached_state;

        let Some(callback) = self.detach_callback.as_ref() else { return };
        if self.attached_state == AttachedState::None {
            callback.request(DETACH_DEBOUNCE_MS, DetachDebounced::ID, 0);
        } else {
            callback.cancel(DetachDebounced::ID);
            self.notify_port_role();
        }
    }
}

impl ScalarHandler<DetachDebounced> for SubscriptionServer {
    fn handle(
        &mut self,
        _msg: DetachDebounced,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        // Cancelling does not recall a callback the ticktimer already queued, so the port
        // may well be back by the time this arrives.
        if self.attached_state != AttachedState::None {
            return;
        }
        self.notify_port_role();
    }
}
