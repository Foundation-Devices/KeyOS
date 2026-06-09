// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gpio::{GpioPin, IrqMessage, PinSettings};
use server::{CheckedConn, CheckedPermissions, WithAllPermissions};
use settings::global::{AirlockMode, DeviceName, UsbEnabled};
use usb::host::messages::SetEnabled;

use crate::device::messages::{OtgMode, SetCableConnected, SetDeviceEmulationEnabled};
use crate::host::messages::HostOtgMode;
use crate::PowerManagerExtApi;

gpio::use_api!();
settings::use_api!();

#[derive(Default)]
pub struct SubscriptionServer {
    device: CheckedConn<WithAllPermissions<DevicePermissions>>,
    host: CheckedConn<WithAllPermissions<HostPermissions>>,
    settings: SettingsApi,
    power_manager_ext: PowerManagerExtApi,
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

impl server::ServerMessages for SubscriptionServer {
    const NAME: &'static str = "";

    fn messages() -> &'static [server::MessageDef<Self>] { &[] }
}

impl server::Server for SubscriptionServer {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        self.settings.server_subscribe_device_name(context);
        self.settings.server_subscribe_usb_enabled(context);
        self.settings.server_subscribe_airlock_mode(context);
        let gpio_api = GpioApi::default();
        gpio_api
            .claim_pin(GpioPin::UsbOtgId, PinSettings::InterruptBoth, false)
            .expect("Could not claim pin");

        log::debug!("Enabling OTG_ID IRQ");
        gpio_api.enable_irq(GpioPin::UsbOtgId, context).expect("Could not subscribe to gpio interrupt");

        gpio_api
            .claim_pin(GpioPin::UsbVbusIrq, PinSettings::InterruptBoth, false)
            .expect("Could not claim pin");

        log::debug!("Enabling VBUS IRQ");
        gpio_api.enable_irq(GpioPin::UsbVbusIrq, context).expect("Could not subscribe to gpio interrupt");

        let usb_otg_pin = gpio_api.get_pin(GpioPin::UsbOtgId).unwrap();
        let usb_vbus = gpio_api.get_pin(GpioPin::UsbVbusIrq).unwrap();
        // The pin is LOW if there is an OTG device present.
        self.device.send_scalar(OtgMode(!usb_otg_pin));
        self.host.send_scalar(HostOtgMode(!usb_otg_pin));
        self.device.send_scalar(SetCableConnected(usb_vbus));
    }
}

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

impl server::ScalarEventHandler<UsbEnabled> for SubscriptionServer {
    fn handle(
        &mut self,
        UsbEnabled(enabled): UsbEnabled,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        log::debug!("received usb enabled event {enabled}");
        self.device
            .try_send_scalar(SetDeviceEmulationEnabled(enabled))
            .inspect_err(|e| log::warn!("failed to set usb device enabled {enabled:?} {e:?}"))
            .ok();
        self.host
            .try_send_scalar(SetEnabled(enabled))
            .inspect_err(|e| log::warn!("failed to set usb host enabled {enabled:?} {e:?}"))
            .ok();
    }
}

impl server::ScalarEventHandler<AirlockMode> for SubscriptionServer {
    fn handle(&mut self, msg: AirlockMode, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        self.airlock_is_rw = msg == AirlockMode::ReadWrite
    }
}

impl server::ScalarEventHandler<IrqMessage> for SubscriptionServer {
    fn handle(&mut self, msg: IrqMessage, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        log::trace!("GPIO IRQ: {msg:?}");
        match msg.pin {
            GpioPin::UsbOtgId => {
                // The pin is LOW if there is an OTG device present.
                self.device.send_scalar(OtgMode(!msg.is_high));
                self.host.send_scalar(HostOtgMode(!msg.is_high));
            }
            GpioPin::UsbVbusIrq => {
                if !msg.is_high {
                    // Return mass storage to read-only when the cable is
                    // unplugged. This is now independent of Developer Mode,
                    // which no longer governs AirlockMode.
                    if self.airlock_is_rw {
                        log::info!("USB cable disconnected, setting airlock mode to ReadOnly");
                        self.settings.set_airlock_mode(AirlockMode::ReadOnly);
                    }
                    // Undo any Never priority set by the host server's timeout backoff.
                    self.power_manager_ext.set_otg_priority(power_manager::OtgPriority::Automatic).ok();
                }
                self.device.send_scalar(SetCableConnected(msg.is_high));
            }
            _ => log::warn!("Unexpected GPIO IRQ: {msg:?}"),
        }
    }
}
