// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{
    fs::{create_dir_all, read_dir, File, OpenOptions},
    sync::mpsc,
};

use gui_server_api::consts::{DEVICE_HEIGHT, DEVICE_WIDTH, SCREEN_HEIGHT, SCREEN_WIDTH};
use image::{
    codecs::{gif::GifEncoder, png::PngEncoder},
    ExtendedColorType, ImageEncoder, RgbaImage,
};

use crate::{gui_permissions::GuiPermissions, MainWindow, GIF_DELAY_MS, SCREENSHOTS_DIR};

pub fn setup(window: &MainWindow) {
    window.on_screenshot(|grab_entire_device| {
        screenshot(grab_entire_device).unwrap_or_else(|error| {
            log::warn!("Failed to take screenshot: {}", error);
        })
    });

    let recording = match recording() {
        Ok(recording) => recording,
        Err(e) => {
            log::warn!("Failed to set up screen recording system: {}", e);
            return;
        }
    };

    let recording_copy = recording.clone();

    window.on_record(move |grab_entire_device| {
        recording.send(RecordingMessage::Start(grab_entire_device)).unwrap_or_else(|error| {
            log::warn!("Failed to start recording: {}", error);
        });
    });

    window.on_stop(move || {
        recording_copy.send(RecordingMessage::Stop).unwrap_or_else(|error| {
            log::warn!("Failed to stop recording: {}", error);
        });
    });
}

pub fn screenshot(grab_entire_device: bool) -> anyhow::Result<()> {
    create_dir_all(format!("../../{SCREENSHOTS_DIR}"))?;

    let file_name =
        &numbered_file(if grab_entire_device { "device_screenshot_" } else { "screenshot_" }, ".png")?;

    let (width, height) = get_frame_dimensions(grab_entire_device);

    let frame = if grab_entire_device {
        gui_server_api::simulator::SimulatorApi::<GuiPermissions>::default().device_frame()?
    } else {
        gui_server_api::GuiApiLight::<GuiPermissions>::default().capture_screen()?.as_slice().to_vec()
    };

    let Some(screen) = RgbaImage::from_vec(width as u32, height as u32, frame) else {
        anyhow::bail!("Could not convert screenshot to RgbaImage")
    };
    let file = File::create(format!("../../{SCREENSHOTS_DIR}/{file_name}"))?;

    PngEncoder::new(file).write_image(&screen, screen.width(), screen.height(), ExtendedColorType::Rgba8)?;

    log::info!("Screenshot saved to {SCREENSHOTS_DIR}/{file_name}");

    Ok(())
}

pub enum RecordingMessage {
    Start(bool),
    Stop,
}

pub fn recording() -> anyhow::Result<mpsc::Sender<RecordingMessage>> {
    let (sender, receiver) = mpsc::channel();

    create_dir_all(format!("../../{SCREENSHOTS_DIR}"))?;

    std::thread::spawn(move || {
        let mut encoder: std::option::Option<GifEncoder<File>> = None;
        let mut file_name = String::new();
        let mut grab_entire_device = false;
        loop {
            match receiver.try_recv() {
                Ok(RecordingMessage::Start(entire_device)) => {
                    grab_entire_device = entire_device;
                    (encoder, file_name) = match start_recording(grab_entire_device) {
                        Ok((encoder, file_name)) => (encoder, file_name),
                        Err(e) => {
                            log::warn!("Could not start recording: {}", e);
                            continue;
                        }
                    };
                }
                Ok(RecordingMessage::Stop) => {
                    encoder = None;
                    log::info!("Recording saved to {SCREENSHOTS_DIR}/{file_name}");
                }
                Err(_) => {}
            }
            if let Some(ref mut encoder) = encoder {
                record_frame(encoder, grab_entire_device).unwrap_or_else(|error| {
                    log::warn!("Could not record frame: {}", error);
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(GIF_DELAY_MS as u64));
        }
    });
    Ok(sender)
}

fn start_recording(grab_entire_device: bool) -> anyhow::Result<(Option<GifEncoder<File>>, String)> {
    let file_name =
        numbered_file(if grab_entire_device { "device_recording_" } else { "screen_recording_" }, ".gif")?;

    create_dir_all(format!("../../{SCREENSHOTS_DIR}"))?;

    let file =
        OpenOptions::new().append(true).create(true).open(format!("../../{SCREENSHOTS_DIR}/{file_name}"))?;

    let mut encoder = GifEncoder::new_with_speed(file, 30);
    encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;

    log::info!("Recording to {SCREENSHOTS_DIR}/{file_name}");
    Ok((Some(encoder), file_name))
}

fn record_frame(encoder: &mut GifEncoder<File>, grab_entire_device: bool) -> anyhow::Result<()> {
    let (width, height) = get_frame_dimensions(grab_entire_device);

    let frame = if grab_entire_device {
        gui_server_api::simulator::SimulatorApi::<GuiPermissions>::default().device_frame()?
    } else {
        gui_server_api::GuiApiLight::<GuiPermissions>::default().capture_screen()?.as_slice().to_vec()
    };

    let Some(screen) = RgbaImage::from_vec(width, height, frame) else {
        anyhow::bail!("Could not convert recording screenshot to RgbaImage")
    };

    let delay = image::Delay::from_numer_denom_ms(GIF_DELAY_MS, 1);
    let gif_frame = image::Frame::from_parts(screen, 0, 0, delay);

    encoder.encode_frame(gif_frame)?;

    Ok(())
}

fn numbered_file(prefix: &str, ext: &str) -> anyhow::Result<String> {
    let dir_files = read_dir(format!("../../{SCREENSHOTS_DIR}"))?;

    let mut screenshot_numbers: Vec<u32> = dir_files
        .filter_map(|entry| {
            let entry = entry.ok()?;

            if entry.file_type().ok()?.is_file() {
                entry.file_name().to_str()?.strip_prefix(prefix)?.strip_suffix(ext)?.parse().ok()
            } else {
                None
            }
        })
        .collect();

    screenshot_numbers.sort();
    let file_name = screenshot_numbers
        .last()
        .map(|n| format!("{prefix}{}{ext}", n + 1))
        .unwrap_or_else(|| format!("{prefix}1{ext}"));

    Ok(file_name)
}

fn get_frame_dimensions(grab_entire_device: bool) -> (u32, u32) {
    if grab_entire_device {
        (DEVICE_WIDTH as u32, DEVICE_HEIGHT as u32)
    } else {
        (SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32)
    }
}
