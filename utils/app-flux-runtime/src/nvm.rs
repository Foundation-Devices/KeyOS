// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Non-volatile storage for a Flux app's `N_storage` region, backed by the app's
//! own AppData through `FileBacked`. Each child persists its own image directly;
//! there is no host RPC and no per-app key, since AppData already scopes the file
//! to the calling app.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use file_backed::{FileBacked, FileBackedPermissions};

const NVM_FILE: &str = "nvm.bin";

// Installed by `init_nvm` with the app's concrete fs permission type, so `nvm_write` (reached from
// C, with no generic context) can persist the region with the right permissions. A library can't
// name a manifest-scoped permission type, so the app hands one in via `init_nvm::<P>`.
static PERSIST_FN: OnceLock<fn()> = OnceLock::new();

/// The app's NVM region, registered by `init_nvm`. Zero length means unregistered, in which
/// case NVM writes stay in memory and are lost when the app exits.
static NVM_BASE: AtomicUsize = AtomicUsize::new(0);
static NVM_LEN: AtomicUsize = AtomicUsize::new(0);

fn nvm_region() -> Option<(*mut u8, usize)> {
    let base = NVM_BASE.load(Ordering::Relaxed);
    let len = NVM_LEN.load(Ordering::Relaxed);
    (base != 0 && len != 0).then_some((base as *mut u8, len))
}

/// Register the app's NVM region and restore whatever was persisted to its AppData.
///
/// A Flux app's `nvm_write` only reaches its own memory, and each launch is a fresh process,
/// so without this its storage resets every time and any user setting is lost. The region is
/// left untouched when nothing is persisted, so the app's `storage_init` still sees a zeroed
/// struct on first run and applies its own defaults.
///
/// # Safety
/// `base` must point to at least `len` writable bytes that live for the whole process.
pub unsafe fn init_nvm<P: FileBackedPermissions>(base: *mut u8, len: usize) {
    if base.is_null() || len == 0 {
        log::error!("init_nvm: refusing to register an empty NVM region");
        return;
    }
    NVM_BASE.store(base as usize, Ordering::Relaxed);
    NVM_LEN.store(len, Ordering::Relaxed);
    // The C nvm_write path has no generic context, so record how to persist with `P` now.
    let _ = PERSIST_FN.set(persist_region::<P>);

    let (backing, restored) = FileBacked::<Vec<u8>, P>::new(NVM_FILE, fs::Location::AppData);
    if !restored {
        log::debug!("init_nvm: nothing persisted yet, leaving the region as-is");
        return;
    }
    let image: Vec<u8> = (*backing).clone();
    if image.is_empty() {
        return;
    }
    if image.len() != len {
        log::error!("init_nvm: stored image is {} bytes for a {len}-byte region, ignoring", image.len());
        return;
    }
    // SAFETY: `base`/`len` are the region just registered above; this function's
    // contract requires `base` to point to at least `len` writable bytes that live
    // for the whole process, and the `image.len() != len` guard makes the source
    // exactly `len` bytes. The source is a heap Vec and the destination the app's
    // static NVM region, so they never overlap.
    unsafe { core::ptr::copy_nonoverlapping(image.as_ptr(), base, len) };
    log::debug!("init_nvm: restored {len} bytes of NVM");
}

/// Whether a write lands anywhere in the registered region, and so changes what must be
/// persisted. Writes elsewhere are ordinary memory that never needs saving.
fn write_touches_nvm(dst: *mut core::ffi::c_void, len: u32) -> bool {
    let Some((base, region_len)) = nvm_region() else {
        return false;
    };
    let start = dst as usize;
    let base = base as usize;
    start < base.saturating_add(region_len) && start.saturating_add(len as usize) > base
}

/// Persist the whole registered region to the app's AppData. Cheap enough to do on every
/// write that touches it: storage structs are a handful of bytes and apps only write them
/// when a setting changes.
fn persist_region<P: FileBackedPermissions>() {
    let Some((base, len)) = nvm_region() else {
        return;
    };
    let mut image = vec![0u8; len];
    // SAFETY: `base`/`len` are the region registered by `init_nvm`, so `base` points
    // to at least `len` readable bytes for the process lifetime; `image` is a fresh
    // `len`-byte Vec, so it holds exactly `len` writable bytes and cannot overlap the
    // region.
    unsafe { core::ptr::copy_nonoverlapping(base, image.as_mut_ptr(), len) };
    let (mut backing, _) = FileBacked::<Vec<u8>, P>::new(NVM_FILE, fs::Location::AppData);
    // The guard writes the file when it drops.
    *backing.guard() = image;
}

/// Write into memory and mirror the region to the app's AppData, so the change survives a
/// relaunch. Falls back to a plain memory write when no region is registered or the write
/// lands outside it, which is what apps without persistent storage want.
pub fn nvm_write_persist(dst: *mut core::ffi::c_void, src: *const core::ffi::c_void, len: u32) {
    crate::runtime::nvm_write_memory(dst, src, len);
    if write_touches_nvm(dst, len) {
        if let Some(persist) = PERSIST_FN.get() {
            persist();
        }
    }
}
