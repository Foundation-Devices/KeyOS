// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{io::Write, time::Duration};

pub use fs::Location;
use fs::OpenFlags;

fs::use_api!();

const LOG_ROTATE_AT_SIZE: usize = 100 * 1024;
const LOG_FILES_TO_KEEP: usize = 10;
const LOG_BUFFER_SIZE: usize = 0x4000;

#[derive(Clone, Copy)]
pub struct Config {
    pub location: Location,
    pub directory: &'static str,
    pub file_prefix: &'static str,
    pub description: &'static str,
    pub retry_on_error: bool,
}

fn log_file_path(config: Config, index: usize) -> String {
    format!("{}/{prefix}.{index}.log", config.directory, prefix = config.file_prefix)
}

fn open_new_log_file(fs: &FileSystem, config: Config) -> Result<File, fs::Error> {
    if let Err(e) = fs.remove(log_file_path(config, LOG_FILES_TO_KEEP - 1), config.location) {
        match e {
            fs::Error::FileNotFound => {}
            _ => log::warn!("Error removing last {}: {e:?}", config.description),
        }
    }

    for i in (0..(LOG_FILES_TO_KEEP - 1)).rev() {
        if let Err(e) = fs.rename(log_file_path(config, i), log_file_path(config, i + 1), config.location) {
            match e {
                fs::Error::FileNotFound => {}
                _ => log::warn!("Error renaming {} #{i}: {e:?}", config.description),
            }
        }
    }

    log::info!(
        "Opening {} at {}",
        config.description,
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );

    match fs.create_dir(config.directory, config.location) {
        Ok(_) | Err(fs::Error::FileAlreadyExists) => {}
        Err(e) => return Err(e),
    }

    fs.open_file(
        log_file_path(config, 0),
        config.location,
        OpenFlags { read: false, write: true, create: true },
    )
}

fn open_log_file(fs: &FileSystem, config: Config) -> File {
    loop {
        fs.wait_for_filesystem(config.location);
        match open_new_log_file(fs, config) {
            Ok(file) => return file,
            Err(e) if !config.retry_on_error => panic!("Could not open {}: {e:?}", config.description),
            Err(e) => {
                log::warn!("Could not open {}, retrying: {e:?}", config.description);
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

pub fn run(config: Config) -> ! {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::System3).unwrap();

    let fs = FileSystem::default();
    let log_buffer = xous::map_memory(None, None, LOG_BUFFER_SIZE, xous::MemoryFlags::W)
        .expect("Could not allocate buffer");
    let log_reader = log_server::reader::LogReader::default();
    let mut sum_len = 0;
    let mut log_file = open_log_file(&fs, config);

    loop {
        let len = log_reader.read(log_buffer);
        let buf = &log_buffer.as_slice()[..len];

        if let Err(e) = log_file.write_all(buf).and_then(|_| log_file.flush()) {
            if !config.retry_on_error {
                panic!("Could not write {}: {e}", config.description);
            }

            log::warn!("Could not write {}, reopening: {e}", config.description);
            core::mem::drop(log_file);
            log_file = open_log_file(&fs, config);
            sum_len = 0;
            continue;
        }

        sum_len += len;
        if sum_len > LOG_ROTATE_AT_SIZE {
            core::mem::drop(log_file);
            log_file = open_log_file(&fs, config);
            sum_len = 0;
        }
    }
}
