// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Read;

/// Collects the font blobs found on the search path as `&'static [u8]`. The caller registers them
/// with the `FontContext` once the Slint context exists (see `KeyOsPlatform::bind_context`).
pub fn register_fonts<P>(fs: &fs::FileSystem<P>) -> Vec<&'static [u8]>
where
    P: server::CheckedPermissions,
    P: server::MessageAllowed<fs::messages::OpenDirMessage>,
    P: server::MessageAllowed<fs::messages::OpenFileMessage>,
    P: server::MessageAllowed<fs::messages::CloseFile>,
    P: server::MessageAllowed<fs::messages::CloseDir>,
    P: server::MessageAllowed<fs::messages::NextEntry>,
    P: server::MessageAllowed<fs::messages::ReadFile>,
    P: fs::MapFilePermissions,
{
    // Fonts resolve along a search path: an app's own fonts (under AppResources)
    // take precedence, and the common system fonts fill in the ones it does not
    // provide. App resources are untrusted, so they are read and leaked rather
    // than memory-mapped (mapping an app-controlled file is unsafe); the trusted
    // common assets stay mapped.
    let mut fonts: Vec<&'static [u8]> = Vec::new();
    let mut app_fonts: Vec<String> = Vec::new();
    for_each_font_in_location(fs, fs::Location::AppResources, false, |font_name| {
        fonts.push(load_leaked_font(fs, fs::Location::AppResources, font_name));
        app_fonts.push(font_name.to_owned());
    });
    for_each_font_in_location(fs, fs::Location::CommonAssets, true, |font_name| {
        if app_fonts.iter().any(|name| name == font_name) {
            return;
        }
        fonts.push(load_mapped_font(fs, fs::Location::CommonAssets, font_name));
    });
    fonts
}

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

fn load_mapped_font<P>(fs: &fs::FileSystem<P>, location: fs::Location, font_name: &str) -> &'static [u8]
where
    P: server::CheckedPermissions,
    P: fs::MapFilePermissions,
{
    let mapping = fs
        .map_file(location, format!("fonts/{font_name}"))
        .unwrap_or_else(|e| panic!("Could not load font {font_name} from {location:?}: {e:?}"));
    // Transmuting to static because we know we are not dropping this memory.
    unsafe { core::mem::transmute::<&[u8], &'static [u8]>(mapping.as_slice()) }
}

fn load_leaked_font<P>(fs: &fs::FileSystem<P>, location: fs::Location, font_name: &str) -> &'static [u8]
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
    font_data.leak()
}
