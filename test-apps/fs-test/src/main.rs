// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    io::{Read, Seek, SeekFrom, Write},
    thread,
    time::Duration,
};

use fs::OpenFlags;
use server::xous;

fs::use_api!();
security::use_api!();

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    thread::sleep(Duration::from_secs(1));

    log::info!("fs tests starting");

    let fs = FileSystem::default();

    log::info!("Login result: {:?}", Security::default().log_in("123456".into()));

    let mut non_contiguous_buffer = xous::map_memory(None, None, 0x4000, xous::MemoryFlags::W).unwrap();

    // We on-demand map these out of order to explicitly break contiguity
    non_contiguous_buffer.as_slice_mut::<u8>()[0x2000] = 1;
    non_contiguous_buffer.as_slice_mut::<u8>()[0x1000] = 2;
    non_contiguous_buffer.as_slice_mut::<u8>()[0x3000] = 3;
    non_contiguous_buffer.as_slice_mut::<u8>()[0x0000] = 0;

    let mut aligned_buffer = xous::map_memory(None, None, 0x4000, xous::MemoryFlags::W).unwrap();
    let aligned_test_data: Vec<u8> = (0..0x4000usize).map(|i| (i * 13) as u8).collect();

    for location in [fs::Location::System, fs::Location::AppData, fs::Location::Airlock, fs::Location::User] {
        log::info!("fs tests: {location:?}");
        // Create file.
        let mut file =
            fs.open_file("/test", location, OpenFlags { read: true, write: true, create: true }).unwrap();

        // Write file data.
        let data = b"zut";
        file.write_all(data).unwrap();
        file.flush().unwrap();

        let metadata = fs.metadata("/test", location).unwrap();
        assert_eq!(metadata.size, 3);

        // Read file data.
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut read = [0; 3];
        file.read_exact(&mut read).unwrap();
        assert_eq!(&read, data);

        // Remove file.
        let result = fs.remove("/test", location);
        assert!(matches!(result, Err(fs::Error::FileInUse)));
        drop(file);
        fs.remove("/test", location).unwrap();
        let removed = fs.open_file("/test", location, OpenFlags { read: true, write: true, create: false });
        assert!(matches!(removed, Err(fs::Error::FileNotFound)));

        // Create directory.
        fs.create_dir("/a", location).unwrap();
        // Remove leftovers from previous tests if there are any
        fs.remove("/a/d", location).ok();
        fs.remove("/a/p", location).ok();

        // Create some subdirs and files.
        fs.create_dir("/a/b", location).unwrap();
        fs.create_dir("/a/c", location).unwrap();
        fs.open_file("/a/e", location, OpenFlags { read: false, write: true, create: true }).unwrap();

        // Rename a file.
        let file =
            fs.open_file("/a/e", location, OpenFlags { read: true, write: false, create: false }).unwrap();
        let result = fs.rename("/a/e", "/a/d", location);
        assert!(matches!(result, Err(fs::Error::FileInUse)));
        drop(file);
        fs.rename("/a/e", "/a/d", location).unwrap();
        let result = fs.open_file("/a/d", location, OpenFlags { read: true, write: false, create: false });
        assert!(result.is_ok());

        // List the directory.
        let dir = fs.open_dir("/a", location).unwrap();

        let entry = dir.next_entry().unwrap().unwrap();
        assert_eq!(entry.name, ".");
        let entry = dir.next_entry().unwrap().unwrap();
        assert_eq!(entry.name, "..");
        let entry = dir.next_entry().unwrap().unwrap();
        assert_eq!(entry.name, "b");
        let entry = dir.next_entry().unwrap().unwrap();
        assert_eq!(entry.name, "c");
        let entry = dir.next_entry().unwrap().unwrap();
        assert_eq!(entry.name, "d");
        let entry = dir.next_entry().unwrap();
        assert!(entry.is_none());

        // Test file truncating.
        let mut file =
            fs.open_file("/truncate", location, OpenFlags { read: true, write: true, create: true }).unwrap();
        file.write_all(b"1234567890").unwrap();
        file.seek(SeekFrom::Start(5)).unwrap();
        file.truncate().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut data = Vec::new();
        file.read_to_end(&mut data).unwrap();
        assert_eq!(data, b"12345");

        // Check if permission flags are actually checked
        assert!(fs
            .open_file("/a/p", location, OpenFlags { read: false, write: false, create: false },)
            .is_err());

        assert!(fs
            .open_file("/a/p", location, OpenFlags { read: true, write: false, create: true },)
            .is_err());

        let mut file =
            fs.open_file("/a/p", location, OpenFlags { read: true, write: true, create: true }).unwrap();
        file.write_all(b"54321").unwrap();
        drop(file);

        let mut file =
            fs.open_file("/a/p", location, OpenFlags { read: false, write: true, create: false }).unwrap();
        assert!(file.read_to_end(&mut data).is_err());
        drop(file);

        let mut file =
            fs.open_file("/a/p", location, OpenFlags { read: true, write: false, create: false }).unwrap();
        assert!(file.write_all(b"asdfg").is_err());
        drop(file);

        log::info!("fs aligment tests: {location:?}");
        for offset in [0, 5, 512, 0x1000, 0x1001] {
            for size in [0, 5, 512, 0x1000, 0x1001, 0x4000] {
                for file_offset in [0, 5, 512, 0x1000, 0x1001] {
                    for file_size in [5, 512, 0x1000, 0x1001, 0x4000] {
                        if file_offset + size > file_size || size + offset > aligned_test_data.len() {
                            continue;
                        }
                        let mut file = fs
                            .open_file(
                                "/aligmnent.bin",
                                location,
                                OpenFlags { read: true, write: true, create: true },
                            )
                            .unwrap();
                        file.write_all(&aligned_test_data[0..file_size]).unwrap();
                        file.truncate().unwrap();
                        file.seek(SeekFrom::Start(file_offset as u64)).unwrap();
                        file.read_exact(&mut aligned_buffer.as_slice_mut()[offset..offset + size]).unwrap();
                        assert_eq!(
                            aligned_test_data[file_offset..file_offset + size],
                            aligned_buffer.as_slice::<u8>()[offset..offset + size]
                        );
                        file.seek(SeekFrom::Start(file_offset as u64)).unwrap();
                        file.write_all(&aligned_buffer.as_slice_mut()[offset..offset + size]).unwrap();

                        file.seek(SeekFrom::Start(file_offset as u64)).unwrap();
                        file.read_exact(&mut non_contiguous_buffer.as_slice_mut()[offset..offset + size])
                            .unwrap();
                        assert_eq!(
                            aligned_test_data[file_offset..file_offset + size],
                            non_contiguous_buffer.as_slice::<u8>()[offset..offset + size]
                        );
                        file.seek(SeekFrom::Start(file_offset as u64)).unwrap();
                        file.write_all(&non_contiguous_buffer.as_slice_mut()[offset..offset + size]).unwrap();
                    }
                }
            }
        }
    }

    log::info!("fs tests passed");
}
