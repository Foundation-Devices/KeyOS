// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    io::{Read, Write},
    thread::sleep,
    time::Duration,
};

use fs::{Location, OpenFlags};

fs::use_api!();

pub fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let mut fs = FileSystem::default();

    loop {
        log::info!("Waiting for a pendrive to show up");
        loop {
            match fs.open_dir("".to_owned(), Location::Usb) {
                Ok(_) => break,
                Err(e) => {
                    if !matches!(e, fs::Error::NoMedia) {
                        log::warn!("Unexpected FS Error: {e}");
                    }
                    sleep(Duration::from_millis(100));
                }
            }
        }
        log::info!("Listing files on the pendrive");
        log::info!("=======");
        if let Err(e) = recurse_into("".to_owned(), &mut fs) {
            log::warn!("Error during recursion: {e}");
        }
        log::info!("=======");
        if let Err(e) = test_file_write(&mut fs) {
            log::warn!("Error during file writing: {e}");
        }

        log::info!("Waiting for pendrive to disconnect");
        while fs.open_dir("".to_owned(), Location::Usb).is_ok() {
            sleep(Duration::from_millis(100));
        }
    }
}

fn recurse_into(path: String, fs: &mut FileSystem) -> Result<(), fs::Error> {
    let dir = fs.open_dir(path.clone(), Location::Usb)?;
    while let Some(de) = dir.next_entry()? {
        if de.name == "." || de.name == ".." {
            continue;
        }
        let path = format!("{path}/{}", de.name);
        log::info!("{path}");
        if de.is_dir {
            recurse_into(path, fs)?;
        }
    }
    Ok(())
}

fn test_file_write(fs: &mut FileSystem) -> Result<(), fs::Error> {
    log::info!("Writing test file");
    let mut f =
        fs.open_file("test_file.bin", Location::Usb, OpenFlags { read: false, write: true, create: true })?;
    let mut data = vec![123; 64 * 1024];
    getrandom::getrandom(&mut data[..16]).ok();
    *data.last_mut().unwrap() = 69;
    f.truncate()?;
    f.write_all(&data)?;
    drop(f);

    log::info!("Reading back test file");
    let mut f =
        fs.open_file("test_file.bin", Location::Usb, OpenFlags { read: true, write: false, create: false })?;
    let mut new_data = Vec::new();
    f.read_to_end(&mut new_data)?;
    if data != new_data {
        log::error!("Data read is not what was written.");
        log::error!("Beginning: {:?} vs. {:?}", &data[0..32], &new_data[0..32]);
        log::error!("End: {:?} vs. {:?}", &data[data.len() - 8..], &new_data[new_data.len() - 8..]);
    } else {
        log::info!("File write test successful.");
    }
    Ok(())
}
