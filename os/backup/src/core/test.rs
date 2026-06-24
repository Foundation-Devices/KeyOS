// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{Read, Write};
use std::time::SystemTime;

use backup::DO_NOT_BACKUP_FOLDER;
use fs::adapter::test_utils::FsTest;
use fs::OpenFlags;

use super::*;

struct TestCrypto {
    key: BackupKey,
}

impl v1::CryptoAdapter for TestCrypto {
    fn new(key: &BackupKey) -> Result<Self, std::io::Error> { Ok(Self { key: key.clone() }) }

    fn encrypt_chunk(&mut self, chunk: &mut [u8]) -> Result<(), std::io::Error> {
        for block in chunk.chunks_exact_mut(crypto::AES_BLOCK_SIZE) {
            for (i, b) in block.iter_mut().enumerate() {
                *b ^= self.key.as_bytes()[i];
            }
        }
        Ok(())
    }

    fn decrypt_chunk(&mut self, chunk: &mut [u8]) -> Result<(), std::io::Error> {
        for block in chunk.chunks_exact_mut(crypto::AES_BLOCK_SIZE) {
            for (i, b) in block.iter_mut().enumerate() {
                *b ^= self.key.as_bytes()[i];
            }
        }
        Ok(())
    }
}

impl v2::CryptoAdapter for TestCrypto {
    fn new(key: &BackupKey) -> Result<Self, std::io::Error> { Ok(Self { key: key.clone() }) }

    fn encrypt_chunk(
        &mut self,
        iv: [u8; 12],
        aad: &[u8],
        chunk: &mut [u8],
        tag: &mut [u8; 16],
    ) -> Result<(), std::io::Error> {
        for (i, b) in chunk.iter_mut().enumerate() {
            *b ^= self.key.as_bytes()[i % self.key.as_bytes().len()];
        }
        *tag = fake_tag(&iv, aad, chunk);
        Ok(())
    }

    fn decrypt_chunk(
        &mut self,
        iv: [u8; 12],
        aad: &[u8],
        chunk: &mut [u8],
        expected_tag: &[u8; 16],
    ) -> Result<(), std::io::Error> {
        if fake_tag(&iv, aad, chunk) != *expected_tag {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "tag mismatch"));
        }
        for (i, b) in chunk.iter_mut().enumerate() {
            *b ^= self.key.as_bytes()[i % self.key.as_bytes().len()];
        }
        Ok(())
    }
}

fn fake_tag(iv: &[u8; 12], aad: &[u8], data: &[u8]) -> [u8; 16] {
    let mut tag = [0u8; 16];
    for (i, b) in iv.iter().enumerate() {
        tag[i % 16] ^= *b;
    }
    for (i, b) in aad.iter().enumerate() {
        tag[i % 16] ^= b.wrapping_add(0x11);
    }
    for (i, b) in data.iter().enumerate() {
        tag[i % 16] ^= b.wrapping_add(0x22);
    }
    tag
}

struct TarEntry {
    path: String,
    mode: u32,
    contents: Vec<u8>,
}

fn tar_entries(fs: &FsTest, encrypted_path: &str, backup_key: &BackupKey) -> Vec<TarEntry> {
    let mut backup_file =
        fs.open_file(encrypted_path, fs::Location::EncryptedRoot, OpenFlags::READ_ONLY).unwrap();
    let reader = v2::DecryptingReader::<_, TestCrypto>::new(&mut backup_file, backup_key).unwrap();
    let mut archive = tar::Archive::new(reader);
    let mut result = Vec::new();

    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_str().unwrap().to_string();
        let mode = entry.header().mode().unwrap() & 0o777;
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents).unwrap();

        result.push(TarEntry { path, mode, contents });
    }

    result
}

fn init() { simple_logger::SimpleLogger::new().with_level(log::LevelFilter::Debug).env().init().ok(); }

fn v2_restore_rejects_backup(mutate: impl FnOnce(&mut Vec<u8>)) {
    init();
    let fs = FsTest::default();
    let test_key = BackupKey::from_app_seed([0x42; 32]);

    let data: Vec<u8> = (0..150_000u32).map(|i| i as u8).collect();
    fs.write_file("appdata/app1/data.bin", &data, fs::Location::EncryptedRoot);
    let backup_file = create_backup::<_, TestCrypto>(
        &fs,
        "backup/backup.tar",
        fs::Location::EncryptedRoot,
        &test_key,
        None,
    )
    .unwrap();

    let mut encrypted = fs.read_file_contents(&backup_file.path, fs::Location::EncryptedRoot).unwrap();
    mutate(&mut encrypted);
    fs.write_file(&backup_file.path, &encrypted, fs::Location::EncryptedRoot);

    fs.write_file("appdata/app1/data.bin", b"current", fs::Location::EncryptedRoot);
    let result = restore_backup::<_, TestCrypto, TestCrypto>(
        &fs,
        &backup_file.path,
        fs::Location::EncryptedRoot,
        &test_key,
    );

    assert!(result.is_err());
    assert_eq!(
        fs.read_file_contents("appdata/app1/data.bin", fs::Location::EncryptedRoot).unwrap(),
        b"current"
    );
}

const DISK_CHUNK_SIZE: usize = v2::ChunkHeader::SIZE + CHUNK_SIZE + v2::TAG_SIZE;
const FIRST_CHUNK_OFFSET: usize = v2::PREFIX.len() + v2::BASE_NONCE_SIZE;

#[test]
fn single_app_backup() {
    init();
    let fs = FsTest::default();

    let backup_path = "backup/backup.tar";

    let test_file_path = "appdata/app1/data.txt";
    let test_content = b"hello world";

    fs.write_file(test_file_path, test_content, fs::Location::EncryptedRoot);

    let test_key = BackupKey::from_app_seed([0x42; 32]);
    let backup_file =
        create_backup::<_, TestCrypto>(&fs, backup_path, fs::Location::EncryptedRoot, &test_key, None)
            .unwrap();
    let encrypted = fs.read_file_contents(&backup_file.path, fs::Location::EncryptedRoot).unwrap();
    assert!(encrypted.starts_with(v2::PREFIX));

    let entries = tar_entries(&fs, &backup_file.path, &test_key);

    assert_eq!(entries.len(), 2);

    assert_eq!(entries[0].path, METADATA_FILE);
    let metadata: BackupMetadata = serde_json::from_slice(&entries[0].contents).unwrap();
    assert_eq!(metadata.created_at, backup_file.created_at);
    assert_eq!(metadata.device_name, None);

    let file = &entries[1];
    assert_eq!(file.path, test_file_path);
    assert_eq!(file.mode, 0o644);
    assert_eq!(file.contents, test_content);
}

#[test]
fn device_name_in_metadata() {
    init();
    let fs = FsTest::default();

    fs.write_file("appdata/app1/data.txt", b"x", fs::Location::EncryptedRoot);
    let test_key = BackupKey::from_app_seed([0x42; 32]);
    let backup_file = create_backup::<_, TestCrypto>(
        &fs,
        "backup/backup.tar",
        fs::Location::EncryptedRoot,
        &test_key,
        Some("My Prime".to_string()),
    )
    .unwrap();

    let entries = tar_entries(&fs, &backup_file.path, &test_key);
    let metadata: BackupMetadata = serde_json::from_slice(&entries[0].contents).unwrap();
    assert_eq!(metadata.device_name.as_deref(), Some("My Prime"));

    let legacy: BackupMetadata =
        serde_json::from_str(r#"{"created_at":{"secs_since_epoch":0,"nanos_since_epoch":0}}"#).unwrap();
    assert_eq!(legacy.device_name, None);
}

#[test]
fn backup_and_restore() {
    init();
    let fs = FsTest::default();

    let backup_path = "backup/backup.tar";

    let small_bin_path = "appdata/wallet/small.bin";
    let medium_bin_path = "appdata/wallet/medium.bin";
    let large_bin_path = "appdata/photos/vacation.jpg";
    let config_path = "appdata/settings/config.json";
    let nested_path = "appdata/photos/deep/nested/image.png";
    let new_app_path = "appdata/new_app/data.txt";

    let small_binary: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
    fs.write_file(small_bin_path, &small_binary, fs::Location::EncryptedRoot);

    let medium_binary: Vec<u8> = (0..100_000).map(|i| ((i * 7) % 256) as u8).collect();
    fs.write_file(medium_bin_path, &medium_binary, fs::Location::EncryptedRoot);

    let large_binary: Vec<u8> = (0..2_000_000).map(|i| ((i * 13 + 42) % 256) as u8).collect();
    fs.write_file(large_bin_path, &large_binary, fs::Location::EncryptedRoot);

    let text_data = b"some text configuration data";
    fs.write_file(config_path, text_data, fs::Location::EncryptedRoot);

    let nested_binary: Vec<u8> = (0..50_000).map(|i| ((i * 3) % 256) as u8).collect();
    fs.write_file(nested_path, &nested_binary, fs::Location::EncryptedRoot);

    let original_mtime = {
        let original_file =
            fs.open_file(small_bin_path, fs::Location::EncryptedRoot, OpenFlags::READ_ONLY).unwrap();

        let original_metadata = original_file.metadata().unwrap();
        original_metadata.modified
    };

    let test_key = BackupKey::from_app_seed([0x42; 32]);
    let backup_file =
        create_backup::<_, TestCrypto>(&fs, backup_path, fs::Location::EncryptedRoot, &test_key, None)
            .unwrap();

    fs.write_file(small_bin_path, b"corrupted", fs::Location::EncryptedRoot);
    fs.write_file(large_bin_path, b"corrupted", fs::Location::EncryptedRoot);
    fs.write_file(new_app_path, b"should not exist after restore", fs::Location::EncryptedRoot);

    let metadata = restore_backup::<_, TestCrypto, TestCrypto>(
        &fs,
        &backup_file.path,
        fs::Location::EncryptedRoot,
        &test_key,
    )
    .unwrap();

    assert_eq!(metadata.created_at, backup_file.created_at, "backup metadata created_at mismatch");

    assert_eq!(fs.read_file_contents(small_bin_path, fs::Location::EncryptedRoot).unwrap(), small_binary);
    assert_eq!(fs.read_file_contents(medium_bin_path, fs::Location::EncryptedRoot).unwrap(), medium_binary);
    assert_eq!(fs.read_file_contents(large_bin_path, fs::Location::EncryptedRoot).unwrap(), large_binary);
    assert_eq!(fs.read_file_contents(config_path, fs::Location::EncryptedRoot).unwrap(), text_data);
    assert_eq!(fs.read_file_contents(nested_path, fs::Location::EncryptedRoot).unwrap(), nested_binary);

    let result = fs.open_file(new_app_path, fs::Location::EncryptedRoot, OpenFlags::READ_ONLY);
    assert!(result.is_err(), "new_app should not exist after restore");

    let restored_file =
        fs.open_file(small_bin_path, fs::Location::EncryptedRoot, OpenFlags::READ_ONLY).unwrap();
    let restored_metadata = restored_file.metadata().unwrap();
    let restored_mtime = restored_metadata.modified;
    drop(restored_file);

    let original_ts = datetime_to_timestamp(&original_mtime);
    let restored_ts = datetime_to_timestamp(&restored_mtime);
    let ts_diff = restored_ts.abs_diff(original_ts);
    assert!(ts_diff <= 2, "mtime mismatch: restored={restored_ts}, original={original_ts}, diff={ts_diff}s");
}

#[test]
fn restore_v1_backup() {
    init();
    let fs = FsTest::default();
    let test_key = BackupKey::from_app_seed([0x42; 32]);
    let created_at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1234);

    fs.write_file("appdata/app1/data.txt", b"legacy restored", fs::Location::EncryptedRoot);
    let backup_file = v1::create_backup::<_, TestCrypto>(
        &fs,
        "backup/v1.tar",
        fs::Location::EncryptedRoot,
        &test_key,
        created_at,
    )
    .unwrap();
    fs.write_file("appdata/app1/data.txt", b"old", fs::Location::EncryptedRoot);

    let encrypted = fs.read_file_contents(&backup_file.path, fs::Location::EncryptedRoot).unwrap();
    assert!(!encrypted.starts_with(v2::PREFIX));

    let metadata =
        restore_backup::<_, TestCrypto, TestCrypto>(&fs, &backup_file.path, backup_file.location, &test_key)
            .unwrap();

    assert_eq!(metadata.created_at, created_at);
    assert_eq!(
        fs.read_file_contents("appdata/app1/data.txt", fs::Location::EncryptedRoot).unwrap(),
        b"legacy restored"
    );
}

#[test]
fn restore_v1_backup_with_short_reads() {
    struct ShortReader<'a>(&'a [u8]);

    impl Read for ShortReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = 3.min(buf.len()).min(self.0.len());
            let (chunk, rest) = self.0.split_at(n);
            buf[..n].copy_from_slice(chunk);
            self.0 = rest;
            Ok(n)
        }
    }

    init();
    let fs = FsTest::default();
    let test_key = BackupKey::from_app_seed([0x42; 32]);
    let created_at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1234);

    fs.write_file("appdata/app1/data.txt", b"legacy restored", fs::Location::EncryptedRoot);
    let backup_file = v1::create_backup::<_, TestCrypto>(
        &fs,
        "backup/v1.tar",
        fs::Location::EncryptedRoot,
        &test_key,
        created_at,
    )
    .unwrap();
    fs.write_file("appdata/app1/data.txt", b"old", fs::Location::EncryptedRoot);

    let encrypted = fs.read_file_contents(&backup_file.path, fs::Location::EncryptedRoot).unwrap();
    let reader = ShortReader(&encrypted);
    let metadata = v1::restore_backup::<_, _, TestCrypto>(&fs, reader, &test_key, encrypted.len()).unwrap();

    assert_eq!(metadata.created_at, created_at);
    assert_eq!(
        fs.read_file_contents("appdata-restore-temp/appdata/app1/data.txt", fs::Location::EncryptedRoot)
            .unwrap(),
        b"legacy restored"
    );
}

#[test]
fn v2_rejects_corrupt_tag() {
    v2_restore_rejects_backup(|encrypted| {
        *encrypted.last_mut().unwrap() ^= 0xff;
    });
}

#[test]
fn v2_rejects_missing_eof() {
    v2_restore_rejects_backup(|encrypted| {
        encrypted.truncate(encrypted.len() - (v2::ChunkHeader::SIZE + v2::TAG_SIZE));
    });
}

#[test]
fn v2_rejects_spliced_eof() {
    v2_restore_rejects_backup(|encrypted| {
        let eof = encrypted[encrypted.len() - (v2::ChunkHeader::SIZE + v2::TAG_SIZE)..].to_vec();
        encrypted.truncate(FIRST_CHUNK_OFFSET + DISK_CHUNK_SIZE);
        encrypted.extend_from_slice(&eof);
    });
}

#[test]
fn v2_rejects_reordered_chunks() {
    v2_restore_rejects_backup(|encrypted| {
        let a = FIRST_CHUNK_OFFSET;
        let b = FIRST_CHUNK_OFFSET + DISK_CHUNK_SIZE;
        let chunk0 = encrypted[a..a + DISK_CHUNK_SIZE].to_vec();
        let chunk1 = encrypted[b..b + DISK_CHUNK_SIZE].to_vec();
        encrypted[a..a + DISK_CHUNK_SIZE].copy_from_slice(&chunk1);
        encrypted[b..b + DISK_CHUNK_SIZE].copy_from_slice(&chunk0);
    });
}

#[test]
fn do_not_backup_folder_omitted() {
    init();
    let fs = FsTest::default();

    let backup_path = "backup/backup.tar";

    let app1_data_path = "appdata/app1/data.txt";
    let app2_config_path = "appdata/app2/config.json";
    let cache_path = format!("appdata/app2/{DO_NOT_BACKUP_FOLDER}/cache.tmp");
    let log_path = format!("appdata/app1/{DO_NOT_BACKUP_FOLDER}/temp.log");
    let nested_path = format!("appdata/app1/some_folder/{DO_NOT_BACKUP_FOLDER}/secret.txt");

    fs.write_file(app1_data_path, b"important data", fs::Location::EncryptedRoot);
    fs.write_file(app2_config_path, b"config", fs::Location::EncryptedRoot);

    fs.write_file(&cache_path, b"cache data", fs::Location::EncryptedRoot);
    fs.write_file(&log_path, b"log data", fs::Location::EncryptedRoot);
    fs.write_file(&nested_path, b"nested secret", fs::Location::EncryptedRoot);

    let test_key = BackupKey::from_app_seed([0x42; 32]);
    let backup_file =
        create_backup::<_, TestCrypto>(&fs, backup_path, fs::Location::EncryptedRoot, &test_key, None)
            .unwrap();

    let entries = tar_entries(&fs, &backup_file.path, &test_key);

    assert_eq!(
        entries.len(),
        3,
        "Expected 3 files in backup, got {}: {:?}",
        entries.len(),
        entries.iter().map(|e| &e.path).collect::<Vec<_>>()
    );

    let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
    assert!(paths.contains(&METADATA_FILE.to_string()));
    assert!(paths.contains(&app1_data_path.to_string()));
    assert!(paths.contains(&app2_config_path.to_string()));

    assert!(
        !paths.iter().any(|p| p.contains(DO_NOT_BACKUP_FOLDER)),
        "do_not_backup files should not be in backup, but found: {:?}",
        paths
    );
}

#[test]
fn chunk_size_aligned_padding() {
    init();

    let mut data = vec![0xAA; CHUNK_SIZE];
    data[CHUNK_SIZE - 1] = 0x10;

    let test_key = BackupKey::from_app_seed([0x42; 32]);
    let mut encrypted = Vec::new();
    {
        let mut writer = v2::EncryptingWriter::<_, TestCrypto>::new(&mut encrypted, &test_key).unwrap();
        writer.write_all(&data).unwrap();
        writer.finish().unwrap();
    }

    let mut decrypted = Vec::new();
    {
        let mut reader = v2::DecryptingReader::<_, TestCrypto>::new(&encrypted[..], &test_key).unwrap();
        reader.read_to_end(&mut decrypted).unwrap();
    }

    assert_eq!(decrypted.len(), data.len());
    assert_eq!(decrypted, data);
}

#[test]
fn partial_final_frame_round_trips() {
    init();

    let data = b"not block aligned".to_vec();
    let test_key = BackupKey::from_app_seed([0x42; 32]);
    let mut encrypted = Vec::new();
    {
        let mut writer = v2::EncryptingWriter::<_, TestCrypto>::new(&mut encrypted, &test_key).unwrap();
        writer.write_all(&data).unwrap();
        writer.finish().unwrap();
    }

    let mut decrypted = Vec::new();
    {
        let mut reader = v2::DecryptingReader::<_, TestCrypto>::new(&encrypted[..], &test_key).unwrap();
        reader.read_to_end(&mut decrypted).unwrap();
    }

    assert_eq!(decrypted, data);
}
