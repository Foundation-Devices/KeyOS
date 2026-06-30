use core::sync::atomic::{AtomicU32, Ordering};

use atsama5d27::pit::{Pit, PIV_MAX};
use keyos::{
    PID1_STACK_BOTTOM, PID1_STACK_PAGE_COUNT, RSTC_KERNEL_ADDR, RTT_BUFFERS_START_VIRT_ADDR,
    RTT_CONTROL_BLOCK_VIRT_ADDR,
};
#[cfg(all(feature = "trace-systemview", feature = "trace-aborts-systemview"))]
use systemview_keyos::send_system_desc_interrupt;
use systemview_keyos::{send_system_desc_core, send_system_desc_device, send_system_desc_os, SystemView};
use utralib::{HW_PIT_BASE, HW_RSTC_BASE};
use xous::{MemoryFlags, MemoryRange, PID};

use crate::mem::{MemoryManager, PAGE_SIZE};
use crate::process::INITIAL_TID;

#[cfg(all(feature = "trace-systemview", feature = "trace-aborts-systemview"))]
const ABORT_ISR_ID: u32 = 0xff;

const CONTROL_BLOCK_SIZE: usize = PAGE_SIZE;
const RTT_UP_BUF_SIZE: usize = PAGE_SIZE * 48;
const RTT_DOWN_BUF_SIZE: usize = PAGE_SIZE;

static RUNNING_TIMESTAMP: AtomicU32 = AtomicU32::new(0);

pub fn init() {
    const _: () = assert!(
        HW_RSTC_BASE == HW_PIT_BASE & !(PAGE_SIZE - 1),
        "HW_PIT_BASE and HW_RSTC_BASE are not on the same page"
    );

    // Map RTT control block at a fixed address
    MemoryManager::with_mut(|memory_manager| {
        memory_manager
            .map_range(
                0,
                RTT_CONTROL_BLOCK_VIRT_ADDR as *mut usize,
                CONTROL_BLOCK_SIZE,
                MemoryFlags::W | MemoryFlags::POPULATE | MemoryFlags::NO_CACHE,
                true,
            )
            .expect("unable to map RTT control block memory")
    });

    // Allocate RTT buffer for outgoing SystemView events
    let mut rtt_up_buf_mem = MemoryManager::with_mut(|memory_manager| {
        memory_manager
            .map_range(
                0,
                RTT_BUFFERS_START_VIRT_ADDR as *mut usize,
                RTT_UP_BUF_SIZE,
                MemoryFlags::W | MemoryFlags::POPULATE | MemoryFlags::NO_CACHE,
                true,
            )
            .expect("couldn't allocate SystemView UP buffer")
    });

    // Allocate RTT DOWN buffer for incoming SystemView commands
    let mut rtt_down_buf_mem = MemoryManager::with_mut(|memory_manager| {
        memory_manager
            .map_range(
                0,
                (RTT_BUFFERS_START_VIRT_ADDR + RTT_UP_BUF_SIZE).next_multiple_of(PAGE_SIZE) as *mut usize,
                RTT_DOWN_BUF_SIZE,
                MemoryFlags::W | MemoryFlags::POPULATE | MemoryFlags::NO_CACHE,
                true,
            )
            .expect("couldn't allocate SystemView DOWN buffer")
    });

    let pit_addr = RSTC_KERNEL_ADDR as u32 + (HW_PIT_BASE as u32 - HW_RSTC_BASE as u32);
    let mut pit = Pit::with_alt_base_addr(pit_addr);
    pit.set_enabled(false);
    pit.set_interrupt(false);
    pit.set_interval(PIV_MAX);
    pit.reset();

    SystemView::init(
        rtt_up_buf_mem.as_slice_mut(),
        rtt_down_buf_mem.as_slice_mut(),
        492_000_000,
        492_000_000,
    );

    println!("[SystemView] Waiting for the recorder to connect...");
    SystemView::wait_for_recorder();
    pit.set_enabled(true);
}

#[no_mangle]
extern "C" fn send_system_description() {
    send_system_desc_os!("KeyOS");
    send_system_desc_core!("ARMv7A");
    send_system_desc_device!("ATSAMA5D28");

    // Register special data & prefetch abort "ISR"
    #[cfg(all(feature = "trace-systemview", feature = "trace-aborts-systemview"))]
    {
        send_system_desc_interrupt!(255, "Abort");
    }
}

#[no_mangle]
extern "C" fn send_task_list() {
    let kernel_stack_start = PID1_STACK_BOTTOM;
    let kernel_stack_size = PAGE_SIZE * PID1_STACK_PAGE_COUNT;
    let stack = unsafe { MemoryRange::new(kernel_stack_start, kernel_stack_size).expect("stack") };

    // At this point only the kernel process is set up and available, send its info and that it's running
    let pid = PID::new(1).unwrap();
    let tid = INITIAL_TID;
    SystemView::thread_send_info(pid, tid, "kernel", stack);
    SystemView::task_exec_begin(systemview_keyos::pid_tid_to_id(pid, tid));

    let pid = PID::new(1).unwrap();
    let tid = 2;
    SystemView::thread_send_info(pid, tid, "kernel (swi)", stack);
    SystemView::task_exec_begin(systemview_keyos::pid_tid_to_id(pid, tid));
}

#[no_mangle]
extern "C" fn os_get_time() -> u64 {
    0 // TODO
}

static CURRENT_ISR_NUM: AtomicU32 = AtomicU32::new(0);

pub fn set_current_isr(isr: u32) { CURRENT_ISR_NUM.store(isr, Ordering::SeqCst); }

#[cfg(all(feature = "trace-systemview", feature = "trace-aborts-systemview"))]
pub fn set_abort() { set_current_isr(ABORT_ISR_ID); }

#[no_mangle]
extern "C" fn get_current_isr() -> u32 { CURRENT_ISR_NUM.load(Ordering::SeqCst) }

#[no_mangle]
extern "C" fn get_timestamp() -> u32 {
    let pit_addr = RSTC_KERNEL_ADDR as u32 + (HW_PIT_BASE as u32 - HW_RSTC_BASE as u32);
    let mut pit = Pit::with_alt_base_addr(pit_addr);
    let pit_elapsed = pit.read();
    pit.reset();

    let timestamp = RUNNING_TIMESTAMP.load(Ordering::Relaxed);
    let timestamp = timestamp.wrapping_add(pit_elapsed);
    RUNNING_TIMESTAMP.store(timestamp, Ordering::Relaxed);
    timestamp
}
