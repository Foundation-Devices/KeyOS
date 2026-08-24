// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::Location;
use server::{BlockingArchiveHandler, ServerContext};

use crate::utils::convert_cosign2_header;
use crate::{
    messages::{BootloaderInfo, GetBootloaderInfo, GetKeyOsInfo, GetRecoveryInfo, KeyOsInfo, RecoveryInfo},
    RecoveryWorkerServer,
};

const KEYOS_IMAGE_PATH: &str = "/keyos/app.bin";
const RECOVERY_IMAGE_PATH: &str = "/recovery.bin";

impl BlockingArchiveHandler<GetBootloaderInfo> for RecoveryWorkerServer {
    fn handle(
        &mut self,
        _msg: GetBootloaderInfo,
        _pid: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetBootloaderInfo as server::BlockingArchive>::Response {
        let res = hash_bootloader(&crate::CryptoApi::default());
        let Ok(bootloader_hash) = res else {
            log::error!("Couldn't hash the bootloader: {res:?}");
            return BootloaderInfo { hash_str: Default::default(), hash: Default::default() };
        };
        BootloaderInfo { hash_str: hex::encode(bootloader_hash), hash: bootloader_hash }
    }
}

impl BlockingArchiveHandler<GetKeyOsInfo> for RecoveryWorkerServer {
    fn handle(
        &mut self,
        _msg: GetKeyOsInfo,
        _pid: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetKeyOsInfo as server::BlockingArchive>::Response {
        let (hash, hash_str, version, date_str, _, _) = verify_fw_file(KEYOS_IMAGE_PATH, Location::System);
        KeyOsInfo { hash_str, hash, version, date_str }
    }
}

impl BlockingArchiveHandler<GetRecoveryInfo> for RecoveryWorkerServer {
    fn handle(
        &mut self,
        _request: GetRecoveryInfo,

        _pid: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetRecoveryInfo as server::BlockingArchive>::Response {
        let (hash, hash_str, version, date_str, _, _) = verify_fw_file(RECOVERY_IMAGE_PATH, Location::Boot);
        RecoveryInfo { hash_str, hash, version, date_str }
    }
}

/// Hash the bootloader plaintext currently resident in SRAM.
fn hash_bootloader<P: crypto::ShaPermissions>(crypto: &crypto::CryptoApi<P>) -> anyhow::Result<[u8; 32]> {
    const BOOTLOADER_MAX_SIZE: usize = 1024 * 64;
    // The sixth vector contains the actual size. Keep this synchronized with
    // BOOTLOADER_SIZE_VECTOR in xtask/src/bootloader.rs.
    const BOOTLOADER_SIZE_IDX: usize = 5;
    let sram = xous::DropDeallocate::new(
        xous::map_memory(
            Some(std::num::NonZero::new(utralib::HW_SRAM0_MEM).expect("non-zero")),
            None,
            BOOTLOADER_MAX_SIZE,
            xous::MemoryFlags::DEV,
        )
        .map_err(|e| anyhow::anyhow!("couldn't map bootloader SRAM: {e:?}"))?,
    );

    let bootloader_size = sram.as_slice::<usize>()[BOOTLOADER_SIZE_IDX];
    if bootloader_size > BOOTLOADER_MAX_SIZE {
        anyhow::bail!("bootloader size {bootloader_size} exceeds max {BOOTLOADER_MAX_SIZE}");
    }

    let mut bootloader_mem = xous::DropDeallocate::new(
        xous::map_memory(None, None, BOOTLOADER_MAX_SIZE, xous::MemoryFlags::W)
            .map_err(|e| anyhow::anyhow!("couldn't map bootloader scratch: {e:?}"))?,
    );
    bootloader_mem.as_slice_mut::<u8>()[..bootloader_size]
        .copy_from_slice(&sram.as_slice::<u8>()[..bootloader_size]);

    Ok(crypto.sha256(&bootloader_mem.as_slice::<u8>()[..bootloader_size])?)
}

fn verify_fw_file(name: &str, location: Location) -> ([u8; 32], String, String, String, u32, bool) {
    let res = fw_utils::hash::verify_cosign2(
        &crate::FileSystem::default(),
        &crate::CryptoApi::default(),
        name,
        location,
        |_| (),
        cfg!(feature = "production"),
    );
    convert_cosign2_header(res)
}
