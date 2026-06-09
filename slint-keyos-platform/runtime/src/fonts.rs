// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

const DEFAULT_FONT: &str = "Montserrat";

#[cfg(keyos)]
use std::io::Read;

#[cfg(keyos)]
pub fn register_fonts<P>(fs: &fs::FileSystem<P>)
where
    P: server::CheckedPermissions,
    P: server::MessageAllowed<fs::messages::OpenDirMessage>,
    P: server::MessageAllowed<fs::messages::OpenFileMessage>,
    P: server::MessageAllowed<fs::messages::CloseFile>,
    P: server::MessageAllowed<fs::messages::CloseDir>,
    P: server::MessageAllowed<fs::messages::NextEntry>,
    P: server::MessageAllowed<fs::messages::ReadFile>,
    P: server::MessageAllowed<fs::messages::MapFileMessage>,
{
    // Fonts resolve along a search path: an app's own fonts (under AppResources)
    // take precedence, and the common system fonts fill in the ones it does not
    // provide. App resources are untrusted, so they are read and leaked rather
    // than memory-mapped (mapping an app-controlled file is unsafe); the trusted
    // common assets stay mapped.
    let mut app_fonts: Vec<String> = Vec::new();
    for_each_font_in_location(fs, fs::Location::AppResources, false, |font_name| {
        register_leaked_font(fs, fs::Location::AppResources, font_name);
        app_fonts.push(font_name.to_owned());
    });
    for_each_font_in_location(fs, fs::Location::CommonAssets, true, |font_name| {
        if app_fonts.iter().any(|name| name == font_name) {
            return;
        }
        register_mapped_font(fs, fs::Location::CommonAssets, font_name);
    });
    i_slint_common::sharedfontdb::set_default_font_family(DEFAULT_FONT);
}

#[cfg(keyos)]
fn for_each_font_in_location<P, F>(
    fs: &fs::FileSystem<P>,
    location: fs::Location,
    required: bool,
    mut register: F,
) where
    P: server::CheckedPermissions,
    P: server::MessageAllowed<fs::messages::OpenDirMessage>,
    P: server::MessageAllowed<fs::messages::CloseDir>,
    P: server::MessageAllowed<fs::messages::NextEntry>,
    F: FnMut(&str),
{
    let fonts_dir = match fs.open_dir("fonts", location) {
        Ok(fonts_dir) => fonts_dir,
        Err(e) if required => panic!("Could not open {location:?} fonts dir: {e:?}"),
        Err(_) => return,
    };
    while let Some(font_entry) =
        fonts_dir.next_entry().unwrap_or_else(|e| panic!("Could not read {location:?} fonts dir: {e:?}"))
    {
        if font_entry.is_file {
            register(&font_entry.name);
        }
    }
}

#[cfg(keyos)]
fn register_mapped_font<P>(fs: &fs::FileSystem<P>, location: fs::Location, font_name: &str)
where
    P: server::CheckedPermissions,
    P: server::MessageAllowed<fs::messages::MapFileMessage>,
{
    let mapping = fs
        .map_file(location, format!("fonts/{font_name}"))
        .unwrap_or_else(|e| panic!("Could not load font {font_name} from {location:?}: {e:?}"));
    // Transmuting to static because we know we are not dropping this memory.
    let mapping = unsafe { core::mem::transmute::<&[u8], &'static [u8]>(mapping.as_slice()) };
    register_font_from_memory(font_name, mapping);
}

#[cfg(keyos)]
fn register_leaked_font<P>(fs: &fs::FileSystem<P>, location: fs::Location, font_name: &str)
where
    P: server::CheckedPermissions,
    P: server::MessageAllowed<fs::messages::OpenFileMessage>,
    P: server::MessageAllowed<fs::messages::CloseFile>,
    P: server::MessageAllowed<fs::messages::ReadFile>,
{
    let mut file = fs
        .open_file(format!("fonts/{font_name}"), location, fs::OpenFlags::READ_ONLY)
        .unwrap_or_else(|e| panic!("Could not open font {font_name} from {location:?}: {e:?}"));
    let mut font_data = Vec::new();
    file.read_to_end(&mut font_data)
        .unwrap_or_else(|e| panic!("Could not read font {font_name} from {location:?}: {e:?}"));
    register_font_from_memory(font_name, font_data.leak());
}

#[cfg(keyos)]
fn register_font_from_memory(font_name: &str, font_data: &'static [u8]) {
    i_slint_common::sharedfontdb::register_font_from_memory(font_data)
        .unwrap_or_else(|e| panic!("Could not register font {font_name}: {e:?}"));
}

#[cfg(not(keyos))]
pub fn register_fonts<FS>(_fs: &FS) {
    // Fonts resolve along a search path: an app's own fonts (under
    // FOUNDATION_APP_RESOURCES_DIR) take precedence, and the shared/common fonts
    // fill in the ones it does not provide.
    let app_dir = std::env::var_os("FOUNDATION_APP_RESOURCES_DIR")
        .map(|root| std::path::PathBuf::from(root).join("fonts"))
        .filter(|path| path.is_dir());
    let common_dir = ["../../resources/fonts", "../../ui/ui/fonts"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.is_dir());

    if app_dir.is_none() && common_dir.is_none() {
        panic!("Could not find a fonts directory. Tried FOUNDATION_APP_RESOURCES_DIR/fonts, ../../resources/fonts, and ../../ui/ui/fonts");
    }

    let mut registered: Vec<std::ffi::OsString> = Vec::new();
    for font_dir in app_dir.iter().chain(common_dir.iter()) {
        for font_file in std::fs::read_dir(font_dir).unwrap() {
            let font_path = font_file.unwrap().path();
            if !is_supported_font(&font_path) {
                continue;
            }
            let Some(font_name) = font_path.file_name().map(|name| name.to_owned()) else {
                continue;
            };
            // App fonts are registered first, so skip any common font the app overrides.
            if registered.contains(&font_name) {
                continue;
            }
            registered.push(font_name);

            let font_data = std::fs::read(&font_path).unwrap().leak();
            i_slint_common::sharedfontdb::register_font_from_memory(font_data)
                .expect("Could not register individual font");
        }
    }
    i_slint_common::sharedfontdb::set_default_font_family(DEFAULT_FONT);
}

#[cfg(not(keyos))]
fn is_supported_font(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("ttf" | "otf" | "ttc")
    )
}
