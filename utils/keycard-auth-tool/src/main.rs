// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::CStr;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use backup_shard::Shard;
use chrono::{DateTime, Local};
use clap::Parser;
use colored::*;
use hmac::{Hmac, Mac};
use ndef::{Message, Payload, Record, RecordType};
use pcsc::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

#[derive(Parser)]
#[command(name = "keycard-auth-tool")]
#[command(about = "A CLI tool for keycard authentication and provisioning")]
#[command(version = "0.1.0")]
struct Cli {
    /// Path to the configuration file
    #[arg(short = 'c', long = "config")]
    config: PathBuf,

    /// Erase mode: Clear NDEF data from cards instead of provisioning them
    #[arg(short = 'e', long = "erase")]
    erase: bool,

    /// Verify mode: Read and verify card data without writing
    #[arg(short = 'v', long = "verify")]
    verify: bool,

    /// Parse mode: Display parsed Shard fields when verifying
    #[arg(short = 'p', long = "parse")]
    parse: bool,
}

#[derive(Deserialize)]
struct Config {
    #[serde(rename = "keycard-authenticity-secret")]
    keycard_authenticity_secret: String,
}

fn main() {
    let cli = Cli::parse();

    // Load configuration
    let config = match load_config(&cli.config) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("{}", format!("Error loading config: {}", err).red());
            std::process::exit(1);
        }
    };

    // Parse the secret key
    let secret_key = match hex::decode(&config.keycard_authenticity_secret) {
        Ok(key) => {
            if key.len() != 32 {
                eprintln!("{}", "Error: Secret key must be exactly 32 bytes (64 hex characters)".red());
                std::process::exit(1);
            }
            key
        }
        Err(err) => {
            eprintln!("{}", format!("Error parsing secret key: {}", err).red());
            std::process::exit(1);
        }
    };

    if cli.erase {
        println!("{}", "KeyCard Authenticity Tool - ERASE MODE".red().bold());
        println!("{}", "WARNING: This will erase NDEF data from cards!".yellow().bold());
    } else if cli.verify {
        println!("{}", "KeyCard Authenticity Tool - VERIFY MODE".green().bold());
        println!("{}", "This will read and verify card data without writing".cyan());
    } else {
        println!("{}", "KeyCard Authenticity Provisioning Tool".cyan().bold());
    }
    println!("{}", "Initializing NFC reader...".yellow());

    // Initialize PC/SC context and reader once
    let ctx = match Context::establish(Scope::User) {
        Ok(ctx) => ctx,
        Err(err) => {
            eprintln!("{}", format!("Failed to establish PC/SC context: {}", err).red());
            std::process::exit(1);
        }
    };

    // List available readers
    let mut readers_buf = [0; 2048];
    let readers = match ctx.list_readers(&mut readers_buf) {
        Ok(readers) => readers,
        Err(err) => {
            eprintln!("{}", format!("Failed to list readers: {}", err).red());
            std::process::exit(1);
        }
    };

    let reader_names: Vec<_> = readers.collect();
    if reader_names.is_empty() {
        eprintln!("{}", "No NFC readers found. Please connect an ACR122U or compatible reader.".red());
        std::process::exit(1);
    }

    let reader = reader_names[0];
    println!("{}", format!("Using reader: {}", reader.to_string_lossy()).green());
    println!("{}", "Waiting for NFC cards...".yellow());
    println!("{}", "Press Ctrl+C to exit".dimmed());

    // Main loop - continuously monitor for card changes
    let mut last_state = State::UNAWARE;
    let mut loop_counter = 0u32;
    loop {
        match monitor_and_process_card(
            &ctx,
            reader,
            &secret_key,
            &mut last_state,
            cli.erase,
            cli.verify,
            cli.parse,
        ) {
            Ok(()) => {
                // Continue monitoring
            }
            Err(err) => {
                print_error(&format!("Reader error: {}", err));
                // Reset state on persistent errors to prevent getting stuck
                last_state = State::UNAWARE;
                thread::sleep(Duration::from_millis(1000));
            }
        }

        // Periodic state reset to prevent getting stuck (every ~10 seconds when idle)
        loop_counter = loop_counter.wrapping_add(1);
        if loop_counter % 100 == 0 {
            // Debug: Show current state every 10 seconds
            // println!(
            //     "{}",
            //     format!(
            //         "Debug: Current state: {:?}, Loop: {}",
            //         last_state, loop_counter
            //     )
            //     .dimmed()
            // );

            if last_state.contains(State::PRESENT) {
                // Check if card is still actually present
                match ctx.connect(reader, ShareMode::Shared, Protocols::ANY) {
                    Ok(_) => {
                        // println!(
                        //     "{}",
                        //     "Debug: Card still present according to reader".dimmed()
                        // );
                    }
                    Err(_) => {
                        // Card not present but state says it is - reset state
                        // println!(
                        //     "{}",
                        //     "Debug: State mismatch - resetting to UNAWARE".yellow()
                        // );
                        last_state = State::UNAWARE;
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn load_config(path: &PathBuf) -> Result<Config, Box<dyn std::error::Error>> {
    let contents = fs::read_to_string(path)?;
    let config: Config = toml::from_str(&contents)?;
    Ok(config)
}

fn compute_hmac(secret_key: &[u8], uid: &[u8], data: &Shard) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    // Compute SHA256 hash
    let mut hasher = Sha256::new();
    hasher.update(data.hmac_input(uid));
    let hash = hasher.finalize();

    // Compute HMAC
    let mut mac = HmacSha256::new_from_slice(secret_key)?;
    mac.update(&hash);
    let result = mac.finalize();

    let mut hmac_bytes = [0u8; 32];
    hmac_bytes.copy_from_slice(&result.into_bytes());
    Ok(hmac_bytes)
}

fn format_uid(uid: &[u8]) -> String {
    uid.iter().map(|b| format!("0x{:02X}", b)).collect::<Vec<_>>().join(" ")
}

fn print_success(_uid: &[u8]) {
    println!("{}", "Card provisioned OK!".green());
}

fn print_error(message: &str) {
    println!("{}", format!("ERROR: {}", message).red());
}

fn print_shard_details(shard: &Shard) {
    println!("{}", "--- Parsed Shard Fields ---".cyan().bold());

    let version = match &shard.shard {
        backup_shard::ShardVersion::V0(_) => "V0",
        backup_shard::ShardVersion::V1(_) => "V1",
    };
    println!("  Version:              {}", version);
    println!("  device_id:            {}", hex::encode(shard.device_id()));
    println!("  seed_fingerprint:     {}", hex::encode(shard.seed_fingerprint()));
    println!("  seed_shamir_share:    {}", hex::encode(shard.seed_shamir_share()));
    println!("  share_index:          {}", shard.seed_shamir_share_index());
    println!("  part_of_magic_backup: {}", shard.part_of_magic_backup());
    println!("  hmac:                 {}", hex::encode(shard.hmac()));

    if let backup_shard::ShardVersion::V1(_) = &shard.shard {
        let ts = shard.timestamp();
        let formatted = DateTime::from_timestamp(ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "invalid".to_string());
        println!("  timestamp:            {} ({})", ts, formatted);
        println!("  scheme:               {} of {}", shard.scheme_threshold(), shard.scheme_share_count());
    }

    println!("{}", "---------------------------".cyan().bold());
}

fn monitor_and_process_card(
    ctx: &Context,
    reader: &CStr,
    secret_key: &[u8],
    last_state: &mut State,
    erase_mode: bool,
    verify_mode: bool,
    parse_mode: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Use simple connection-based detection for better compatibility
    match ctx.connect(reader, ShareMode::Shared, Protocols::ANY) {
        Ok(card) => {
            // Card is present
            if !last_state.contains(State::PRESENT) {
                let timestamp = Local::now().format("%b %d, %Y: %H:%M:%S");
                println!(
                    "{}",
                    format!(
                        "-- {} Card Detected! ------------------------------------------------------------",
                        timestamp
                    )
                    .cyan()
                );
                *last_state = State::PRESENT;

                let result = if erase_mode {
                    erase_card_direct(&card)
                } else if verify_mode {
                    verify_card_direct(&card, secret_key, parse_mode)
                } else {
                    process_card_direct(&card, secret_key)
                };

                match result {
                    Ok(()) => {
                        // Card processed successfully, wait for it to be removed
                        match wait_for_card_removal_direct(ctx, reader) {
                            Ok(()) => {
                                // Card has been removed, reset state to detect new cards
                                *last_state = State::UNAWARE;
                            }
                            Err(_) => {
                                // Card removal detection failed, force state reset
                                *last_state = State::UNAWARE;
                                thread::sleep(Duration::from_millis(500));
                            }
                        }
                    }
                    Err(err) => {
                        let operation = if erase_mode {
                            "Card erase"
                        } else if verify_mode {
                            "Card verification"
                        } else {
                            "Card processing"
                        };
                        print_error(&format!("{} failed: {}", operation, err));

                        // If we get reader communication errors, wait longer to let reader reset
                        if err.to_string().contains("Invalid response length")
                            || err.to_string().contains("status: 63 00")
                        {
                            println!(
                                "{}",
                                "Reader communication error - waiting for reader to reset...".yellow()
                            );
                            thread::sleep(Duration::from_millis(3000));
                        } else {
                            thread::sleep(Duration::from_millis(1000));
                        }

                        // Even after errors, we should wait for card removal to prevent
                        // immediately detecting the same card as "new"
                        match wait_for_card_removal_direct(ctx, reader) {
                            Ok(()) => {
                                *last_state = State::UNAWARE;
                            }
                            Err(_) => {
                                // If card removal detection fails, force state reset
                                *last_state = State::UNAWARE;
                                thread::sleep(Duration::from_millis(500));
                            }
                        }
                    }
                }
            }
        }
        Err(_) => {
            // No card present
            if last_state.contains(State::PRESENT) {
                println!("{}", "Card removed. Ready for next card.".blue());
                *last_state = State::EMPTY;
            }
        }
    }

    Ok(())
}

fn process_card_direct(card: &Card, secret_key: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    // Read the UID
    let uid = read_uid(card)?;
    println!("{}", format!("UID: {}", format_uid(&uid)).blue());

    // Check if card is already provisioned
    if is_card_provisioned(card, secret_key, &uid)? {
        print_error("Card is already provisioned!");
        return Ok(());
    }

    // Create the keycard data structure
    let mut keycard_data = Shard::default();
    let hmac = compute_hmac(secret_key, &uid, &keycard_data)?;
    keycard_data.set_hmac(hmac);

    // Write NDEF record to the card
    println!("{}", "Writing to card...".blue());
    let cbor_data = write_ndef_record(card, &keycard_data)?;

    // Verify the write was successful
    println!("{}", "Verifying card...".blue());
    let read_back = read_ndef_record(card)?;
    if read_back != cbor_data {
        return Err("Verification failed: written data doesn't match".into());
    }

    // Print success message
    print_success(&uid);

    Ok(())
}

fn verify_card_direct(card: &Card, secret_key: &[u8], parse: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Read the UID
    let uid = read_uid(card)?;
    println!("{}", format!("UID: {}", format_uid(&uid)).blue());

    // Check if card is provisioned and verify the data
    match is_card_provisioned(card, secret_key, &uid)? {
        true => {
            // Read and display the card data for additional info
            match read_ndef_record(card) {
                Ok(data) => {
                    if !data.is_empty() {
                        match Shard::decode(&data) {
                            Ok(keycard_data) => {
                                println!(
                                    "{}",
                                    format!("Data length: {} bytes", keycard_data.encode().len()).blue()
                                );
                                println!(
                                    "{}",
                                    format!("Data: {}", hex::encode(keycard_data.encode())).blue()
                                );
                                println!("{}", format!("HMAC: {}", hex::encode(keycard_data.hmac())).blue());
                                if parse {
                                    print_shard_details(&keycard_data);
                                }
                                println!("{}", "Card verification PASSED!".green().bold());
                            }
                            Err(_) => {
                                println!("{}", "Warning: Could not parse card data structure".yellow());
                                println!("{}", "Card verification PASSED!".green().bold());
                            }
                        }
                    } else {
                        println!("{}", "Card verification PASSED!".green().bold());
                    }
                }
                Err(_) => {
                    println!("{}", "Warning: Could not read card data for display".yellow());
                    println!("{}", "Card verification PASSED!".green().bold());
                }
            }
        }
        false => {
            println!("{}", "Card verification FAILED!".red().bold());

            // Try to read what's on the card for debugging
            match read_ndef_record(card) {
                Ok(data) => {
                    if data.is_empty() {
                        println!("{}", "Card appears to be empty (no NDEF data)".yellow());
                    } else {
                        println!("{}", format!("Card contains {} bytes of data", data.len()).yellow());
                        match Shard::decode(&data) {
                            Ok(keycard_data) => {
                                println!(
                                    "{}",
                                    "Data appears to be in KeyCard format but HMAC verification failed"
                                        .yellow()
                                );
                                let expected_hmac = compute_hmac(secret_key, &uid, &keycard_data)?;
                                println!(
                                    "{}",
                                    format!("Expected HMAC: {}", hex::encode(expected_hmac)).yellow()
                                );
                                println!(
                                    "{}",
                                    format!("Actual HMAC:   {}", hex::encode(keycard_data.hmac())).yellow()
                                );
                                if parse {
                                    print_shard_details(&keycard_data);
                                }
                            }
                            Err(_) => {
                                println!("{}", "Data is not in expected KeyCard format".yellow());
                                println!(
                                    "{}",
                                    format!("Raw data: {}", hex::encode(&data[..data.len().min(32)]))
                                        .dimmed()
                                );
                                if data.len() > 32 {
                                    println!("{}", "... (truncated)".dimmed());
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    println!("{}", format!("Could not read card data: {}", err).red());
                }
            }

            return Err("Card verification failed".into());
        }
    }

    Ok(())
}

const NDEF_FIRST_PAGE: u8 = 4;

const LAST_SECRET_PAGE: u8 = 35;

fn erase_card_direct(card: &Card) -> Result<(), Box<dyn std::error::Error>> {
    // Read the UID for display purposes
    let uid = read_uid(card)?;
    println!("{}", format!("UID: {}", format_uid(&uid)).blue());

    // Check if card has any NDEF data
    match read_ndef_record(card) {
        Ok(data) => {
            if data.is_empty() {
                println!("{}", "Card is already empty (no NDEF data)".yellow());
            } else {
                println!("{}", "Erasing card...".blue());
            }
        }
        Err(_) => {
            println!("{}", "Erasing card...".blue());
        }
    }

    // Write empty NDEF structure to erase the card
    // NTAG 216 empty NDEF: 03 00 FE 00 (NDEF TLV with 0 length + terminator)
    let empty_ndef_page = [0x03, 0x00, 0xFE, 0x00];
    write_page(card, NDEF_FIRST_PAGE, &empty_ndef_page)?;

    let empty_page = [0x00, 0x00, 0x00, 0x00];
    for page in (NDEF_FIRST_PAGE + 1)..=LAST_SECRET_PAGE {
        write_page(card, page, &empty_page)?;
    }

    // Verify the erase was successful
    println!("{}", "Verifying erase...".blue());
    if read_page(card, NDEF_FIRST_PAGE)? != empty_ndef_page {
        return Err("Verification failed: card was not properly erased".into());
    }
    if read_page(card, LAST_SECRET_PAGE)? != empty_page {
        return Err("Verification failed: share pages were not cleared".into());
    }
    print_success_erase(&uid);

    Ok(())
}

fn print_success_erase(_uid: &[u8]) {
    println!("{}", "Card erased OK!".green());
}

fn wait_for_card_removal_direct(ctx: &Context, reader: &CStr) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "Waiting for card to be removed...".bright_black());

    // Simple polling approach with timeout - more reliable with ACR122U
    let start_time = std::time::Instant::now();
    let timeout = Duration::from_secs(30); // 30 second timeout

    loop {
        thread::sleep(Duration::from_millis(300));

        // Check for timeout
        if start_time.elapsed() > timeout {
            println!("{}", "Card removal timeout - assuming card was removed".yellow());
            break;
        }

        // Try to connect to see if card is still there
        match ctx.connect(reader, ShareMode::Shared, Protocols::ANY) {
            Ok(_) => {
                // Card still present, continue waiting
            }
            Err(_) => {
                // Card removed or connection failed
                println!("{}", "Card removed. Ready for next card.".blue());
                break;
            }
        }
    }

    Ok(())
}

fn read_uid(card: &Card) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // NTAG 216 UID is stored in pages 0-1
    // Page 0: UID[0-2], BCC0
    // Page 1: UID[3-6]

    let page0 = read_page(card, 0)?;
    let page1 = read_page(card, 1)?;

    // Extract UID bytes
    let mut uid = Vec::new();
    uid.extend_from_slice(&page0[0..3]); // UID[0-2]
    uid.extend_from_slice(&page1[0..4]); // UID[3-6]

    Ok(uid)
}

fn read_page(card: &Card, page: u8) -> Result<[u8; 4], Box<dyn std::error::Error>> {
    // For ACR122U with NTAG, use direct APDU command
    // APDU: FF CA 00 00 00 (Get UID) or FF B0 [page] 00 04 (Read Binary)
    let command = [
        0xFF, 0xB0, 0x00, page, 0x04, // Read Binary: page as P2, read 4 bytes
    ];

    let mut response = [0; 32];
    let response_data = card.transmit(&command, &mut response)?;

    // Check for successful response (should end with 90 00)
    if response_data.len() < 6 {
        return Err(format!("Invalid response length from card: {} bytes", response_data.len()).into());
    }

    // Check for success status (90 00 at the end)
    let status_len = response_data.len();
    if status_len < 2 || response_data[status_len - 2] != 0x90 || response_data[status_len - 1] != 0x00 {
        return Err(format!(
            "NTAG read command failed with status: {:02X} {:02X}",
            response_data[status_len - 2],
            response_data[status_len - 1]
        )
        .into());
    }

    // Extract the 4-byte page data (everything except the last 2 status bytes)
    if response_data.len() < 6 {
        return Err("Insufficient data in response".into());
    }

    let mut page_data = [0u8; 4];
    page_data.copy_from_slice(&response_data[0..4]);
    Ok(page_data)
}

fn write_page(card: &Card, page: u8, data: &[u8; 4]) -> Result<(), Box<dyn std::error::Error>> {
    // For ACR122U with NTAG, use Update Binary APDU
    // APDU: FF D6 [page] 00 04 [4 bytes data]
    let mut command = [0u8; 9];
    command[0] = 0xFF;
    command[1] = 0xD6; // Update Binary
    command[2] = 0x00;
    command[3] = page; // Page number as P2
    command[4] = 0x04; // Length of data
    command[5..9].copy_from_slice(data);

    let mut response = [0; 16];
    let response_data = card.transmit(&command, &mut response)?;

    // Check for success status (90 00 at the end)
    if response_data.len() < 2 {
        return Err(format!("Invalid write response length: {} bytes", response_data.len()).into());
    }

    let status_len = response_data.len();
    if response_data[status_len - 2] != 0x90 || response_data[status_len - 1] != 0x00 {
        return Err(format!(
            "NTAG write command failed with status: {:02X} {:02X}",
            response_data[status_len - 2],
            response_data[status_len - 1]
        )
        .into());
    }

    Ok(())
}

fn is_card_provisioned(
    card: &Card,
    secret_key: &[u8],
    uid: &[u8],
) -> Result<bool, Box<dyn std::error::Error>> {
    // Try to read existing NDEF data
    match read_ndef_record(card) {
        Ok(data) => {
            if data.is_empty() {
                return Ok(false);
            }

            // Try to parse as our CBOR structure
            match Shard::decode(&data) {
                Ok(keycard_data) => {
                    // Verify the HMAC matches what we would compute for the data
                    let expected_hmac = compute_hmac(secret_key, uid, &keycard_data)?;
                    let is_valid = keycard_data.hmac() == &expected_hmac;
                    Ok(is_valid)
                }
                Err(_) => {
                    // Not our format, consider it unprovisioned
                    Ok(false)
                }
            }
        }
        Err(_) => {
            // No NDEF data or read error, consider it unprovisioned
            Ok(false)
        }
    }
}

fn write_ndef_record(card: &Card, keycard_data: &Shard) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut ndef_msg = Message::default();
    let encoded_data = keycard_data.encode();
    let ndef_rec1 = Record::new(None, Payload::RTD(RecordType::Cbor(encoded_data)));
    ndef_msg.append_record(ndef_rec1);
    let raw_msg = ndef_msg.to_vec().map_err(|_| "Invalid NDEF message")?;

    if raw_msg.len() > 888 {
        return Err("Data too large for NTAG 216".into());
    }

    // Write NDEF to card starting at page 4
    write_ndef_to_pages(card, &raw_msg)?;

    Ok(ndef_msg.records[0].payload().map_err(|_| "Invalid NDEF message")?)
}

fn ndef_tlv(ndef_data: &[u8]) -> Vec<u8> {
    let mut all_data = Vec::new();

    // NDEF TLV: Type=0x03, Length, Value
    all_data.push(0x03); // NDEF Message TLV

    if ndef_data.len() < 255 {
        all_data.push(ndef_data.len() as u8);
    } else {
        all_data.push(0xFF);
        all_data.push((ndef_data.len() >> 8) as u8);
        all_data.push((ndef_data.len() & 0xFF) as u8);
    }

    all_data.extend_from_slice(ndef_data);
    all_data.push(0xFE); // Terminator TLV

    // Pad to 4-byte boundary
    while all_data.len() % 4 != 0 {
        all_data.push(0x00);
    }

    all_data
}

fn write_ndef_to_pages(card: &Card, ndef_data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    // NTAG 216 NDEF structure:
    // Page 4+: NDEF message
    let all_data = ndef_tlv(ndef_data);

    // Write data to pages starting from page 4
    let mut page = NDEF_FIRST_PAGE;
    for chunk in all_data.chunks(4) {
        let mut page_data = [0u8; 4];
        page_data[..chunk.len()].copy_from_slice(chunk);
        write_page(card, page, &page_data)?;
        page += 1;

        if page > 225 {
            // NTAG 216 has pages 0-225
            return Err("Data too large for NTAG 216".into());
        }
    }

    Ok(())
}

fn read_ndef_record(card: &Card) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Read page 4 to get NDEF TLV header
    let page4 = read_page(card, 4)?;

    if page4[0] != 0x03 {
        return Err("No NDEF message found".into());
    }

    // Parse length
    let (ndef_length, data_start_offset) = if page4[1] != 0xFF {
        (page4[1] as usize, 2)
    } else {
        if page4.len() < 4 {
            return Err("Invalid NDEF length format".into());
        }
        let length = ((page4[2] as usize) << 8) | (page4[3] as usize);
        (length, 4)
    };

    // Calculate how many pages we need to read
    let total_bytes_needed = data_start_offset + ndef_length;
    let pages_needed = total_bytes_needed.div_ceil(4); // Round up

    // Read all necessary pages
    let mut all_data = Vec::new();
    for page in 4..(4 + pages_needed as u8) {
        let page_data = read_page(card, page)?;
        all_data.extend_from_slice(&page_data);
    }

    // Extract just the NDEF message data
    if all_data.len() < data_start_offset + ndef_length {
        return Err("Insufficient data read from card".into());
    }

    let ndef_message = all_data[data_start_offset..data_start_offset + ndef_length].to_vec();

    let ndef_msg = Message::try_from(ndef_message.as_slice()).map_err(|_| "Invalid NDEF message")?;

    if ndef_msg.records.len() != 1 {
        // Empty NDEF message means no data, return empty vector
        return Ok(Vec::new());
    }

    if !ndef_msg.records[0].is_type_cbor() {
        return Err("Not an external type record".into());
    }

    Ok(ndef_msg.records[0].payload().map_err(|_| "Invalid NDEF message")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hmac_computation() {
        let secret_key =
            hex::decode("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap();
        let uid = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x12, 0x34, 0x56];

        let hmac = compute_hmac(&secret_key, &uid, &Shard::default()).unwrap();

        // Verify HMAC is 32 bytes
        assert_eq!(hmac.len(), 32);

        // Verify HMAC is deterministic
        let hmac2 = compute_hmac(&secret_key, &uid, &Shard::default()).unwrap();
        assert_eq!(hmac, hmac2);

        // Verify different data produces different HMAC
        let different_shard = Shard::new(
            [1; 32],       // different device_id
            [2; 32],       // different seed_fingerprint
            vec![3, 4, 5], // different seed_shamir_share
            1,             // different share index
            false,         // different part_of_magic_backup
        );
        let hmac3 = compute_hmac(&secret_key, &uid, &different_shard).unwrap();
        assert_ne!(hmac, hmac3);
    }

    #[test]
    fn erase_clears_every_share_page() {
        for share_len in [16usize, 32] {
            let marker = vec![0xAB; share_len];
            let mut shard = Shard::new([0x22; 32], [0x33; 32], marker.clone(), 1, true);
            shard.set_hmac([0x11; 32]);

            let mut ndef_msg = Message::default();
            let record = Record::new(None, Payload::RTD(RecordType::Cbor(shard.encode())));
            ndef_msg.append_record(record);
            let tlv = ndef_tlv(&ndef_msg.to_vec().unwrap());

            let start =
                tlv.windows(share_len).position(|window| window == marker).expect("share is in the record");
            let last_page = usize::from(NDEF_FIRST_PAGE) + (start + share_len - 1) / 4;
            assert!(
                last_page <= usize::from(LAST_SECRET_PAGE),
                "a {share_len}-byte share reaches page {last_page}, past the erased range"
            );
        }
    }

    #[test]
    fn test_format_uid() {
        let uid = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x12, 0x34, 0x56];
        let formatted = format_uid(&uid);
        assert_eq!(formatted, "0xDE 0xAD 0xBE 0xEF 0x12 0x34 0x56");
    }

    #[test]
    fn test_keycard_data_serialization() {
        let keycard_data = Shard::default();

        let mut ndef_msg = Message::default();
        let encoded_data = keycard_data.encode();
        let ndef_rec1 = Record::new(None, Payload::RTD(RecordType::Cbor(encoded_data)));
        ndef_msg.append_record(ndef_rec1);
        let serialized = ndef_msg.to_vec().unwrap();

        let ndef_msg = Message::try_from(serialized.as_slice()).unwrap();
        assert_eq!(ndef_msg.records.len(), 1);
        assert!(ndef_msg.records[0].is_type_cbor());
        let payload = ndef_msg.records[0].payload().unwrap();
        let deserialized = Shard::decode(&payload).unwrap();

        assert_eq!(keycard_data.hmac(), deserialized.hmac());
        assert_eq!(keycard_data.encode(), deserialized.encode());
    }
}
