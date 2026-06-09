// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    slint::{Image, ModelRc, SharedPixelBuffer, SharedString, VecModel},
    slint_keyos_platform::{
        app,
        gui_server_api::navigation::filepicker::{
            AllowedExtensions, AllowedLocations, Location, SelectFileOptions,
        },
        navigation::select_file,
    },
};

use crate::gui_permissions::GuiPermissions;

/// The locations available for file selection
const LOCATIONS: &[Location] = &[Location::Internal, Location::External];
const ALLOWED_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp"];

app!("Image Viewer");

fn app_main(_cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    ui.global::<Callbacks>().on_load_image({
        let weak_ui = ui.as_weak();
        move |filename| {
            let ui = weak_ui.unwrap();
            match load_image(&filename) {
                Some(img) => img,
                None => ui.global::<Images>().invoke_icon("remove".into(), 32.0),
            }
        }
    });

    ui.global::<Callbacks>().on_popup_select_file({
        let weak_ui = ui.as_weak();
        move || {
            let ui = weak_ui.unwrap();
            if let Some((paths, index)) = file_selection_popup() {
                ui.global::<Global>().set_file_paths(ModelRc::new(
                    paths.into_iter().map(|s| s.into()).collect::<VecModel<SharedString>>(),
                ));
                ui.global::<Global>().set_current_file_idx(index as _);
            } else {
                log::info!("Popup dismissed");
                ui.global::<Global>().set_current_file_idx(0);
                ui.global::<Global>().set_file_paths([].into());
            }
        }
    });

    ui.run().expect("UI running");
}

fn file_selection_popup() -> Option<(Vec<String>, usize)> {
    log::debug!("Opening file selection popup");
    let options = SelectFileOptions::default()
        .with_hidden_allowed(false)
        .with_search_allowed(false)
        .with_start_location(LOCATIONS[0])
        .with_allowed_locations(AllowedLocations::specific(LOCATIONS.to_owned()))
        .with_allowed_extensions(AllowedExtensions::specific::<&str, Vec<&str>>(
            ALLOWED_EXTENSIONS.iter().copied().collect(),
        ));

    let (file_path, location) =
        select_file::<GuiPermissions>(options).expect("select file navigation")?.files().get(0).cloned()?;
    log::debug!("File selection popup result: {file_path:?} at {location:?}");

    let (location, location_prefix) = match location {
        Location::Internal => (fs::Location::User, "user"),
        Location::Airlock => (fs::Location::Airlock, "airlock"),
        Location::External => (fs::Location::Usb, "usb"),
    };

    let (dir_path, filename) = file_path.rsplit_once("/")?;

    let fs = FileSystem::default();
    let dir = fs.open_dir(dir_path, location).ok()?;
    let mut entries = vec![];
    let mut selected_index = 0;
    while let Some(entry) = dir.next_entry().ok()? {
        let name = entry.name.as_str();
        if !entry.is_file || name.starts_with('.') {
            continue;
        }
        let Some((_, extension)) = name.rsplit_once('.') else { continue };
        if !ALLOWED_EXTENSIONS.contains(&extension) {
            continue;
        }
        if name == filename {
            selected_index = entries.len();
        }
        entries.push(format!("{location_prefix}:/{dir_path}/{name}"));
    }
    entries.sort_unstable();
    log::debug!("Entries returned: {entries:?}");
    log::info!("File selected: {}", entries[selected_index]);
    Some((entries, selected_index))
}

fn load_image(path: &str) -> Option<Image> {
    log::info!("Loading image at {path}");
    let (prefix, path) = path.split_once(":/")?;
    let location = match prefix {
        "user" => fs::Location::User,
        "usb" => fs::Location::Usb,
        _ => return None,
    };
    let fs = FileSystem::default();
    let file = fs
        .open_file(path, location, fs::OpenFlags { read: true, write: false, create: false })
        .map_err(|e| {
            log::warn!("Error opening file {path}: {e:?}");
            e
        })
        .ok()?;
    let mut decoder = image::ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .map_err(|e| {
            log::warn!("Error guessing file format of {path}: {e:?}");
            e
        })
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1000);
    limits.max_image_height = Some(1000);
    limits.max_alloc = Some(4 * 1024 * 1024);
    decoder.limits(limits);

    let dynamic_image = decoder
        .decode()
        .map_err(|e| {
            log::warn!("Error decoding {path}: {e:?}");
            e
        })
        .ok()?;
    log::info!(
        "Image successfully loaded ({}x{}@{:?})",
        dynamic_image.width(),
        dynamic_image.height(),
        dynamic_image.color()
    );
    let image = if dynamic_image.color().has_alpha() {
        let rgba8image = dynamic_image.into_rgba8();
        Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
            rgba8image.as_raw(),
            rgba8image.width(),
            rgba8image.height(),
        ))
    } else {
        let rgb8image = dynamic_image.into_rgb8();
        Image::from_rgb8(SharedPixelBuffer::clone_from_slice(
            rgb8image.as_raw(),
            rgb8image.width(),
            rgb8image.height(),
        ))
    };
    Some(image)
}
