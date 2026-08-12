use {
    crate::pmc::PeripheralId,
    utralib::{
        utra::aic::{
            EOICR_ENDIT,
            IDCR_INTD,
            IECR_INTEN,
            IPR0,
            IPR1,
            IPR2,
            IPR3,
            ISR_IRQID,
            IVR,
            SMR_PRIORITY,
            SMR_SRCTYPE,
            SPU_SIVR,
            SSR_INTSEL,
            SVR_VECTOR,
        },
        *,
    },
};

pub struct Aic {
    base_addr: u32,
}

// const UNLOCK_KEY: u32 = 0xB6D81C4D;
const MAX_INTERRUPT_DEPTH: usize = 8;
const MAX_NUM_SOURCES: usize = 127;

#[derive(Debug)]
#[repr(C)]
pub enum SourceKind {
    /// High-level sensitive for internal source. Low-level sensitive for external source.
    LevelSensitive = 0,

    /// Negative-edge triggered for external source.
    ExternalNegativeEdge = 1,

    /// High-level sensitive for internal source. High-level sensitive for external
    /// source.
    ExternalHighLevel = 2,

    /// Positive-edge triggered for external source.
    ExternalPositiveEdge = 3,
}

#[derive(Debug)]
pub struct InterruptEntry {
    pub peripheral_id: PeripheralId,
    pub vector_fn_ptr: usize,
    pub kind: SourceKind,
    pub priority: u32,
}

#[derive(Debug)]
pub struct IrqPendingInfo {
    pub irqs_0_31: u32,
    pub irqs_32_63: u32,
    pub irqs_64_95: u32,
    pub irqs_96_127: u32,
}

impl Default for Aic {
    fn default() -> Aic {
        Self::new()
    }
}

impl Aic {
    #[inline]
    pub fn new() -> Self {
        Self {
            base_addr: HW_AIC_BASE as u32,
        }
    }

    /// Sets the spurious interrupt handler function address.
    #[inline]
    pub fn set_spurious_handler_fn_ptr(&mut self, handler_fn_addr: usize) {
        let mut aic_csr = CSR::new(self.base_addr as *mut u32);
        aic_csr.wfo(SPU_SIVR, handler_fn_addr as u32);
    }

    /// Creates AIC instance with a different base address.
    /// Used with virtual memory or when choosing between secured and non-secured versions
    /// of AIC.
    #[inline]
    pub fn with_alt_base_addr(base_addr: u32) -> Self {
        Self { base_addr }
    }

    #[inline]
    pub fn init(&mut self) {
        let mut aic_csr = CSR::new(self.base_addr as *mut u32);

        // Disable interrupts from all sources
        for i in 0..MAX_NUM_SOURCES {
            // Select interrupt source
            aic_csr.wfo(SSR_INTSEL, i as u32);

            armv7::asm::dsb();
            armv7::asm::isb();

            // Disable interrupt source
            aic_csr.wfo(IDCR_INTD, 1);
        }

        // Pop all possible nested interrupts from internal hw stack
        for _ in 0..MAX_INTERRUPT_DEPTH {
            aic_csr.wfo(EOICR_ENDIT, 1);
        }
    }

    /// Sets the handler for the interrupt.
    #[inline]
    pub fn set_interrupt_handler(&mut self, handler: InterruptEntry) {
        let mut aic_csr = CSR::new(self.base_addr as *mut u32);

        aic_csr.wfo(SSR_INTSEL, handler.peripheral_id as u32);
        aic_csr.rmwf(SMR_SRCTYPE, handler.kind as u32);
        aic_csr.rmwf(SMR_PRIORITY, handler.priority);
        aic_csr.wfo(SVR_VECTOR, handler.vector_fn_ptr as u32);

        // Enable the interrupt
        aic_csr.wfo(IECR_INTEN, 1);
    }

    /// # Panics
    /// Panics if the AIC returned an interrupt source that is unknown.
    #[inline]
    pub fn current_irq_source(&self) -> PeripheralId {
        let aic_csr = CSR::new(self.base_addr as *mut u32);
        (aic_csr.rf(ISR_IRQID) as u8)
            .try_into()
            .expect("invalid peripheral ID")
    }

    #[inline]
    pub fn read_ivr(&mut self) -> u32 {
        let aic_csr = CSR::new(self.base_addr as *mut u32);
        aic_csr.r(IVR)
    }

    /// Should be called from the end of the ISR.
    #[inline]
    pub fn interrupt_completed(&mut self) {
        let mut aic_csr = CSR::new(self.base_addr as *mut u32);
        aic_csr.wfo(EOICR_ENDIT, 1);
    }

    /// Enables or disables specific IRQ.
    #[inline]
    pub fn set_interrupt_enabled(&mut self, id: PeripheralId, enabled: bool) {
        let mut aic_csr = CSR::new(self.base_addr as *mut u32);
        aic_csr.wfo(SSR_INTSEL, id as u32);

        armv7::asm::dsb();
        armv7::asm::isb();

        if enabled {
            aic_csr.wfo(IECR_INTEN, 1);
        } else {
            aic_csr.wfo(IDCR_INTD, 1);
        }
    }

    /// Returns a 128-bit mask of pending IRQs.
    #[inline]
    pub fn get_pending_irqs(&self) -> IrqPendingInfo {
        let aic_csr = CSR::new(self.base_addr as *mut u32);

        IrqPendingInfo {
            irqs_0_31: aic_csr.r(IPR0),
            irqs_32_63: aic_csr.r(IPR1),
            irqs_64_95: aic_csr.r(IPR2),
            irqs_96_127: aic_csr.r(IPR3),
        }
    }
}
