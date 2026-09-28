// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use num_traits::{FromPrimitive, ToPrimitive};
use power_manager::{AttachedState, OtgPriority, Status};
use server::{
    AsScalar, CheckedConn, CheckedPermissions, FromScalar, MessageId as _, ScalarHandler, WithAllPermissions,
};
#[cfg(not(feature = "recovery-os"))]
use settings::global::{AirlockMode, DeviceName, UsbEnabled};
use usb::host::messages::{Subscribe, UsbEvent};
use xous_ticktimer::TicktimerCallback;

use crate::device::messages::SetDeviceEmulationEnabled;
use crate::host::implementation::UsbHostServer;
use crate::host::messages::SetEnabled;
use crate::PowerManagerExtApi;

/// Long enough to sit out the port controller's soft reset and the CC debounce that
/// follows it.
const DETACH_DEBOUNCE_MS: usize = 500;
/// How long the peer gets to take the host role once we yield it: a DRP toggle plus a
/// CC debounce, 1.25s measured on an iPhone, with headroom for a slower one. A
/// peripheral never takes it, so this is also how long we wait before hosting one.
const HOST_ROLE_PROBE_MS: usize = 2000;
/// Must outlast a whole enumeration attempt, or we stop sourcing VBUS to a device that
/// would have come up. The extra 100ms is TSIGATT, what a device is allowed for
/// signalling attach once VBUS is valid. One attempt is budgeted and not a retry, so a
/// device that starts over can meet the backoff partway through the second.
const DEVICE_CONNECTION_TIMEOUT_MS: usize = ehci::max_enumeration_ms(UsbHostServer::WORK_PERIOD_MS) + 100;

/// The state's timer ran out. Carries the state it was armed for, because cancelling
/// does not recall a callback the ticktimer already queued.
#[derive(Debug, server::Message, Clone)]
pub struct StateTimedOut(pub PortState);

#[cfg(not(feature = "recovery-os"))]
settings::use_api!();

/// One timer, armed by the state being entered and cancelled by any transition.
#[derive(num_derive::FromPrimitive, num_derive::ToPrimitive, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PortState {
    /// Nothing on the cable.
    #[default]
    Empty,
    /// Lost the attach; an attach within DETACH_DEBOUNCE_MS cancels it.
    Detaching,
    /// A host (or an accessory) on the other end.
    HostAttached,
    /// Sourcing VBUS with nothing enumerated for up to DEVICE_CONNECTION_TIMEOUT_MS.
    WaitingForDevice,
    /// Sourcing VBUS with a device on the bus.
    HostingDevice,
    /// Yielded the host role for HOST_ROLE_PROBE_MS. A laptop declares itself
    /// self-powered, but so does a mains-powered drive we are happy to host, and yielding
    /// is what tells them apart. The soft reset empties the port, so only an attach ends
    /// it early.
    ProbingPeerHostRole,
}

impl FromScalar<1> for PortState {
    fn from_scalar([value]: [u32; 1]) -> Self { PortState::from_u32(value).unwrap_or_default() }
}

impl AsScalar<1> for PortState {
    fn as_scalar(&self) -> [u32; 1] { [self.to_u32().unwrap_or_default()] }
}

#[derive(Default, server::Server)]
#[name = "os/usb-internal"]
pub struct PortServer {
    device: CheckedConn<WithAllPermissions<DevicePermissions>>,
    host: CheckedConn<WithAllPermissions<HostPermissions>>,
    power_manager_ext: PowerManagerExtApi,
    timer: Option<TicktimerCallback>,
    port_state: PortState,
    peer_declined_host_role: bool,
    enabled: bool,
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

impl server::Server for PortServer {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        #[cfg(not(feature = "recovery-os"))]
        {
            self.settings.server_subscribe_device_name(context);
            self.settings.server_subscribe_usb_enabled(context);
            self.settings.server_subscribe_airlock_mode(context);
        }

        self.timer = Some(TicktimerCallback::new(context.sid()).expect("Could not connect to ticktimer"));
        // Subscribing answers with the current status, which seeds the port state.
        self.power_manager_ext.subscribe_status(context);
        self.host.subscribe_archive_infallible(Subscribe, context);

        #[cfg(feature = "recovery-os")]
        {
            self.enabled = true;
        }
    }
}

impl PortServer {
    fn timer(&self) -> &TicktimerCallback { self.timer.as_ref().expect("Timer is set up in on_start") }

    fn transition_to(&mut self, next: PortState) {
        self.timer().cancel(StateTimedOut::ID);
        self.port_state = next;
        match next {
            PortState::Empty => {
                self.power_manager_ext.set_otg_priority(self.idle_otg_priority()).ok();
                #[cfg(not(feature = "recovery-os"))]
                if self.airlock_is_rw {
                    log::info!("Cable disconnected, setting airlock mode to ReadOnly");
                    self.settings.set_airlock_mode(AirlockMode::ReadOnly);
                }
            }
            PortState::Detaching => self.arm_timer(DETACH_DEBOUNCE_MS),
            PortState::HostAttached => {}
            PortState::WaitingForDevice => self.arm_timer(DEVICE_CONNECTION_TIMEOUT_MS),
            PortState::HostingDevice => {}
            PortState::ProbingPeerHostRole => {
                self.power_manager_ext.set_otg_priority(OtgPriority::Never).ok();
                self.arm_timer(HOST_ROLE_PROBE_MS);
            }
        }
        self.enable_controller_for_role();
    }

    fn arm_timer(&self, timeout_ms: usize) {
        self.timer().request(timeout_ms, StateTimedOut::ID, self.port_state.to_usize().unwrap_or_default());
    }

    /// What the port controller goes back to once the port is empty. Never while USB is
    /// disabled, so a disabled port never sources VBUS.
    fn idle_otg_priority(&self) -> OtgPriority {
        if self.enabled {
            OtgPriority::Automatic
        } else {
            OtgPriority::Never
        }
    }

    /// Each controller runs only while the port is in its role, and never with USB disabled.
    fn enable_controller_for_role(&self) {
        let sourcing = matches!(self.port_state, PortState::WaitingForDevice | PortState::HostingDevice);
        self.host.send_scalar(SetEnabled(self.enabled && sourcing));
        self.device.send_scalar(SetDeviceEmulationEnabled(
            self.enabled && self.port_state == PortState::HostAttached,
        ));
    }
}

#[cfg(not(feature = "recovery-os"))]
impl server::ArchiveEventHandler<DeviceName> for PortServer {
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
impl server::ScalarEventHandler<UsbEnabled> for PortServer {
    fn handle(
        &mut self,
        UsbEnabled(enabled): UsbEnabled,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        log::debug!("received usb enabled event {enabled}");
        self.enabled = enabled;
        self.power_manager_ext.set_otg_priority(self.idle_otg_priority()).ok();
        self.enable_controller_for_role();
    }
}

#[cfg(not(feature = "recovery-os"))]
impl server::ScalarEventHandler<AirlockMode> for PortServer {
    fn handle(&mut self, msg: AirlockMode, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        self.airlock_is_rw = msg == AirlockMode::ReadWrite
    }
}

impl server::ArchiveEventHandler<UsbEvent> for PortServer {
    fn handle(
        &mut self,
        msg: server::Owned<UsbEvent>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let Ok(event) = msg.deserialize() else { return };
        // One device on the bus at a time, so a Disconnect means it is empty.
        match event {
            UsbEvent::Connect { descriptors, .. } => {
                // XXX: Stale once a peripheral is unplugged during its own probe: the port
                //      already reads empty, so nothing clears it, and a laptop that lands as
                //      source next is hosted instead of probed until it is unplugged.
                if !self.peer_declined_host_role && descriptors.is_self_powered() {
                    // XXX: The host has already broadcast this device, so a mains-powered
                    //      drive gets a Connect/Disconnect pair from the probe before we host
                    //      it for real.
                    log::info!("Device is self-powered, offering it the host role: {descriptors:?}");
                    self.transition_to(PortState::ProbingPeerHostRole);
                } else {
                    self.transition_to(PortState::HostingDevice);
                }
            }
            UsbEvent::Disconnect { .. } => {
                if self.port_state == PortState::HostingDevice {
                    self.transition_to(PortState::WaitingForDevice);
                }
            }
        }
    }
}

impl server::ScalarEventHandler<Status> for PortServer {
    fn handle(&mut self, msg: Status, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        // Every status change is broadcast, so most reports repeat the state we hold.
        match msg.attached_state {
            AttachedState::None => match self.port_state {
                PortState::Empty | PortState::Detaching | PortState::ProbingPeerHostRole => {}
                _ => self.transition_to(PortState::Detaching),
            },
            AttachedState::Sink | AttachedState::Accessory => match self.port_state {
                PortState::HostAttached => {}
                _ => {
                    log::info!("A host is attached, running the device controller");
                    self.transition_to(PortState::HostAttached)
                }
            },
            AttachedState::Source => match self.port_state {
                PortState::WaitingForDevice | PortState::HostingDevice | PortState::ProbingPeerHostRole => {}
                _ => {
                    log::info!("Sourcing VBUS, waiting for a device to turn up");
                    self.transition_to(PortState::WaitingForDevice)
                }
            },
        }
    }
}

impl ScalarHandler<StateTimedOut> for PortServer {
    fn handle(
        &mut self,
        StateTimedOut(state): StateTimedOut,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if state != self.port_state {
            return;
        }
        match state {
            PortState::Detaching => {
                log::info!("Port is empty, allowing host mode again");
                self.peer_declined_host_role = false;
                self.transition_to(PortState::Empty);
            }
            PortState::WaitingForDevice => {
                log::info!(
                    "No device after {DEVICE_CONNECTION_TIMEOUT_MS}ms of host-mode activation, backing off OTG"
                );
                // The soft reset behind Never makes the port report empty, and the state follows.
                self.power_manager_ext.set_otg_priority(OtgPriority::Never).ok();
            }
            PortState::ProbingPeerHostRole => {
                // Rd against Rd reads the same as an empty port, so a peer that leaves it
                // empty for the whole probe has no host role to take.
                log::info!("The peer declined the host role, taking it back");
                self.peer_declined_host_role = true;
                self.transition_to(PortState::Empty);
            }
            _ => {}
        }
    }
}
