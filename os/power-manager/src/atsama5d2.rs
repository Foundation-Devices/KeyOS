// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashSet;

use atsama5d27::{
    pmc::{PeripheralId, Pmc},
    rstc::Rstc,
};
use power_manager::{messages::*, PowerManagerError};
use server::{BlockingScalar, BlockingScalarHandler};
use utralib::{HW_PMC_BASE, HW_RSTC_BASE};
use xous::MemoryFlags;

const DMA_CAPABLE_PERIPHERALS: [PeripheralId; 11] = [
    PeripheralId::Xdmac0,
    // Not included because it is owned by the kernel, and the kernel will not go to
    // deep sleep while using it.
    // PeripheralId::Xdmac1,
    PeripheralId::Lcdc,
    PeripheralId::Sdmmc0,
    PeripheralId::Sdmmc1,
    PeripheralId::Isi,
    // Not included because it is only active when the MCU or other DMA peripherals are active
    // PeripheralId::Aesb,
    PeripheralId::Icm,
    PeripheralId::Uhphs,
    PeripheralId::Udphs,
    PeripheralId::Gmac,
    PeripheralId::Can0Int0,
    PeripheralId::Can1Int0,
];

#[derive(server::Server)]
#[name = "os/power-manager"]
pub struct PowerManagerServer {
    pmc: Pmc,
    rstc: Rstc,
    enabled_peripherals: HashSet<PeripheralId>,
    utmi_clock_enabled: bool,
}

#[derive(server::Permissions, Clone, Default)]
#[all_permissions]
#[server_name = "os/power-manager-ext"]
struct ExtPermissions;

impl server::Server for PowerManagerServer {}

impl PowerManagerServer {
    pub fn new() -> Result<Self, PowerManagerError> {
        // Map the PMC
        let pmc_mem = xous::map_memory(
            Some(xous::MemoryAddress::new(HW_PMC_BASE).unwrap()),
            None,
            0x1000,
            MemoryFlags::W | MemoryFlags::DEV,
        )?;

        log::debug!("Initializing PMC");
        let pmc_addr = pmc_mem.as_ptr() as u32;
        let mut pmc = Pmc::with_alt_base_addr(pmc_addr);

        let mut enabled_peripherals = HashSet::default();
        for pid in 2..60 {
            let Ok(pid) = PeripheralId::try_from(pid) else { continue };
            if pmc.is_peripheral_clock_enabled(pid) {
                log::trace!("Found enabled peripheral: {pid:?}");
                enabled_peripherals.insert(pid);
            }
        }

        // Map the RSTC peripheral
        let rstc_mem = xous::map_memory(
            Some(xous::MemoryAddress::new(HW_RSTC_BASE).unwrap()),
            None,
            0x1000,
            MemoryFlags::W | MemoryFlags::DEV,
        )?;

        log::debug!("Initializing RSTC");
        let rstc_addr = rstc_mem.as_ptr() as u32;
        let rstc = Rstc::with_alt_base_addr(rstc_addr);

        log::debug!("Power manager initialized");

        Ok(Self { pmc, rstc, enabled_peripherals, utmi_clock_enabled: false })
    }

    fn update_utmi_clock(&mut self) {
        // The USB peripherals obviously use the UTMI clock, but it is also used as a generic clock input by
        // the SDMMC0 controller (as set up by at91bootstrap)
        if self.enabled_peripherals.contains(&PeripheralId::Uhphs)
            || self.enabled_peripherals.contains(&PeripheralId::Udphs)
            || self.enabled_peripherals.contains(&PeripheralId::Sdmmc0)
        {
            if !self.utmi_clock_enabled {
                log::trace!("Enabling UTMI clock");
                self.pmc.enable_utmi_clock();
                while !self.pmc.is_utmi_clock_ready() {
                    xous::yield_slice()
                }
                self.utmi_clock_enabled = true;
            }
        } else if self.utmi_clock_enabled {
            log::trace!("Disabling UTMI clock");
            self.pmc.disable_utmi_clock();
            self.utmi_clock_enabled = false;
        }
    }

    fn detect_potential_dma(&mut self) {
        let dma_possible = DMA_CAPABLE_PERIPHERALS.iter().any(|m| self.enabled_peripherals.contains(m));
        log::trace!("DMA possible: {dma_possible:?}");
        // We don't know if any DMA is actually in progress, but just to be sure if the peripheral is clocked,
        // assume that it is also doing DMA in the background.
        #[cfg(not(feature = "recovery-os"))]
        xous::set_power_management(if dma_possible {
            xous::DramIdleMode::KeepClocked
        } else {
            xous::DramIdleMode::LowPower
        })
        .ok();
    }
}

impl BlockingScalarHandler<Shutdown> for PowerManagerServer {
    fn handle(&mut self, _msg: Shutdown, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        // Disable USB boost before shutting down, so we don't
        // continue draining the battery into a connected slave device.
        let ext_api = power_manager::PowerManagerExtApi::<ExtPermissions>::default();
        ext_api.set_usb_boost(false).ok();

        xous::rsyscall(xous::SysCall::Shutdown(0)).unwrap();
        panic!("Shutdown syscall did not shut down");
    }
}

impl BlockingScalarHandler<Reboot> for PowerManagerServer {
    fn handle(&mut self, _msg: Reboot, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        self.rstc.do_reset();
        #[allow(clippy::empty_loop)]
        loop {}
    }
}

impl BlockingScalarHandler<SetPeripheralEnabled> for PowerManagerServer {
    fn handle(
        &mut self,
        msg: SetPeripheralEnabled,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetPeripheralEnabled as BlockingScalar>::Response {
        if msg.enabled {
            if !self.enabled_peripherals.contains(&msg.peripheral) {
                log::trace!("Enabling clock of {:?}", msg.peripheral);
                self.pmc.enable_peripheral_clock(msg.peripheral);
                self.enabled_peripherals.insert(msg.peripheral);
            }
        } else if self.enabled_peripherals.contains(&msg.peripheral) {
            log::trace!("Disabling clock of {:?}", msg.peripheral);
            self.pmc.disable_peripheral_clock(msg.peripheral);
            self.enabled_peripherals.remove(&msg.peripheral);
        }

        self.update_utmi_clock();
        self.detect_potential_dma();
    }
}
