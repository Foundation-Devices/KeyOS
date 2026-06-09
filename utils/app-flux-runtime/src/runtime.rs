// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(keyos))]
use std::sync::Mutex;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    OnceLock,
};

use gui_app_emu_flux::syscall_id::*;

const CX_OK: u32 = 0x00000000;
const CX_INTERNAL_ERROR: u32 = 0xFFFFFF85;
const CX_INVALID_PARAMETER: u32 = 0xFFFFFF88;
const MEM_ALLOC_ALIGN: usize = 8;
const MEM_ALLOC_HEADER_SIZE: usize =
    (core::mem::size_of::<usize>() + MEM_ALLOC_ALIGN - 1) & !(MEM_ALLOC_ALIGN - 1);

#[macro_export]
macro_rules! use_flux_runtime_api {
    () => {
        gui_app_emu_flux::use_api!();

        static FLUX_API: std::sync::LazyLock<FluxApi> = std::sync::LazyLock::new(FluxApi::new);

        pub(crate) fn flux_api() -> &'static FluxApi { &FLUX_API }

        fn flux_svc_call(syscall_id: u32, parameters: *mut core::ffi::c_void) -> Option<u32> {
            flux_api().svc_call(syscall_id, parameters).ok()
        }

        fn flux_io_seph_send(data: &[u8]) { flux_api().io_seph_send(data); }

        fn flux_io_seph_recv(max_len: usize) -> Option<Vec<u8>> { flux_api().io_seph_recv(max_len) }

        fn flux_syscall_buffer(id: u32, arg: u32, data: &mut [u8]) -> usize {
            flux_api().syscall_buffer(id, arg, data)
        }
    };
}

pub type SvcCallHook = fn(u32, *mut core::ffi::c_void) -> Option<u32>;
pub type SendSephHook = fn(&[u8]);
pub type RecvSephHook = fn(usize) -> Option<Vec<u8>>;
pub type SyscallBufferHook = fn(u32, u32, &mut [u8]) -> usize;
pub type NvmWriteHook = fn(*mut core::ffi::c_void, *const core::ffi::c_void, u32);
pub type CurrentAppTagHook = fn(u32) -> Option<&'static [u8]>;

#[derive(Clone, Copy)]
pub struct RuntimeHooks {
    svc_call: SvcCallHook,
    io_seph_send: SendSephHook,
    io_seph_recv: RecvSephHook,
    syscall_buffer: SyscallBufferHook,
    nvm_write: NvmWriteHook,
    current_app_tag: CurrentAppTagHook,
}

impl RuntimeHooks {
    pub fn new(
        svc_call: SvcCallHook,
        io_seph_send: SendSephHook,
        io_seph_recv: RecvSephHook,
        syscall_buffer: SyscallBufferHook,
    ) -> Self {
        Self {
            svc_call,
            io_seph_send,
            io_seph_recv,
            syscall_buffer,
            nvm_write: nvm_write_memory,
            current_app_tag: |_| None,
        }
    }

    pub fn with_nvm_write(mut self, nvm_write: NvmWriteHook) -> Self {
        self.nvm_write = nvm_write;
        self
    }

    pub fn with_current_app_tag(mut self, current_app_tag: CurrentAppTagHook) -> Self {
        self.current_app_tag = current_app_tag;
        self
    }
}

static RUNTIME_HOOKS: OnceLock<RuntimeHooks> = OnceLock::new();

pub fn init(hooks: RuntimeHooks) {
    if RUNTIME_HOOKS.set(hooks).is_err() {
        log::debug!("Flux runtime hooks already initialized");
    }
}

fn hooks() -> &'static RuntimeHooks {
    RUNTIME_HOOKS.get().unwrap_or_else(|| {
        log::error!("Flux runtime hooks were used before initialization");
        std::process::abort();
    })
}

pub(crate) fn svc_call(syscall_id: u32, parameters: *mut core::ffi::c_void) -> u32 {
    (hooks().svc_call)(syscall_id, parameters).unwrap_or(0)
}

pub(crate) fn syscall_buffer(id: u32, arg: u32, data: &mut [u8]) -> usize {
    (hooks().syscall_buffer)(id, arg, data)
}

fn send_seph(data: &[u8]) { (hooks().io_seph_send)(data); }

fn recv_seph(max_len: usize) -> Option<Vec<u8>> { (hooks().io_seph_recv)(max_len) }

pub fn nvm_write_memory(dst: *mut core::ffi::c_void, src: *const core::ffi::c_void, len: u32) {
    if dst.is_null() || len == 0 {
        return;
    }
    unsafe {
        if src.is_null() {
            core::ptr::write_bytes(dst as *mut u8, 0, len as usize);
        } else {
            core::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, len as usize);
        }
    }
}

pub fn nvm_write_noop(_dst: *mut core::ffi::c_void, _src: *const core::ffi::c_void, _len: u32) {}

/// Storage for the SDK's try_context pointer.
/// The SDK's TRY/CATCH/THROW mechanism uses try_context_get/set to manage
/// a stack of exception handlers (jmp_buf). We must store the pointer so
/// that THROW can longjmp back to the correct handler.
static TRY_CONTEXT_PTR: AtomicUsize = AtomicUsize::new(0);

/// Accumulation buffer for io_seph_send partial writes.
/// The SDK sends SEPH TLV packets in multiple calls, for example a 3-byte
/// header followed by the payload. We accumulate bytes until a full packet is
/// ready.
#[cfg(not(keyos))]
static SEPH_SEND_BUF: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Last touch event state for touch_get_last_info.
static LAST_TOUCH_STATE: AtomicUsize = AtomicUsize::new(0);
static LAST_TOUCH_X: AtomicUsize = AtomicUsize::new(0);
static LAST_TOUCH_Y: AtomicUsize = AtomicUsize::new(0);

fn capture_touch_state(data: &[u8]) {
    // FingerEvent TLV: [0x0c, len_hi, len_lo, state, x_hi, x_lo, y_hi, y_lo]
    if data.len() >= 8 && data[0] == 0x0c {
        let state = data[3];
        let x = i16::from_be_bytes([data[4], data[5]]);
        let y = i16::from_be_bytes([data[6], data[7]]);
        LAST_TOUCH_STATE.store(state as usize, Ordering::Relaxed);
        LAST_TOUCH_X.store(x as usize, Ordering::Relaxed);
        LAST_TOUCH_Y.store(y as usize, Ordering::Relaxed);
        log::trace!("capture_touch: state={state}, x={x}, y={y}");
    }
}

#[no_mangle]
pub extern "C" fn SVC_Call(syscall_id: u32, parameters: *mut core::ffi::c_void) -> u32 {
    log::trace!("SVC_Call(syscall_id: {:08x}, parameters: {:?})", syscall_id, parameters);
    match syscall_id {
        SYSCALL_IO_SEPH_SEND_ID => {
            let parameters: &[u32] = unsafe { core::slice::from_raw_parts(parameters as *const u32, 2) };
            let buffer = parameters[0] as *const u8;
            let length = parameters[1] as usize;
            let data: &[u8] = unsafe { core::slice::from_raw_parts(buffer, length) };
            if !data.is_empty() {
                log::debug!("SVC io_seph_send: tag=0x{:02x} len={}", data[0], length);
            }
            send_seph(data);
            0
        }
        SYSCALL_IO_SEPH_RECV_ID => {
            let parameters: &[u32] = unsafe { core::slice::from_raw_parts(parameters as *const u32, 3) };
            let buffer = parameters[0] as *mut u8;
            let maxlength = parameters[1] as usize;
            let _flags = parameters[2];
            let dest: &mut [u8] = unsafe { core::slice::from_raw_parts_mut(buffer, maxlength) };
            if let Some(data) = recv_seph(maxlength) {
                let data_len = data.len();
                if data_len > 0 && data_len <= maxlength {
                    log::debug!("SVC io_seph_recv: tag=0x{:02x} len={}", data[0], data_len);
                    capture_touch_state(&data);
                    dest[..data_len].copy_from_slice(&data);
                    data_len as u32
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    0
                }
            } else {
                std::thread::sleep(std::time::Duration::from_millis(10));
                0
            }
        }
        _ => {
            log::debug!("SVC_Call: id=0x{:08x}", syscall_id);
            svc_call(syscall_id, parameters)
        }
    }
}

#[no_mangle]
pub extern "C" fn SVC_cx_call(syscall_id: u32, parameters: *mut u32) -> u32 {
    log::warn!("SVC_cx_call(syscall_id: 0x{:08x}, parameters: {:?})", syscall_id, parameters);
    svc_call(syscall_id, parameters as *mut core::ffi::c_void)
}

#[no_mangle]
pub extern "C" fn halt() {
    log::error!("halt() called - this should not happen");
    loop {}
}

#[cfg(keyos)]
extern "C" {
    fn io_seph_send(buf: *const u8, len: u16);
}

#[cfg(not(keyos))]
#[no_mangle]
pub extern "C" fn io_seph_is_status_sent() -> u32 { 1 }

#[cfg(not(keyos))]
#[no_mangle]
pub extern "C" fn io_seph_recv(buf: *mut u8, maxlen: u16, _flags: u32) -> u16 {
    let maxlen = maxlen as usize;
    loop {
        if let Some(data) = recv_seph(maxlen) {
            let data_len = data.len();
            if data_len > 0 && data_len <= maxlen {
                log::debug!("io_seph_recv: tag=0x{:02x} len={}", data[0], data_len);
                capture_touch_state(&data);
                let dest = unsafe { core::slice::from_raw_parts_mut(buf, maxlen) };
                dest[..data_len].copy_from_slice(&data);
                return data_len as u16;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(not(keyos))]
#[no_mangle]
pub extern "C" fn io_seph_send(buf: *const u8, len: u16) {
    let data = unsafe { core::slice::from_raw_parts(buf, len as usize) };
    let mut acc = SEPH_SEND_BUF.lock().unwrap();
    acc.extend_from_slice(data);
    while acc.len() >= 3 {
        let payload_len = u16::from_be_bytes([acc[1], acc[2]]) as usize;
        let total_len = 3 + payload_len;
        if acc.len() >= total_len {
            let packet: Vec<u8> = acc.drain(..total_len).collect();
            log::debug!("io_seph_send: tag=0x{:02x} len={}", packet[0], payload_len);
            send_seph(&packet);
            std::thread::yield_now();
        } else {
            break;
        }
    }
}

#[no_mangle]
pub extern "C" fn nvm_write(dst: *mut core::ffi::c_void, src: *const core::ffi::c_void, len: u32) {
    (hooks().nvm_write)(dst, src, len);
}

#[no_mangle]
pub extern "C" fn os_global_pin_is_validated() -> u32 { 1 }

#[no_mangle]
pub extern "C" fn os_lib_end() {
    log::debug!("os_lib_end() called");
}

#[no_mangle]
pub extern "C" fn os_sched_current_task() -> u32 { 0 }

#[no_mangle]
pub extern "C" fn os_sched_exit(status: u32) {
    let exit_code = if status == 0xFF { 0 } else { status as i32 };
    log::debug!("os_sched_exit(0x{status:08x}) - exiting with code {exit_code}");
    for _ in 0..5 {
        server::xous::yield_slice();
    }
    std::process::exit(exit_code);
}

#[no_mangle]
pub extern "C" fn os_serial(_serial: *mut u8, _max_len: u32) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn os_setting_get(_setting_id: u32, _value: *mut u8, _max_len: u32) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn os_ux(_params: *const core::ffi::c_void) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn try_context_get() -> *mut core::ffi::c_void {
    TRY_CONTEXT_PTR.load(Ordering::Relaxed) as *mut core::ffi::c_void
}

#[no_mangle]
pub extern "C" fn try_context_set(ctx: *mut core::ffi::c_void) -> *mut core::ffi::c_void {
    let old = TRY_CONTEXT_PTR.swap(ctx as usize, Ordering::Relaxed);
    old as *mut core::ffi::c_void
}

fn read_nbgl_area_raw(area: *const core::ffi::c_void) -> [u8; 10] {
    let ptr = area as *const u8;
    let mut buf = [0u8; 10];
    unsafe { core::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), 10) };
    buf
}

fn area_to_seph_be(raw: &[u8; 10]) -> [u8; 10] {
    let x0 = u16::from_le_bytes([raw[0], raw[1]]);
    let y0 = u16::from_le_bytes([raw[2], raw[3]]);
    let width = u16::from_le_bytes([raw[4], raw[5]]);
    let height = u16::from_le_bytes([raw[6], raw[7]]);
    let mut be = [0u8; 10];
    be[0..2].copy_from_slice(&x0.to_be_bytes());
    be[2..4].copy_from_slice(&y0.to_be_bytes());
    be[4..6].copy_from_slice(&width.to_be_bytes());
    be[6..8].copy_from_slice(&height.to_be_bytes());
    be[8] = raw[8];
    be[9] = raw[9];
    be
}

fn send_seph_tlv(tag: u8, payload: &[u8]) {
    let len = payload.len() as u16;
    let mut pkt = Vec::with_capacity(3 + payload.len());
    pkt.push(tag);
    pkt.extend_from_slice(&len.to_be_bytes());
    pkt.extend_from_slice(payload);
    #[allow(unused_unsafe)]
    unsafe {
        io_seph_send(pkt.as_ptr(), pkt.len() as u16)
    };
}

fn nbgl_bpp_to_read_bpp(bpp: u8) -> u8 {
    match bpp {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn nbgl_frontDrawLine(area: *const core::ffi::c_void, color: u32, mask: u32) {
    let raw = read_nbgl_area_raw(area);
    let area_bytes = area_to_seph_be(&raw);
    let mut payload = Vec::with_capacity(12);
    payload.extend_from_slice(&area_bytes);
    payload.push(mask as u8);
    payload.push(color as u8);
    send_seph_tlv(0xFC, &payload);
}

#[no_mangle]
pub extern "C" fn nbgl_frontDrawRect(area: *const core::ffi::c_void) {
    let raw = read_nbgl_area_raw(area);
    let area_bytes = area_to_seph_be(&raw);
    send_seph_tlv(0xFA, &area_bytes);
}

#[no_mangle]
pub extern "C" fn nbgl_frontRefreshArea(area: *const core::ffi::c_void, _mode: u32, _post_action: u32) {
    let raw = read_nbgl_area_raw(area);
    let area_bytes = area_to_seph_be(&raw);
    send_seph_tlv(0xFB, &area_bytes);
}

#[no_mangle]
pub extern "C" fn nbgl_frontDrawHorizontalLine(area: *const core::ffi::c_void, mask: u8, color: u32) {
    let raw = read_nbgl_area_raw(area);
    let area_bytes = area_to_seph_be(&raw);
    let mut payload = Vec::with_capacity(12);
    payload.extend_from_slice(&area_bytes);
    payload.push(mask);
    payload.push(color as u8);
    send_seph_tlv(0xFC, &payload);
}

#[no_mangle]
pub extern "C" fn nbgl_frontDrawImage(
    area: *const core::ffi::c_void,
    buffer: *const u8,
    transformation: u32,
    color_map: u32,
) {
    let raw = read_nbgl_area_raw(area);
    let area_bytes = area_to_seph_be(&raw);
    let width = u16::from_le_bytes([raw[4], raw[5]]) as usize;
    let height = u16::from_le_bytes([raw[6], raw[7]]) as usize;
    let bpp = nbgl_bpp_to_read_bpp(raw[9]);
    let bit_size = width * height * bpp as usize;
    let buffer_size = bit_size.div_ceil(8);

    let mut payload = Vec::with_capacity(10 + buffer_size + 2);
    payload.extend_from_slice(&area_bytes);
    if !buffer.is_null() && buffer_size > 0 {
        let data = unsafe { core::slice::from_raw_parts(buffer, buffer_size) };
        payload.extend_from_slice(data);
    } else {
        payload.resize(10 + buffer_size, 0);
    }
    payload.push(transformation as u8);
    payload.push(color_map as u8);
    send_seph_tlv(0xFD, &payload);
}

#[no_mangle]
pub extern "C" fn nbgl_frontDrawImageFile(
    area: *const core::ffi::c_void,
    buffer: *const u8,
    color_map: u32,
    _uzlib_chunk_buffer: *const u8,
) {
    let raw = read_nbgl_area_raw(area);
    let area_bytes = area_to_seph_be(&raw);
    if buffer.is_null() {
        log::warn!("nbgl_frontDrawImageFile: NULL buffer, skipping");
        return;
    }
    let header = unsafe { core::slice::from_raw_parts(buffer, 8) };
    let data_len = header[5] as usize | ((header[6] as usize) << 8) | ((header[7] as usize) << 16);
    let total_file_len = 8 + data_len;
    let file_data = unsafe { core::slice::from_raw_parts(buffer, total_file_len) };

    let mut payload = Vec::with_capacity(10 + 1 + total_file_len);
    payload.extend_from_slice(&area_bytes);
    payload.push(color_map as u8);
    payload.extend_from_slice(file_data);
    send_seph_tlv(0xFE, &payload);
}

#[no_mangle]
pub extern "C" fn nbgl_frontDrawImageRle(
    area: *const core::ffi::c_void,
    buffer: *const u8,
    buffer_len: u32,
    fore_color: u32,
    nb_skipped_bytes: u8,
) {
    let raw = read_nbgl_area_raw(area);
    let area_bytes = area_to_seph_be(&raw);
    let buf_len = buffer_len as usize;

    let mut payload = Vec::with_capacity(10 + 2 + buf_len);
    payload.extend_from_slice(&area_bytes);
    payload.push(nb_skipped_bytes);
    payload.push(fore_color as u8);
    if !buffer.is_null() && buf_len > 0 {
        let data = unsafe { core::slice::from_raw_parts(buffer, buf_len) };
        payload.extend_from_slice(data);
    }
    send_seph_tlv(0xFF, &payload);
}

#[no_mangle]
pub extern "C" fn nbgl_drawText(
    area: *const core::ffi::c_void,
    text: *const u8,
    text_len: u16,
    font_id: u32,
    font_color: u32,
) -> u32 {
    let raw = read_nbgl_area_raw(area);
    let area_bytes = area_to_seph_be(&raw);

    let text_len_usize = text_len as usize;
    let mut payload = Vec::with_capacity(14 + text_len_usize);
    payload.extend_from_slice(&area_bytes);
    payload.push(font_id as u8);
    payload.push(font_color as u8);
    payload.extend_from_slice(&(text_len).to_be_bytes());
    if !text.is_null() && text_len_usize > 0 {
        let data = unsafe { core::slice::from_raw_parts(text, text_len_usize) };
        payload.extend_from_slice(data);
    }
    send_seph_tlv(0xF9, &payload);

    font_id
}

#[no_mangle]
pub extern "C" fn nbgl_screen_reinit() {}

#[no_mangle]
pub extern "C" fn nbgl_wait_pipeline() {}

#[no_mangle]
pub extern "C" fn os_flags() -> u32 { 0 }

#[no_mangle]
pub extern "C" fn os_perso_is_pin_set() -> u32 { 1 }

#[no_mangle]
pub extern "C" fn os_pki_load_certificate(
    _format: u32,
    _certificate: *const u8,
    _certificate_len: u32,
    _trusted_name: *mut u8,
    _trusted_name_len: *mut u32,
    _public_key: *mut u8,
) -> u32 {
    0
}

#[no_mangle]
pub extern "C" fn os_registry_get_current_app_tag(tag: u32, buffer: *mut u8, max_len: u32) -> u32 {
    let Some(value) = (hooks().current_app_tag)(tag) else {
        return 0;
    };
    let copy_len = value.len().min(max_len as usize);
    if !buffer.is_null() && copy_len > 0 {
        unsafe { core::ptr::copy_nonoverlapping(value.as_ptr(), buffer, copy_len) };
    }
    copy_len as u32
}

#[no_mangle]
pub extern "C" fn os_sched_is_running(_task_id: u32) -> u32 { 1 }

#[no_mangle]
pub extern "C" fn os_sched_last_status(_task_id: u32) -> u32 { 0xAA }

#[no_mangle]
pub extern "C" fn os_sched_yield(_status: u32) {}

#[no_mangle]
pub extern "C" fn os_perso_derive_eip2333(
    _mode: u32,
    _path: *const u32,
    _path_len: u32,
    _private_key: *mut u8,
) -> u32 {
    log::debug!("os_perso_derive_eip2333 called - returning error");
    1
}

#[no_mangle]
pub extern "C" fn os_lib_call(_call_parameters: *mut core::ffi::c_void) -> u32 {
    log::debug!("os_lib_call called - returning error");
    1
}

#[no_mangle]
pub extern "C" fn touch_get_last_info(info: *mut core::ffi::c_void) -> u32 {
    let state = LAST_TOUCH_STATE.load(Ordering::Relaxed) as u8;
    let x = LAST_TOUCH_X.load(Ordering::Relaxed) as u16;
    let y = LAST_TOUCH_Y.load(Ordering::Relaxed) as u16;
    let ptr = info as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(x.to_ne_bytes().as_ptr(), ptr, 2);
        core::ptr::copy_nonoverlapping(y.to_ne_bytes().as_ptr(), ptr.add(2), 2);
        *ptr.add(4) = state;
        *ptr.add(5) = 0;
        *ptr.add(6) = 0;
        *ptr.add(7) = 0;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn cx_rng_no_throw(buffer: *mut u8, len: u32) -> u32 {
    if len == 0 {
        return CX_OK;
    }
    if buffer.is_null() {
        return CX_INVALID_PARAMETER;
    }

    let buf = core::slice::from_raw_parts_mut(buffer, len as usize);
    match getrandom::getrandom(buf) {
        Ok(()) => CX_OK,
        Err(e) => {
            log::error!("cx_rng_no_throw: getrandom failed: {e:?}");
            CX_INTERNAL_ERROR
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn cx_get_random_bytes(buffer: *mut u8, len: u32) -> u32 {
    cx_rng_no_throw(buffer, len)
}

#[no_mangle]
pub unsafe extern "C" fn cx_trng_get_random_data(buffer: *mut u8, len: u32) {
    let _ = cx_rng_no_throw(buffer, len);
}

fn random_u32() -> Option<u32> {
    let mut bytes = [0u8; 4];
    if let Err(e) = getrandom::getrandom(&mut bytes) {
        log::error!("random_u32: getrandom failed: {e:?}");
        return None;
    }
    Some(u32::from_le_bytes(bytes))
}

fn random_u32_range(low: u32, high: u32) -> u32 {
    if high <= low {
        return low;
    }

    let span = high - low;
    let limit = ((u64::from(u32::MAX) + 1) / u64::from(span)) * u64::from(span);
    loop {
        let Some(value) = random_u32() else {
            return low;
        };
        let value = u64::from(value);
        if value < limit {
            return low + (value % u64::from(span)) as u32;
        }
    }
}

#[no_mangle]
pub extern "C" fn cx_rng_u32_range(low: u32, high: u32) -> u32 { random_u32_range(low, high) }

#[no_mangle]
pub extern "C" fn cx_rng_u32_range_func(low: u32, high: u32, _rng: *const core::ffi::c_void) -> u32 {
    random_u32_range(low, high)
}

#[no_mangle]
pub extern "C" fn os_io_tx_cmd(type_: u8, buffer: *const u8, length: u16, _timeout_ms: *const u32) -> i32 {
    if buffer.is_null() || length == 0 {
        return 0;
    }
    let raw = unsafe { core::slice::from_raw_parts(buffer, length as usize) };

    if type_ == 0x10 {
        let len = raw.len();
        let mut packet = Vec::with_capacity(3 + len);
        packet.push(0x53);
        packet.push((len >> 8) as u8);
        packet.push(len as u8);
        packet.extend_from_slice(raw);
        log::debug!("os_io_tx_cmd: RAW_APDU -> Rapdu ({} bytes): {:02x?}", len, &raw[..len.min(16)]);
        send_seph(&packet);
        return length as i32;
    }

    if raw.len() < 3 {
        return 0;
    }
    let payload_len = u16::from_be_bytes([raw[1], raw[2]]) as usize;
    let total = 3 + payload_len;
    if total > raw.len() {
        send_seph(raw);
    } else {
        send_seph(&raw[..total]);
    }
    0
}

#[no_mangle]
pub extern "C" fn os_io_stop() {}

#[no_mangle]
pub extern "C" fn os_io_start() {}

#[no_mangle]
pub extern "C" fn os_io_init() {}

#[no_mangle]
pub extern "C" fn os_io_rx_evt(buf: *mut u8, maxlen: u16, _timeout_ms: *mut u32, _check_se: u32) -> i32 {
    let maxlen = maxlen as usize;
    if buf.is_null() || maxlen < 2 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        return 0;
    }
    if let Some(data) = recv_seph(maxlen - 1) {
        let data_len = data.len();
        if data_len > 0 && data_len + 1 <= maxlen {
            let dest = unsafe { core::slice::from_raw_parts_mut(buf, maxlen) };

            if data[0] == 0x16 && data_len >= 3 {
                let apdu = &data[3..];
                if !apdu.is_empty() && apdu.len() + 1 <= maxlen {
                    log::debug!(
                        "os_io_rx_evt: CapduEvent -> RAW_APDU ({} bytes): {:02x?}",
                        apdu.len(),
                        &apdu[..apdu.len().min(16)]
                    );
                    dest[0] = 0x10;
                    dest[1..1 + apdu.len()].copy_from_slice(apdu);
                    return (apdu.len() + 1) as i32;
                }
            }

            if data[0] != 0x0e {
                log::debug!(
                    "os_io_rx_evt: tag=0x{:02x} len={} data={:02x?}",
                    data[0],
                    data_len,
                    &data[..data_len.min(16)]
                );
            }
            capture_touch_state(&data);
            dest[0] = 0x01;
            dest[1..1 + data_len].copy_from_slice(&data);
            return (data_len + 1) as i32;
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
    0
}

#[no_mangle]
pub extern "C" fn mem_init(_heap_start: *mut u8, _heap_size: usize) -> *mut core::ffi::c_void {
    1usize as *mut core::ffi::c_void
}

#[no_mangle]
pub extern "C" fn mem_alloc(_ctx: *mut core::ffi::c_void, nb_bytes: usize) -> *mut core::ffi::c_void {
    if nb_bytes == 0 {
        return core::ptr::null_mut();
    }
    let total_size = match MEM_ALLOC_HEADER_SIZE.checked_add(nb_bytes) {
        Some(size) => size,
        None => return core::ptr::null_mut(),
    };
    let layout = match std::alloc::Layout::from_size_align(total_size, MEM_ALLOC_ALIGN) {
        Ok(l) => l,
        Err(_) => return core::ptr::null_mut(),
    };
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        return core::ptr::null_mut();
    }
    unsafe {
        (ptr as *mut usize).write(nb_bytes);
        ptr.add(MEM_ALLOC_HEADER_SIZE) as *mut core::ffi::c_void
    }
}

#[no_mangle]
pub extern "C" fn mem_free(_ctx: *mut core::ffi::c_void, ptr: *mut core::ffi::c_void) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let base = (ptr as *mut u8).sub(MEM_ALLOC_HEADER_SIZE);
        let nb_bytes = (base as *const usize).read();
        let Some(total_size) = MEM_ALLOC_HEADER_SIZE.checked_add(nb_bytes) else {
            return;
        };
        let Ok(layout) = std::alloc::Layout::from_size_align(total_size, MEM_ALLOC_ALIGN) else {
            return;
        };
        std::alloc::dealloc(base, layout);
    }
}

#[cfg(not(keyos))]
#[no_mangle]
pub extern "C" fn strchr(s: *const i8, c: i32) -> *const i8 {
    unsafe {
        let mut p = s;
        while *p != 0 {
            if *p as i32 == c {
                return p;
            }
            p = p.add(1);
        }
        if c == 0 {
            return p;
        }
        core::ptr::null()
    }
}

#[cfg(not(keyos))]
mod hosted_stubs {
    #[no_mangle]
    pub static mut G_ux_params: [u8; 128] = [0; 128];

    #[no_mangle]
    pub extern "C" fn common_app_init() {}

    #[no_mangle]
    pub extern "C" fn app_exit() {
        log::debug!("app_exit called");
        std::process::exit(0);
    }

    #[no_mangle]
    pub extern "C" fn io_exchange(_channel: u32, _tx_len: u16) -> u16 { 0 }

    #[no_mangle]
    pub extern "C" fn io_seproxyhal_io_heartbeat() {}

    #[no_mangle]
    pub extern "C" fn io_seproxyhal_play_tune(_tune_id: u32) {}

    #[no_mangle]
    pub extern "C" fn os_io_seph_cmd_piezo_play_tune(_tune_id: u32) {}

    #[no_mangle]
    pub extern "C" fn cx_sha3_init_no_throw(_ctx: *mut u8, _size: u32) -> u32 { 0 }

    #[no_mangle]
    pub extern "C" fn cx_sha224_init_no_throw(_ctx: *mut u8) -> u32 { 0 }
}

#[allow(non_upper_case_globals)]
#[no_mangle]
pub static _bss: u8 = 0;

#[allow(non_upper_case_globals)]
#[no_mangle]
pub static _ebss: u8 = 0;
