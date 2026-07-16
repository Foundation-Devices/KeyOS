// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared flash-level helpers for SAM-BA operations.
//!
//! Used by both `xtask flash` and `passport-drive samba flash` to avoid
//! duplicating device-wait logic and reboot sequences.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::Sambuca;

/// Sector size used in the boot image layout.
pub const SECTOR_SIZE: usize = 512;

/// Progress callback payload.
pub enum FlashProgress {
    Writing { percent: usize },
    Verifying { percent: usize },
    Patched { chunks: usize, attempts: usize },
}

/// Wait for a SAM-BA device to appear on USB, with timeout.
pub fn wait_for_device(timeout: Duration) -> Result<Sambuca> {
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() > deadline {
            bail!("Timeout waiting for SAM-BA device ({:.0}s)", timeout.as_secs_f64());
        }
        if let Ok(s) = Sambuca::new() {
            return Ok(s);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Reboot the device from SAM-BA mode into normal mode.
pub fn reboot_to_normal(sambuca: &mut Sambuca) -> Result<()> {
    sambuca.write_u32(0xF804_8054, 0x6683_0000).context("reset boot bits")?;
    sambuca.write_u32(0xF804_8000, 0xA500_0001).context("kick reset controller")?;
    Ok(())
}

impl Sambuca {
    /// Flash data to the device at the given byte offset.
    ///
    /// `data` is sector-aligned by the caller.
    /// `verify`: if true, verify after writing.
    /// `progress` is called with status updates.
    pub fn flash_image(
        &mut self,
        data: &[u8],
        offset: u64,
        verify: bool,
        mut progress: impl FnMut(FlashProgress),
    ) -> Result<()> {
        // Allow time for SAM-BA to settle.
        std::thread::sleep(Duration::from_millis(500));

        let mut flash_app =
            self.initialize_flash_applet(0, 1, 0, 8, 3).context("initializing flash applet")?;

        let mut last_pct = 0;
        flash_app
            .write_flash(offset, data, |written| {
                let pct = written * 100 / data.len();
                if pct != last_pct {
                    last_pct = pct;
                    progress(FlashProgress::Writing { percent: pct });
                }
            })
            .context("writing flash")?;

        if verify {
            last_pct = 0;
            let stats = flash_app
                .verify_flash(
                    offset,
                    data,
                    |read| {
                        let pct = read * 100 / data.len();
                        if pct != last_pct {
                            last_pct = pct;
                            progress(FlashProgress::Verifying { percent: pct });
                        }
                    },
                    true,
                )
                .context("verifying flash")?;
            if stats.num_chunks_patched > 0 {
                progress(FlashProgress::Patched {
                    chunks: stats.num_chunks_patched,
                    attempts: stats.num_attempts,
                });
            }
        }

        Ok(())
    }

    /// Dump flash contents to a writer.
    pub fn dump_flash(
        &mut self,
        offset: u64,
        length: usize,
        writer: &mut impl Write,
        mut progress: impl FnMut(usize),
    ) -> Result<()> {
        std::thread::sleep(Duration::from_millis(500));

        let mut flash_app =
            self.initialize_flash_applet(0, 1, 0, 8, 3).context("initializing flash applet")?;

        flash_app
            .read_flash(offset, length, writer, |read| {
                progress(read);
            })
            .context("reading flash")?;

        Ok(())
    }
}
