// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;

use server::MessageId as _;
use {
    crate::pins::{
        self, pioa_irq_mask, piob_irq_mask, pioc_irq_mask, piod_irq_mask, GpioPinOperations, Port,
    },
    gpio::messages::*,
    gpio::{GpioApiError, GpioPin, PinSettings},
    server::{ScalarEventSubscriber, Server, ServerContext},
    xous::{arch::irq::IrqNumber, PID},
};

#[derive(Debug, server::Message)]
pub(crate) struct SubscriberDisconnected(pub xous::CID);

fn is_reserved_pin(pin: GpioPin) -> bool { matches!(pin, GpioPin::NoiseEn) }

#[derive(Default)]
struct GpioServerState {
    claimed_gpios: HashMap<GpioPin, (PID, PinSettings)>,
    irq_pins_pioa: Vec<(GpioPin, ScalarEventSubscriber<IrqMessage>)>,
    irq_pins_piob: Vec<(GpioPin, ScalarEventSubscriber<IrqMessage>)>,
    irq_pins_pioc: Vec<(GpioPin, ScalarEventSubscriber<IrqMessage>)>,
    irq_pins_piod: Vec<(GpioPin, ScalarEventSubscriber<IrqMessage>)>,
}

#[derive(Debug, server::Server)]
#[name = "os/gpio"]
pub struct GpioServer {}

static mut GPIO_SERVER_STATE: Option<GpioServerState> = None;

impl Server for GpioServer {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        xous::register_system_event_handler(
            xous::SystemEvent::Disconnected,
            context.sid(),
            SubscriberDisconnected::ID,
        )
        .unwrap();
    }
}

impl GpioServer {
    pub fn init() -> Result<Self, xous::Error> {
        crate::implementation::init()?;

        Ok(GpioServer {})
    }

    pub fn is_pin_claimed(&self, pin: GpioPin) -> bool { self.pin_claimed_by(pin).is_some() }

    pub fn pin_claimed_by(&self, pin: GpioPin) -> Option<(PID, PinSettings)> {
        unsafe { (&*core::ptr::addr_of!(GPIO_SERVER_STATE)).as_ref()?.claimed_gpios.get(&pin).cloned() }
    }

    pub fn claim_pin(
        &mut self,
        ClaimPin { pin, pin_settings, debounce }: ClaimPin,
        sender: PID,
    ) -> Result<(), GpioApiError> {
        if is_reserved_pin(pin) {
            log::error!("Pin {pin:?} is reserved for internal use and can't be claimed");
            return Err(GpioApiError::AlreadyClaimed);
        }

        if self.is_pin_claimed(pin) {
            log::error!("Pin {pin:?} is already claimed");
            return Err(GpioApiError::AlreadyClaimed);
        }

        unsafe { (&mut *core::ptr::addr_of_mut!(GPIO_SERVER_STATE)).as_mut() }
            .ok_or(GpioApiError::InternalError)?
            .claimed_gpios
            .insert(pin, (sender, pin_settings));

        pin.configure(pin_settings, debounce);

        Ok(())
    }

    fn subscribe_irq_pin(&mut self, pin: GpioPin, subscriber: ScalarEventSubscriber<IrqMessage>) {
        let state =
            unsafe { (&mut *core::ptr::addr_of_mut!(GPIO_SERVER_STATE)).as_mut() }.expect("initialized");

        match pin.port() {
            Port::A => state.irq_pins_pioa.push((pin, subscriber)),
            Port::B => state.irq_pins_piob.push((pin, subscriber)),
            Port::C => state.irq_pins_pioc.push((pin, subscriber)),
            Port::D => state.irq_pins_piod.push((pin, subscriber)),
        }
    }
}

impl server::BlockingScalarHandler<ClaimPin> for GpioServer {
    fn handle(
        &mut self,
        claim_pin: ClaimPin,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), GpioApiError> {
        self.claim_pin(claim_pin, sender)?;

        Ok(())
    }
}

impl server::ScalarEventSubscriptionHandler<EnableIrq> for GpioServer {
    fn handle(
        &mut self,
        EnableIrq(pin): EnableIrq,
        subscriber: ScalarEventSubscriber<IrqMessage>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), GpioApiError> {
        let (pid, settings) = self.pin_claimed_by(pin).ok_or(GpioApiError::PinNotClaimed)?;
        if pid != subscriber.pid() {
            log::error!("EnableIrq: access denied, pin {pin:?} is claimed by {pid:} and not {subscriber:?}");
            return Err(GpioApiError::AccessDenied);
        }

        if !settings.is_interrupt() {
            log::error!("EnableIrq: pin {pin:?} isn't configured as interrupt source");
            return Err(GpioApiError::PinNotConfiguredAsIrq);
        }

        self.subscribe_irq_pin(pin, subscriber);
        pin.set_interrupt(true);

        Ok(())
    }
}

impl server::BlockingScalarHandler<SetIrq> for GpioServer {
    fn handle(
        &mut self,
        SetIrq(pin, is_active): SetIrq,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), GpioApiError> {
        let (pid, settings) = self.pin_claimed_by(pin).ok_or(GpioApiError::PinNotClaimed)?;
        if pid != sender {
            log::error!("SetIrq: access denied, pin {pin:?} is claimed by {pid:} and not {sender:}");
            return Err(GpioApiError::AccessDenied);
        }

        if !settings.is_interrupt() {
            log::error!("SetIrq: pin {pin:?} isn't configured as interrupt source");
            return Err(GpioApiError::PinNotConfiguredAsIrq);
        }

        pin.set_interrupt(is_active);

        Ok(())
    }
}

impl server::BlockingScalarHandler<SetPin> for GpioServer {
    fn handle(
        &mut self,
        SetPin(pin, is_high): SetPin,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), GpioApiError> {
        let (pid, settings) = self.pin_claimed_by(pin).ok_or(GpioApiError::PinNotClaimed)?;
        if pid != sender {
            log::error!("SetPin: access denied, pin {pin:?} is claimed by {pid:} and not {sender:}");
            return Err(GpioApiError::AccessDenied);
        }

        if !settings.is_output() {
            log::error!("SetPin: pin {pin:?} isn't configured as digital output");
            return Err(GpioApiError::PinNotConfiguredAsOutput);
        }

        pin.set(is_high);

        Ok(())
    }
}

impl server::BlockingScalarHandler<GetPin> for GpioServer {
    fn handle(
        &mut self,
        GetPin(pin): GetPin,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Result<bool, GpioApiError> {
        let (pid, _settings) = self.pin_claimed_by(pin).ok_or(GpioApiError::PinNotClaimed)?;
        if pid != sender {
            log::error!("GetPin: access denied, pin {pin:?} is claimed by {pid:} and not {sender:}");
            return Err(GpioApiError::AccessDenied);
        }

        Ok(pin.get())
    }
}

impl server::ScalarHandler<SubscriberDisconnected> for GpioServer {
    fn handle(
        &mut self,
        msg: SubscriberDisconnected,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let state =
            unsafe { (&mut *core::ptr::addr_of_mut!(GPIO_SERVER_STATE)).as_mut() }.expect("initialized");
        state.irq_pins_pioa.retain(|(_pin, s)| s.cid() != msg.0);
        state.irq_pins_piob.retain(|(_pin, s)| s.cid() != msg.0);
        state.irq_pins_pioc.retain(|(_pin, s)| s.cid() != msg.0);
        state.irq_pins_piod.retain(|(_pin, s)| s.cid() != msg.0);
    }
}

macro_rules! impl_irq_handler_fn {
    ($fn_name:ident, $mask_fn:ident, $pins:ident) => {
        fn $fn_name(_irq_no: usize, arg: *mut usize) {
            let state = unsafe { &mut *(arg as *mut GpioServerState) };
            let mask = $mask_fn();

            for (pin, subscriber) in &state.$pins {
                if pin.had_irq_fired(mask) {
                    subscriber.send(&IrqMessage { pin: *pin, is_high: pin.get() }).ok();
                }
            }
        }
    };
}

impl_irq_handler_fn!(pioa_irq_handler, pioa_irq_mask, irq_pins_pioa);
impl_irq_handler_fn!(piob_irq_handler, piob_irq_mask, irq_pins_piob);
impl_irq_handler_fn!(pioc_irq_handler, pioc_irq_mask, irq_pins_pioc);
impl_irq_handler_fn!(piod_irq_handler, piod_irq_mask, irq_pins_piod);

pub fn init() -> Result<(), xous::Error> {
    // Enabling PIOA is enough, as B-D are only used as interrupt sources.
    pins::map_gpio_ports()?;
    pins::init_debouncing()?;

    pins::init_flexcom2_pins();
    pins::init_twi_pins();
    pins::init_isc_pins();
    pins::init_spi0_pins();
    pins::init_noise_bias_pin();

    unsafe {
        GPIO_SERVER_STATE = Some(GpioServerState::default());
    }

    init_irq_handlers()?;
    Ok(())
}

fn init_irq_handlers() -> Result<(), xous::Error> {
    log::debug!("Initializing IRQ handlers");

    xous::claim_interrupt(IrqNumber::Pioa, pioa_irq_handler, unsafe {
        (&mut *core::ptr::addr_of_mut!(GPIO_SERVER_STATE)).as_mut().expect("initialized")
            as *mut GpioServerState as *mut usize
    })?;
    xous::claim_interrupt(IrqNumber::Piob, piob_irq_handler, unsafe {
        (&mut *core::ptr::addr_of_mut!(GPIO_SERVER_STATE)).as_mut().expect("initialized")
            as *mut GpioServerState as *mut usize
    })?;

    xous::claim_interrupt(IrqNumber::Pioc, pioc_irq_handler, unsafe {
        (&mut *core::ptr::addr_of_mut!(GPIO_SERVER_STATE)).as_mut().expect("initialized")
            as *mut GpioServerState as *mut usize
    })?;
    xous::claim_interrupt(IrqNumber::Piod, piod_irq_handler, unsafe {
        (&mut *core::ptr::addr_of_mut!(GPIO_SERVER_STATE)).as_mut().expect("initialized")
            as *mut GpioServerState as *mut usize
    })?;

    Ok(())
}
