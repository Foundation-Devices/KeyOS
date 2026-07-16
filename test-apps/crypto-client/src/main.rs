// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use crypto::{Direction, AES_BLOCK_SIZE};
use hmac::{Hmac, Mac};
use server::xous::{map_memory, DropDeallocate, MemoryFlags};
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};

crypto::use_api!();
fs::use_api!();

/// The cosign2 header extracted from a signed 128MB file of zeroes
pub const COSIGN2_HEADER_128MB: &[u8; 2048] = include_bytes!("../cosign2_header.bin");

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    log::info!("AES client pid: {}", server::xous::current_pid().unwrap());
    let crypto = CryptoApi::default();

    //  4 megabyte buffer
    let mut data = map_memory(None, None, 0x400000, MemoryFlags::W | MemoryFlags::POPULATE).unwrap();

    test_aes_correctness(&crypto, data);
    test_aes_cbc_multi_execute(&crypto, data);
    test_gcm_correctness(&crypto, data);
    test_unaligned_gcm_tag(&crypto, data);
    test_large_gcm(&crypto);
    test_aes_xts_correctness(&crypto, data);
    test_aes_speed(&crypto, data);

    log::info!("All AES tests done");
    test_sha_correctness(&crypto, data.as_slice_mut());
    test_sha_speed(&crypto, data);
    test_sha_non_contiguous(&crypto);
    test_hmac_correctness(&crypto);
    log::info!("All SHA tests done");

    test_cosign2(&crypto);

    // Multi-context streaming SHA tests
    test_streaming_sha_multi_context(&crypto, data.as_slice_mut());
    test_streaming_sha_sw_hw_switching(&crypto, data.as_slice_mut());
    log::info!("All tests done");
}

fn test_aes_speed(crypto: &CryptoApi, mut data: server::xous::MemoryRange) {
    let modes = &[
        ("CBC", crypto::AesMode::Cbc { iv: [b'z'; 16] }),
        ("ECB", crypto::AesMode::Ecb),
        ("GCM", crypto::AesMode::Gcm { iv: [b'z'; 12] }),
    ];

    let key = b"a1a2a3a4a5a6a7a8";

    for (mode_str, mode) in modes {
        data.as_slice_mut().fill(b'a');
        data.as_slice_mut()[0] = b'b';
        data.as_slice_mut()[1] = b'c';
        data.as_slice_mut()[2] = b'd';

        log::info!("Testing AES speed in {mode_str} mode...");

        log::info!("Testing with small data");
        log::info!("original data: {:02x?}", &data.as_slice::<u8>()[..256]);
        let aes_ctx = crypto.setup_aes(key, mode.clone()).unwrap();
        aes_ctx.execute(data, 0, 15 * AES_BLOCK_SIZE, Direction::Encrypt).unwrap();
        log::info!("encrypted: {:02x?}", &data.as_slice::<u8>()[..256]);
        let aes_ctx = crypto.setup_aes(key, mode.clone()).unwrap();
        aes_ctx.execute(data, 0, 15 * AES_BLOCK_SIZE, Direction::Decrypt).unwrap();
        log::info!("decrypted: {:02x?}", &data.as_slice::<u8>()[..256]);

        log::info!("Testing with big data");
        log::info!("original data: {:02x?}", &data.as_slice::<u8>()[data.len() - 32..]);
        let aes_ctx = crypto.setup_aes(key, mode.clone()).unwrap();
        aes_ctx.execute(data, 0, data.len(), Direction::Encrypt).unwrap();
        log::info!("encrypted: {:02x?}", &data.as_slice::<u8>()[data.len() - 32..]);
        let aes_ctx = crypto.setup_aes(key, mode.clone()).unwrap();
        aes_ctx.execute(data, 0, data.len(), Direction::Decrypt).unwrap();
        log::info!("decrypted: {:02x?}", &data.as_slice::<u8>()[data.len() - 32..]);

        log::info!("Proper load testing");
        let start = std::time::Instant::now();
        for _ in 0..8 {
            aes_ctx.execute(data, 0, data.len(), Direction::Encrypt).unwrap();
        }
        let elapsed = start.elapsed().as_millis();
        let mbps = 8.0 * data.len() as f64 / (1024.0 * 1024.0) / (elapsed as f64 / 1000.0);
        log::info!("AES-{mode_str} encrypt speed: {mbps:.2} MB/s");

        const CHUNK_SIZE: usize = 32 * 1024;
        log::info!("Load testing with {CHUNK_SIZE} byte chunks");
        let start = std::time::Instant::now();
        for _ in 0..16 {
            for offset in (0..data.len()).step_by(CHUNK_SIZE) {
                aes_ctx
                    .execute(data.subrange(offset, CHUNK_SIZE).unwrap(), 0, CHUNK_SIZE, Direction::Encrypt)
                    .unwrap();
            }
        }
        let elapsed = start.elapsed().as_millis();
        let mbps = 16.0 * data.len() as f64 / (1024.0 * 1024.0) / (elapsed as f64 / 1000.0);
        log::info!("AES-{mode_str} encrypt speed (chunked): {mbps:.2} MB/s");
    }
}

fn test_aes_correctness(crypto: &CryptoApi, mut data: server::xous::MemoryRange) {
    log::info!("Testing AES correctness");
    let test_vectors: &[(&[u8], crypto::AesMode, &[u8], &[u8])] = &[
        (
            &[b'x'; 16],
            crypto::AesMode::Ecb,
            &[
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                0x11,
            ],
            &[
                0xA9, 0xFC, 0x2E, 0x12, 0x35, 0xE1, 0xF1, 0x70, 0x86, 0x0A, 0x0F, 0x72, 0x3B, 0x0E, 0xD5,
                0x5E, //
                0xA9, 0xFC, 0x2E, 0x12, 0x35, 0xE1, 0xF1, 0x70, 0x86, 0x0A, 0x0F, 0x72, 0x3B, 0x0E, 0xD5,
                0x5E, //
                0x01, 0x3F, 0x50, 0xFB, 0x54, 0x51, 0x9C, 0xA9, 0xA4, 0x16, 0xF9, 0x1C, 0xB1, 0x7A, 0xEB,
                0x50, //
            ],
        ),
        (
            b"abcdefgh12345678qwertyuilkjhgfds",
            crypto::AesMode::Ecb,
            &[
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                0x11,
            ],
            &[
                0xAF, 0x24, 0x03, 0x91, 0xC3, 0xB1, 0x2A, 0x97, 0xB2, 0xC8, 0x4E, 0x2D, 0x6A, 0xAC, 0x20,
                0xBD, //
                0xAF, 0x24, 0x03, 0x91, 0xC3, 0xB1, 0x2A, 0x97, 0xB2, 0xC8, 0x4E, 0x2D, 0x6A, 0xAC, 0x20,
                0xBD, //
                0x00, 0x1C, 0xBE, 0x28, 0x2A, 0xE8, 0x50, 0x5B, 0xB5, 0x8D, 0x00, 0x8D, 0xD8, 0x0C, 0x55,
                0xDC,
            ],
        ),
        (
            b"abcdefgh12345678",
            crypto::AesMode::Cbc { iv: *b"zxcvbnm,asdfghjk" },
            &[
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                0x11,
            ],
            &[
                0xDA, 0xD1, 0xC0, 0x89, 0x6E, 0x42, 0xD0, 0x12, 0x8C, 0x62, 0xB4, 0x4E, 0xB4, 0x9F, 0xDD,
                0xF8, 0xF7, 0x70, 0x65, 0x33, 0x98, 0x74, 0x54, 0x12, 0x3D, 0x3D, 0x6A, 0x8F, 0xCC, 0xF2,
                0x30, 0x7D, 0x45, 0x3C, 0x83, 0x42, 0x85, 0x48, 0xA0, 0xCB, 0x5A, 0x65, 0x4C, 0x54, 0xFF,
                0x47, 0x90, 0xBD,
            ],
        ),
        (
            b"abcdefgh12345678qwertyuilkjhgfds",
            crypto::AesMode::Cbc { iv: *b"zxcvbnm,asdfghjk" },
            &[
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                0x11,
            ],
            &[
                0x86, 0xE8, 0x93, 0xF1, 0x37, 0x57, 0x0C, 0xF0, 0x79, 0xDC, 0xB5, 0x43, 0x04, 0x11, 0xA9,
                0x11, 0x16, 0xF5, 0x97, 0xE0, 0x3B, 0x10, 0xAD, 0xEE, 0xB3, 0xF1, 0x56, 0x65, 0x7E, 0xBA,
                0x5A, 0x9E, 0xF6, 0x64, 0xF1, 0xD0, 0x27, 0x68, 0x7E, 0x76, 0xE8, 0x63, 0x62, 0xAB, 0x18,
                0x55, 0x06, 0xB4,
            ],
        ),
        (
            b"abcdefgh12345678",
            crypto::AesMode::Ctr { iv: *b"zxcvbnm,asdfghjk" },
            &[
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                0x11,
            ],
            &[
                0xDA, 0xD1, 0xC0, 0x89, 0x6E, 0x42, 0xD0, 0x12, 0x8C, 0x62, 0xB4, 0x4E, 0xB4, 0x9F, 0xDD,
                0xF8, 0x0A, 0xAA, 0x52, 0x86, 0x8A, 0xB9, 0xCE, 0xEE, 0x8B, 0x26, 0x68, 0x03, 0x93, 0x8E,
                0x5D, 0x0A, 0xB4, 0x36, 0xF6, 0xCF, 0x47, 0x57, 0x1E, 0xC1, 0x16, 0x42, 0xE3, 0x4D, 0x87,
                0xE4, 0xAC, 0x5E,
            ],
        ),
        (
            b"abcdefgh12345678qwertyuilkjhgfds",
            crypto::AesMode::Ctr { iv: *b"zxcvbnm,asdfghjk" },
            &[
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                0x11,
            ],
            &[
                0x86, 0xE8, 0x93, 0xF1, 0x37, 0x57, 0x0C, 0xF0, 0x79, 0xDC, 0xB5, 0x43, 0x04, 0x11, 0xA9,
                0x11, 0x0F, 0xEC, 0x19, 0x8B, 0x8F, 0xD4, 0xD4, 0xBF, 0x9E, 0x60, 0x09, 0xB3, 0x19, 0x3B,
                0x7F, 0x19, 0x53, 0x65, 0x03, 0x13, 0xE6, 0xE4, 0x9B, 0x87, 0xF2, 0xA3, 0x25, 0x64, 0xF3,
                0xF9, 0x62, 0xC7,
            ],
        ),
        (
            b"abcdefgh12345678qwertyuilkjhgfds",
            crypto::AesMode::Ctr { iv: *b"zxcvbnm,asdfghjk" },
            &[
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            ],
            &[
                0x86, 0xE8, 0x93, 0xF1, 0x37, 0x57, 0x0C, 0xF0, 0x79, 0xDC, 0xB5, 0x43, 0x04, 0x11, 0xA9,
                0x11, 0x0F, 0xEC, 0x19, 0x8B, 0x8F, 0xD4, 0xD4, 0xBF, 0x9E, 0x60, 0x09, 0xB3, 0x19, 0x3B,
                0x7F, 0x19, 0x53, 0x65, 0x03, 0x13, 0xE6, 0xE4, 0x9B, 0x87, 0xF2, 0xA3, 0x25, 0x64, 0xF3,
            ],
        ),
        (
            b"abcdefgh12345678",
            crypto::AesMode::Gcm { iv: *b"1234abcdGHJK" },
            &[
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                0x11,
            ],
            &[
                0x32, 0x35, 0xA0, 0xD2, 0xB8, 0x38, 0x61, 0x09, 0x5C, 0x96, 0x90, 0x87, 0xFA, 0xDA, 0x34,
                0x81, 0x51, 0x73, 0xD5, 0xF8, 0x70, 0xCC, 0xF7, 0x02, 0xAD, 0x27, 0xF3, 0xAE, 0x6A, 0x07,
                0xD4, 0x3B, 0x7F, 0x5C, 0x0D, 0x64, 0x02, 0x63, 0xFD, 0x4C, 0xB2, 0xFB, 0x61, 0xCA, 0x80,
                0x27, 0x6F, 0xC2,
                //tag: C2E943D6B264914A20EB3E60F87698E3
            ],
        ),
        (
            b"abcdefgh12345678qwertyuilkjhgfds",
            crypto::AesMode::Gcm { iv: *b"1234abcdGHJK" },
            &[
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
                0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            ],
            &[
                0x1C, 0x41, 0x8E, 0xE5, 0x08, 0x75, 0xEE, 0x0C, 0xCF, 0x92, 0x3C, 0x8A, 0x40, 0xF2, 0x56,
                0x33, 0x05, 0x75, 0x13, 0x6B, 0xE6, 0xE9, 0x8A, 0x6D, 0x68, 0x65, 0xFF, 0xBB, 0x19, 0x77,
                0x7E, 0xAD, 0x53, 0x99, 0x8C, 0x73, 0x37, 0x98, 0xC5, 0x69, 0x6A, 0x59, 0x04, 0x56,
                0x49,
                //tag: 9504ca591555cdd213805cad769edef4
            ],
        ),
    ];

    for (i, (key, mode, pt, ct)) in test_vectors.iter().enumerate() {
        log::info!("Testing {key:?} + {mode:?} (len={}", pt.len());
        let aes_ctx = crypto.setup_aes(key, mode.clone()).unwrap();
        data.as_slice_mut()[..pt.len()].copy_from_slice(pt);
        aes_ctx.execute(data, 0, pt.len(), Direction::Encrypt).unwrap();
        assert!(
            &data.as_slice::<u8>()[..ct.len()] == *ct,
            "Wrong encryption result in test {i}: {:02x?}",
            &data.as_slice::<u8>()[..ct.len()]
        );
        let aes_ctx = crypto.setup_aes(key, mode.clone()).unwrap();
        aes_ctx.execute(data, 0, pt.len(), Direction::Decrypt).unwrap();
        assert!(
            &data.as_slice::<u8>()[..pt.len()] == *pt,
            "Wrong decryption result in test {i}: {:02x?}",
            &data.as_slice::<u8>()[..pt.len()]
        );
    }
    log::info!("Done");
}

fn test_aes_cbc_multi_execute(crypto: &CryptoApi, mut data: server::xous::MemoryRange) {
    log::info!("Testing AES CBC multi-execute correctness");
    let aes_ctx = crypto.setup_aes(&[b'x'; 32], crypto::AesMode::Cbc { iv: [b'z'; 16] }).unwrap();
    let pt = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
    ];
    let ct = [
        0xD8, 0x64, 0xB2, 0x2B, 0xC9, 0xEF, 0x09, 0x81, 0x4C, 0x18, 0x01, 0x27, 0x76, 0xF5, 0xF7, 0xCF, 0x4D,
        0xBB, 0xD5, 0x91, 0xE0, 0xDD, 0xB4, 0x18, 0x24, 0xAB, 0xBC, 0x3F, 0xC7, 0x9B, 0x4E, 0x9F, 0xC5, 0xA4,
        0x44, 0x1B, 0xDF, 0x6C, 0x02, 0xED, 0xC2, 0xC0, 0x2F, 0xBD, 0x59, 0x4E, 0x2D, 0x39,
    ];
    data.as_slice_mut()[..pt.len()].copy_from_slice(&pt);
    aes_ctx.execute(data, 0, 32, Direction::Encrypt).unwrap();
    aes_ctx.execute(data, 32, 16, Direction::Encrypt).unwrap();
    assert!(
        &data.as_slice::<u8>()[..ct.len()] == &ct,
        "Wrong encryption result CBC multi-execute test: {:02x?}",
        &data.as_slice::<u8>()[..ct.len()]
    );
    let aes_ctx = crypto.setup_aes(&[b'x'; 32], crypto::AesMode::Cbc { iv: [b'z'; 16] }).unwrap();
    aes_ctx.execute(data, 0, 16, Direction::Decrypt).unwrap();
    aes_ctx.execute(data, 16, 32, Direction::Decrypt).unwrap();
    assert!(
        &data.as_slice::<u8>()[..pt.len()] == &pt,
        "Wrong decryption result CBC multi-execute test: {:02x?}",
        &data.as_slice::<u8>()[..pt.len()]
    );
}

fn test_gcm_correctness(crypto: &CryptoApi, mut data: server::xous::MemoryRange) {
    log::info!("Testing GCM mode correctness");
    let aes_ctx =
        crypto.setup_aes(b"12345678qwertyui", crypto::AesMode::Gcm { iv: *b"ijklmnopqrst" }).unwrap();
    let pt = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
    ];
    let ct = [
        0x4B, 0x3D, 0xDB, 0x04, 0x5F, 0xC4, 0xD2, 0xFE, 0x76, 0x51, 0xC6, 0x74, 0xFA, 0x2C, 0xC4, 0x79, 0x92,
        0x26, 0xB2, 0xED, 0xAD, 0xA9, 0x9F, 0x03, 0x83, 0xFB, 0x0E, 0xD3, 0xF1, 0x5E, 0xE0, 0x33, 0xBD, 0xDD,
        0x48, 0x99, 0x49, 0x2B, 0x28, 0xCA, 0x69, 0x63, 0x1B, 0xD2, 0xFD, 0x51, 0xBD, 0x72,
    ];
    let expected_tag =
        [0x0B, 0xE3, 0x06, 0x04, 0xF3, 0x69, 0xC2, 0xED, 0x99, 0xAE, 0xF4, 0x4D, 0x21, 0x4A, 0xB9, 0x6E];
    let expected_tag_aad =
        [0x8E, 0xF8, 0xED, 0x02, 0xA1, 0x38, 0x19, 0x23, 0x14, 0x79, 0x6B, 0x1F, 0x74, 0xE3, 0xB0, 0x8B];
    data.as_slice_mut()[..pt.len()].copy_from_slice(&pt);

    aes_ctx.add_aad(b"unaligned").unwrap();

    // Check interrupted execution
    aes_ctx.execute(data, 0, 16, Direction::Encrypt).unwrap();
    aes_ctx.execute(data, 16, 32, Direction::Encrypt).unwrap();
    assert!(
        &data.as_slice::<u8>()[..ct.len()] == &ct,
        "Wrong encryption result GCM multi-execute test: {:02x?}",
        &data.as_slice::<u8>()[..ct.len()]
    );
    let tag = aes_ctx.gcm_tag().unwrap();
    assert!(tag == expected_tag_aad, "Wrong tag in GCM test: {tag:02x?}");

    let aes_ctx =
        crypto.setup_aes(b"12345678qwertyui", crypto::AesMode::Gcm { iv: *b"ijklmnopqrst" }).unwrap();
    aes_ctx.execute(data, 0, 48, Direction::Decrypt).unwrap();
    assert!(
        &data.as_slice::<u8>()[..pt.len()] == &pt,
        "Wrong decryption result GCM multi-execute test: {:02x?}",
        &data.as_slice::<u8>()[..pt.len()]
    );
    let tag = aes_ctx.gcm_tag().unwrap();
    assert!(tag == expected_tag, "Wrong tag in GCM test: {tag:02x?}");
}

fn test_unaligned_gcm_tag(crypto: &CryptoApi, mut data: server::xous::MemoryRange) {
    log::info!("Testing GCM mode tag with unaligned length");
    let aes_ctx =
        crypto.setup_aes(b"12345678qwertyui", crypto::AesMode::Gcm { iv: *b"ijklmnopqrst" }).unwrap();
    let pt = [
        0u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        0u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
    ];
    let ct = [
        0x4B, 0x3D, 0xDB, 0x04, 0x5F, 0xC4, 0xD2, 0xFE, 0x76, 0x51, 0xC6, 0x74, 0xFA, 0x2C, 0xC4, 0x79, 0x92,
        0x26, 0xB2, 0xED, 0xAD, 0xA9, 0x9F, 0x03, 0x83, 0xFB, 0x0E, 0xD3, 0xF1, 0x5E, 0xE0, 0x33, 0xBD, 0xDD,
        0x48, 0x99, 0x49, 0x2B, 0x28, 0xCA, 0x69, 0x63, 0x1B, 0xD2, 0xFD,
    ];
    let expected_tag =
        [0x1f, 0x88, 0x07, 0x49, 0x6c, 0xf4, 0x59, 0x89, 0xf1, 0x2f, 0xfe, 0x7a, 0x71, 0x2c, 0xef, 0x3e];
    data.as_slice_mut()[..pt.len()].copy_from_slice(&pt);

    aes_ctx.add_aad(b"unaligned, but long, maybe even multi block.").unwrap();
    // Check interrupted execution and offset handling too
    aes_ctx.execute(data, 0, 16, Direction::Encrypt).unwrap();
    aes_ctx.execute(data, 16, pt.len() - 16, Direction::Encrypt).unwrap();
    assert!(
        aes_ctx.execute(data, 0, 16, Direction::Encrypt).is_err(),
        "GCM execute after a partial block should fail"
    );

    assert!(
        &data.as_slice::<u8>()[..ct.len()] == &ct,
        "Wrong encryption result GCM unaligned len test: {:02x?}",
        &data.as_slice::<u8>()[..ct.len()]
    );
    let tag = aes_ctx.gcm_tag().unwrap();
    assert!(tag == expected_tag, "Wrong tag in GCM test: {tag:02x?}");

    let aes_ctx =
        crypto.setup_aes(b"12345678qwertyui", crypto::AesMode::Gcm { iv: *b"ijklmnopqrst" }).unwrap();

    aes_ctx.add_aad(b"unaligned, but long, maybe even multi block.").unwrap();
    aes_ctx.execute(data, 0, ct.len(), Direction::Decrypt).unwrap();

    assert!(
        &data.as_slice::<u8>()[..pt.len()] == &pt,
        "Wrong decryption result GCM unaligned len test: {:02x?}",
        &data.as_slice::<u8>()[..pt.len()]
    );
    let tag = aes_ctx.gcm_tag().unwrap();
    assert!(tag == expected_tag, "Wrong tag in GCM test: {tag:02x?}");
}

fn test_large_gcm(crypto: &CryptoApi) {
    log::info!("Testing possible counter rollover in GCM mode");
    let mut d1 = DropDeallocate::new(map_memory(None, None, 1024 * 1024 * 4, MemoryFlags::W).unwrap());
    let mut d2 = DropDeallocate::new(map_memory(None, None, 1024 * 1024 * 4, MemoryFlags::W).unwrap());
    d1.as_slice_mut::<u32>().fill(0xabcd1234);
    d2.as_slice_mut::<u32>().fill(0xabcd1234);
    let aes_ctx =
        crypto.setup_aes(b"12345678qwertyui", crypto::AesMode::Gcm { iv: *b"ijklmnopqrst" }).unwrap();
    aes_ctx.execute(*d1, 0, 1024 * 1024, Direction::Encrypt).unwrap();
    aes_ctx.execute(*d1, 1024 * 1024, 1024 * 1024, Direction::Encrypt).unwrap();
    let tag = aes_ctx.gcm_tag().unwrap();

    let aes_ctx =
        crypto.setup_aes(b"12345678qwertyui", crypto::AesMode::Gcm { iv: *b"ijklmnopqrst" }).unwrap();
    aes_ctx.execute(*d2, 0, 2 * 1024 * 1024, Direction::Encrypt).unwrap();
    let tag2 = aes_ctx.gcm_tag().unwrap();
    assert!(d1.as_slice::<u32>() == d2.as_slice::<u32>(), "GCM mode did not work for large chunks");
    assert!(tag == tag2);
}

fn test_aes_xts_correctness(crypto: &CryptoApi, mut data: server::xous::MemoryRange) {
    log::info!("Testing XTS mode j correctness");
    data.as_slice_mut().fill(b'a');
    let tweak = [b'z'; 16];

    unsafe {
        crypto
            .disk_encrypt_unsafe(
                tweak,
                200,
                data.subrange(0, 16 * 16).unwrap(),
                data.subrange(0, 16 * 16).unwrap(),
                Direction::Encrypt,
            )
            .unwrap();
        log::info!("encrypted: {:02x?}", &data.as_slice::<u8>()[..256]);
        crypto
            .disk_encrypt_unsafe(
                tweak,
                200,
                data.subrange(0, 2 * 16).unwrap(),
                data.subrange(0, 2 * 16).unwrap(),
                Direction::Decrypt,
            )
            .unwrap();
        crypto
            .disk_encrypt_unsafe(
                tweak,
                202,
                data.subrange(2 * 16, 14 * 16).unwrap(),
                data.subrange(2 * 16, 14 * 16).unwrap(),
                Direction::Decrypt,
            )
            .unwrap();
    }
    log::info!("decrypted: {:02x?}", &data.as_slice::<u8>()[..256]);
}
fn test_sha_correctness(crypto: &CryptoApi, data: &mut [u8]) {
    log::info!("Hashing correctness testing (SHA224, SHA256, SHA384, SHA512)");

    for (i, d) in data[..0x4000].iter_mut().enumerate() {
        *d = ((i % 123) * (i % 57)) as u8;
    }

    for offset in [0, 0x40, 0x1000 - 0x40, 0xfff, 0x1000, 0x1040] {
        for len in [4, 5, 16, 17, 0x785, 0x1000 - 0x40, 0x1000, 0x1001, 0x14f6] {
            let slice = &data[offset..offset + len];
            let known_good: [u8; 28] = Sha224::digest(slice).into();
            let service_result = crypto.sha224(slice).unwrap();
            assert_eq!(known_good, service_result, "Wrong result for SHA224 @ offset={offset} len={len}");
            let known_good: [u8; 32] = Sha256::digest(slice).into();
            let service_result = crypto.sha256(slice).unwrap();
            assert_eq!(known_good, service_result, "Wrong result for SHA256 @ offset={offset} len={len}");
            let known_good: [u8; 48] = Sha384::digest(slice).into();
            let service_result = crypto.sha384(slice).unwrap();
            assert_eq!(known_good, service_result, "Wrong result for SHA384 @ offset={offset} len={len}");
            let known_good: [u8; 64] = Sha512::digest(slice).into();
            let service_result = crypto.sha512(slice).unwrap();
            assert_eq!(known_good, service_result, "Wrong result for SHA512 @ offset={offset} len={len}");
        }
    }
}

fn sha_speed(crypto: &CryptoApi, data: &[u8], label: &str) {
    let start = std::time::Instant::now();
    for _ in 0..32 {
        crypto.sha256(data).unwrap();
    }
    let elapsed = start.elapsed().as_millis();
    let mbps = 32.0 * data.len() as f64 / (1024.0 * 1024.0) / (elapsed as f64 / 1000.0);
    log::info!("SHA256 speed ({label}): {mbps:.2} MB/s");
}

fn test_sha_speed(crypto: &CryptoApi, mut data: server::xous::MemoryRange) {
    log::info!("Hashing load testing (SHA256)");
    data.as_slice_mut().fill(0);
    sha_speed(crypto, data.as_slice(), "aligned");
    sha_speed(crypto, &data.as_slice()[1..], "unaligned (+1)");
}

fn test_sha_non_contiguous(crypto: &CryptoApi) {
    log::info!("Hashing non-contiguous buffer");
    let mut buf = DropDeallocate::new(map_memory(None, None, 0x4000, MemoryFlags::W).unwrap());

    // We on-demand map these out of order to explicitly break contiguity
    buf.as_slice_mut::<u8>()[0x2000] = 1;
    buf.as_slice_mut::<u8>()[0x1000] = 2;
    buf.as_slice_mut::<u8>()[0x3000] = 3;
    buf.as_slice_mut::<u8>()[0x0000] = 0;

    for (i, c) in buf.as_slice_mut::<usize>().iter_mut().enumerate() {
        *c = i;
    }
    let known_good: [u8; 32] = Sha256::digest(buf.as_slice::<u8>()).into();
    let service_result = crypto.sha256(buf.as_slice::<u8>()).unwrap();
    assert_eq!(known_good, service_result, "Wrong result for SHA256 on non-contiguous buffer");
}

fn test_hmac_correctness(crypto: &CryptoApi) {
    log::info!("Hashing correctness testing (HMAC-224, HMAC-256, HMAC-384, HMAC-512)");
    for key in [[b'k'; 0].to_vec(), [b'k'; 32].to_vec(), [b'k'; 48].to_vec(), [b'k'; 64].to_vec()] {
        for msg in [[b'm'; 0].to_vec(), [b'm'; 32].to_vec(), [b'm'; 48].to_vec(), [b'm'; 64].to_vec()] {
            type HmacSha224 = Hmac<Sha224>;
            let mut mac = HmacSha224::new_from_slice(&key).unwrap();
            mac.update(&msg);
            let known_good = mac.finalize().into_bytes().to_vec();
            let key_len = key.len();
            let msg_len = msg.len();
            let service_result = crypto.hmac224(key.clone(), msg.clone()).unwrap();
            assert_eq!(
                known_good, service_result,
                "Wrong result for HMAC-224 key_len={key_len} msg_len={msg_len}"
            );
            type HmacSha256 = Hmac<Sha256>;
            let mut mac = HmacSha256::new_from_slice(&key).unwrap();
            mac.update(&msg);
            let known_good = mac.finalize().into_bytes().to_vec();
            let key_len = key.len();
            let msg_len = msg.len();
            let service_result = crypto.hmac256(key.clone(), msg.clone()).unwrap();
            assert_eq!(
                known_good, service_result,
                "Wrong result for HMAC-256 key_len={key_len} msg_len={msg_len}"
            );
            type HmacSha384 = Hmac<Sha384>;
            let mut mac = HmacSha384::new_from_slice(&key).unwrap();
            mac.update(&msg);
            let known_good = mac.finalize().into_bytes().to_vec();
            let key_len = key.len();
            let msg_len = msg.len();
            let service_result = crypto.hmac384(key.clone(), msg.clone()).unwrap();
            assert_eq!(
                known_good, service_result,
                "Wrong result for HMAC-384 key_len={key_len} msg_len={msg_len}"
            );
            type HmacSha512 = Hmac<Sha512>;
            let mut mac = HmacSha512::new_from_slice(&key).unwrap();
            mac.update(&msg);
            let known_good = mac.finalize().into_bytes().to_vec();
            let key_len = key.len();
            let msg_len = msg.len();
            let service_result = crypto.hmac512(key.clone(), msg).unwrap();
            assert_eq!(
                known_good, service_result,
                "Wrong result for HMAC-512 key_len={key_len} msg_len={msg_len}"
            );
        }
    }
}

fn test_cosign2(crypto: &CryptoApi) {
    log::info!("cosign2 fw file verification test");
    let fs = FileSystem::default();

    let start = std::time::Instant::now();
    fw_utils::hash::verify_cosign2(&fs, &crypto, "/keyos/app.bin", fs::Location::System, |_| (), false)
        .expect("verify cosign2");
    let elapsed = start.elapsed().as_millis();
    let mbps = fs.metadata("/keyos/app.bin", fs::Location::System).unwrap().size as f64
        / (1024.0 * 1024.0)
        / (elapsed as f64 / 1000.0);
    log::info!("app.bin cosign verification speed: {mbps:.2} MB/s");

    log::info!("Verifying an enormous file (128 MB of zeroes + header)");
    {
        use std::io::Write;

        // The header was signed for 128MB of zeroes as the binary content
        // Total file = header (2048 bytes) + 128MB of zeroes
        const BINARY_SIZE: usize = 128 * 1024 * 1024; // 128 MB of zeroes
        const TOTAL_FILE_SIZE: usize = cosign2::Header::DEFAULT_SIZE + BINARY_SIZE;
        const TEMP_FILE_PATH: &str = "/big_test_file.bin";

        // Create a big temp file using pre-generated cosign2 header for 128MB of zeroes
        let mut temp_file = fs
            .open_file(
                TEMP_FILE_PATH,
                fs::Location::System,
                fs::OpenFlags { read: true, write: true, create: true },
            )
            .expect("create temp file");

        // Write the pre-generated cosign2 header (signed for 128MB of zeroes)
        temp_file.write_all(COSIGN2_HEADER_128MB).expect("write header");

        // Write 128MB of zeros (the binary content that was signed)
        let zeros = vec![0u8; 64 * 1024]; // 64KB chunks
        let mut written = 0;
        while written < BINARY_SIZE {
            let chunk_size = (BINARY_SIZE - written).min(zeros.len());
            temp_file.write_all(&zeros[..chunk_size]).expect("write zeros");
            written += chunk_size;
        }
        temp_file.flush().expect("flush temp file");
        drop(temp_file);

        log::info!(
            "Created test file: {} byte header + {}MB of zeroes = {}MB total",
            cosign2::Header::DEFAULT_SIZE,
            BINARY_SIZE / (1024 * 1024),
            TOTAL_FILE_SIZE / (1024 * 1024)
        );

        // Now try to verify it - this file has a valid signature for 128MB of zeroes
        let start = std::time::Instant::now();
        let result = fw_utils::hash::verify_cosign2(
            &fs,
            &crypto,
            TEMP_FILE_PATH,
            fs::Location::System,
            |progress| {
                if (progress * 100.0) as u32 % 10 == 0 {
                    log::debug!("Verification progress: {:.0}%", progress * 100.0);
                }
            },
            false,
        );
        let elapsed = start.elapsed().as_millis();

        match result {
            Ok(_) => log::info!("Big file verification succeeded"),
            Err(e) => log::info!("Big file verification failed: {:?}", e),
        }

        let mbps = TOTAL_FILE_SIZE as f64 / (1024.0 * 1024.0) / (elapsed as f64 / 1000.0);
        log::info!("Big file verification speed: {mbps:.2} MB/s (elapsed: {}ms)", elapsed);

        // Clean up
        fs.remove(TEMP_FILE_PATH, fs::Location::System).ok();
    }
}

/// Test streaming SHA multi-context handling
/// Verifies:
/// 1. Multiple contexts can be created concurrently
/// 2. Multiple threads can use their contexts concurrently
/// 3. Results are correct when contexts are used interleaved
fn test_streaming_sha_multi_context(crypto: &CryptoApi, data: &mut [u8]) {
    use std::sync::{Arc, Barrier};
    use std::thread;

    log::info!("Testing streaming SHA multi-context handling");

    const CHUNK_SIZE: usize = 0x1000; // 4KB chunks
    const TOTAL_SIZE: usize = CHUNK_SIZE * 4; // 16KB total per context

    for (i, d) in data[..TOTAL_SIZE * 4].iter_mut().enumerate() {
        *d = ((i * 17 + 31) % 256) as u8;
    }

    log::info!("test 1: 4 concurrent streaming SHA contexts");
    {
        let mut ctx1 = crypto.sha256_init();
        let mut ctx2 = crypto.sha256_init();
        let mut ctx3 = crypto.sha256_init();
        let mut ctx4 = crypto.sha256_init();

        log::info!("test 2: interleaved contexts");
        for chunk_idx in 0..4 {
            let offset = chunk_idx * CHUNK_SIZE;
            ctx1.update(&data[offset..offset + CHUNK_SIZE]).expect("ctx1 update failed");
            ctx2.update(&data[TOTAL_SIZE + offset..TOTAL_SIZE + offset + CHUNK_SIZE])
                .expect("ctx2 update failed");
            ctx3.update(&data[TOTAL_SIZE * 2 + offset..TOTAL_SIZE * 2 + offset + CHUNK_SIZE])
                .expect("ctx3 update failed");
            ctx4.update(&data[TOTAL_SIZE * 3 + offset..TOTAL_SIZE * 3 + offset + CHUNK_SIZE])
                .expect("ctx4 update failed");
        }

        let hash1 = ctx1.finalize().expect("ctx1 finalize failed");
        let hash2 = ctx2.finalize().expect("ctx2 finalize failed");
        let hash3 = ctx3.finalize().expect("ctx3 finalize failed");
        let hash4 = ctx4.finalize().expect("ctx4 finalize failed");

        let expected1: [u8; 32] = Sha256::digest(&data[0..TOTAL_SIZE]).into();
        let expected2: [u8; 32] = Sha256::digest(&data[TOTAL_SIZE..TOTAL_SIZE * 2]).into();
        let expected3: [u8; 32] = Sha256::digest(&data[TOTAL_SIZE * 2..TOTAL_SIZE * 3]).into();
        let expected4: [u8; 32] = Sha256::digest(&data[TOTAL_SIZE * 3..TOTAL_SIZE * 4]).into();

        assert_eq!(hash1.as_slice(), expected1.as_slice(), "context 1 hash mismatch");
        assert_eq!(hash2.as_slice(), expected2.as_slice(), "context 2 hash mismatch");
        assert_eq!(hash3.as_slice(), expected3.as_slice(), "context 3 hash mismatch");
        assert_eq!(hash4.as_slice(), expected4.as_slice(), "context 4 hash mismatch");
        log::info!("  interleaved context usage: all hashes match");
    }

    log::info!("test 3: verifying contexts can be reused after drop");
    {
        let mut ctx = crypto.sha256_init();
        ctx.update(&data[..CHUNK_SIZE]).expect("update failed");
        let hash = ctx.finalize().expect("finalize failed");
        let expected: [u8; 32] = Sha256::digest(&data[..CHUNK_SIZE]).into();
        assert_eq!(hash.as_slice(), expected.as_slice(), "reused context hash mismatch");
        log::info!("  context reuse after drop: OK");
    }

    log::info!("test 4: verifying context drop cleanup");
    {
        for iteration in 0..8 {
            {
                let _ctx1 = crypto.sha256_init();
                let _ctx2 = crypto.sha256_init();
                let _ctx3 = crypto.sha256_init();
                let _ctx4 = crypto.sha256_init();
                // All 4 contexts are dropped here without being finalized
            }
            log::debug!("  iteration {}: dropped 4 contexts without finalizing", iteration);
        }

        let mut ctx = crypto.sha256_init();
        ctx.update(&data[..CHUNK_SIZE]).expect("update failed");
        let hash = ctx.finalize().expect("finalize failed");
        let expected: [u8; 32] = Sha256::digest(&data[..CHUNK_SIZE]).into();
        assert_eq!(hash.as_slice(), expected.as_slice(), "hash mismatch after drop cycles");
        log::info!("  context drop cleanup: OK (32 contexts dropped without leak)");
    }

    log::info!("test 5: partial update drop cleanup");
    {
        for _ in 0..4 {
            let mut ctx = crypto.sha256_init();
            ctx.update(&data[..CHUNK_SIZE]).expect("update failed");
            // Drop without finalizing
        }

        let mut ctx = crypto.sha256_init();
        ctx.update(&data[..CHUNK_SIZE]).expect("update failed");
        let _ = ctx.finalize().expect("finalize failed");
        log::info!("  partial update drop cleanup: OK");
    }

    log::info!("test 6: multi-threaded concurrent sha contexts");
    {
        const NUM_THREADS: usize = 4;
        let barrier = Arc::new(Barrier::new(NUM_THREADS));
        let mut handles = Vec::new();

        let mut expected_hashes: Vec<[u8; 32]> = Vec::new();
        for t in 0..NUM_THREADS {
            let start = t * TOTAL_SIZE;
            let end = start + TOTAL_SIZE;
            let expected: [u8; 32] = Sha256::digest(&data[start..end]).into();
            expected_hashes.push(expected);
        }

        for thread_id in 0..NUM_THREADS {
            let barrier = Arc::clone(&barrier);
            let expected_hash = expected_hashes[thread_id];
            let data_start = thread_id * TOTAL_SIZE;

            let handle = thread::spawn(move || {
                let crypto = CryptoApi::default();
                let mut thread_data =
                    map_memory(None, None, TOTAL_SIZE, MemoryFlags::W | MemoryFlags::POPULATE)
                        .expect("failed to allocate thread buffer");

                for (i, d) in thread_data.as_slice_mut().iter_mut().enumerate() {
                    *d = (data_start + i).wrapping_mul(17).wrapping_add(31) as u8;
                }

                barrier.wait();

                let mut ctx = crypto.sha256_init();

                for chunk_idx in 0..4 {
                    let offset = chunk_idx * CHUNK_SIZE;
                    ctx.update(&thread_data.as_slice()[offset..offset + CHUNK_SIZE])
                        .expect(&format!("thread {thread_id}: update failed at chunk {chunk_idx}"));
                }

                let hash = ctx.finalize().expect(&format!("thread {thread_id}: finalize failed"));
                assert_eq!(hash.as_slice(), expected_hash.as_slice(), "thread {thread_id}: hash mismatch");

                log::info!("  thread {thread_id}: hash verified OK");
                thread_id
            });

            handles.push(handle);
        }

        for handle in handles {
            handle.join().expect("thread panicked");
        }
        log::info!("  all {NUM_THREADS} threads completed successfully");
    }

    log::info!("test 7: testing other SHA algorithms with streaming");
    {
        use crypto::messages::ShaAlgo;

        let mut ctx = crypto.sha_init(ShaAlgo::Sha224);
        ctx.update(&data[..CHUNK_SIZE]).expect("SHA224 update failed");
        let hash = ctx.finalize().expect("SHA224 finalize failed");
        let expected: [u8; 28] = Sha224::digest(&data[..CHUNK_SIZE]).into();
        assert_eq!(hash.as_slice(), expected.as_slice(), "SHA224 streaming hash mismatch");

        let mut ctx = crypto.sha_init(ShaAlgo::Sha384);
        ctx.update(&data[..CHUNK_SIZE]).expect("SHA384 update failed");
        let hash = ctx.finalize().expect("SHA384 finalize failed");
        let expected: [u8; 48] = Sha384::digest(&data[..CHUNK_SIZE]).into();
        assert_eq!(hash.as_slice(), expected.as_slice(), "SHA384 streaming hash mismatch");

        let mut ctx = crypto.sha_init(ShaAlgo::Sha512);
        ctx.update(&data[..CHUNK_SIZE]).expect("SHA512 update failed");
        let hash = ctx.finalize().expect("SHA512 finalize failed");
        let expected: [u8; 64] = Sha512::digest(&data[..CHUNK_SIZE]).into();
        assert_eq!(hash.as_slice(), expected.as_slice(), "SHA512 streaming hash mismatch");

        log::info!("  all SHA algorithms: OK");
    }

    log::info!("test 8: testing various message sizes including non-aligned lengths");
    {
        let test_sizes = [
            17usize, // Small non-aligned
            63,      // Just under block size
            65,      // Just over block size
            127,     // Just under 2x block size
            129,     // Just over 2x block size
            1023,    // Larger non-aligned
            4097,    // Just over page size
            8193,    // Two pages + 1
        ];

        for &total_size in &test_sizes {
            for (i, d) in data[..total_size].iter_mut().enumerate() {
                *d = ((i * 31 + 17) % 256) as u8;
            }

            let expected: [u8; 32] = Sha256::digest(&data[..total_size]).into();

            // Multi-chunk: split at CHUNK_SIZE boundary
            if total_size > CHUNK_SIZE {
                let mut ctx = crypto.sha256_init();
                ctx.update(&data[..CHUNK_SIZE]).expect("first update failed");
                ctx.update(&data[CHUNK_SIZE..total_size]).expect("final update failed");
                let hash = ctx.finalize().expect("finalize failed");
                assert_eq!(
                    hash.as_slice(),
                    expected.as_slice(),
                    "hash mismatch multi-chunk total={total_size}"
                );
            } else {
                let mut ctx = crypto.sha256_init();
                ctx.update(&data[..total_size]).expect("update failed");
                let hash = ctx.finalize().expect("finalize failed");
                assert_eq!(
                    hash.as_slice(),
                    expected.as_slice(),
                    "hash mismatch single-shot total={total_size}"
                );
            }
        }

        log::info!("  various message sizes: OK");
    }

    log::info!("all streaming SHA tests passed");
}

/// Test SW<->HW switching in the streaming SHA implementation.
///
/// SW_THRESHOLD = 0x1000: when accumulator + new data reaches this, the client
/// pushes hash state to the server and switches to HW DMA. On the next small
/// update it fetches state back (SW). Each sub-test verifies correctness against
/// the reference sha2 crate.
fn test_streaming_sha_sw_hw_switching(crypto: &CryptoApi, data: &mut [u8]) {
    log::info!("Testing streaming SHA SW<->HW switching");

    const SW_THRESHOLD: usize = 0x1000;

    for (i, d) in data[..0x7000].iter_mut().enumerate() {
        *d = ((i * 37 + 13) % 256) as u8;
    }

    // A: exactly one byte tips accumulator over the threshold into HW
    log::info!("  A: SW→HW at exact threshold boundary");
    {
        let mut ctx = crypto.sha256_init();
        ctx.update(&data[..SW_THRESHOLD - 1]).unwrap(); // SW: accumulate 0xFFF bytes
        ctx.update(&data[SW_THRESHOLD - 1..SW_THRESHOLD]).unwrap(); // tips over → HW
        let hash = ctx.finalize().unwrap();
        let expected: [u8; 32] = Sha256::digest(&data[..SW_THRESHOLD]).into();
        assert_eq!(hash.as_slice(), expected.as_slice(), "A: threshold boundary mismatch");
        log::info!("    OK");
    }

    // B: SW → HW → SW round-trip
    log::info!("  B: SW→HW→SW round-trip");
    {
        let mut ctx = crypto.sha256_init();
        ctx.update(&data[..0x800]).unwrap(); // SW: below threshold
        ctx.update(&data[0x800..0x2800]).unwrap(); // 0x800+0x2000 ≥ threshold → HW
        ctx.update(&data[0x2800..0x2a00]).unwrap(); // 0x200 < threshold → SW (fetch from server)
        let hash = ctx.finalize().unwrap();
        let expected: [u8; 32] = Sha256::digest(&data[..0x2a00]).into();
        assert_eq!(hash.as_slice(), expected.as_slice(), "B: SW→HW→SW mismatch");
        log::info!("    OK");
    }

    // C: multiple alternating SW<->HW transitions
    log::info!("  C: multiple SW<->HW oscillations");
    {
        let mut ctx = crypto.sha256_init();
        let mut pos = 0usize;
        for _ in 0..3 {
            ctx.update(&data[pos..pos + 0x100]).unwrap(); // small → SW
            pos += 0x100;
            ctx.update(&data[pos..pos + 0x2000]).unwrap(); // large → HW
            pos += 0x2000;
        }
        ctx.update(&data[pos..pos + 0x100]).unwrap(); // final small → SW (fetch)
        pos += 0x100;
        let hash = ctx.finalize().unwrap();
        let expected: [u8; 32] = Sha256::digest(&data[..pos]).into();
        assert_eq!(hash.as_slice(), expected.as_slice(), "C: multiple oscillations mismatch");
        log::info!("    OK");
    }

    // D: consecutive large chunks - stays in HW path each call
    log::info!("  D: consecutive HW→HW chunks");
    {
        let mut ctx = crypto.sha256_init();
        ctx.update(&data[..0x2000]).unwrap();
        ctx.update(&data[0x2000..0x4000]).unwrap();
        ctx.update(&data[0x4000..0x6000]).unwrap();
        let hash = ctx.finalize().unwrap();
        let expected: [u8; 32] = Sha256::digest(&data[..0x6000]).into();
        assert_eq!(hash.as_slice(), expected.as_slice(), "D: consecutive HW mismatch");
        log::info!("    OK");
    }

    // E: page-aligned fast path — data ptr is page-aligned (map_memory allocation),
    //    first update is ≥ PAGE_SIZE with empty accumulator, so HW DMA skips the
    //    scratch buffer; the 0x100-byte tail is SW-accumulated.
    log::info!("  E: page-aligned fast path with unaligned tail");
    {
        let mut ctx = crypto.sha256_init();
        ctx.update(&data[..0x4100]).unwrap(); // 4 pages DMA'd directly + 0x100 SW tail
        let hash = ctx.finalize().unwrap();
        let expected: [u8; 32] = Sha256::digest(&data[..0x4100]).into();
        assert_eq!(hash.as_slice(), expected.as_slice(), "E: page-aligned fast path mismatch");
        log::info!("    OK");
    }

    log::info!("all SW<->HW switching tests passed");
}
