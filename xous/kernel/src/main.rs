// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(keyos, no_main)]
#![cfg_attr(keyos, no_std)]

#[cfg(keyos)]
#[cfg_attr(not(keyos), macro_use)]
extern crate bitflags;

#[macro_use]
mod debug;

mod arch;

#[macro_use]
mod args;
mod io;
mod irq;
mod macros;
mod mem;
mod permission_requests;
mod platform;
mod process;
mod scheduler;
mod server;
mod services;
mod syscall;

use services::SystemServices;
use xous::*;

#[cfg(keyos)]
#[no_mangle]
/// This function is called from KeyOS startup code to initialize various kernel structures
/// based on arguments passed by the bootloader. It is unused when running under an operating system.
///
/// # Safety
///
/// This is safe to call only to initialize the kernel.
pub unsafe extern "C" fn init(arg_offset: *const u32) -> ! {
    // For early debug printout like panics, etc.
    platform::atsama5d2::uart::init();
    // TRNG is needed for ASLR when creating system userland processes
    platform::atsama5d2::rand::init();
    keyos::stack_canary::set_stack_guard(platform::rand::get_u32());

    args::KernelArguments::init(arg_offset);
    let args = args::KernelArguments::get();
    // Everything needs memory, so the first thing we should do is initialize the memory manager.
    crate::mem::MemoryManager::with_mut(|mm| {
        mm.init_from_memory(keyos::ALLOCATION_TRACKER_OFFSET as _);
    });

    #[cfg(feature = "trace-systemview")]
    {
        crate::platform::atsama5d2::systemview::init();
    }

    SystemServices::with_mut(|system_services| system_services.init_from_memory(&args));

    // Unmap the transparent loader page used to jump here
    #[cfg(keyos)]
    crate::mem::MemoryManager::with_mut(|mm| {
        mm.unmap_range((keyos::LOADER_CODE_ADDRESS & !(0xfff)) as _, keyos::PAGE_SIZE)
            .expect("Could not unmap first loader page")
    });

    // Now that the memory manager is set up, perform any architecture and
    // platform specific initializations.
    arch::init();
    platform::init();

    // rand::init() already clears the initial pipe, but pump the TRNG a little more out of no other reason
    // than sheer paranoia
    platform::rand::get_u32();
    platform::rand::get_u32();

    main();

    // `main` is not supposed to return on keyos
    unreachable!()
}

/// Common main function for KeyOS and hosted environments.
fn main() {
    // Run the scheduler for the first time.
    #[cfg(keyos)]
    yield_slice();
    // Special case for testing: idle can return `false` to indicate exit
    while arch::idle() {}
}
