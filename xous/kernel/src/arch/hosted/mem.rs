// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

pub const PAGE_SIZE: usize = 4096;
use xous::{Error, MemoryFlags, PID};

use crate::mem::MemoryManager;

#[derive(Copy, Clone, Default, Debug, PartialEq)]
pub struct MemoryMapping {
    pid: usize,
}

impl MemoryMapping {
    /// Get the currently active memory mapping.  Note that the actual root pages
    /// may be found at virtual address `PAGE_TABLE_ROOT_OFFSET`.
    pub fn current() -> MemoryMapping { MemoryMapping { pid: 0 } }

    /// Get the "PID" (actually, ASID) from the current mapping
    pub fn get_pid(self) -> PID { crate::arch::process::current_pid() }

    /// Set this mapping as the systemwide mapping.
    /// **Note:** This should only be called from an interrupt in the
    /// kernel, which should be mapped into every possible address space.
    /// As such, this will only have an observable effect once code returns
    /// to userspace.
    pub fn activate(self) {
        // This is a no-op on hosted environments
    }

    /// Does nothing in hosted mode.
    pub unsafe fn allocate(&mut self, _pid: PID) -> Result<(), xous::Error> { Ok(()) }

    pub fn destroy(&self) {}

    // ---- Dummy counterparts to crate::arch:arm::mem::MemoryMapping ----

    pub fn map_page(
        &mut self,
        _mm: &mut MemoryManager,
        _phys: usize,
        _virt: *mut usize,
        _flags: MemoryFlags,
        _map_user: bool,
    ) -> Result<(), Error> {
        Ok(())
    }

    pub fn unmap_page(&self, _virt: *mut usize) -> Result<(), Error> { Ok(()) }

    pub fn move_page(
        &mut self,
        _mm: &mut MemoryManager,
        _src_addr: *mut usize,
        _dest_space: &mut MemoryMapping,
        _dest_addr: *mut usize,
    ) -> Result<usize, Error> {
        Ok(0)
    }

    pub fn lend_page(
        &mut self,
        _mm: &mut MemoryManager,
        _src_addr: *mut usize,
        _dest_space: &mut MemoryMapping,
        _dest_addr: *mut usize,
        _mutable: bool,
    ) -> Result<usize, Error> {
        Ok(0)
    }

    pub fn return_page(
        &mut self,
        _src_addr: *mut usize,
        _dest_space: &mut MemoryMapping,
        _dest_addr: *mut usize,
    ) -> Result<usize, Error> {
        Ok(0)
    }

    pub fn virt_to_phys(&self, virt: *const usize) -> Result<usize, Error> { Ok(virt as usize) }

    pub fn invalidate_page(&self, _virt: *mut usize, _phys: usize) {}

    pub fn find_free_run(&self, from: usize, _end: usize, _needed: usize) -> Option<usize> { Some(from) }
}
