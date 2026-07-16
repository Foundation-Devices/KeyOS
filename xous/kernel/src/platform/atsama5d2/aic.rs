// SPDX-FileCopyrightText: 2022 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use atsama5d27::{
    aic::{Aic, InterruptEntry, SourceKind},
    pmc::PeripheralId,
};
use keyos::{AIC_KERNEL_ADDR, SAIC_KERNEL_ADDR};
use xous::arch::irq::IrqNumber;

pub static mut AIC_KERNEL: Option<AicKernel> = None;
pub static mut SAIC_KERNEL: Option<AicKernel> = None;

const NO_MORE_IRQS: usize = 0xFFFFFFFF;

pub struct AicKernel {
    base_addr: usize,
    pub inner: Option<Aic>,
}

impl AicKernel {
    pub fn new(addr: usize) -> AicKernel { AicKernel { base_addr: addr, inner: None } }

    pub fn init(&mut self) {
        let mut aic = Aic::with_alt_base_addr(self.base_addr as u32);
        aic.init();
        aic.set_spurious_handler_fn_ptr(NO_MORE_IRQS);
        self.inner = Some(aic);
    }

    pub fn enable_interrupt(&mut self, kind: SourceKind, id: PeripheralId) {
        if let Some(aic) = &mut self.inner {
            let handler = InterruptEntry { peripheral_id: id, vector_fn_ptr: id as usize, kind, priority: 0 };
            aic.set_interrupt_handler(handler);
        } else {
            panic!("AIC is not initialized")
        }
    }

    pub fn disable_interrupt(&mut self, id: PeripheralId) {
        if let Some(aic) = &mut self.inner {
            aic.set_interrupt_enabled(id, false);
        } else {
            panic!("AIC is not initialized")
        }
    }

    pub fn get_pending_irq(&mut self) -> Option<PeripheralId> {
        if let Some(aic) = &mut self.inner {
            let ivr = aic.read_ivr();
            if ivr as usize == NO_MORE_IRQS {
                None
            } else {
                (ivr as u8).try_into().ok()
            }
        } else {
            panic!("AIC is not initialized")
        }
    }

    pub fn interrupt_completed(&mut self) {
        if let Some(aic) = &mut self.inner {
            aic.interrupt_completed()
        } else {
            panic!("AIC is not initialized")
        }
    }
}

pub fn init() {
    let mut aic_kernel = AicKernel::new(AIC_KERNEL_ADDR);
    aic_kernel.init();

    unsafe {
        AIC_KERNEL = Some(aic_kernel);
    }

    let mut saic_kernel = AicKernel::new(SAIC_KERNEL_ADDR);
    saic_kernel.init();

    unsafe {
        SAIC_KERNEL = Some(saic_kernel);
    }
}

fn peripheral_id_to_irq_no(id: PeripheralId) -> IrqNumber {
    klog!("Pending IRQ: {:?}", id);

    match id {
        PeripheralId::Pit => IrqNumber::PeriodicIntervalTimer,

        PeripheralId::Uart0 => IrqNumber::Uart0,
        PeripheralId::Uart1 => IrqNumber::Uart1,
        PeripheralId::Uart2 => IrqNumber::Uart2,
        PeripheralId::Uart3 => IrqNumber::Uart3,
        PeripheralId::Uart4 => IrqNumber::Uart4,

        PeripheralId::Pioa => IrqNumber::Pioa,
        PeripheralId::Piob => IrqNumber::Piob,
        PeripheralId::Pioc => IrqNumber::Pioc,
        PeripheralId::Piod => IrqNumber::Piod,

        PeripheralId::Isi => IrqNumber::Isi,
        PeripheralId::Lcdc => IrqNumber::Lcdc,

        PeripheralId::Uhphs => IrqNumber::Uhphs,
        PeripheralId::Udphs => IrqNumber::Udphs,

        PeripheralId::Tc0 => IrqNumber::Tc0,
        PeripheralId::Tc1 => IrqNumber::Tc1,

        PeripheralId::Sdmmc0 => IrqNumber::Sdmmc0,

        PeripheralId::Xdmac0 => IrqNumber::Xdmac0,
        PeripheralId::Xdmac1 => IrqNumber::Xdmac1,

        PeripheralId::Sys => IrqNumber::Sys,

        PeripheralId::Flexcom2 => IrqNumber::Flexcom2,

        PeripheralId::Secumod => IrqNumber::Secumod,
        PeripheralId::Icm => IrqNumber::Icm,

        PeripheralId::Twi0 => IrqNumber::Twi0,

        _ => panic!("Unable to find IrqNumber for PeripheralId: {:?}", id),
    }
}

pub fn set_irq_enabled(irq_no: IrqNumber, enabled: bool) {
    let sama5d2_irq_no = match irq_no {
        IrqNumber::PeriodicIntervalTimer => PeripheralId::Pit,

        IrqNumber::Uart0 => PeripheralId::Uart0,
        IrqNumber::Uart1 => PeripheralId::Uart1,
        IrqNumber::Uart2 => PeripheralId::Uart2,
        IrqNumber::Uart3 => PeripheralId::Uart3,
        IrqNumber::Uart4 => PeripheralId::Uart3,

        IrqNumber::Pioa => PeripheralId::Pioa,
        IrqNumber::Piob => PeripheralId::Piob,
        IrqNumber::Pioc => PeripheralId::Pioc,
        IrqNumber::Piod => PeripheralId::Piod,

        IrqNumber::Isi => PeripheralId::Isi,
        IrqNumber::Lcdc => PeripheralId::Lcdc,

        IrqNumber::Uhphs => PeripheralId::Uhphs,
        IrqNumber::Udphs => PeripheralId::Udphs,

        IrqNumber::Tc0 => PeripheralId::Tc0,
        IrqNumber::Tc1 => PeripheralId::Tc1,

        IrqNumber::Sdmmc0 => PeripheralId::Sdmmc0,

        IrqNumber::Xdmac0 => PeripheralId::Xdmac0,
        IrqNumber::Xdmac1 => PeripheralId::Xdmac1,

        IrqNumber::Sys => PeripheralId::Sys,

        IrqNumber::Flexcom2 => PeripheralId::Flexcom2,

        IrqNumber::Secumod => PeripheralId::Secumod,
        IrqNumber::Icm => PeripheralId::Icm,

        IrqNumber::Twi0 => PeripheralId::Twi0,
    };

    unsafe {
        let aic = (&mut *core::ptr::addr_of_mut!(AIC_KERNEL)).as_mut().expect("AIC is not initialized");

        if enabled {
            aic.enable_interrupt(SourceKind::LevelSensitive, sama5d2_irq_no);
        } else {
            aic.disable_interrupt(sama5d2_irq_no);
        }
    }
}

pub fn get_pending_irq() -> Option<IrqNumber> {
    unsafe {
        let aic = (&mut *core::ptr::addr_of_mut!(AIC_KERNEL)).as_mut().expect("AIC is not initialized");
        Some(peripheral_id_to_irq_no(aic.get_pending_irq()?))
    }
}

pub fn acknowledge_irq() {
    unsafe {
        let aic = (&mut *core::ptr::addr_of_mut!(AIC_KERNEL)).as_mut().expect("AIC is not initialized");
        aic.interrupt_completed();
    };
}
