// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    io::{Read, Seek, Write},
    thread,
    time::Duration,
};

use fs::{Location, OpenFlags};
use server::xous::{self, MemoryFlags};

fs::use_api!();
security::use_api!();
#[cfg(keyos)]
usb::use_host_api!();

const TEST_FILE_NAME: &str = "big.bin";
const COPY1_FILE_NAME: &str = "big_copy1.bin";
const COPY2_FILE_NAME: &str = "big_copy2.bin";
const BUFFER_LEN: usize = 1024 * 1024;
const BUFFER_COUNT: usize = 8;
const RANDOM_READ_COUNT: usize = 1000;

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    thread::sleep(Duration::from_secs(1));

    log::info!("fs benchmark starting");
    log::info!("Login result: {:?}", Security::default().log_in("123456".into()));

    let mut data_buf =
        xous::map_memory(None, None, BUFFER_LEN, MemoryFlags::W | MemoryFlags::POPULATE).unwrap();
    let data = data_buf.as_slice_mut();
    for (i, d) in data.iter_mut().enumerate() {
        *d = i as u8
    }
    let fs = FileSystem::default();
    log::info!("Initialized");

    let test_locations = [
        Location::Airlock,
        Location::AppData,
        Location::System,
        #[cfg(keyos)]
        Location::Usb,
    ];

    for location in test_locations {
        #[cfg(keyos)]
        if location == Location::Usb {
            let usb_host = UsbHost::default();
            usb_host.set_enabled(true).unwrap();
            while fs.open_dir("/", location).is_err() {
                log::info!("Waiting for Usb drive insertion");
                thread::sleep(Duration::from_secs(2));
            }
        }
        log::info!("Testing location {location:?}");
        fs.remove(TEST_FILE_NAME, location).ok();
        fs.remove(COPY1_FILE_NAME, location).ok();
        fs.remove(COPY2_FILE_NAME, location).ok();

        // --- Measure writing ---

        let write_start = std::time::Instant::now();
        {
            let mut file = fs
                .open_file(TEST_FILE_NAME, location, OpenFlags { read: false, write: true, create: true })
                .unwrap();

            for _ in 0..BUFFER_COUNT {
                file.write_all(&data).unwrap();
            }
            file.flush().unwrap();
        }
        let elapsed = write_start.elapsed().as_secs_f32();

        log::info!(
            "Write speed at {location:?}: {:.1} MB/s",
            (BUFFER_LEN * BUFFER_COUNT) as f32 / (1024.0 * 1024.0) / elapsed
        );

        // --- Measure reading ---

        let mut read_data_buf =
            xous::map_memory(None, None, BUFFER_LEN, MemoryFlags::W | MemoryFlags::POPULATE).unwrap();
        let read_data = read_data_buf.as_slice_mut();
        let read_start = std::time::Instant::now();
        {
            let mut file = fs
                .open_file(TEST_FILE_NAME, location, OpenFlags { read: true, write: false, create: false })
                .unwrap();

            for _ in 0..BUFFER_COUNT {
                file.read_exact(read_data).unwrap();
            }
        }
        let elapsed = read_start.elapsed().as_secs_f32();

        log::info!(
            "Read speed at {location:?}: {:.1} MB/s",
            (BUFFER_LEN * BUFFER_COUNT) as f32 / (1024.0 * 1024.0) / elapsed
        );
        // --- Measure std copy ---

        let read_start = std::time::Instant::now();
        {
            let mut from = fs
                .open_file(TEST_FILE_NAME, location, OpenFlags { read: true, write: false, create: false })
                .unwrap();
            let mut to = fs
                .open_file(COPY1_FILE_NAME, location, OpenFlags { read: false, write: true, create: true })
                .unwrap();

            std::io::copy(&mut from, &mut to).unwrap();
        }
        let elapsed = read_start.elapsed().as_secs_f32();

        log::info!(
            "std::io::copy speed at {location:?}: {:.1} MB/s",
            (BUFFER_LEN * BUFFER_COUNT) as f32 / (1024.0 * 1024.0) / elapsed
        );

        // --- Measure optimized copy ---

        let read_start = std::time::Instant::now();
        {
            let mut from = fs
                .open_file(TEST_FILE_NAME, location, OpenFlags { read: true, write: false, create: false })
                .unwrap();
            let mut to = fs
                .open_file(COPY2_FILE_NAME, location, OpenFlags { read: false, write: true, create: true })
                .unwrap();

            while from.copy_block_to(&mut to, fs::MAX_ASYNC_LEN).unwrap() != 0 {}
        }
        let elapsed = read_start.elapsed().as_secs_f32();

        log::info!(
            "copy_block speed at {location:?}: {:.1} MB/s",
            (BUFFER_LEN * BUFFER_COUNT) as f32 / (1024.0 * 1024.0) / elapsed
        );

        // --- Measure random seek+read ---

        let file_len = (BUFFER_LEN * BUFFER_COUNT) as u64;
        let mut chunk = [0u8; 256];
        let mut rng: u32 = 0x2545_f491;
        let seek_start = std::time::Instant::now();
        {
            let mut file = fs
                .open_file(TEST_FILE_NAME, location, OpenFlags { read: true, write: false, create: false })
                .unwrap();

            for _ in 0..RANDOM_READ_COUNT {
                rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                let offset = rng as u64 % (file_len - chunk.len() as u64);
                file.seek(std::io::SeekFrom::Start(offset)).unwrap();
                file.read_exact(&mut chunk).unwrap();
            }
        }
        let elapsed = seek_start.elapsed().as_secs_f32();

        log::info!(
            "Random seek+read speed at {location:?}: {} ops/s",
            (RANDOM_READ_COUNT as f32 / elapsed) as u32
        );

        // --- Check file contents ---
        for name in [TEST_FILE_NAME, COPY1_FILE_NAME, COPY2_FILE_NAME] {
            let mut file =
                fs.open_file(name, location, OpenFlags { read: true, write: false, create: false }).unwrap();

            for iter in 0..BUFFER_COUNT {
                file.read_exact(read_data).unwrap();
                if data != read_data {
                    for (i, (d1, d2)) in data.iter().zip(read_data.iter()).enumerate() {
                        if d1 != d2 {
                            panic!("Read wrong data at iteration {iter} index={i}, {d1}!={d2}");
                        }
                    }
                }
            }
        }
        log::info!("Read check successful");
    }

    log::info!("fs benchmark done");
}
