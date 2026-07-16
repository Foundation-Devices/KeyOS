// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{collections::BTreeMap, sync::Mutex};

use xous::MemoryRange;

use crate::{CAMERA_BYTES_PER_PX, CAMERA_FB_SIZE_BYTES, CAMERA_HEIGHT, CAMERA_MARGIN, CAMERA_WIDTH};

pub struct Frame {
    id: u32,
}

static SHMEM_CACHE: Mutex<BTreeMap<u32, MemoryRange>> = Mutex::new(BTreeMap::new());

impl Frame {
    pub fn allocate() -> (Self, &'static mut [u8]) {
        for _ in 0..10 {
            let id = rand::random::<u32>();
            if let Ok(shmem) =
                shared_memory::ShmemConf::new().size(CAMERA_FB_SIZE_BYTES).os_id(Self::os_id(id)).create()
            {
                let shmem = Box::leak(Box::new(shmem));
                let slice = unsafe { shmem.as_slice_mut() };
                for pixel in slice.chunks_mut(4) {
                    // Set alpha to opaque
                    pixel[3] = 255;
                }
                return (Self { id }, slice);
            }
        }
        panic!("Could not create shmem for camera");
    }

    fn os_id(id: u32) -> String { format!("/xous_cam_buf_{id}") }

    pub fn padded_range(&self) -> xous::MemoryRange {
        if let Some(range) = SHMEM_CACHE.lock().unwrap().get(&self.id) {
            return *range;
        }
        let shmem = shared_memory::ShmemConf::new()
            .size(CAMERA_FB_SIZE_BYTES)
            .os_id(Self::os_id(self.id))
            .open()
            .unwrap();
        let shmem = Box::leak(Box::new(shmem));
        let slice = unsafe { shmem.as_slice_mut() };
        let range = unsafe { MemoryRange::new(slice.as_ptr() as usize, slice.len()).unwrap() };
        SHMEM_CACHE.lock().unwrap().insert(self.id, range);
        range
    }

    pub fn content(&self) -> xous::MemoryRange {
        self.padded_range()
            .subrange(
                CAMERA_WIDTH * CAMERA_MARGIN * CAMERA_BYTES_PER_PX,
                CAMERA_WIDTH * CAMERA_HEIGHT * CAMERA_BYTES_PER_PX,
            )
            .unwrap()
    }
}

impl server::AsScalar<1> for Frame {
    fn as_scalar(&self) -> [u32; 1] { [self.id] }
}

impl server::FromScalar<1> for Frame {
    fn from_scalar([id]: [u32; 1]) -> Self { Self { id } }
}
