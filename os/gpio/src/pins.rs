// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    atsama5d27::pio::{Direction, Event, Func, Pio, PioA, PioB, PioC, PioD, PioPort, SecurePio},
    gpio::{GpioPin, PinSettings},
    utralib::{HW_PIO_BASE, HW_SPIO_BASE},
};

const SLOW_CLOCK_FREQ: u32 = 32763;

static mut PIO_ADDR: Option<u32> = None;
static mut SPIO_ADDR: Option<u32> = None;

pub(crate) fn map_gpio_ports() -> Result<(), xous::Error> {
    log::debug!("Mapping GPIO ports");

    let mem = xous::map_memory(
        xous::MemoryAddress::new(HW_PIO_BASE),
        None,
        0x1000,
        xous::MemoryFlags::W | xous::MemoryFlags::DEV,
    )?;
    let addr = mem.as_ptr() as u32;
    log::debug!("Mapped PIO to 0x{:08x}", addr);
    unsafe {
        PIO_ADDR = Some(addr);
    }

    let mem = xous::map_memory(
        xous::MemoryAddress::new(HW_SPIO_BASE),
        None,
        0x1000,
        xous::MemoryFlags::W | xous::MemoryFlags::DEV,
    )?;
    let addr = mem.as_ptr() as u32;
    unsafe {
        SPIO_ADDR = Some(addr);
    }

    Ok(())
}

const DEBOUNCE_FILTER_FREQ_CUTOFF_HZ: u32 = 10;
pub(crate) fn init_debouncing() -> Result<(), xous::Error> {
    log::debug!("Initializing debouncing filter");

    let spio = SecurePio::with_alt_base_addr(unsafe { SPIO_ADDR }.expect("SPIO not mapped"));
    if spio.is_write_protected() {
        spio.set_write_protected(false);
    }
    spio.set_debounce_filter(SLOW_CLOCK_FREQ, DEBOUNCE_FILTER_FREQ_CUTOFF_HZ);
    Ok(())
}

pub(crate) fn init_twi_pins() {
    let addr = unsafe { PIO_ADDR.unwrap() };

    let mut scl = Pio::pc28();
    scl.set_alt_base_addr(addr);
    scl.set_func(Func::E); // TWI

    let mut sda = Pio::pc27();
    sda.set_alt_base_addr(addr);
    sda.set_func(Func::E); // TWI
}

pub(crate) fn init_isc_pins() {
    let addr = unsafe { PIO_ADDR.unwrap() };

    // Assign from PC13 to PC24 to func C which is ISC
    PioC::configure_pins_by_mask(Some(addr), 0x1ffe000, Func::C, None);
}

pub(crate) fn init_flexcom2_pins() {
    let addr = unsafe { PIO_ADDR.unwrap() };

    let mut flexcom_tx = Pio::pd26();
    flexcom_tx.set_alt_base_addr(addr);
    flexcom_tx.set_func(Func::C);

    let mut flexcom_rx = Pio::pd27();
    flexcom_rx.set_alt_base_addr(addr);
    flexcom_rx.set_func(Func::C);
}

pub(crate) fn init_noise_bias_pin() { GpioPin::NoiseEn.configure(PinSettings::OutputHigh, false); }

pub(crate) fn init_spi0_pins() {
    let addr = unsafe { PIO_ADDR.unwrap() };
    // SPI pins
    let mut sck = Pio::pa14();
    sck.set_alt_base_addr(addr);
    sck.set_func(Func::A); // SPI0_SPCK
    let mut mosi = Pio::pa15();
    mosi.set_alt_base_addr(addr);
    mosi.set_func(Func::A); // SPI0_MOSI
    let mut miso = Pio::pa16();
    miso.set_alt_base_addr(addr);
    miso.set_func(Func::A); // SPI0_MISO
    let mut cs0 = Pio::pa17();
    cs0.set_alt_base_addr(addr);
    cs0.set_func(Func::A); // SPI0_NPCS0
    let mut cs1 = Pio::pa18();
    cs1.set_alt_base_addr(addr);
    cs1.set_func(Func::A); // SPI0_NPCS1
    let mut cs2 = Pio::pa19();
    cs2.set_alt_base_addr(addr);
    cs2.set_func(Func::A); // SPI0_NPCS2
}

#[derive(Debug, Copy, Clone)]
#[allow(dead_code)]
pub enum Port {
    A,
    B,
    C,
    D,
}

macro_rules! impl_pin_irq_check {
    ($self:ident, $mask:ident, [$($name:ident => ($port:ident, $pin:ident)),+]) => {
        match $self {
            $(
                GpioPin::$name => {
                    let pin = Pio::$pin();
                    let pin_id = pin.pin_id();
                    let pin_bit = 1 << pin_id;

                    $mask & pin_bit != 0
                }
            )+
        }
    }
}

macro_rules! impl_pin_set {
    ($self:ident, $hi:ident, [$($name:ident => ($_port:ident, $pin:ident)),+]) => {
        match $self {
            $(
                GpioPin::$name => {
                    let mut pin = Pio::$pin();
                    pin.set_alt_base_addr(unsafe { PIO_ADDR.expect("PIO not mapped") });
                    pin.set($hi);
                }
            )+
        }
    }
}

macro_rules! impl_pin_get {
    ($self:ident, [$($name:ident => ($_port:ident, $pin:ident)),+]) => {
        match $self {
            $(
                GpioPin::$name => {
                    let mut pin = Pio::$pin();
                    pin.set_alt_base_addr(unsafe { PIO_ADDR.expect("PIO not mapped") });
                    pin.get()
                }
            )+
        }
    }
}

macro_rules! impl_pin_port {
    ($self:ident, [$($name:ident => ($port:ident, $pin:ident)),+]) => {
        match $self {
            $(
                GpioPin::$name => Port::$port,
            )+
        }
    }
}

macro_rules! impl_pin_configure {
    ($self:ident, $settings:ident, $debounce:ident, [$($name:ident => ($_port:ident, $pin:ident)),+]) => {
        match $self {
            $(
                GpioPin::$name => {
                    let mut pin = Pio::$pin();
                    log::debug!("PIO addr: {:08x?}", unsafe { PIO_ADDR });
                    pin.set_alt_base_addr(unsafe { PIO_ADDR.expect("PIO not mapped") });

                    log::debug!("Setting as GPIO");
                    pin.set_func(Func::Gpio);

                    match $settings {
                        PinSettings::Input => {
                            pin.set_direction(Direction::Input);
                        }
                        PinSettings::OutputHigh => {
                            pin.set_direction(Direction::Output);
                            pin.set(true);
                        }
                        PinSettings::OutputLow => {
                            pin.set_direction(Direction::Output);
                            pin.set(false);
                        }
                        PinSettings::OutputOpenDrainHighZ => {
                            pin.set_direction(Direction::Output);
                            pin.set_open_drain(true);
                            pin.set(true);
                        }
                        PinSettings::OutputOpenDrainLow => {
                            pin.set_direction(Direction::Output);
                            pin.set_open_drain(true);
                            pin.set(false);
                        }
                        PinSettings::InterruptFalling => {
                            pin.set_direction(Direction::Input);
                            pin.set_event_detection(Event::Falling);
                        }
                        PinSettings::InterruptRising => {
                            pin.set_direction(Direction::Input);
                            pin.set_event_detection(Event::Rising);
                        }
                        PinSettings::InterruptBoth => {
                            log::debug!("Setting direction Input");
                            pin.set_direction(Direction::Input);

                            log::debug!("Setting event detection Both");
                            pin.set_event_detection(Event::Both);
                        }
                    }

                    log::debug!("Setting debounce: {}", $debounce);
                    pin.set_debounce_filter($debounce);
                }
            )+
        }
    }
}

macro_rules! impl_set_interrupt {
    ($self:ident, $interrupt:ident, [$($name:ident => ($_port:ident, $pin:ident)),+]) => {
        match $self {
            $(
                GpioPin::$name => {
                    let mut pin = Pio::$pin();
                    pin.set_alt_base_addr(unsafe { PIO_ADDR.expect("PIO not mapped") });
                    pin.set_interrupt($interrupt);
                }
            )+
        }
    }
}

pub(crate) trait GpioPinOperations {
    fn set(&self, hi: bool);
    fn get(&self) -> bool;

    fn had_irq_fired(&self, mask: u32) -> bool;

    fn configure(&self, pin_settings: PinSettings, debounce: bool);

    fn port(&self) -> Port;

    fn set_interrupt(&self, interrupt: bool);
}

macro_rules! impl_gpio_pin {
    ($pins:tt) => {
        impl GpioPinOperations for GpioPin {
            fn set(&self, hi: bool) {
                impl_pin_set!(self, hi, $pins);
            }

            fn get(&self) -> bool { impl_pin_get!(self, $pins) }

            // # Internal note:
            // Use `PortX::get_interrupt_status()` for every port to get fresh IRQ masks.
            // Then, for every pin, call this function and provide the mask as an argument.
            fn had_irq_fired(&self, mask: u32) -> bool { impl_pin_irq_check!(self, mask, $pins) }

            fn configure(&self, pin_settings: PinSettings, debounce: bool) {
                impl_pin_configure!(self, pin_settings, debounce, $pins);
                log::debug!("Done configuring");
            }

            fn port(&self) -> Port { impl_pin_port!(self, $pins) }

            fn set_interrupt(&self, interrupt: bool) { impl_set_interrupt!(self, interrupt, $pins) }
        }
    };
}

pub(crate) fn pioa_irq_mask() -> u32 {
    if let Some(pio_base) = unsafe { PIO_ADDR } {
        PioA::get_interrupt_status(pio_base) & PioA::get_interrupt_mask(pio_base)
    } else {
        unreachable!("PIO isn't mapped")
    }
}

pub(crate) fn piob_irq_mask() -> u32 {
    if let Some(pio_base) = unsafe { PIO_ADDR } {
        PioB::get_interrupt_status(pio_base) & PioB::get_interrupt_mask(pio_base)
    } else {
        unreachable!("PIO isn't mapped")
    }
}
pub(crate) fn pioc_irq_mask() -> u32 {
    if let Some(pio_base) = unsafe { PIO_ADDR } {
        PioC::get_interrupt_status(pio_base) & PioC::get_interrupt_mask(pio_base)
    } else {
        unreachable!("PIO isn't mapped")
    }
}
pub(crate) fn piod_irq_mask() -> u32 {
    if let Some(pio_base) = unsafe { PIO_ADDR } {
        PioD::get_interrupt_status(pio_base) & PioD::get_interrupt_mask(pio_base)
    } else {
        unreachable!("PIO isn't mapped")
    }
}

impl_gpio_pin!([
    UsbOtgId => (A, pa20),
    LedDrvPwdnB => (A, pa22),
    AlsIrqB => (A, pa24),
    PowerButton => (A, pa25),
    BatChgStat => (A, pa26),
    AcclIntB => (A, pa28),
    BatChgOtg => (A, pa29),
    NoiseEn => (A, pa31),
    CamPwdn => (B, pb0),
    LcdRstB => (B, pb1),
    CtpRstB => (B, pb2),
    CamLdoPwdnB => (B, pb3),
    BtIrqB => (B, pb6),
    FuelIrqB => (B, pb10),
    UsbCtrlIrqB => (C, pc9),
    HfbIn => (C, pc11),
    BtEepWpB => (C, pc12),
    BtRst => (C, pc25),
    NfcIrqB => (C, pc29),
    NfcIntB => (C, pc31),
    CtpIrqB => (D, pd9),
    BatChgEnB => (D, pd20),
    HfbEn => (D, pd21),
    LedChgPmpEn => (D, pd23),
    UsbVbusIrq => (D, pd25)
]);
