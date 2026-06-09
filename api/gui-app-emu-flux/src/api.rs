// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use server::{CheckedConn, CheckedPermissions, MessageAllowed};
use xous::DropDeallocate;

use crate::messages::{RecvSeph, SendSeph, SvcCall, SyscallBuffer};

pub const SERVER_NAME: &str = "os/gui-app-emu-flux";
const PAGE_SIZE: usize = 4096;

#[macro_export]
macro_rules! use_api {
    () => {
        mod flux_permissions {
            use gui_app_emu_flux::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/gui-app-emu-flux"]
            pub struct FluxPermissions;
        }
        type FluxApi = gui_app_emu_flux::api::FluxApi<flux_permissions::FluxPermissions>;
    };
}

pub struct FluxApi<P: CheckedPermissions> {
    conn: CheckedConn<P>,
}

impl<P: CheckedPermissions> Default for FluxApi<P> {
    fn default() -> Self { Self::new() }
}

impl<P: CheckedPermissions> FluxApi<P> {
    pub fn new() -> Self { Self { conn: CheckedConn::default() } }

    pub fn try_new_with_timeout(timeout: Duration) -> Option<Self> {
        Some(Self { conn: CheckedConn::try_connect_with_timeout(timeout)? })
    }

    pub fn svc_call(&self, syscall_id: u32, parameters: *mut core::ffi::c_void) -> Result<u32, xous::Error>
    where
        P: MessageAllowed<SvcCall>,
    {
        self.conn.try_send_blocking_scalar(SvcCall(syscall_id, parameters as u32))
    }

    pub fn io_seph_send(&self, data: &[u8])
    where
        P: MessageAllowed<SendSeph>,
    {
        self.conn.send_archive(SendSeph(data.to_vec()));
    }

    pub fn io_seph_recv(&self, max_len: usize) -> Option<Vec<u8>>
    where
        P: MessageAllowed<RecvSeph>,
    {
        self.conn.send_blocking_archive(RecvSeph(max_len))
    }

    /// Send a syscall buffer message (LendMut) for operations that need shared memory.
    ///
    /// The caller fills `data` with input; on return, `data` contains the output
    /// written by the server handler.
    ///
    /// # Arguments
    /// * `syscall_id` - The syscall ID
    /// * `arg` - Additional argument (interpretation depends on syscall)
    /// * `data` - The mutable buffer for input/output
    ///
    /// # Returns
    /// The response value from the server (0 on success, usize::MAX on error).
    pub fn syscall_buffer(&self, syscall_id: u32, arg: u32, data: &mut [u8]) -> usize
    where
        P: MessageAllowed<SyscallBuffer>,
    {
        let alloc_len = data.len().max(PAGE_SIZE).next_multiple_of(PAGE_SIZE);
        let mem = xous::map_memory(None, None, alloc_len, xous::MemoryFlags::W | xous::MemoryFlags::POPULATE);
        let mut mem = match mem {
            Ok(m) => DropDeallocate::new(m),
            Err(e) => {
                log::error!("syscall_buffer: failed to allocate memory: {:?}", e);
                return usize::MAX;
            }
        };

        // Copy input data into the shared buffer
        let shared = mem.as_slice_mut::<u8>();
        let copy_len = data.len().min(shared.len());
        shared[..copy_len].copy_from_slice(&data[..copy_len]);

        let msg = SyscallBuffer { buf: *mem, syscall_id, arg };
        let result = self.conn.lend_mut(msg);

        // Copy output data back from the shared buffer
        let shared = mem.as_slice::<u8>();
        data[..copy_len].copy_from_slice(&shared[..copy_len]);

        result
    }
}
