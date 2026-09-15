extern crate alloc;

use core::{array, fmt::Debug, marker::PhantomData, mem::transmute};

use bitfield::bitfield;
use vcell::VolatileCell;

#[repr(C)]
pub struct CapabilityRegisters {
    pub caplength: VolatileCell<u8>,
    _reserved: u8,
    pub version: VolatileCell<u16>,
    pub structural_params: VolatileCell<StructuralParameters>,
    pub capability_params: VolatileCell<u32>,
}

#[repr(C)]
pub struct OperationalRegisters {
    pub cmd: VolatileCell<Command>,
    pub status: VolatileCell<Status>,
    pub interrupt_enable: VolatileCell<Interrupt>,
    pub frame_index: VolatileCell<u32>,
    pub control_data_segment: VolatileCell<u32>,
    pub periodic_list: VolatileCell<u32>,
    pub async_list: VolatileCell<QueueHeadPointer>,
    _reserved: [u32; 9],
    pub config: VolatileCell<u32>,
    pub ports: [VolatileCell<PortControlRegisters>; 3],
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct StructuralParameters(u32);
    impl Debug;

    pub n_ports, _: 3,0;
    pub power_control, _: 4;
    // TODO: rest of the bits
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct Command(u32);
    impl Debug;

    pub run, set_run: 0;
    pub host_controller_reset, set_host_controller_reset: 1;
    pub frame_list_size, set_frame_list_size: 3, 2;
    pub periodic_schedule_enable, set_periodic_schedule_enable: 4;
    pub async_schedule_enable, set_async_schedule_enable: 5;
    pub doorbell, set_doorbell: 6;
    pub interrupt_threshold, set_interrupt_threshold: 23, 16;
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct Status(u32);
    impl Debug;

    pub interrupt, set_interrupt: 0;
    pub error_interrupt, set_error_interrupt: 1;
    pub port_change, set_port_change: 2;
    pub frame_list_rollover, set_frame_list_rollover: 3;
    pub host_system_error, set_host_system_error: 4;
    pub interrupt_on_async_advance, set_interrupt_on_async_advance: 5;
    pub halted, _: 12;
    pub reclamation, _: 13;
    pub periodic_schedule_status, _: 14;
    pub async_schedule_status, _: 15;
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct Interrupt(u32);
    impl Debug;

    pub interrupt, set_interrupt: 0;
    pub error_interrupt, set_error_interrupt: 1;
    pub port_change, set_port_change: 2;
    pub frame_list_rollover, set_frame_list_rollover: 3;
    pub host_system_error, set_host_system_error: 4;
    pub async_advance, set_async_advance: 5;
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct PortControlRegisters(u32);
    impl Debug;

    pub connected,set_connected: 0;
    pub connected_changed,set_connected_changed: 1;
    pub enabled,set_enabled: 2;
    pub enabled_changed,set_enabled_changed: 3;
    pub overcurrent,_: 4;
    pub overcurrent_changed,set_overcurrent_changed: 5;
    pub force_resume,set_force_resume: 6;
    pub suspend,set_suspend: 7;
    pub reset,set_reset: 8;
    pub line_status,_: 11, 10;
    pub port_power, set_port_power: 12;
    pub owner,set_owner: 13;
    pub port_indicator, set_port_indicator: 15,14;
    pub port_test_control, set_port_test_control: 19,16;
    pub wake_on_connect,set_wake_on_connect: 20;
    pub wake_on_disconnect,set_wake_on_disconnect: 21;
    pub wake_on_overcurrent,set_wake_on_overcurrent: 22;
}

#[repr(C, align(32))]
pub struct QueueHead {
    pub next: VolatileCell<QueueHeadPointer>,
    pub info1: VolatileCell<QueueHeadInfo1>,
    pub info2: VolatileCell<QueueHeadInfo2>,
    pub current_qtd: VolatileCell<QtdPointer>,
    pub next_qtd: VolatileCell<QtdPointer>,
    pub alternate_next_qtd: VolatileCell<QtdPointer>,
    pub token: VolatileCell<QtdToken>,
    pub buffers: [VolatileCell<u32>; 5],
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct QueueHeadInfo1(u32);
    impl Debug;

    pub u8, address,set_address:6,0;
    pub inactivate_on_next,set_inactivate_on_next:7;
    pub u8, endpoint, set_endpoint: 11,8;
    pub endpoint_speed,set_endpoint_speed:13,12;
    pub data_toggle_control,set_data_toggle_control:14;
    pub is_head,set_is_head:15;
    pub u16, max_packet_length,set_max_packet_length:26,16;
    // Control endpoint ommitted, as it is for USB1.x
    pub nak_count_reload,set_nak_count_reload:31,28;

}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct QueueHeadInfo2(u32);
    impl Debug;

    pub interrupt_schedule_mask,set_interrupt_schedule_mask:7,0;
    // USB1.x stuff and split transactions ommitted
    pub multiplier,set_multiplier:31,30;

}

#[repr(C, align(32))]
pub struct Qtd {
    pub next: VolatileCell<QtdPointer>,
    pub alternate_next: VolatileCell<QtdPointer>,
    pub token: VolatileCell<QtdToken>,
    pub buffers: [VolatileCell<u32>; 5],
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct QtdToken(u32);
    impl Debug;

    pub ping,set_ping:0;
    // split state and missed uframe ommitted
    pub xact_err,set_xact_err:3;
    pub babble,set_babble:4;
    pub buffer_error,set_buffer_error:5;
    pub halted,set_halted:6;
    pub active,set_active:7;
    pub u8, from into PidCode, pid,set_pid:9,8;
    pub error_counter,set_error_counter:11,10;
    pub current_page,set_current_page:14,12;
    pub interrupt_on_complete,set_interrupt_on_complete:15;
    pub u16, total_bytes,set_total_bytes:30,16;
    pub data_toggle,set_data_toggle:31;
}

impl QtdToken {
    /// Halted because the device answered STALL, rather than because of a transaction
    /// error, babble, or a buffer problem. Only a STALL leaves the device refusing the
    /// pipe until a ClearFeature, and only then does its data toggle go back to DATA0.
    pub fn device_stalled(&self) -> bool {
        self.halted() && !self.xact_err() && !self.babble() && !self.buffer_error()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PidCode {
    Out = 0,
    In = 1,
    Setup = 2,
    Reserved = 3,
}

#[repr(transparent)]
pub struct ListElementPointer<T> {
    pub(crate) ptr: u32,
    pub(crate) _phantom_data: PhantomData<T>,
}

impl<T> Debug for ListElementPointer<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { write!(f, "0x{:x}", self.ptr) }
}

impl<T> Clone for ListElementPointer<T> {
    fn clone(&self) -> Self { *self }
}
impl<T> Copy for ListElementPointer<T> {}

pub type QueueHeadPointer = ListElementPointer<QueueHead>;

pub type QtdPointer = ListElementPointer<Qtd>;

impl Default for QueueHead {
    fn default() -> Self {
        let mut info1 = QueueHeadInfo1(0);
        info1.set_endpoint_speed(2); // High Speed, i.e. USB2.0
        info1.set_nak_count_reload(3);
        let mut info2 = QueueHeadInfo2(0);
        info2.set_multiplier(1);
        Self {
            next: VolatileCell::new(QueueHeadPointer::TERMINATE),
            info1: VolatileCell::new(info1),
            info2: VolatileCell::new(info2),
            current_qtd: VolatileCell::new(QtdPointer::TERMINATE),
            next_qtd: VolatileCell::new(QtdPointer::TERMINATE),
            alternate_next_qtd: VolatileCell::new(QtdPointer::TERMINATE),
            token: VolatileCell::new(QtdToken(0)),
            buffers: array::from_fn(|_| VolatileCell::new(0)),
        }
    }
}

impl<T> ListElementPointer<T> {
    pub const TERMINATE: Self = Self { ptr: 1, _phantom_data: PhantomData };
}

impl Qtd {
    pub fn new(pid: PidCode, data: &[u8], virt_to_phys: impl Fn(*const u8) -> usize) -> Self {
        let mut token = QtdToken(0);
        token.set_active(true);
        token.set_pid(pid);
        token.set_error_counter(3);
        token.set_total_bytes(data.len() as u16);
        token.set_interrupt_on_complete(true);
        // Set toggle to DATA0 on setup and DATA1 on in and out, so that
        // control packets with a single data packet in SETUP IN OUT will
        // work correctly.
        // Non-control pipes have an automatic Hw toggle and don't use
        // this value.
        token.set_data_toggle(pid == PidCode::In || pid == PidCode::Out);
        let mut buffers: [u32; 5] = Default::default();
        let mut remaining_len = data.len();
        let mut data_ptr = data.as_ptr();
        if remaining_len > 0 {
            for buffer in &mut buffers {
                *buffer = virt_to_phys(data_ptr) as u32;
                let offset = *buffer as usize & 0xFFF;
                let len_to_subtract = 0x1000 - offset;
                if len_to_subtract >= remaining_len {
                    break;
                }
                remaining_len -= len_to_subtract;
                data_ptr = data_ptr.wrapping_add(len_to_subtract);
            }
        }
        Self {
            next: VolatileCell::new(QtdPointer::TERMINATE),
            alternate_next: VolatileCell::new(QtdPointer::TERMINATE),
            token: VolatileCell::new(token),
            buffers: buffers.map(VolatileCell::new),
        }
    }
}

impl From<u8> for PidCode {
    fn from(value: u8) -> Self {
        if value < 4 { unsafe { transmute::<u8, PidCode>(value) } } else { Self::Reserved }
    }
}

impl From<PidCode> for u8 {
    fn from(value: PidCode) -> Self { value as u8 }
}
