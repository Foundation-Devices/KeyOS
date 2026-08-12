// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use {
    bitflags::*,
    utralib::{utra::sdmmc0::*, CSR, HW_SDMMC0_BASE, HW_SDMMC1_BASE},
};

const WAIT_TIMEOUT: usize = 1_000_000;

const OFFSET_HC1R: u32 = 0x28;
const OFFSET_H1CR_DMASEL: u8 = 3;
const OFFSET_CR: u32 = 0x0E;
const OFFSET_TMR: u32 = 0x0C;
const OFFSET_BSR: u32 = 0x04;
const OFFSET_BCR: u32 = 0x06;
const OFFSET_CCR: u32 = 0x2C;
const OFFSET_TCR: u32 = 0x2e;
const OFFSET_NISTR: u32 = 0x30;
const OFFSET_NISTER: u32 = 0x34;
const OFFSET_NISIER: u32 = 0x38;
const OFFSET_EISTR: u32 = 0x32;
const OFFSET_EISTER: u32 = 0x36;
const OFFSET_EISIER: u32 = 0x3A;
const OFFSET_RR0: u32 = 0x10;
const OFFSET_PCR: u32 = 0x29;

#[derive(Debug)]
pub struct DmaParams {
    pub dma_desc_table_phys_addr: u32,
    pub direction: DataDirection,
    pub blocks: usize,
    pub block_size: u16,
}

pub enum DmaSel {
    SDma = 0x0,
    ADma32 = 0x2,
}

#[derive(Debug)]
pub enum RespType {
    /// No Response
    NoResp = 0,
    /// Response Length 136
    Rl136 = 1,
    /// Response Length 48
    Rl48 = 2,
    /// Response Length 48 with Busy
    Rl48Busy = 3,
}

#[derive(Debug)]
pub enum SdMmcError {
    InhibitWaitTimeout,
    TooManyBlocksRequested,
    NormalStatusWaitTimeout,
    Error(ErrorStatus),
}

#[derive(Debug, Copy, Clone)]
pub enum SDRespType {
    NoResp = 0x00,
    R1 = 0x10,
    R1B = 0x11,
    R2 = 0x20,
    R3 = 0x30,
    R4 = 0x40,
    R5 = 0x50,
    R6 = 0x60,
    R7 = 0x70,
}

bitflags! {
    #[derive(Debug, Default, Copy, Clone)]
    pub struct DMADescAttr: u16 {
        const VALID = 1 << 0;
        const END   = 1 << 1;
        const INT   = 1 << 2;
        const ACT1  = 1 << 4;
        const ACT2  = 1 << 5;
        const TRAN  = Self::ACT2.bits();
        const LINK  = Self::ACT1.bits() | Self::ACT1.bits();
    }
}

#[derive(Debug, Default, Copy, Clone)]
#[repr(C, align(4))]
pub struct ADMADesc {
    pub attr: DMADescAttr,
    pub len: u16,
    pub addr: u32,
}

#[derive(Debug, Copy, Clone)]
pub enum DataDirection {
    Read = 0x11,
    Write = 0x22,
}

#[derive(Debug, Copy, Clone)]
pub enum SDCmdInner {
    GoIdleState = 0,
    AllSendCid = 2,
    SendRelativeAddr = 3,
    Sleep = 5,
    SwitchFun = 6,
    SelectCard = 7,
    SendIfCond = 8,
    SendCsd = 9,
    SendCid = 10,
    StopTransmission = 12,
    SendStatus = 13,
    SetBlockLen = 16,
    ReadSingleBlock = 17,
    ReadMultipleBlocks = 18,
    SetBlockCount = 23,
    WriteSingleBlock = 24,
    WriteMultipleBlocks = 25,
    EraseGroupStart = 35,
    EraseGroupEnd = 36,
    Erase = 38,

    AppCmd = 55,
}

#[derive(Debug, Copy, Clone)]
pub enum SdAppCommand {
    AppSetBusWidth = 6,
    AppSdStatus = 13,
    AppSdSendOpCond = 41,
    AppSendScr = 51,
}

#[derive(Debug, Copy, Clone)]
pub enum MmcCommand {
    SendOpCond = 1,
    SendExtCsd = 8,
    BusTestR = 14,
    BusTestW = 19,
}

#[derive(Debug, Copy, Clone)]
pub enum SDCmd {
    Sd(SDCmdInner),
    SdApp(SdAppCommand),
    Mmc(MmcCommand),
}

impl From<SDCmd> for u32 {
    fn from(value: SDCmd) -> Self {
        match value {
            SDCmd::Sd(cmd) => cmd as u32,
            SDCmd::SdApp(cmd) => cmd as u32,
            SDCmd::Mmc(cmd) => cmd as u32,
        }
    }
}

#[derive(Debug, Copy, Clone)]
enum AutoCmd {
    #[allow(dead_code)]
    Cmd12 = 1,
    Cmd23 = 2,
}

pub type SdResponse = [u32; 4];

bitflags! {
    #[derive(Debug, Copy, Clone)]
    pub struct SdmmcStatus: u32 {
        /// CMD Line level.
        /// This status is used to check the CMD line level to recover from errors, and for debugging.
        const CMDLL = 1 << 24;
        /// DAT3 pin level.
        const DAT3  = 1 << 23;
        /// DAT2 pin level.
        const DAT2  = 1 << 22;
        /// DAT1 pin level.
        const DAT1  = 1 << 21;
        /// DAT0 pin level.
        const DAT0  = 1 << 20;
        /// Write Protect pin level.
        const WRPPL = 1 << 19;
        /// Card Detect pin level (inverse).
        const CARDDPL = 1 << 18;
        /// Card State Stable.
        const CARDSS = 1 << 17;
        /// Card Inserted.
        const CARDINS = 1 << 16;
        /// Buffer Read Enable.
        const BUFRDEN = 1 << 11;
        /// Buffer Write Enable.
        const BUFWREN = 1 << 10;
        /// Read Transfer Active.
        const RTACT = 1 << 9;
        /// Write Transfer Active.
        const WTACT = 1 << 8;
        /// DAT Line Active.
        const DLACT = 1 << 2;
        /// Command Inhibit (DAT).
        const CMDINHD = 1 << 1;
        /// Command Inhibit (CMD).
        const CMDINHC = 1 << 0;
    }
}

bitflags! {
    #[derive(Debug, Copy, Clone)]
    pub struct NormalStatus: u16 {
        const CMDC   = 0x1 << 0;  /* Command Complete */
        const TRFC   = 0x1 << 1;  /* Transfer Complete */
        const BLKGE  = 0x1 << 2;  /* Block Gap Event */
        const DMAINT = 0x1 << 3;  /* DMA Interrupt */
        const BWRRDY = 0x1 << 4;  /* Buffer Write Ready */
        const BRDRDY = 0x1 << 5;  /* Buffer Read Ready */
        const CINS   = 0x1 << 6;  /* Card Insertion */
        const CREM   = 0x1 << 7;  /* Card Removal */
        const CINT   = 0x1 << 8;  /* Card Interrupt */
        const BOOTAR = 0x1 << 14;  /* Boot Acknowledge Received */
        const ERRINT = 0x1 << 15;  /* Error Interrupt */
    }
}

bitflags! {
    #[derive(Debug, Copy, Clone)]
    pub struct ErrorStatus: u16 {
        const CMDTEO = 0x1 << 0;  /* Command Timeout Error */
        const CMDCRC = 0x1 << 1;  /* Command CRC Error */
        const CMDEND = 0x1 << 2;  /* Command End Bit Error */
        const CMDIDX = 0x1 << 3;  /* Command Index Error */
        const DATTEO = 0x1 << 4;  /* Data Timeout Error */
        const DATCRC = 0x1 << 5;  /* Data CRC Error */
        const DATEND = 0x1 << 6;  /* Data End Bit Error */
        const CURLIM = 0x1 << 7;  /* Current Limit Error */
        const ACMD   = 0x1 << 8;  /* Auto CMD Error */
        const ADMA   = 0x1 << 9;  /* ADMA Error */
        const BOOTAE = 0x1 << 12; /* Boot Acknowledge Error */
    }
}

pub struct Sdmmc {
    base_addr: u32,
}

impl Sdmmc {
    #[inline]
    pub fn sdmmc0() -> Self {
        Self {
            base_addr: HW_SDMMC0_BASE as u32,
        }
    }

    #[inline]
    pub fn sdmmc1() -> Self {
        Self {
            base_addr: HW_SDMMC1_BASE as u32,
        }
    }

    #[inline]
    pub fn with_alt_base_addr(base_addr: u32) -> Self {
        Self { base_addr }
    }

    #[inline]
    pub fn disable_clock(&mut self) {
        let mut clock_reg = self.read_u16_reg(OFFSET_CCR);
        clock_reg &= !(1 << 2); // SDCLKEN
        self.write_u16_reg(OFFSET_CCR, clock_reg);
    }

    #[inline]
    pub fn enable_power(&mut self) {
        self.write_byte_reg(OFFSET_PCR, 1);
    }

    #[inline]
    pub fn disable_power(&mut self) {
        self.write_byte_reg(OFFSET_PCR, 0);
    }

    #[inline]
    pub fn enable_clock(&mut self) {
        let mut clock_reg = self.read_u16_reg(OFFSET_CCR);
        clock_reg |= 1 << 2; // SDCLKEN
        self.write_u16_reg(OFFSET_CCR, clock_reg);
    }

    #[inline]
    pub fn status(&self) -> SdmmcStatus {
        let csr = CSR::new(self.base_addr as *mut u32);
        SdmmcStatus::from_bits_truncate(csr.r(PSR))
    }

    #[inline]
    pub fn normal_status(&self) -> NormalStatus {
        NormalStatus::from_bits_truncate(self.read_u16_reg(OFFSET_NISTR))
    }

    #[inline]
    pub fn error_status(&self) -> ErrorStatus {
        ErrorStatus::from_bits_truncate(self.read_u16_reg(OFFSET_EISTR))
    }

    /// Sends the command to the card then waits for the response.
    /// In case of block(s) read/write, uses ADMA so the application code must wait for
    /// the `TRFC` (transfer complete) interrupt in order to get the valid data or
    /// proceed to the next command.
    #[inline]
    pub fn send_command(
        &mut self,
        cmd: SDCmd,
        resp_type: SDRespType,
        argu: u32,
        data: Option<DmaParams>,
    ) -> Result<SdResponse, SdMmcError> {
        self.inhibit_wait(if data.is_some() {
            SdmmcStatus::CMDINHD | SdmmcStatus::CMDINHC
        } else {
            SdmmcStatus::CMDINHC
        })?;

        // ERRINT is read-only and mirrors EISTR, so a bit left over from an earlier command makes
        // this one look like it failed the moment we poll.
        self.clear_error_status(ErrorStatus::all());

        let mut csr = CSR::new(self.base_addr as *mut u32);
        let mut cmd_reg = (u32::from(cmd) << 8) as u16;
        let mut wait_mask = NormalStatus::CMDC;

        match resp_type {
            SDRespType::R1 | SDRespType::R5 | SDRespType::R6 | SDRespType::R7 => {
                cmd_reg |= RespType::Rl48 as u16;
                cmd_reg |= 1 << 3; // CR_CMDCCEN
                cmd_reg |= 1 << 4; // CR_CMDICEN
            }

            SDRespType::R1B => {
                cmd_reg |= RespType::Rl48Busy as u16;
                cmd_reg |= 1 << 3; // CR_CMDCCEN
                cmd_reg |= 1 << 4; // CR_CMDICEN

                wait_mask |= NormalStatus::TRFC;
            }

            SDRespType::R2 => {
                cmd_reg |= RespType::Rl136 as u16;
                cmd_reg |= 1 << 4; // CR_CMDICEN
            }

            SDRespType::R3 | SDRespType::R4 => cmd_reg |= RespType::Rl48 as u16,

            _ => cmd_reg |= RespType::NoResp as u16,
        }

        if let Some(dma_params) = data {
            self.write_byte_reg(
                OFFSET_HC1R,
                self.read_byte_reg(OFFSET_HC1R) | (DmaSel::ADma32 as u8) << OFFSET_H1CR_DMASEL,
            );

            cmd_reg |= 1 << 5; // CR_DPSEL

            let multi_block = matches!(
                cmd,
                SDCmd::Sd(SDCmdInner::ReadMultipleBlocks | SDCmdInner::WriteMultipleBlocks)
            );

            let mut tmr = 1 << 1; // TMR_BCEN
                                  // CMD18/CMD25 stream until something stops them, so they need multi-block mode and a
                                  // block count even for one block. Auto CMD23 is what stops them.
            if multi_block || dma_params.blocks > 1 {
                tmr |= 1 << 5; // TMR_MSBSEL
            }
            if let DataDirection::Read = dma_params.direction {
                tmr |= 1 << 4; // TMR_DTDSEL_READ
            }
            // Enable DMA for these commands
            tmr |= 1; // TMR_DMAEN

            // Auto CMD23 rather than Auto CMD12: CMD12 trails the data phase, and this controller
            // raises TRFC while it is still in flight, so its response could not be checked.
            if multi_block {
                tmr |= (AutoCmd::Cmd23 as u16) << 2;
                csr.wo(SSAR, dma_params.blocks as u32);
            }

            self.write_byte_reg(OFFSET_TCR, 0xe);
            self.write_u16_reg(OFFSET_BSR, dma_params.block_size);
            // Always, or else a stale count from an earlier transfer disagrees with SSAR.
            self.write_u16_reg(OFFSET_BCR, dma_params.blocks as u16);

            self.write_u16_reg(OFFSET_TMR, tmr);

            // Configure DMA descriptors
            csr.wo(ASAR0, dma_params.dma_desc_table_phys_addr);
        }

        csr.wo(ARG1R, argu);
        self.write_u16_reg(OFFSET_CR, cmd_reg); // Send the command

        self.wait_normal_status(wait_mask)?;

        let last_status = self.normal_status();
        self.clear_normal_status(last_status);

        if last_status.contains(NormalStatus::ERRINT) {
            return Err(SdMmcError::Error(self.error_status()));
        }

        // Read the command response
        let mut resp = [0; 4];
        if let SDRespType::R2 = resp_type {
            for (i, resp) in resp.iter_mut().enumerate() {
                *resp = self.read_u32_reg(OFFSET_RR0 + 4 * i as u32);
            }
        } else {
            resp[0] = self.read_u32_reg(OFFSET_RR0);
        }

        Ok(resp)
    }

    /// Unmask status flags so the controller records them.
    #[inline]
    pub fn enable_status(&mut self, normal: NormalStatus, error: ErrorStatus) {
        self.write_u16_reg(OFFSET_NISTER, normal.bits());
        self.write_u16_reg(OFFSET_EISTER, error.bits());
    }

    #[inline]
    pub fn enable_interrupt_signal(&mut self, interrupts: NormalStatus) {
        self.write_u16_reg(OFFSET_NISIER, interrupts.bits());
    }

    #[inline]
    pub fn enable_error_interrupt_signal(&mut self, interrupts: ErrorStatus) {
        self.write_u16_reg(OFFSET_EISIER, interrupts.bits());
    }

    #[inline]
    pub fn clear_normal_status(&mut self, interrupts: NormalStatus) {
        self.write_u16_reg(OFFSET_NISTR, interrupts.bits());
    }

    #[inline]
    pub fn clear_error_status(&mut self, interrupts: ErrorStatus) {
        self.write_u16_reg(OFFSET_EISTR, interrupts.bits());
    }

    fn inhibit_wait(&self, inh_bits: SdmmcStatus) -> Result<SdmmcStatus, SdMmcError> {
        let mut timeout = WAIT_TIMEOUT;

        while timeout > 0 {
            let status = self.status();
            if !status.intersects(inh_bits) {
                return Ok(status);
            }

            timeout -= 1;
        }

        Err(SdMmcError::InhibitWaitTimeout)
    }

    fn wait_normal_status(&self, ns: NormalStatus) -> Result<NormalStatus, SdMmcError> {
        let mut timeout = WAIT_TIMEOUT;

        while timeout > 0 {
            let status = self.normal_status();
            if status.contains(ns) {
                return Ok(ns);
            }

            if status.contains(NormalStatus::ERRINT) {
                return Err(SdMmcError::Error(self.error_status()));
            }

            // if timeout - 1 == 0 {
            //     use core::fmt::Write;
            //     core::writeln!(crate::uart::Uart::<crate::uart::Uart1>::new(), "about to timeout,
            // status: {:?} | expected {:?}", status, ns).ok();     core::writeln!
            // (crate::uart::Uart::<crate::uart::Uart1>::new(), "state: {:?}", self.status()).ok();
            // }

            timeout -= 1;
        }

        Err(SdMmcError::NormalStatusWaitTimeout)
    }

    fn write_u16_reg(&self, offset: u32, data: u16) {
        let reg_addr = self.base_addr + offset;
        unsafe {
            core::arch::asm!(
                "strh {}, [{}]",
                in(reg) data,
                in(reg) reg_addr,
            );
        }
    }

    fn write_byte_reg(&self, offset: u32, byte: u8) {
        let reg_addr = self.base_addr + offset;
        unsafe {
            core::arch::asm!(
                "strb {}, [{}]",
                in(reg) byte,
                in(reg) reg_addr,
            );
        }
    }

    fn read_byte_reg(&self, offset: u32) -> u8 {
        let reg_addr = self.base_addr + offset;
        let mut byte;
        unsafe {
            core::arch::asm!(
                "ldrb {}, [{}]",
                out(reg) byte,
                in(reg) reg_addr,
            );
        }

        byte
    }

    fn read_u16_reg(&self, offset: u32) -> u16 {
        let reg_addr = self.base_addr + offset;
        let mut data;
        unsafe {
            core::arch::asm!(
                "ldrh {}, [{}]",
                out(reg) data,
                in(reg) reg_addr,
            );
        }

        data
    }

    fn read_u32_reg(&self, offset: u32) -> u32 {
        let reg_addr = self.base_addr + offset;
        let mut data;
        unsafe {
            core::arch::asm!(
                "ldr {}, [{}]",
                out(reg) data,
                in(reg) reg_addr,
            );
        }

        data
    }
}
