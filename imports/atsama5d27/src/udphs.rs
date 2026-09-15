// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    bitfield::bitfield,
    utralib::{utra::udphs::*, CSR},
    vcell::VolatileCell,
};

#[derive(Debug, Clone)]
pub struct UsbDevice {
    csr: CSR<u32>,
    endpoints: *mut [UsbDeviceEndpoint; 16],
    dmas: *mut [UsbDeviceDma; 8],
    banks: *mut u8,
}

#[repr(C)]
pub struct UsbDeviceEndpoint {
    pub cfg: VolatileCell<EndpointConfiguration>,
    pub ctl_enable: VolatileCell<EndpointControl>,
    pub ctl_disable: VolatileCell<EndpointControl>,
    pub ctl: VolatileCell<EndpointControl>,
    _reserved: u32,
    pub status_set: VolatileCell<EndpointStatus>,
    pub status_clr: VolatileCell<EndpointStatus>,
    pub status: VolatileCell<EndpointStatus>,
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct EndpointConfiguration(u32);
    impl Debug;

    pub ept_size, set_ept_size: 2, 0; // Endpoint size will be 8<<ept_size bytes
    pub from into EndpointDirection, ept_dir, set_ept_dir: 3, 3;
    pub from into EndpointType, ept_type, set_ept_type: 5, 4;
    pub bank_number, set_bank_number: 7, 6; // 0 means not used
    pub nb_trans, set_nb_trans: 9, 8; // only used for ISO endpoints
    pub mapped, _: 31; // Everything was OK with the mapping
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct EndpointControl(u32);
    impl Debug;

    pub enable, set_enable: 0;
    pub auto_valid, set_auto_valid: 1;
    pub interrupt_disables_dma, set_interrupt_disables_dma: 3;
    pub disable_nyet, set_disable_nyet: 4;
    pub overflow_error_interrupt, set_overflow_error_interrupt: 8;
    pub received_out_interrupt, set_received_out_interrupt: 9;
    pub transmission_complete_interrupt, set_transmission_complete_interrupt: 10;
    pub txrdy_interrupt, set_txrdy_interrupt: 11;
    pub received_setup_interrupt, set_received_setup_interupt: 12;
    pub stall_sent_interrupt, set_stall_sent_interrupt: 13;
    pub nak_in_interrupt, set_nak_in_interrupt: 14;
    pub nak_out_interrupt, set_nak_out_interrupt: 15;
    pub busy_bank_interrupt, set_busy_bank_interrupt: 18;
    pub short_packet, set_short_packet: 31;
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct EndpointStatus(u32);
    impl Debug;

    pub force_stall, set_force_stall: 5;
    // Only TOGGLESQ in the clear register, where it resets the data toggle to DATA0.
    pub toggle_sequence, set_toggle_sequence: 6;
    pub overflow_error, set_overflow_error:8;
    pub received_out, set_received_out:9;
    pub transmission_complete, set_transmmission_complete:10;
    pub tx_packet_ready, set_tx_packet_ready:11;
    pub received_setup, set_received_setup:12;
    pub stall_sent, set_stall_sent: 13;
    pub nak_in, set_nak_in: 14;
    pub nak_out, set_nak_out: 15;
    pub current_bank, set_current_bank: 17, 16;
    pub busy_bank, set_busy_bank: 19, 18;
    pub byte_count, set_byte_count: 30, 20;
    pub short_packet, set_short_packet: 31;
}

#[repr(C)]
pub struct UsbDeviceDma {
    pub next_desc: VolatileCell<u32>,
    pub address: VolatileCell<u32>,
    pub control: VolatileCell<DmaControl>,
    pub status: VolatileCell<DmaStatus>,
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct DmaControl(u32);
    impl Debug;

    pub enable, set_enable: 0;
    pub load_next, set_load_next:1;
    pub end_of_transfer_enable, set_end_of_transfer_enable: 2;
    pub end_of_buffer_enable, set_end_of_buffer_enable: 3;
    pub end_of_transfer_interrupt, set_end_of_transfer_interrupt: 4;
    pub end_of_buffer_interrupt, set_end_of_buffer_interrupt: 5;
    pub descriptor_loaded_interrupt, set_descriptor_loaded_interrupt: 6;
    pub burst_lock, set_burst_lock: 7;
    pub u16, length, set_length: 31, 16;
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct DmaStatus(u32);
    impl Debug;

    pub enable, _: 0;
    pub active, _: 1;
    pub end_of_transfer, _: 4;
    pub end_of_buffer, _: 5;
    pub descriptor_loaded, _: 6;
    pub u16, length, _: 31, 16;
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct InterruptStatus(u32);
    impl Debug;

    pub high_speed, _: 0;
    pub suspend, _: 1;
    pub micro_sof, _: 2;
    pub start_of_frame, _: 3;
    pub end_of_reset, _: 4;
    pub wake_up, _: 5;
    pub end_of_resume, _: 6;
    pub upstream_resume, _: 7;
    pub endpoint, _: 8, 8, 16;
    pub dma, _: 24, 24, 8;
}

// The numbers here apply both to USB-standard and to the ATSAMA5D2 UDHPS registers.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum EndpointType {
    Control = 0,
    Isochronous = 1,
    Bulk = 2,
    Interrupt = 3,
}

impl From<u32> for EndpointType {
    fn from(value: u32) -> Self {
        match value {
            1 => Self::Isochronous,
            2 => Self::Bulk,
            3 => Self::Interrupt,
            _ => Self::Control,
        }
    }
}

impl From<EndpointType> for u32 {
    fn from(value: EndpointType) -> Self {
        value as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum EndpointDirection {
    Out = 0,
    In = 1,
}
impl From<u32> for EndpointDirection {
    fn from(value: u32) -> Self {
        if value != 0 {
            Self::In
        } else {
            Self::Out
        }
    }
}

impl From<EndpointDirection> for u32 {
    fn from(value: EndpointDirection) -> Self {
        value as u32
    }
}

impl UsbDevice {
    #[inline]
    pub fn new(registers_virt: *mut u8, banks_virt: *mut u8) -> Self {
        let csr = CSR::new(registers_virt as _);
        let endpoints = registers_virt.wrapping_add(0x100) as *mut [UsbDeviceEndpoint; 16];
        let dmas = registers_virt.wrapping_add(0x300) as *mut [UsbDeviceDma; 8];
        Self {
            csr,
            endpoints,
            dmas,
            banks: banks_virt,
        }
    }

    #[inline]
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled {
            self.csr.rmwf(CTRL_EN_UDPHS, 1);
            self.csr.rmwf(CTRL_DETACH, 0);
        } else {
            self.csr.rmwf(CTRL_EN_UDPHS, 0);
        }
    }

    #[inline]
    pub fn reset_endpoint(&mut self, ep_number: usize) {
        assert!(ep_number < 16);
        self.csr.wo(EPTRST, 1 << ep_number);
    }

    #[inline]
    pub fn endpoint(&mut self, ep_number: usize) -> &mut UsbDeviceEndpoint {
        unsafe { &mut (*self.endpoints)[ep_number] }
    }

    #[inline]
    pub fn dma(&mut self, ep_number: usize) -> &mut UsbDeviceDma {
        unsafe { &mut (*self.dmas)[ep_number] }
    }

    #[inline]
    pub fn enable_endpoint_interrupt(&mut self, ep_number: usize) {
        assert!(ep_number < 16);
        let ien = self.csr.r(IEN) | 1 << (ep_number + 8);
        self.csr.wo(IEN, ien);
    }

    #[inline]
    pub fn enable_dma_interrupt(&mut self, ep_number: usize) {
        assert!(ep_number < 8);
        let ien = self.csr.r(IEN) | 1 << (ep_number + 24);
        self.csr.wo(IEN, ien);
    }

    #[inline]
    pub fn interrupt_status(&self) -> InterruptStatus {
        InterruptStatus(self.csr.r(INTSTA))
    }

    #[inline]
    pub fn clear_interrupt(&mut self, status: InterruptStatus) {
        self.csr.wo(CLRINT, status.0)
    }

    #[inline]
    pub fn read_endpoint_memory(&mut self, ep_number: usize, offset: usize, buffer: &mut [u8]) {
        for (i, b) in buffer.iter_mut().enumerate() {
            *b = unsafe {
                self.banks
                    .wrapping_add(ep_number * 0x10000 + offset + i)
                    .read_volatile()
            }
        }
    }

    #[inline]
    pub fn write_endpoint_memory(&mut self, ep_number: usize, offset: usize, buffer: &[u8]) {
        for (i, b) in buffer.iter().enumerate() {
            unsafe {
                self.banks
                    .wrapping_add(ep_number * 0x10000 + offset + i)
                    .write_volatile(*b)
            }
        }
    }

    #[inline]
    pub fn set_address(&mut self, addr: u8) {
        self.csr.rmwf(CTRL_DEV_ADDR, addr as u32);
        self.csr.rmwf(CTRL_FADDR_EN, if addr == 0 { 0 } else { 1 });
    }
}
