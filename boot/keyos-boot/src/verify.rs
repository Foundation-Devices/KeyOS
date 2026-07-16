// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{hardened_eq, selected_boot_image_kind, BootImageKind, FIRMWARE_LOAD_BASE_ADDR},
    atsama5d27::{
        dma::{DmaChannel, Xdmac},
        pmc::{PeripheralId, Pmc},
        sha::Sha,
    },
    boot_common::{load_os_image_file, random},
    cosign2::{Header, Trust},
    micro_ecc_sys::{uECC_decompress, uECC_secp256k1, uECC_valid_public_key, uECC_verify},
};

#[cfg(not(feature = "production"))]
const KNOWN_SIGNERS: [[u8; 33]; 0] = [
    // [0; 33],
];

#[cfg(feature = "production")]
const KNOWN_SIGNERS: [[u8; 33]; 4] = [
    // Signer 1 - Ken
    [
        0x03, 0xbf, 0x01, 0x4e, 0x1a, 0x37, 0xa1, 0x13, 0x08, 0x9b, 0xea, 0x7b, 0x50, 0xee, 0x9b, 0xd7, 0x73,
        0x31, 0x89, 0xec, 0xd6, 0xaf, 0xb7, 0xe0, 0x51, 0xa6, 0xe9, 0x5f, 0x99, 0xb9, 0x7d, 0xa5, 0xe9,
    ],
    // Signer 2 - Zach
    [
        0x03, 0x04, 0x0e, 0x47, 0xc1, 0xcd, 0xe8, 0x97, 0x80, 0x85, 0xbd, 0xc8, 0xb4, 0x4d, 0xf8, 0x5e, 0x7c,
        0x0b, 0x2e, 0x1e, 0xa5, 0x86, 0x69, 0x7b, 0x5d, 0x38, 0x5e, 0x52, 0x3d, 0x3f, 0x90, 0x8b, 0xc3,
    ],
    // Signer 3 - Jacob
    [
        0x03, 0x8d, 0xe8, 0xdd, 0x1c, 0xba, 0xd8, 0xbf, 0x1d, 0xa7, 0xff, 0x64, 0xb8, 0xa9, 0xb4, 0xa3, 0x75,
        0xf0, 0x20, 0x5e, 0xff, 0x41, 0xf7, 0xf9, 0xdc, 0xa8, 0xe9, 0x1c, 0x4c, 0xf0, 0x95, 0x1d, 0xaa,
    ],
    // Signer 4 - Anon
    [
        0x03, 0xcb, 0x8e, 0x42, 0x19, 0xd3, 0xc8, 0xf2, 0x69, 0xab, 0x2e, 0xd3, 0xac, 0xb7, 0x1a, 0x4b, 0x17,
        0x22, 0xc7, 0x6a, 0x0c, 0x34, 0x8e, 0xa1, 0x1f, 0xa7, 0x9b, 0x46, 0x39, 0xbe, 0xf4, 0x50, 0x94,
    ],
];

#[repr(u32)]
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum VerificationResult {
    Valid = 0xcafebabe,
    Invalid = 0xdeadbeef,
}

// Main OS image is located on the partition 1 (System Volume)
const MAIN_OS_IMAGE_NAME: &str = "1:keyos/app.bin\0";
// Updated Main OS image (if the update was interrupted during just before renaming phase)
const UPDATED_MAIN_OS_IMAGE_NAME: &str = "1:keyos.update/app.bin\0";
// Recovery OS image is located on the partition 0 (Boot Volume)
const RECOVERY_OS_IMAGE_NAME: &str = "0:recovery.bin\0";

pub static mut OS_VERSION: Option<[u8; 20]> = None;
pub static mut OS_BUILD_DATE: Option<[u8; 14]> = None;

struct EccVerifier {}

impl EccVerifier {
    pub fn new() -> Self { EccVerifier {} }
}

impl cosign2::Secp256k1Verify for EccVerifier {
    fn verify_ecdsa(
        &self,
        msg: [u8; 32],
        signature: [u8; 64],
        pubkey: [u8; 33],
    ) -> cosign2::VerificationResult {
        const UECC_SUCCESS: i32 = 1;

        let mut uncompressed_pk = [0; 64];
        unsafe { uECC_decompress(pubkey.as_ptr(), uncompressed_pk.as_mut_ptr(), uECC_secp256k1()) };

        let res = unsafe { uECC_valid_public_key(uncompressed_pk.as_ptr(), micro_ecc_sys::uECC_secp256k1()) };
        if res != UECC_SUCCESS {
            return cosign2::VerificationResult::Invalid;
        }

        // Temporal separation to make single-glitch bypass harder
        random::delay();

        let res = unsafe {
            uECC_verify(
                uncompressed_pk.as_ptr(),
                msg.as_ptr(),
                msg.len() as u32,
                signature.as_ptr(),
                uECC_secp256k1(),
            )
        };
        if res == UECC_SUCCESS {
            cosign2::VerificationResult::Valid
        } else {
            cosign2::VerificationResult::Invalid
        }
    }
}

pub(crate) struct Sha256 {
    sha: Sha,
}

impl Sha256 {
    pub(crate) fn new() -> Self {
        let mut pmc = Pmc::new();
        pmc.enable_peripheral_clock(PeripheralId::Sha);
        pmc.enable_peripheral_clock(PeripheralId::Xdmac0);

        Sha256 { sha: Sha::new() }
    }
}

impl cosign2::Sha256 for Sha256 {
    fn hash(&self, data: &[u8]) -> [u8; 32] {
        let (prefix, aligned_data, postfix) = unsafe { data.align_to::<u32>() };
        if prefix.is_empty() && postfix.is_empty() {
            let channel = Xdmac::xdmac0().channel(DmaChannel::Channel0);
            let sha1_phys_addr = self.sha.dma_in_address();
            channel.configure_peripheral_transfer(Sha::DMA_CONFIG);
            self.sha
                .hash_dma::<atsama5d27::sha::Sha256, _>(aligned_data.len() * 4, || {
                    for chunk in aligned_data.chunks(atsama5d27::dma::BIG_TRANSFER_THRESHOLD / 2) {
                        channel.execute_transfer(chunk.as_ptr() as u32, sha1_phys_addr as u32, chunk.len());
                        while !channel.is_transfer_complete() {}
                    }
                    Ok::<_, ()>(())
                })
                .unwrap()
        } else {
            self.sha.hash::<atsama5d27::sha::Sha256>(data)
        }
    }
}
pub(crate) fn selected_image_name() -> &'static str {
    match selected_boot_image_kind() {
        BootImageKind::Main => MAIN_OS_IMAGE_NAME,
        BootImageKind::Recovery => RECOVERY_OS_IMAGE_NAME,
        BootImageKind::UpdatedMain => UPDATED_MAIN_OS_IMAGE_NAME,
    }
}

pub(crate) fn load_os_version_info() {
    // Load just the header so we can read and set the version and date
    let load_size = unsafe { load_os_image_file(selected_image_name().as_ptr(), true) };
    if load_size != 0 {
        let os_image_slice =
            unsafe { core::slice::from_raw_parts(FIRMWARE_LOAD_BASE_ADDR as *const u8, load_size as usize) };

        if let Some((version, build_date)) = read_version_and_build_date(os_image_slice, false) {
            unsafe {
                OS_VERSION = Some(version);
                OS_BUILD_DATE = Some(build_date);
            }
        }
    }
}

#[inline(never)]
pub(crate) fn verify_os_image(image: &[u8]) -> VerificationResult {
    if let Some((version, build_date)) = read_version_and_build_date(image, true) {
        unsafe {
            OS_VERSION = Some(version);
            OS_BUILD_DATE = Some(build_date);
        }

        return verify_image(image);
    }

    VerificationResult::Invalid
}

#[inline(never)]
fn verify_image(image: &[u8]) -> VerificationResult {
    // Only officially signed recovery OS image is allowed
    #[cfg(feature = "production")]
    const TRUST: &[Trust] = &[Trust::FullyTrusted];
    #[cfg(not(feature = "production"))]
    const TRUST: &[Trust] = &[Trust::FullyTrusted, Trust::ThirdParty, Trust::Disabled];

    let ecc = EccVerifier::new();
    let sha = Sha256::new();

    // Parse and verify firmware signatures
    match Header::parse(image, &KNOWN_SIGNERS, &sha, &ecc, Header::DEFAULT_SIZE) {
        Ok(Some(header)) => {
            if *header.binary_hash() == [0; 32] {
                return VerificationResult::Invalid;
            }

            if !TRUST.contains(&header.trust()) {
                return VerificationResult::Invalid;
            }

            if image.len() <= Header::DEFAULT_SIZE {
                return VerificationResult::Invalid;
            }

            let binary_bytes = &image[Header::DEFAULT_SIZE..];
            if binary_bytes.len() as u32 != header.bin_size() {
                return VerificationResult::Invalid;
            }

            VerificationResult::Valid
        }
        _ => VerificationResult::Invalid,
    }
}

fn read_version_and_build_date(image: &[u8], check_size: bool) -> Option<([u8; 20], [u8; 14])> {
    match Header::parse_unverified(image, Header::DEFAULT_SIZE, check_size) {
        Ok(Some(header)) => {
            let mut version_bytes = [0u8; 20];
            let str_bytes = header.version().as_bytes();
            version_bytes[..str_bytes.len()].copy_from_slice(str_bytes);

            let mut date_bytes = [0u8; 14];
            let str_bytes = header.date().as_bytes();
            date_bytes[..str_bytes.len()].copy_from_slice(str_bytes);

            Some((version_bytes, date_bytes))
        }
        Ok(None) => None,
        Err(_e) => None,
    }
}

#[inline(never)]
pub fn load_and_verify_firmware() -> VerificationResult {
    let load_size = unsafe { load_os_image_file(selected_image_name().as_ptr(), false) };

    if load_size == 0 {
        return VerificationResult::Invalid;
    }

    let os_image_slice =
        unsafe { core::slice::from_raw_parts(FIRMWARE_LOAD_BASE_ADDR as *const u8, load_size as usize) };

    // Double-evaluate the verification with temporal separation.
    let first = verify_os_image(os_image_slice);
    random::delay();
    let second = verify_os_image(os_image_slice);

    // Hardened combine of the two verification runs
    if !hardened_eq(first, VerificationResult::Valid) {
        return VerificationResult::Invalid;
    }

    if !hardened_eq(second, VerificationResult::Valid) {
        return VerificationResult::Invalid;
    }

    VerificationResult::Valid
}

pub(crate) fn get_bootloader_version_and_date() -> ([u8; 8], u64) {
    let mut bootloader_version = [b' '; 8];
    let version_str = env!("CARGO_PKG_VERSION").as_bytes();
    let len = version_str.len().min(bootloader_version.len());
    bootloader_version[..len].copy_from_slice(&version_str[..len]);

    let bootloader_build_date = env!("SOURCE_DATE_EPOCH").parse::<u64>().unwrap();

    (bootloader_version, bootloader_build_date)
}
