#![cfg_attr(target_os = "none", no_std)]

//! Detailed docs are parked under Structs/XousNames down below

pub mod api;

use core::num::NonZeroUsize;

use xous::MemoryFlags;

use crate::api::NAME_MAX_LENGTH;

/// A page-aligned stack allocation for connection requests
#[repr(C, align(4096))]
struct AlignedBuffer {
    data: [u8; 4096],
}
impl AlignedBuffer {
    pub fn new_connect_request(name: &str) -> Self {
        let mut cr = AlignedBuffer { data: [0u8; 4096] };
        let name_bytes = name.as_bytes();
        // Copy the string into our backing store.
        for (&src_byte, dest_byte) in name_bytes.iter().zip(&mut cr.data[0..NAME_MAX_LENGTH]) {
            *dest_byte = src_byte;
        }
        cr
    }

    pub fn new_register_request(sid: xous::SID, name: &str) -> Self {
        let mut cr = AlignedBuffer { data: [0u8; 4096] };
        let name_bytes = name.as_bytes();
        cr.data[0..16].copy_from_slice(&sid.to_bytes());
        // Copy the string into our backing store.
        for (&src_byte, dest_byte) in name_bytes.iter().zip(&mut cr.data[16..NAME_MAX_LENGTH + 16]) {
            *dest_byte = src_byte;
        }
        cr
    }
}

#[doc = include_str!("../README.md")]
#[derive(Debug)]
pub struct XousNames {
    conn: xous::CID,
}
impl XousNames {
    pub fn new() -> Result<Self, xous::Error> {
        let conn = xous::connect(xous::SID::from_bytes(b"xous-name-server").unwrap())
            .expect("Couldn't connect to XousNames");
        Ok(XousNames { conn })
    }

    /// Register a server with the SID of the already running server, and a plaintext `name`
    pub fn register_name(&self, sid: xous::SID, name: &str) -> Result<(), xous::Error> {
        let mut request = AlignedBuffer::new_register_request(sid, name);
        let msg = xous::MemoryMessage {
            id: api::Opcode::Register as usize,
            buf: unsafe {
                // safety: `request` is #[repr(C, align(4096))], and should be exactly on page in size
                xous::MemoryRange::new(
                    &mut request as *mut _ as *mut u8 as usize,
                    core::mem::size_of::<AlignedBuffer>(),
                )?
            },
            offset: None,
            valid: xous::MemorySize::new(name.len().min(NAME_MAX_LENGTH) as usize),
        };
        xous::send_message(self.conn, xous::Message::MutableBorrow(msg))?;

        let result = u32::from_le_bytes(request.data[0..4].try_into().unwrap());
        if result == 0 {
            Ok(())
        } else {
            let error = u32::from_le_bytes(request.data[4..8].try_into().unwrap());
            Err(xous::Error::from_usize(error as usize))
        }
    }

    pub fn request_connection_impl(&self, name: &str, blocking: bool) -> Result<xous::CID, xous::Error> {
        let mut request = AlignedBuffer::new_connect_request(name);
        let msg = xous::MemoryMessage {
            id: if blocking {
                api::Opcode::BlockingConnect as usize
            } else {
                api::Opcode::TryConnect as usize
            },
            buf: unsafe {
                // safety: `request` is #[repr(C, align(4096))], and should be exactly on page in size
                xous::MemoryRange::new(
                    &mut request as *mut _ as *mut u8 as usize,
                    core::mem::size_of::<AlignedBuffer>(),
                )?
            },
            offset: None,
            valid: xous::MemorySize::new(name.len().min(NAME_MAX_LENGTH) as usize),
        };
        xous::send_message(self.conn, xous::Message::MutableBorrow(msg))?;

        let result = u32::from_le_bytes(request.data[0..4].try_into().unwrap());
        let data = u32::from_le_bytes(request.data[4..8].try_into().unwrap());
        if result == 0 {
            Ok(data.into())
        } else {
            Err(xous::Error::from_usize(data as usize))
        }
    }

    /// Requests a permanent connection to server with `name`. Xous names brokers the
    /// entire connection, so the return value is the process-local CID (connection ID);
    /// the 128-bit server ID is never revealed.
    ///
    /// This call will fail if the server has not yet started up, which is a common
    /// problem during the boot process as the server start order is not guaranteed. Refer to
    /// `request_connection_blocking()` for a call that will automatically retry.
    pub fn request_connection(&self, name: &str) -> Result<xous::CID, xous::Error> {
        self.request_connection_impl(name, false)
    }

    /// Requests a permanent connection to server with `name`. Xous names brokers the
    /// entire connection, so the return value is the process-local CID (connection ID);
    /// the 128-bit server ID is never revealed.
    ///
    /// This call uses the API already in place in `libstd`, hence the different style of
    /// argument passing, and tons of `unsafe` code.
    pub fn request_connection_blocking(&self, name: &str) -> Result<xous::CID, xous::Error> {
        self.request_connection_impl(name, true)
    }

    pub fn add_manifest(&self, manifest_data: &[u8]) -> Result<(), xous::Error> {
        let mut buf = DropDeallocate(xous::map_memory(
            None,
            None,
            manifest_data.len().next_multiple_of(0x1000),
            MemoryFlags::W,
        )?);
        buf.0.as_slice_mut()[..manifest_data.len()].copy_from_slice(manifest_data);
        let msg = xous::MemoryMessage {
            id: api::Opcode::AddManifest as usize,
            buf: buf.0,
            offset: None,
            valid: NonZeroUsize::new(manifest_data.len()),
        };
        xous::send_message(self.conn, xous::Message::MutableBorrow(msg))?;

        let buf_slice = buf.0.as_slice::<u8>();
        let result = u32::from_le_bytes(buf_slice[0..4].try_into().unwrap());
        if result == 0 {
            Ok(())
        } else {
            let error = u32::from_le_bytes(buf_slice[4..8].try_into().unwrap());
            Err(xous::Error::from_usize(error as usize))
        }
    }

    pub fn remove_manifest(&self, app_id: xous::AppId) -> Result<(), xous::Error> {
        let mut buf = DropDeallocate(xous::map_memory(None, None, 0x1000, MemoryFlags::W)?);
        buf.0.as_slice_mut()[..app_id.0.len()].copy_from_slice(&app_id.0);
        let msg = xous::MemoryMessage {
            id: api::Opcode::RemoveManifest as usize,
            buf: buf.0,
            offset: None,
            valid: NonZeroUsize::new(app_id.0.len()),
        };
        xous::send_message(self.conn, xous::Message::MutableBorrow(msg))?;

        let buf_slice = buf.0.as_slice::<u8>();
        let result = u32::from_le_bytes(buf_slice[0..4].try_into().unwrap());
        if result == 0 {
            Ok(())
        } else {
            let error = u32::from_le_bytes(buf_slice[4..8].try_into().unwrap());
            Err(xous::Error::from_usize(error as usize))
        }
    }
}

impl Drop for XousNames {
    fn drop(&mut self) { xous::disconnect(self.conn).unwrap(); }
}

struct DropDeallocate(xous::MemoryRange);
impl Drop for DropDeallocate {
    fn drop(&mut self) { xous::unmap_memory(self.0).ok(); }
}
