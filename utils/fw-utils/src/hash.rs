// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Read;
use std::num::NonZero;

use crypto::CryptoApi;
use fs::{FileSystem, Location, OpenFlags};
use micro_ecc_sys::{uECC_decompress, uECC_secp256k1, uECC_valid_public_key, uECC_verify};
use server::{CheckedPermissions, MessageAllowed};
use thiserror::Error;
use xous::{keyos::PAGE_SIZE, DropDeallocate, MemoryFlags, MemoryRange};

// Well-known public keys
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

#[derive(Debug, Error)]
pub enum HashError {
    #[error("xous error: {0:?}")]
    XousError(xous::Error),

    #[error("{0}")]
    CryptoError(#[from] crypto::error::CryptoError),

    #[error("cosign2 error: {0:?}")]
    Cosign2Error(cosign2::Error),

    #[error("cosign2 header is missing")]
    MissingCosign2Header,

    #[error("fs error: {0:?}")]
    FsError(#[from] fs::Error),

    #[error("io error: {0:?}")]
    IoError(#[from] std::io::Error),

    #[error("signature pubkey(s) not trusted")]
    NotTrusted,
}

impl From<xous::Error> for HashError {
    fn from(value: xous::Error) -> Self { HashError::XousError(value) }
}

impl From<cosign2::Error> for HashError {
    fn from(value: cosign2::Error) -> Self { HashError::Cosign2Error(value) }
}

pub fn read_file<P>(
    fs: &FileSystem<P>,
    path: impl Into<String>,
    location: Location,
) -> Result<(DropDeallocate, usize), HashError>
where
    P: CheckedPermissions,
    P: MessageAllowed<fs::messages::GetMetadata>,
    P: MessageAllowed<fs::messages::OpenFileMessage>,
    P: MessageAllowed<fs::messages::ReadFile>,
    P: MessageAllowed<fs::messages::CloseFile>,
{
    let path_str = path.into();
    let metadata = fs.metadata(path_str.clone(), location)?;

    let mut file =
        fs.open_file(path_str.clone(), location, OpenFlags { read: true, write: false, create: false })?;
    let size_aligned =
        if metadata.size == 0 { PAGE_SIZE as u64 } else { metadata.size.next_multiple_of(PAGE_SIZE as u64) };
    let total_size = metadata.size as usize;

    let mut file_mem =
        DropDeallocate::new(xous::map_memory(None, None, size_aligned as usize, xous::MemoryFlags::W)?);

    file.read_exact(&mut file_mem.as_slice_mut()[..total_size])?;

    Ok((file_mem, total_size))
}

/// Calculate the SHA256 hash of the bootloader plaintext in SRAM.
pub fn hash_bootloader<P: crypto::ShaPermissions>(crypto: &CryptoApi<P>) -> Result<[u8; 32], HashError> {
    const BOOTLOADER_MAX_SIZE: usize = 1024 * 64; // 64KB
    const BOOTLOADER_SIZE_IDX: usize = 5; // bootloader actual size is stored in its vector table at this location
    let sram = DropDeallocate::new(xous::map_memory(
        Some(NonZero::new(utralib::HW_SRAM0_MEM).expect("non-zero")),
        None,
        BOOTLOADER_MAX_SIZE,
        xous::MemoryFlags::DEV,
    )?);

    let bootloader_size = sram.as_slice::<usize>()[BOOTLOADER_SIZE_IDX];
    if bootloader_size > BOOTLOADER_MAX_SIZE {
        return Err(HashError::XousError(xous::Error::InternalError));
    }

    let mut bootloader_mem =
        DropDeallocate::new(xous::map_memory(None, None, BOOTLOADER_MAX_SIZE, MemoryFlags::W)?);
    bootloader_mem.as_slice_mut::<u8>()[..bootloader_size]
        .copy_from_slice(&sram.as_slice::<u8>()[..bootloader_size]);

    Ok(crypto.sha256(&bootloader_mem.as_slice::<u8>()[..bootloader_size])?)
}

pub fn verify_cosign2_mem<P: crypto::ShaPermissions>(
    crypto: &CryptoApi<P>,
    data: &[u8],
    check_trust: bool,
) -> Result<cosign2::Header, HashError> {
    let Some(header) = cosign2::Header::parse(
        data,
        &KNOWN_SIGNERS,
        &Sha256 { crypto },
        &EccVerifier {},
        cosign2::Header::DEFAULT_SIZE,
    )?
    else {
        return Err(HashError::MissingCosign2Header);
    };

    if check_trust && header.trust() != cosign2::Trust::FullyTrusted {
        return Err(HashError::NotTrusted);
    }

    Ok(header)
}

/// Buffer size for file reads during cosign2 verification
/// Must be a multiple of `PAGE_SIZE` and `SHA_DMA_ALIGNMENT`
const CHUNK_SIZE_BYTES: usize = 32 * 64 * 512; // 1 mb

/// Verifies the `cosign2` header of a file
pub fn verify_cosign2<P, PC>(
    fs: &FileSystem<P>,
    crypto: &CryptoApi<PC>,
    path: impl Into<String>,
    location: Location,
    progress_fn: impl Fn(f32),
    check_trust: bool,
) -> Result<cosign2::Header, HashError>
where
    P: CheckedPermissions,
    P: MessageAllowed<fs::messages::GetMetadata>,
    P: MessageAllowed<fs::messages::OpenFileMessage>,
    P: MessageAllowed<fs::messages::ReadFile>,
    P: MessageAllowed<fs::messages::CloseFile>,
    PC: crypto::ShaPermissions,
{
    let path_str = path.into();
    let metadata = fs.metadata(path_str.clone(), location)?;
    let total_size = metadata.size as usize;

    let mut file = fs.open_file(path_str, location, OpenFlags { read: true, write: false, create: false })?;

    // Read just the header first
    let header_size = cosign2::Header::DEFAULT_SIZE;
    let header_buf_size = header_size.next_multiple_of(PAGE_SIZE);
    let mut header_mem = DropDeallocate::new(xous::map_memory(None, None, header_buf_size, MemoryFlags::W)?);

    file.read_exact(&mut header_mem.as_slice_mut()[..header_size])?;

    let binary_size = total_size - header_size;

    progress_fn(0.0);

    let sha256_streaming = Sha256Streaming { crypto, progress_fn: &progress_fn, binary_size };

    let Some(header) = cosign2::Header::parse_streaming(
        &header_mem.as_slice()[..header_size],
        binary_size,
        &KNOWN_SIGNERS,
        &Sha256 { crypto },
        &sha256_streaming,
        &EccVerifier {},
        header_size,
        file,
    )?
    else {
        return Err(HashError::MissingCosign2Header);
    };

    if check_trust && header.trust() != cosign2::Trust::FullyTrusted {
        return Err(HashError::NotTrusted);
    }

    progress_fn(1.0);

    Ok(header)
}

pub fn write_file_progress<P>(
    fs: &FileSystem<P>,
    path: impl Into<String>,
    location: Location,
    mem: &MemoryRange,
    total_size: usize,
    progress_fn: impl Fn(f32),
) -> Result<(), HashError>
where
    P: CheckedPermissions,
    P: MessageAllowed<fs::messages::GetMetadata>,
    P: MessageAllowed<fs::messages::OpenFileMessage>,
    P: MessageAllowed<fs::messages::WriteFile>,
    P: MessageAllowed<fs::messages::Flush>,
    P: MessageAllowed<fs::messages::CloseFile>,
    P: MessageAllowed<fs::messages::TruncateFile>,
{
    use std::io::Write;

    let path_str = path.into();

    let mut file =
        fs.open_file(path_str.clone(), location, OpenFlags { read: false, write: true, create: true })?;

    progress_fn(0.0);
    for (chunk_num, chunk) in mem.as_slice()[..total_size].chunks(CHUNK_SIZE_BYTES).enumerate() {
        file.write_all(chunk)?;

        let progress = (CHUNK_SIZE_BYTES as f32 * chunk_num as f32) / total_size as f32;
        progress_fn(progress);
    }

    file.truncate()?;

    progress_fn(1.0);
    Ok(())
}

pub fn read_progress<R: std::io::Read>(
    mut reader: R,
    size: usize,
    progress_fn: impl Fn(f32),
) -> Result<(DropDeallocate, usize), HashError> {
    let size_aligned = if size == 0 { PAGE_SIZE } else { size.next_multiple_of(PAGE_SIZE) };
    let total_size = size;

    let mut file_mem = DropDeallocate::new(xous::map_memory(None, None, size_aligned, xous::MemoryFlags::W)?);

    progress_fn(0.0);

    for (chunk_num, chunk) in file_mem.as_slice_mut()[..total_size].chunks_mut(CHUNK_SIZE_BYTES).enumerate() {
        reader.read_exact(chunk)?;

        let progress = (CHUNK_SIZE_BYTES as f32 * chunk_num as f32) / total_size as f32;
        progress_fn(progress);
    }

    progress_fn(1.0);
    Ok((file_mem, total_size))
}

pub fn read_file_progress<P>(
    fs: &FileSystem<P>,
    path: impl Into<String>,
    location: Location,
    progress_fn: impl Fn(f32),
) -> Result<(DropDeallocate, usize), HashError>
where
    P: CheckedPermissions,
    P: MessageAllowed<fs::messages::GetMetadata>,
    P: MessageAllowed<fs::messages::OpenFileMessage>,
    P: MessageAllowed<fs::messages::ReadFile>,
    P: MessageAllowed<fs::messages::CloseFile>,
{
    let path_str = path.into();
    let metadata = fs.metadata(path_str.clone(), location)?;

    let file =
        fs.open_file(path_str.clone(), location, OpenFlags { read: true, write: false, create: false })?;

    read_progress(file, metadata.size as usize, progress_fn)
}

pub fn copy_file_progress<P>(
    fs: &FileSystem<P>,
    path_src: impl Into<String>,
    location_src: Location,
    path_dst: impl Into<String>,
    location_dst: Location,
    progress_fn: impl Fn(f32),
) -> Result<(), HashError>
where
    P: CheckedPermissions,
    P: MessageAllowed<fs::messages::GetMetadata>,
    P: MessageAllowed<fs::messages::OpenFileMessage>,
    P: MessageAllowed<fs::messages::ReadFile>,
    P: MessageAllowed<fs::messages::WriteFile>,
    P: MessageAllowed<fs::messages::Flush>,
    P: MessageAllowed<fs::messages::CloseFile>,
    P: MessageAllowed<fs::messages::TruncateFile>,
{
    use std::io::Write;

    let path_src_str = path_src.into();
    let metadata = fs.metadata(path_src_str.clone(), location_src)?;

    let mut file_src = fs.open_file(
        path_src_str.clone(),
        location_src,
        OpenFlags { read: true, write: false, create: false },
    )?;

    let path_dst_str = path_dst.into();
    let mut file_dst =
        fs.open_file(path_dst_str, location_dst, OpenFlags { read: false, write: true, create: true })?;

    let total_size = metadata.size as usize;
    let mut buffer = vec![0u8; CHUNK_SIZE_BYTES];

    progress_fn(0.0);

    let mut bytes_copied = 0;
    while bytes_copied < total_size {
        let bytes_remaining = total_size - bytes_copied;
        let chunk_size = bytes_remaining.min(CHUNK_SIZE_BYTES);

        file_src.read_exact(&mut buffer[..chunk_size])?;
        file_dst.write_all(&buffer[..chunk_size])?;

        bytes_copied += chunk_size;
        progress_fn(bytes_copied as f32 / total_size as f32);
    }

    file_dst.truncate()?;
    progress_fn(1.0);

    Ok(())
}

/// Copies from any reader to a file without loading it entirely into memory.
/// If the destination file already exists and is larger than `total_size`,
/// it will be truncated to the new size.
pub fn stream_to_file_progress<P, R: Read>(
    fs: &FileSystem<P>,
    mut reader: R,
    total_size: usize,
    path_dst: impl Into<String>,
    location_dst: Location,
    progress_fn: impl Fn(f32),
) -> Result<(), HashError>
where
    P: CheckedPermissions,
    P: MessageAllowed<fs::messages::OpenFileMessage>,
    P: MessageAllowed<fs::messages::WriteFile>,
    P: MessageAllowed<fs::messages::Flush>,
    P: MessageAllowed<fs::messages::CloseFile>,
    P: MessageAllowed<fs::messages::TruncateFile>,
{
    use std::io::Write;

    let path_dst_str = path_dst.into();
    let mut file_dst =
        fs.open_file(path_dst_str, location_dst, OpenFlags { read: false, write: true, create: true })?;

    let mut buffer = vec![0u8; CHUNK_SIZE_BYTES];

    progress_fn(0.0);

    let mut bytes_written = 0;
    while bytes_written < total_size {
        let bytes_remaining = total_size - bytes_written;
        let chunk_size = bytes_remaining.min(CHUNK_SIZE_BYTES);

        reader.read_exact(&mut buffer[..chunk_size])?;
        file_dst.write_all(&buffer[..chunk_size])?;

        bytes_written += chunk_size;
        progress_fn(bytes_written as f32 / total_size as f32);
    }

    file_dst.truncate()?;
    progress_fn(1.0);

    Ok(())
}

struct Sha256<'a, P: crypto::ShaPermissions> {
    crypto: &'a CryptoApi<P>,
}

impl<'a, P: crypto::ShaPermissions> cosign2::Sha256 for Sha256<'a, P> {
    fn hash(&self, data: &[u8]) -> [u8; 32] { self.crypto.sha256(data).expect("sha256") }
}

/// Stream `total_len` bytes from `reader` through the SHA-256 engine, hashing in fixed-size
/// chunks so the whole input never has to be buffered at once. `progress_fn` is called with the
/// running fraction hashed.
pub fn sha256_streaming<P: crypto::ShaPermissions, R: std::io::Read, F: Fn(f32)>(
    crypto: &CryptoApi<P>,
    total_len: usize,
    reader: R,
    progress_fn: F,
) -> Result<[u8; 32], HashError> {
    use cosign2::Sha256Streaming as _;

    Sha256Streaming { crypto, progress_fn: &progress_fn, binary_size: total_len.max(1) }
        .hash_streaming(total_len, reader)
}

/// Streaming SHA-256 implementation to allow hashing of large files
struct Sha256Streaming<'a, P: crypto::ShaPermissions, F: Fn(f32)> {
    crypto: &'a CryptoApi<P>,
    progress_fn: &'a F,
    binary_size: usize,
}

impl<'a, P: crypto::ShaPermissions, F: Fn(f32)> cosign2::Sha256Streaming for Sha256Streaming<'a, P, F> {
    type Error = HashError;

    fn hash_streaming<R: std::io::Read>(
        &self,
        total_len: usize,
        mut reader: R,
    ) -> Result<[u8; 32], Self::Error> {
        let mut chunk_mem =
            DropDeallocate::new(xous::map_memory(None, None, CHUNK_SIZE_BYTES, MemoryFlags::W)?);

        // Initialize streaming SHA-256 context
        let mut sha_ctx = self.crypto.sha256_init();

        let mut bytes_hashed = 0usize;
        while bytes_hashed < total_len {
            let chunk_size = (total_len - bytes_hashed).min(CHUNK_SIZE_BYTES);

            reader.read_exact(&mut chunk_mem.as_slice_mut()[..chunk_size])?;
            sha_ctx.update(&chunk_mem.as_slice::<u8>()[..chunk_size])?;
            bytes_hashed += chunk_size;

            (self.progress_fn)(bytes_hashed as f32 / self.binary_size as f32 * 0.9);
        }

        // Finalize and get the hash
        let hash_vec = sha_ctx.finalize()?;
        let hash: [u8; 32] =
            hash_vec.try_into().map_err(|_| crypto::error::CryptoError::InvalidDataLength)?;
        Ok(hash)
    }
}

struct EccVerifier {}

impl EccVerifier {
    #[allow(dead_code)]
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
        if res == UECC_SUCCESS {
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
                return cosign2::VerificationResult::Valid;
            }
        }

        cosign2::VerificationResult::Invalid
    }
}
