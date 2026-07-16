// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    fs::{File, OpenOptions},
    io::{stdout, BufWriter, Seek, SeekFrom, Write},
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::Context;
use clap::Args;
use colored::Colorize;
use sambuca::flash::FlashProgress;
use usb_debug_protocol::UsbDebugClient;

use crate::{
    bootimage::{BOOT_IMAGE, SECTOR_SIZE, SYSTEM_PARTITION_START_SECTOR},
    builder::project_root,
};

#[derive(Args)]
pub struct FlashArgs {
    /// Flash the MBR and boot partition
    #[arg(short, long)]
    boot: bool,
    /// Flash the System partition. If both or neither --boot and --system are specified, the whole boot.bin
    /// file will be flashed.
    #[arg(short, long)]
    system: bool,
    /// Use GDB to switch to sam-ba mode.
    /// Set the KEYOS_GDB env var to change the gdb binary to be used
    #[arg(long)]
    switch: bool,

    /// Don't verify after flashing
    #[arg(long)]
    no_verify: bool,
}

#[derive(Args)]
pub struct DumpFlashArgs {
    /// Number of megabytes to dump from flash
    #[arg(short = 'n', long, default_value = "8")]
    megabytes: usize,

    /// Output file path
    #[arg(short, long, default_value = "flash_dump.bin")]
    output: PathBuf,

    /// Offset in bytes from the start of flash (must be 512-byte aligned)
    #[arg(long, default_value = "0")]
    offset: usize,

    /// Use GDB to switch to sam-ba mode.
    /// Set the KEYOS_GDB env var to change the gdb binary to be used
    #[arg(long)]
    switch: bool,
}

const SPINNERS: &[char] = &['|', '/', '-', '\\'];

fn enter_samba_mode(use_script: bool) -> anyhow::Result<()> {
    if sambuca::Sambuca::new().is_ok() {
        return Ok(());
    }

    if use_script {
        println!("Running scripts/reboot-in-samba-mode.sh");
        if !Command::new("scripts/reboot-in-samba-mode.sh")
            .current_dir(project_root())
            .status()
            .context("running scripts/reboot-in-samba-mode.sh failed")?
            .success()
        {
            panic!("Switching to sam-ba mode failed");
        }
        println!("Waiting a bit to let sam-ba mode boot");
        std::thread::sleep(Duration::from_millis(1000));
        return Ok(());
    }

    let client = match UsbDebugClient::open() {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    print!("{} usb-debug interface detected, requesting SAM-BA reboot... ", "ⓘ".blue());
    stdout().flush().ok();
    match client.send(usb_debug_protocol::Command::RebootSamba, Duration::from_secs(5)) {
        Ok(_) => println!("{}", "✓".green()),
        Err(e) => println!("{} ({e})", "⚠".yellow()),
    }
    Ok(())
}

pub fn flash_firmware(args: FlashArgs) -> anyhow::Result<()> {
    if std::fs::metadata(BOOT_IMAGE).is_err() {
        panic!("The {BOOT_IMAGE} file is missing, have you run cargo xtask build-firmware (or build-all)?");
    }
    let mut boot_img = std::fs::read(BOOT_IMAGE).context("reading boot image")?;
    let target_len = boot_img.len().next_multiple_of(512);
    boot_img.extend((0..(target_len - boot_img.len())).map(|_| 0));
    let (data, offset) = if args.boot ^ args.system {
        const SYSTEM_PARTITION_START: usize = SECTOR_SIZE as usize * SYSTEM_PARTITION_START_SECTOR as usize;

        let boot_partition_size = boot_img[..SYSTEM_PARTITION_START]
            .iter()
            .rposition(|b| *b != 0)
            .unwrap()
            .saturating_add(1)
            .next_multiple_of(512);

        println!("{} Boot partition size: {boot_partition_size} bytes", "ⓘ".blue());
        let (data, offset) = if args.boot {
            let data = &boot_img[..boot_partition_size];
            println!("{} Flashing boot partition ({} MB)", "ⓘ".blue(), data.len() / (1024 * 1024));
            (data, 0)
        } else {
            let data = &boot_img[SYSTEM_PARTITION_START..];
            println!("{} Flashing system partition ({} MB)", "ⓘ".blue(), data.len() / (1024 * 1024));
            (data, SYSTEM_PARTITION_START)
        };
        (data, offset)
    } else {
        let data = &boot_img as &[u8];
        println!("{} Flashing full boot.bin ({} MB)", "ⓘ".blue(), data.len() / (1024 * 1024));
        (data, 0)
    };

    enter_samba_mode(args.switch)?;

    print!("Waiting for sam-ba USB device...");
    stdout().flush().context("flushing stdout")?;
    let mut sambuca =
        sambuca::flash::wait_for_device(Duration::from_secs(60)).context("waiting for SAM-BA device")?;
    println!("\r{} Connected to the SAM-BA device", "✓".green());
    println!(
        "{} SAM-BA monitor version: {}",
        "ⓘ".blue(),
        sambuca.version().context("reading SAM-BA version")?
    );
    let start_time = Instant::now();
    let mut last_progress = Instant::now();
    let mut counter = 0;

    sambuca
        .flash_image(data, offset as u64, !args.no_verify, |p| match p {
            FlashProgress::Writing { percent } => {
                if last_progress.elapsed() > Duration::from_millis(100) || percent == 100 {
                    print_progress_bar("Flashing", percent, counter);
                    last_progress = Instant::now();
                    counter += 1;
                }
            }
            FlashProgress::Verifying { percent } => {
                if last_progress.elapsed() > Duration::from_millis(100) || percent == 100 {
                    print_progress_bar("Verifying", percent, counter);
                    last_progress = Instant::now();
                    counter += 1;
                }
            }
            FlashProgress::Patched { chunks, attempts } => {
                println!();
                println!(
                    "{} Fixed {} chunk(s) during verification in {} attempt(s)",
                    "⚠".yellow(),
                    chunks,
                    attempts + 1
                );
            }
        })
        .context("flashing image")?;

    println!();
    println!("{} Done in {:.02}s", "✓".green(), start_time.elapsed().as_secs_f32());
    println!("Rebooting in normal mode");

    sambuca::flash::reboot_to_normal(&mut sambuca).context("rebooting to normal mode")?;

    Ok(())
}

fn print_progress_bar(title: &str, pct: usize, counter: usize) {
    const SKIP_STEPS: usize = 3;
    let progress = SPINNERS[counter % SPINNERS.len()];

    print!("\r");
    print!("{progress} {title} [");
    for _ in (0..pct).filter(|i| i % SKIP_STEPS == 0) {
        print!("=");
    }
    for _ in (0..100_usize.saturating_sub(pct)).filter(|i| i % SKIP_STEPS == 0) {
        print!(" ");
    }
    print!("] {pct}%");

    stdout().flush().unwrap();
}

pub fn dump_flash(args: DumpFlashArgs) -> anyhow::Result<()> {
    if args.megabytes == 0 {
        anyhow::bail!("Size in megabytes must be greater than 0");
    }
    let total_bytes = args.megabytes * 1024 * 1024;
    let offset = args.offset;

    if !offset.is_multiple_of(512) {
        anyhow::bail!("Offset must be 512-byte aligned");
    }

    println!(
        "{} Dumping {} MB from flash at offset {} to {:?}",
        "ⓘ".blue(),
        args.megabytes,
        offset,
        args.output
    );

    enter_samba_mode(args.switch)?;

    let start_time = Instant::now();
    let mut attempt = 0;

    loop {
        attempt += 1;

        match dump_flash_attempt(&args.output, offset, total_bytes) {
            Ok(()) => {
                println!("{} Done in {:.02}s", "✓".green(), start_time.elapsed().as_secs_f32());
                println!("{} Dumped {} bytes to {:?}", "✓".green(), total_bytes, args.output);
                return Ok(());
            }
            Err(e) => {
                println!();
                println!("{} Attempt {} failed: {}. Retrying in 1 second...", "⚠".yellow(), attempt, e);
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn dump_flash_attempt(output: &PathBuf, offset: usize, total_bytes: usize) -> anyhow::Result<()> {
    print!("Waiting for sam-ba USB device...");
    stdout().flush().context("flushing stdout")?;
    let mut sambuca =
        sambuca::flash::wait_for_device(Duration::from_secs(60)).context("waiting for SAM-BA device")?;
    println!("\r{} Connected to the SAM-BA device", "✓".green());
    println!(
        "{} SAM-BA monitor version: {}",
        "ⓘ".blue(),
        sambuca.version().context("reading SAM-BA version")?
    );
    // Let sam-ba get itself together if it recently booted.
    std::thread::sleep(Duration::from_millis(500));

    let mut flash_app =
        sambuca.initialize_flash_applet(0, 1, 0, 8, 3).context("initializing flash applet")?;
    let mut last_progress = Instant::now();

    // Check if file exists and get its size for potential resume
    let existing_size = std::fs::metadata(output).map(|m| m.len() as usize).unwrap_or(0);

    // Round down to 512-byte alignment for safe resume point
    let resume_offset = (existing_size / 512) * 512;

    let (file, bytes_already_read) = if resume_offset > 0 && resume_offset < total_bytes {
        println!(
            "{} Found existing file with {} bytes, resuming from offset {}",
            "ⓘ".blue(),
            existing_size,
            resume_offset
        );
        let mut file = OpenOptions::new().write(true).open(output).context("opening existing output file")?;
        file.seek(SeekFrom::Start(resume_offset as u64)).context("seeking to resume position")?;
        (file, resume_offset)
    } else {
        (File::create(output).context("creating output file")?, 0)
    };

    let mut writer = BufWriter::new(file);
    let mut counter = 0;
    let remaining_bytes = total_bytes - bytes_already_read;

    flash_app
        .read_flash((offset + bytes_already_read) as _, remaining_bytes, &mut writer, |read| {
            let total_read = bytes_already_read + read;
            let pct = total_read * 100 / total_bytes;
            if last_progress.elapsed() > Duration::from_millis(100) || pct == 100 {
                print_progress_bar("Reading", pct, counter);
                last_progress = Instant::now();
                counter += 1;
            }
        })
        .context("reading flash")?;

    writer.flush().context("flushing output file")?;
    println!();

    Ok(())
}
