// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use atsama5d27::sdmmc::{
    ADMADesc, DMADescAttr, DataDirection, DmaParams, ErrorStatus, NormalStatus, SDCmd, SDCmdInner,
    SDRespType, Sdmmc,
};
use server::{CheckedConn, CheckedPermissions, MessageAllowed, MessageId as _};
use ticktimer::TicktimerCallback;
use xous::{arch::irq::IrqNumber, keyos::PAGE_SIZE, MemoryRange};

use crate::{
    error::EmmcError,
    messages::*,
    pipeline::{Direction, HardwareImpl, Pipeline},
    BLOCK_SIZE, SD_BUFFER_BLOCKS,
};

power_manager::use_api!();

/// The relative card address, shifted to the position most commands expect.
const RCA_SHIFTED: u32 = 1 << 16;

const POWER_SAVE_AFTER_MS: usize = 500;

/// Production `HardwareImpl` for the atsama5d27 SDMMC + crypto/dma servers.
pub struct KeyOsHardware {
    sdmmc: Sdmmc,
    sdmmc_addr: u32,
    dma_table: MemoryRange,
    dma_table_phys_addr: u32,
    power_manager: PowerManagerApi,
    enabled: bool,
    suspend_callback: Option<TicktimerCallback>,
    crypto_api: crate::CryptoApi,
}

impl KeyOsHardware {
    fn new() -> Result<Self, crate::error::EmmcError> {
        use atsama5d27::pmc::PeripheralId;
        use utralib::HW_SDMMC0_BASE;
        use xous::MemoryFlags;

        let power_manager = PowerManagerApi::default();
        power_manager.enable_peripheral(PeripheralId::Sdmmc0).unwrap();

        log::debug!("Mapping SDMMC0 peripheral");
        let mem = xous::map_memory(
            xous::MemoryAddress::new(HW_SDMMC0_BASE),
            None,
            0x1000,
            MemoryFlags::W | MemoryFlags::DEV,
        )?;
        let sdmmc_addr = mem.as_ptr() as u32;
        log::debug!("Mapped SDMMC0 to 0x{sdmmc_addr:08x}");
        let sdmmc = Sdmmc::with_alt_base_addr(sdmmc_addr);

        let dma_table_size = {
            const ENTRY: usize = core::mem::size_of::<ADMADesc>();
            let needed = ENTRY * SD_BUFFER_BLOCKS;
            assert!(needed <= 4096, "DMA table won't fit into a single page");
            4096
        };
        let dma_table = xous::map_memory(
            None,
            None,
            dma_table_size,
            MemoryFlags::W | MemoryFlags::POPULATE | MemoryFlags::PLAINTEXT | MemoryFlags::NO_CACHE,
        )?;
        let dma_table_phys_addr = xous::virt_to_phys(dma_table.as_ptr() as usize)? as u32;
        log::debug!("DMA table phys: {dma_table_phys_addr:08x}");

        Ok(Self {
            sdmmc,
            sdmmc_addr,
            dma_table,
            dma_table_phys_addr,
            power_manager,
            enabled: true,
            suspend_callback: None,
            crypto_api: crate::CryptoApi::default(),
        })
    }

    fn enable(&mut self) {
        self.power_manager
            .enable_peripheral(atsama5d27::pmc::PeripheralId::Sdmmc0)
            .expect("Could not enable SDMMC0 clock");
        self.sdmmc.enable_clock();
        self.enabled = true;
        let resp = self.sdmmc.send_command(SDCmd::Sd(SDCmdInner::Sleep), SDRespType::R1B, RCA_SHIFTED, None);
        log::debug!("Response to wake: {resp:08x?}");
        let resp =
            self.sdmmc.send_command(SDCmd::Sd(SDCmdInner::SelectCard), SDRespType::R1, RCA_SHIFTED, None);
        log::debug!("Response to select: {resp:08x?}");
    }

    fn disable(&mut self) {
        self.sdmmc.send_command(SDCmd::Sd(SDCmdInner::SelectCard), SDRespType::NoResp, 0, None).ok();
        const SLEEP_BIT: u32 = 1 << 15;
        let resp = self.sdmmc.send_command(
            SDCmd::Sd(SDCmdInner::Sleep),
            SDRespType::R1B,
            RCA_SHIFTED | SLEEP_BIT,
            None,
        );
        log::debug!("Response to sleep: {resp:08x?}");
        self.sdmmc.disable_clock();
        self.power_manager
            .disable_peripheral(atsama5d27::pmc::PeripheralId::Sdmmc0)
            .expect("Could not disable SDMMC0 clock");
        self.enabled = false;
    }
}

impl HardwareImpl for KeyOsHardware {
    fn sdmmc_dma(&mut self, direction: Direction, block_index: u32, blocks: usize, buffer: *mut u8) {
        if !self.enabled {
            self.enable();
        }
        assert!(blocks <= SD_BUFFER_BLOCKS, "kick_sdmmc: blocks > SD_BUFFER_BLOCKS");

        let (cmd, dma_dir) = match direction {
            Direction::Read => (SDCmdInner::ReadMultipleBlocks, DataDirection::Read),
            Direction::Write => (SDCmdInner::WriteMultipleBlocks, DataDirection::Write),
        };
        let dma_params = DmaParams {
            dma_desc_table_phys_addr: self.dma_table_phys_addr,
            direction: dma_dir,
            blocks,
            block_size: BLOCK_SIZE as u16,
        };

        assert_eq!(buffer as usize & (PAGE_SIZE - 1), 0, "Buffer must be page-aligned");
        let buffer_len = blocks * BLOCK_SIZE;
        let pages = buffer_len.next_multiple_of(PAGE_SIZE) / PAGE_SIZE;

        for (i, desc) in (0..pages).zip(self.dma_table.as_slice_mut::<ADMADesc>().iter_mut()) {
            desc.attr = if i == pages - 1 {
                DMADescAttr::TRAN | DMADescAttr::VALID | DMADescAttr::END
            } else {
                DMADescAttr::TRAN | DMADescAttr::VALID
            };
            desc.len = (buffer_len - i * PAGE_SIZE).min(PAGE_SIZE) as u16;
            desc.addr = xous::virt_to_phys(buffer as usize + i * PAGE_SIZE)
                .expect("virt_to_phys failed for DMA buffer page") as u32;
        }

        self.sdmmc.clear_normal_status(self.sdmmc.normal_status());
        self.sdmmc.enable_interrupt_signal(NormalStatus::TRFC);
        self.sdmmc
            .send_command(SDCmd::Sd(cmd), SDRespType::R1, block_index, Some(dma_params))
            .expect("SDMMC hardware error at kick - unrecoverable");
    }

    fn disk_encrypt(
        &mut self,
        tweak: [u8; 16],
        j: usize,
        src: MemoryRange,
        dst: MemoryRange,
        dir: crypto::Direction,
    ) {
        unsafe { self.crypto_api.disk_encrypt_unsafe(tweak, j, src, dst, dir) }
            .expect("disk_encrypt_unsafe pre-DMA validation failed");
    }

    fn idle(&mut self) {
        if let Some(cb) = &self.suspend_callback {
            cb.request(POWER_SAVE_AFTER_MS, Suspend::ID, 0);
        }
    }

    fn cancel_idle(&mut self) {
        if let Some(cb) = &self.suspend_callback {
            cb.cancel(Suspend::ID);
        }
    }

    fn suspend_sdmmc(&mut self) {
        if !self.enabled {
            log::error!("Suspend called twice");
            return;
        }
        self.disable();
    }
}

#[derive(server::Server)]
#[name = "os/emmc"]
pub struct EmmcServer {
    pub pipeline: Pipeline<KeyOsHardware>,
}

impl server::Server for EmmcServer {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        let int_ctx = Box::into_raw(Box::new(InterruptContext {
            conn: CheckedConn::default(),
            sdmmc: Sdmmc::with_alt_base_addr(self.pipeline.hw.sdmmc_addr),
        }));
        xous::claim_interrupt(IrqNumber::Sdmmc0, sdmmc0_irq_handler, int_ctx as *mut usize)
            .expect("emmc: claim SDMMC0 IRQ failed");
        self.pipeline.hw.sdmmc.enable_error_interrupt_signal(ErrorStatus::all());

        self.pipeline
            .hw
            .crypto_api
            .subscribe_disk_encrypt_complete(context)
            .expect("subscribe to crypto DiskEncryptComplete");

        let cb = TicktimerCallback::new(context.sid()).expect("emmc: ticktimer connect failed");
        cb.request(POWER_SAVE_AFTER_MS, <Suspend as server::MessageId>::ID, 0);
        self.pipeline.hw.suspend_callback = Some(cb);
    }
}

impl EmmcServer {
    pub fn new() -> Result<Self, EmmcError> {
        let hw = KeyOsHardware::new()?;
        Ok(EmmcServer { pipeline: Pipeline::new(hw) })
    }
}

#[derive(Default, Clone)]
struct IrqConn;

impl CheckedPermissions for IrqConn {
    const NAME: &str = "os/emmc";
}

impl MessageAllowed<SdmmcDone> for IrqConn {}

struct InterruptContext {
    pub conn: CheckedConn<IrqConn>,
    pub sdmmc: Sdmmc,
}

fn sdmmc0_irq_handler(_irq_no: usize, arg: *mut usize) {
    let ctx = unsafe { &mut *(arg as *mut InterruptContext) };
    let ns = ctx.sdmmc.normal_status();
    if ns.contains(NormalStatus::TRFC) || ns.contains(NormalStatus::ERRINT) {
        if ns.contains(NormalStatus::ERRINT) {
            let es = ctx.sdmmc.error_status();
            ctx.sdmmc.clear_error_status(es);
            log::error!("SDMMC ERRINT: {ns:?} {es:?}");
        }
        ctx.sdmmc.clear_normal_status(ns & (NormalStatus::TRFC | NormalStatus::ERRINT));
        ctx.sdmmc.enable_interrupt_signal(NormalStatus::empty());
        ctx.conn.send_scalar_nowait(SdmmcDone).ok();
    }
}
