// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use utralib::{utra::xdmac0::*, CSR, HW_XDMAC0_BASE, HW_XDMAC1_BASE};

// Number of registers per DMA channel
const DMA_CHANNEL_NUM_REGISTERS: u32 = 0x40;

const CIE_REG_OFFSET: u32 = 0x50;
const CID_REG_OFFSET: u32 = 0x54;
const CIS_REG_OFFSET: u32 = 0x5C;
const CSA_REG_OFFSET: u32 = 0x60;
const CDA_REG_OFFSET: u32 = 0x64;
const CNDA_REG_OFFSET: u32 = 0x68;
const CNDC_REG_OFFSET: u32 = 0x6C;
const CUBC_REG_OFFSET: u32 = 0x70;
const CBC_REG_OFFSET: u32 = 0x74;
const CC_REG_OFFSET: u32 = 0x78;

pub const DMA_CHANNELS: usize = 16;
pub const BIG_TRANSFER_CHUNK_SIZE: usize = 0x100000;
pub const BIG_TRANSFER_THRESHOLD: usize = 0x800000;

pub const MASK_BLOCK_INTERRUPT: u32 = 1 << 0;
pub const MASK_LINKED_LIST_INTERRUPT: u32 = 1 << 1;
pub const MASK_DISABLE_INTERRUPT: u32 = 1 << 2;

pub struct Xdmac {
    base_addr: u32,
}

impl Xdmac {
    #[inline]
    pub fn xdmac0() -> Xdmac {
        Xdmac {
            base_addr: HW_XDMAC0_BASE as u32,
        }
    }

    #[inline]
    pub fn xdmac1() -> Xdmac {
        Xdmac {
            base_addr: HW_XDMAC1_BASE as u32,
        }
    }

    #[inline]
    pub fn with_alt_base_addr(addr: usize) -> Xdmac {
        Xdmac {
            base_addr: addr as u32,
        }
    }

    #[inline]
    pub fn channel(&self, ch: DmaChannel) -> XdmacChannel {
        XdmacChannel {
            xdmac_base_addr: self.base_addr,
            channel: ch,
        }
    }

    #[inline]
    pub fn gis(&self) -> u32 {
        let dma = CSR::new(self.base_addr as *mut u32);
        dma.r(XDMAC_GIS)
    }

    #[inline]
    pub fn gim(&self) -> u32 {
        let dma = CSR::new(self.base_addr as *mut u32);
        dma.r(XDMAC_GIM)
    }
}

pub struct XdmacChannel {
    xdmac_base_addr: u32,
    channel: DmaChannel,
}

impl XdmacChannel {
    /// Sets up a peripheral-to-memory DMA transfer.
    #[inline]
    pub fn configure_peripheral_transfer(&self, config: DmaPeripheralTransferConfig) {
        let dma = CSR::new(self.xdmac_base_addr as *mut u32);

        let direction_flags = match config.direction {
            // A Note about SIF and DIF:
            // Only system interface 1 is connected to the 32 bit bridge (i.e. most peripherals), so
            // that has to be used as the source/destination for peripheral transfers.
            // (See SAMA5D2 datasheet "Table 18-6:  Master to Slave Access on H32MX"
            // The other interface should be the other one to allow two parallel masters to work
            // simultaneously.
            // XXX: This will NOT work for the NFC command register
            DmaTransferDirection::PeripheralToMemory => {
                dma.ms(XDMAC_CC0_SAM, 0) // Source address constant
                | dma.ms(XDMAC_CC0_DAM, 1) // Destination address auto-increments
                | dma.ms(XDMAC_CC0_DSYNC, 0) // PER2MEM
                | dma.ms(XDMAC_CC0_SIF, 1) // Source is a peripheral
                | dma.ms(XDMAC_CC0_DIF, 0)
            }
            DmaTransferDirection::MemoryToPeripheral => {
                dma.ms(XDMAC_CC0_SAM, 1) // Source address auto-increments
                | dma.ms(XDMAC_CC0_DAM, 0) // Destination address constant
                | dma.ms(XDMAC_CC0_DSYNC, 1) // MEM2PER
                | dma.ms(XDMAC_CC0_DIF, 1) // Destination is a peripheral
                | dma.ms(XDMAC_CC0_SIF, 0)
            }
        };

        let cc: u32 = dma.ms(XDMAC_CC0_TYPE, 1) // Synchronized mode
            | dma.ms(XDMAC_CC0_PERID, config.peripheral_id as u32)
            | dma.ms(XDMAC_CC0_PROT, 0) // Secured channel
            | dma.ms(XDMAC_CC0_SWREQ, 0) // Hardware request line
            | dma.ms(XDMAC_CC0_DWIDTH, config.data_width as u32)
            | dma.ms(XDMAC_CC0_CSIZE, config.chunk_size as u32)
            | dma.ms(XDMAC_CC0_MBSIZE, 3) // Memory burst size: 16
            | direction_flags;

        self.set_cc(cc);
    }

    #[inline]
    pub fn configure_memset_transfer(&self, data_width: DmaDataWidth) {
        let dma = CSR::new(self.xdmac_base_addr as *mut u32);

        let cc: u32 = dma.ms(XDMAC_CC0_TYPE, 0) // Self-triggered mode
            | dma.ms(XDMAC_CC0_MBSIZE, 3) // Memory burst size: 16
            | dma.ms(XDMAC_CC0_PROT, 0) // Secured channel
            | dma.ms(XDMAC_CC0_MEMSET, 1) // Memset mode
            | dma.ms(XDMAC_CC0_CSIZE, DmaChunkSize::C1 as u32)
            | dma.ms(XDMAC_CC0_DWIDTH, data_width as u32)
            | dma.ms(XDMAC_CC0_DAM, 1) // Destination address auto-increments
            | dma.ms(XDMAC_CC0_PERID, 1) // Unused peripheral ID
            ;
        self.set_cc(cc);
    }

    /// Starts the DMA transfer.
    ///
    /// The DMA transfer must be configured beforehand by calling one of the following
    /// methods:
    /// - [`XdmacChannel::configure_peripheral_to_memory`]
    /// - Memory-memory: TODO
    #[inline]
    pub fn execute_transfer(&self, src: u32, dst: u32, data_size: usize) {
        // Clear the channel status by reading
        let _ = self.interrupt_status();

        // Configure the transfer parameters
        self.set_sa_da(src, dst);
        self.set_data_size(data_size);

        // Make sure all memory transfers are completed before enabling the DMA
        armv7::asm::dmb();

        // Start the transfer
        self.enable();
    }

    /// Starts the DMA transfer using a linked list of descriptors.
    ///
    /// The DMA transfer must be configured beforehand.
    #[inline]
    pub fn execute_transfer_ll(&self, params: ExecuteTransferLlParams) {
        // Clear the channel status by reading
        let _ = self.interrupt_status();

        self.set_sa_da(params.src, params.dst);
        const NDE: u32 = 1;
        const NDSUP: u32 = 1 << 1;
        const NDDUP: u32 = 1 << 2;
        const NDVIEW_SHIFT: u32 = 3;
        self.set_next_descriptor(
            params.first_descriptor,
            NDE | (params.first_descriptor_type << NDVIEW_SHIFT)
                | if params.src_from_descriptor { NDSUP } else { 0 }
                | if params.dst_from_descriptor { NDDUP } else { 0 },
        );

        // Make sure all memory transfers are completed before enabling the DMA
        armv7::asm::dmb();

        // Start the transfer
        self.enable();
    }

    /// Checks the interrupt status. This operation clears the interrupt status.
    #[inline]
    pub fn interrupt_status(&self) -> u32 {
        unsafe { self.reg_by_offset(CIS_REG_OFFSET).read_volatile() }
    }

    #[inline]
    pub fn is_transfer_complete(&self) -> bool {
        let cis = self.interrupt_status();

        // Check if BIS=1
        cis & 1 != 0
    }

    #[inline]
    pub fn suspend(&self) {
        let mut dma = CSR::new(self.xdmac_base_addr as *mut u32);
        let ch_bit = self.channel as u32;

        dma.wo(XDMAC_GRWS, 1 << ch_bit);
    }

    /// Enables the DMA channel.
    /// Resets the `transfer complete` flag.
    #[inline]
    pub fn enable(&self) {
        let mut dma = CSR::new(self.xdmac_base_addr as *mut u32);
        let ch_bit = self.channel as u32;

        dma.wo(XDMAC_GE, 1 << ch_bit);
    }

    /// Disables the DMA channel.
    #[inline]
    pub fn disable(&self) {
        let mut dma = CSR::new(self.xdmac_base_addr as *mut u32);
        let ch_bit = self.channel as u32;

        dma.wo(XDMAC_GD, 1 << ch_bit);
    }

    #[inline]
    pub fn software_flush(&self) {
        let mut dma = CSR::new(self.xdmac_base_addr as *mut u32);
        let ch_bit = self.channel as u32;

        dma.wo(XDMAC_GSWF, 1 << ch_bit);
    }

    #[inline]
    pub fn software_request(&self) {
        let mut dma = CSR::new(self.xdmac_base_addr as *mut u32);
        let ch_bit = self.channel as u32;

        dma.wo(XDMAC_GSWR, 1 << ch_bit);
    }

    #[inline]
    pub fn set_interrupt(&self, enable: bool) {
        let mut dma = CSR::new(self.xdmac_base_addr as *mut u32);
        let ch_bit = self.channel as u32;

        if enable {
            dma.wo(XDMAC_GIE, 1 << ch_bit);
        } else {
            dma.wo(XDMAC_GID, 1 << ch_bit);
        }
    }

    #[inline]
    pub fn set_bi_interrupt(&self, enable: bool) {
        if enable {
            self.set_cie(MASK_BLOCK_INTERRUPT);
        } else {
            self.set_cid(MASK_BLOCK_INTERRUPT);
        }
    }

    #[inline]
    pub fn set_li_interrupt(&self, enable: bool) {
        if enable {
            self.set_cie(MASK_LINKED_LIST_INTERRUPT);
        } else {
            self.set_cid(MASK_LINKED_LIST_INTERRUPT);
        }
    }

    #[inline]
    pub fn set_di_interrupt(&self, enable: bool) {
        if enable {
            self.set_cie(MASK_DISABLE_INTERRUPT);
        } else {
            self.set_cid(MASK_DISABLE_INTERRUPT);
        }
    }

    /// Sets the value of the `CC` register for this channel.
    fn set_cc(&self, cc_val: u32) {
        unsafe { self.reg_by_offset(CC_REG_OFFSET).write_volatile(cc_val) }
    }

    /// Sets the value of the `CIE` (channel interrupt enable) register.
    fn set_cie(&self, cie_val: u32) {
        unsafe { self.reg_by_offset(CIE_REG_OFFSET).write_volatile(cie_val) }
    }

    /// Sets the value of the `CID` (channel interrupt disable) register.
    fn set_cid(&self, cid_val: u32) {
        unsafe { self.reg_by_offset(CID_REG_OFFSET).write_volatile(cid_val) }
    }

    /// Sets channel's source and destination addresses.
    fn set_sa_da(&self, sa: u32, da: u32) {
        unsafe {
            self.reg_by_offset(CSA_REG_OFFSET).write_volatile(sa);
            self.reg_by_offset(CDA_REG_OFFSET).write_volatile(da);
        }
    }

    #[inline]
    pub fn remaining_data_size(&self) -> u32 {
        unsafe { self.reg_by_offset(CUBC_REG_OFFSET).read_volatile() }
    }

    fn set_data_size(&self, size: usize) {
        unsafe {
            // If the size does not fit into UBLEN, do multiple microblocks.
            if size > BIG_TRANSFER_THRESHOLD {
                // Make sure we are aligned and fit into blocklen, otherwise this
                // function will not do what is expected.
                assert_eq!(size & (BIG_TRANSFER_CHUNK_SIZE - 1), 0);
                assert!(size / BIG_TRANSFER_CHUNK_SIZE < 0x1000);
                self.reg_by_offset(CBC_REG_OFFSET)
                    .write_volatile((size / BIG_TRANSFER_CHUNK_SIZE - 1) as u32);
                self.reg_by_offset(CUBC_REG_OFFSET)
                    .write_volatile(BIG_TRANSFER_CHUNK_SIZE as u32);
            } else {
                self.reg_by_offset(CBC_REG_OFFSET).write_volatile(0);
                self.reg_by_offset(CUBC_REG_OFFSET)
                    .write_volatile(size as u32)
            }
        }
    }

    fn set_next_descriptor(&self, descriptor: u32, control: u32) {
        unsafe {
            self.reg_by_offset(CNDA_REG_OFFSET)
                .write_volatile(descriptor);
            self.reg_by_offset(CNDC_REG_OFFSET).write_volatile(control);
        }
    }
    #[inline]
    pub fn last_descriptor(&self) -> u32 {
        unsafe { self.reg_by_offset(CNDA_REG_OFFSET).read_volatile() }
    }

    fn reg_by_offset(&self, offset: u32) -> *mut u32 {
        let reg_addr =
            self.xdmac_base_addr + offset + self.channel as u32 * DMA_CHANNEL_NUM_REGISTERS;
        reg_addr as *mut u32
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExecuteTransferLlParams {
    pub src: u32,
    pub dst: u32,
    pub first_descriptor: u32,
    pub first_descriptor_type: u32,
    pub src_from_descriptor: bool,
    pub dst_from_descriptor: bool,
}

#[derive(Debug, Clone)]
#[repr(C)]
pub struct View0Descriptor {
    pub next_descriptor: u32,
    pub control: DescriptorControl,
    pub address: u32,
}

bitfield::bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    pub struct DescriptorControl(u32);
    impl Debug;

    pub ublen, set_ublen: 23, 0;
    pub next_descriptor_enable, set_next_descriptor_enable: 24;
    pub next_source_update, set_next_source_update: 25;
    pub next_destination_update, set_next_destination_update: 26;
    pub next_view_type, set_next_view_type: 28, 27;
}

#[derive(Debug, Clone)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct DmaPeripheralTransferConfig {
    /// The id of the peripheral to use. This is different from the PMC peripheral ids.
    pub peripheral_id: DmaPeripheralId,
    pub direction: DmaTransferDirection,
    /// The data element width of the transfer. Addresses need to be aligned to this
    /// width. Specified for each peripheral in the datasheet.
    pub data_width: DmaDataWidth,
    /// How many data elements to transfer with a single AXI transaction.
    /// Specified for each peripheral in the datasheet.
    /// When in doubt, use 16.
    pub chunk_size: DmaChunkSize,
}

impl Default for DmaPeripheralTransferConfig {
    fn default() -> Self {
        Self {
            peripheral_id: DmaPeripheralId::Mem2Mem,
            direction: DmaTransferDirection::PeripheralToMemory,
            data_width: DmaDataWidth::D32,
            chunk_size: DmaChunkSize::C16,
        }
    }
}

#[derive(Debug, Copy, Clone)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum DmaChannel {
    Channel0 = 0,
    Channel1 = 1,
    Channel2 = 2,
    Channel3 = 3,
    Channel4 = 4,
    Channel5 = 5,
    Channel6 = 6,
    Channel7 = 7,
    Channel8 = 8,
    Channel9 = 9,
    Channel10 = 10,
    Channel11 = 11,
    Channel12 = 12,
    Channel13 = 13,
    Channel14 = 14,
    Channel15 = 15,
}

impl DmaChannel {
    #[inline]
    pub fn from_usize(value: usize) -> Option<Self> {
        match value {
            0 => Some(Self::Channel0),
            1 => Some(Self::Channel1),
            2 => Some(Self::Channel2),
            3 => Some(Self::Channel3),
            4 => Some(Self::Channel4),
            5 => Some(Self::Channel5),
            6 => Some(Self::Channel6),
            7 => Some(Self::Channel7),
            8 => Some(Self::Channel8),
            9 => Some(Self::Channel9),
            10 => Some(Self::Channel10),
            11 => Some(Self::Channel11),
            12 => Some(Self::Channel12),
            13 => Some(Self::Channel13),
            14 => Some(Self::Channel14),
            15 => Some(Self::Channel15),
            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum DmaDataWidth {
    D8 = 0,
    D16 = 1,
    D32 = 2,
    D64 = 3,
}

impl DmaDataWidth {
    #[inline]
    pub fn byte_len(&self) -> usize {
        match self {
            DmaDataWidth::D8 => 1,
            DmaDataWidth::D16 => 2,
            DmaDataWidth::D32 => 4,
            DmaDataWidth::D64 => 8,
        }
    }
}

#[derive(Debug, Copy, Clone)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum DmaChunkSize {
    C1 = 0,
    C2 = 1,
    C4 = 2,
    C8 = 3,
    C16 = 4,
}

#[derive(Debug, Copy, Clone)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum DmaTransferDirection {
    PeripheralToMemory = 0,
    MemoryToPeripheral = 1,
}

#[derive(Debug, Copy, Clone)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum DmaPeripheralId {
    TwiHs0Tx = 0,
    TwiHs0Rx = 1,
    TwiHs1Tx = 2,
    TwiHs1Rx = 3,

    Qspi0Tx = 4,
    Qspi0Rx = 5,

    Spi0Tx = 6,
    Spi0Rx = 7,
    Spi1Tx = 8,
    Spi1Rx = 9,

    PwmTx = 10,

    Flexcom0Tx = 11,
    Flexcom0Rx = 12,
    Flexcom1Tx = 13,
    Flexcom1Rx = 14,
    Flexcom2Tx = 15,
    Flexcom2Rx = 16,
    Flexcom3Tx = 17,
    Flexcom3Rx = 18,
    Flexcom4Tx = 19,
    Flexcom4Rx = 20,

    Ssc0Tx = 21,
    Ssc0Rx = 22,
    Ssc1Tx = 23,
    Ssc1Rx = 24,

    AdcRx = 25,

    AesTx = 26,
    AesRx = 27,

    Sha = 30,

    Uart0Tx = 35,
    Uart0Rx = 36,
    Uart1Tx = 37,
    Uart1Rx = 38,
    Uart2Tx = 39,
    Uart2Rx = 40,
    Uart3Tx = 41,
    Uart3Rx = 42,
    Uart4Tx = 43,
    Uart4Rx = 44,

    /// Special "peripheral" to be used with memory-to-memory transfers.
    Mem2Mem = 0x7F,
}
