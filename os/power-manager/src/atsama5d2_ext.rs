// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use bq24157::Bq24157;
use bq27421::Bq27421;
use gpio::{GpioPin, PinSettings};
use i2c::Peripheral;
use power_manager::{messages::*, OtgPriority};
use power_manager::{ChargeStatus, PowerManagerError, Status};
use server::{
    BlockingArchiveHandler, BlockingScalar, BlockingScalarHandler, ScalarEventHandler, ScalarEventSubscriber,
    ScalarEventSubscriptionHandler, ScalarHandler, ServerContext,
};
use tusb320::Tusb320;

i2c::use_api!();
gpio::use_api!();

#[derive(server::Server)]
#[name = "os/power-manager-ext"]
pub struct PowerManagerServerExt {
    charger: Bq24157<I2cPeripheral>,
    last_reported_charge_fault: Option<bq24157::ChargeFault>,
    num_faults: u32,
    fuel_gauge: Bq27421<I2cPeripheral>,
    port_controller: Tusb320<I2cPeripheral>,
    status_update_subscribers: Vec<ScalarEventSubscriber<Status>>,
    last_status: Option<Status>,
}

impl server::Server for PowerManagerServerExt {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        let gpio_api: gpio::GpioApi<gpio_permissions::GpioPermissions> = gpio::GpioApi::default();
        gpio_api
            .enable_irq(GpioPin::BatChgStat, context)
            .expect("Could not enable charger status GPIO interrupt");
        gpio_api.enable_irq(GpioPin::FuelIrqB, context).expect("Could not enable fuel gauge GPIO interrupt");
        gpio_api
            .enable_irq(GpioPin::UsbCtrlIrqB, context)
            .expect("Could not enable USB port controller GPIO interrupt");
    }
}

impl ScalarEventHandler<gpio::IrqMessage> for PowerManagerServerExt {
    fn handle(&mut self, msg: gpio::IrqMessage, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        match msg.pin {
            gpio::GpioPin::BatChgStat => {
                log::debug!("irq: battery charger status changed");
            }
            gpio::GpioPin::FuelIrqB => {
                log::debug!("irq: fuel gauge status changed");
            }
            gpio::GpioPin::UsbCtrlIrqB => {
                log::debug!("irq: USB port controller status changed");
                self.port_controller.clear_interrupt().ok();
            }
            _ => return,
        }

        self.update_status();
    }
}

impl PowerManagerServerExt {
    pub fn new() -> Result<Self, PowerManagerError> {
        log::debug!("Claiming I2C peripherals");
        let i2c_api = I2cApi::default();
        let charger_periph = i2c_api.claim_peripheral(Peripheral::BatteryCharger)?;
        let charger = Bq24157::new(charger_periph);
        let fuel_gauge_periph = i2c_api.claim_peripheral(Peripheral::FuelGauge)?;
        let fuel_gauge = Bq27421::new(fuel_gauge_periph);
        let port_controller_periph = i2c_api.claim_peripheral(Peripheral::UsbPortController)?;
        let mut port_controller = Tusb320::new(port_controller_periph);
        port_controller.clear_interrupt().ok(); // Allow for the new interrupts to be seen
        port_controller.set_mode_select(OtgPriority::Automatic.into()).ok();

        log::debug!("Claiming interrupt pins");
        let gpio_api = GpioApi::default();
        gpio_api
            .claim_pin(GpioPin::BatChgStat, PinSettings::InterruptFalling, false)
            .expect("Could not claim batt charger stat IRQ pin");
        gpio_api
            .claim_pin(GpioPin::FuelIrqB, PinSettings::InterruptFalling, false)
            .expect("Could not claim fuel gauge IRQ pin");
        gpio_api
            .claim_pin(GpioPin::UsbCtrlIrqB, PinSettings::InterruptFalling, false)
            .expect("Could not claim USB port controller IRQ pin");

        log::debug!("Power manager initialized");

        Ok(Self {
            charger,
            fuel_gauge,
            port_controller,
            last_reported_charge_fault: None,
            num_faults: 0,
            status_update_subscribers: Vec::new(),
            last_status: None,
        })
    }

    fn set_usb_boost(&mut self, enabled: bool) -> SetUsbBoostResponse {
        let mut ctrl = match self.charger.control() {
            Ok(ctrl) => ctrl,
            Err(e) => {
                log::error!("Error getting control register: {e:?}");
                return SetUsbBoostResponse { success: false, previous_state: false };
            }
        };
        let previous_state = ctrl.opa_mode();
        ctrl.set_opa_mode(enabled);
        match self.charger.set_control(ctrl) {
            Ok(()) => SetUsbBoostResponse { success: true, previous_state },
            Err(e) => {
                log::error!("Error setting control register: {e:?}");
                SetUsbBoostResponse { success: false, previous_state }
            }
        }
    }

    fn set_fuel_gauge_hibernate(&mut self) -> bool {
        match self.fuel_gauge.set_hibernate() {
            Ok(()) => true,
            Err(e) => {
                log::error!("Error setting fuel gauge hibernate mode: {e:?}");
                false
            }
        }
    }
}

impl BlockingScalarHandler<GetStatus> for PowerManagerServerExt {
    fn handle(
        &mut self,
        _msg: GetStatus,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetStatus as BlockingScalar>::Response {
        log::trace!(
            "State of charge: {}  Charge current: {}  Voltage: {}  Capacity: {}",
            self.fuel_gauge.state_of_charge().unwrap(),
            self.fuel_gauge.charge_current().unwrap(),
            self.fuel_gauge.voltage().unwrap(),
            self.fuel_gauge.capacity().unwrap(),
        );

        self.update_status()
    }
}

impl BlockingScalarHandler<SetUsbBoost> for PowerManagerServerExt {
    fn handle(
        &mut self,
        msg: SetUsbBoost,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> SetUsbBoostResponse {
        self.set_usb_boost(msg.enabled)
    }
}

impl ScalarHandler<SetOtgPriority> for PowerManagerServerExt {
    fn handle(
        &mut self,
        SetOtgPriority(otg_priority): SetOtgPriority,
        sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        log::debug!("Setting OTG priority to {otg_priority:?} by PID {sender}");
        if let Err(e) = self.port_controller.set_mode_select(otg_priority.into()) {
            log::error!("Error setting OTG priority: {e:?}");
        }

        if otg_priority == OtgPriority::Never {
            // Soft reset forces the TUSB320 to re-negotiate the CC connection
            // with the new mode, without requiring a cable replug.
            if let Err(e) = self.port_controller.soft_reset() {
                log::error!("Error soft-resetting TUSB320: {e:?}");
            }
        }
    }
}

impl BlockingArchiveHandler<GetExtendedStatus> for PowerManagerServerExt {
    fn handle(
        &mut self,
        _msg: GetExtendedStatus,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetExtendedStatus as server::BlockingArchive>::Response {
        self.extended_status()
    }
}

impl ScalarHandler<ClearChargeFault> for PowerManagerServerExt {
    fn handle(
        &mut self,
        _msg: ClearChargeFault,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.last_reported_charge_fault = None;
    }
}

impl BlockingScalarHandler<Shutdown> for PowerManagerServerExt {
    fn handle(&mut self, _msg: Shutdown, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        // Disable USB boost before shutting down, so we don't
        // continue draining the battery into a connected slave device.
        self.set_usb_boost(false);
        if !self.set_fuel_gauge_hibernate() {
            log::error!("Failed to set fuel gauge hibernate mode before shutdown");
        }

        xous::rsyscall(xous::SysCall::Shutdown(0)).unwrap();
        panic!("Shutdown syscall did not shut down");
    }
}

impl ScalarEventSubscriptionHandler<StatusSubscribe> for PowerManagerServerExt {
    fn handle(
        &mut self,
        _msg: StatusSubscribe,
        subscriber: ScalarEventSubscriber<Status>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        let status = self.update_status();
        if subscriber.send(&status).is_err() {
            // If we couldn't send the update, then we won't add a subscriber
            return Ok(());
        }

        self.status_update_subscribers.push(subscriber);

        Ok(())
    }
}

/// Battery percentage to keep hidden from the user as a reserve.
/// The real SoC is remapped so that this threshold appears as 0% displayed.
const STRATEGIC_BATTERY_RESERVE_PCT: u8 = 5;

/// Map the raw battery SoC to the charge value shown to the user.
/// Values at or below the reserve floor are clamped to 0. Above it they
/// scale linearly so that 100% raw still appears as 100% displayed.
fn reported_soc(raw: u8) -> u8 {
    if raw <= STRATEGIC_BATTERY_RESERVE_PCT {
        0
    } else {
        ((raw - STRATEGIC_BATTERY_RESERVE_PCT) as u16 * 100 / (100 - STRATEGIC_BATTERY_RESERVE_PCT as u16))
            as u8
    }
}

impl PowerManagerServerExt {
    fn charge_status(&mut self) -> ChargeStatus {
        let Ok(raw_status) = self.charger.status() else {
            return ChargeStatus::Fault;
        };
        match raw_status.stat() {
            0 => {
                if raw_status.is_boost() {
                    ChargeStatus::Boosting
                } else {
                    ChargeStatus::Idle
                }
            }
            1 => ChargeStatus::Charging,
            2 => ChargeStatus::ChargeDone,
            _ => {
                if let Some(fault) = raw_status.charge_fault() {
                    // Normal and SleepMode aren't faults
                    if matches!(fault, bq24157::ChargeFault::Normal | bq24157::ChargeFault::SleepMode) {
                        // Normal state, no fault
                        return ChargeStatus::Idle;
                    }

                    // Ignores NoBattery fault, the actual fault should come later.
                    // Ignores BadAdaptor as this fault can often happen when the charger is disconnected
                    if !matches!(fault, bq24157::ChargeFault::NoBattery | bq24157::ChargeFault::BadAdaptor) {
                        log::warn!("Charger reported a fault: {fault:?}");

                        self.last_reported_charge_fault = Some(fault);
                        self.num_faults = self.num_faults.saturating_add(1);
                        self.reset_charger();
                    }
                }

                ChargeStatus::Fault
            }
        }
    }

    fn extended_status(&mut self) -> Option<ExtendedStatus> {
        let current = self.fuel_gauge.charge_current().ok()?;
        let voltage_mv = self.fuel_gauge.voltage().ok()?;
        let capacity_mah = self.fuel_gauge.capacity().ok()?;
        let remaining_capacity_mah = self.fuel_gauge.remaining_capacity().ok()?;

        Some(ExtendedStatus {
            current,
            voltage_mv,
            capacity_mah,
            remaining_capacity_mah,
            last_reported_fault: self.last_reported_charge_fault.map(Into::into),
            num_reported_faults: self.num_faults,
        })
    }

    fn reset_charger(&mut self) {
        if let Err(e) = self.charger.reset_charger() {
            log::error!("Error resetting charger: {e:?}");
            return;
        }

        if let Err(e) = self.charger.apply_register_dump(&keyos::batt::CHARGER_CONFIG_DUMP) {
            log::error!("Error resetting battery charger: {e:?}");
        }
    }

    fn update_status(&mut self) -> Status {
        let raw_soc = self.fuel_gauge.state_of_charge().unwrap_or_default();
        let status = Status {
            charge_status: self.charge_status(),
            battery_percent: reported_soc(raw_soc),
            attached_state: self.port_controller.attached_state().unwrap_or_default().into(),
        };

        if self.last_status != Some(status) {
            self.last_status.replace(status);
            self.status_update_subscribers.retain(|subscriber| subscriber.send(&status).is_ok())
        }

        status
    }
}
