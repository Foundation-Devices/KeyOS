// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::DirEntry;
use slint_keyos_platform::{
    async_archive, gui_server_api::navigation::filepicker::AllowedExtensions, slint::SharedString,
};
use whence::WhenceExt;

use crate::{fs_permissions::FileSystemPermissions, FileSystem, SortDirection, SortMode};

#[derive(Clone)]
pub struct ListingParams {
    pub path: String,
    pub location: fs::Location,
    pub allowed_extensions: AllowedExtensions,
    pub sort_mode: SortMode,
    pub sort_direction: SortDirection,
    pub search_query: Option<SharedString>,
    pub show_hidden: bool,
    pub allow_dirs: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FilterCounts {
    pub excluded: usize,
    pub excluded_by_search: usize,
}

pub async fn list_directory(
    fs: FileSystem,
    params: ListingParams,
) -> whence::Result<(Vec<DirEntry>, FilterCounts), fs::Error> {
    let search_query = params.search_query.map(|v| v.to_lowercase());

    let dir = fs.open_dir(params.path, params.location).whence()?;
    let mut entries = vec![];
    let mut counts = FilterCounts::default();
    loop {
        let entry = async_archive::<FileSystemPermissions, _>(dir.next_entry_async()).await.whence()?;
        let Some(entry) = entry else {
            break;
        };
        let name = entry.name.as_str();
        if !params.allow_dirs && entry.is_dir {
            continue;
        }

        if name.starts_with('.') {
            if name == "." || name == ".." {
                continue;
            }
            if !params.show_hidden {
                counts.excluded += 1;
                continue;
            }
        }

        if let Some(query) = &search_query {
            if !name.to_lowercase().contains(query) {
                counts.excluded += 1;
                counts.excluded_by_search += 1;
                continue;
            }
        }

        if entry.is_file {
            let extension = name.split('.').next_back();
            if let Some(extension) = extension {
                if matches!(params.allowed_extensions, AllowedExtensions::Specific(_))
                    && !params.allowed_extensions.contains(extension)
                {
                    counts.excluded += 1;
                    continue;
                }
            } else if !matches!(params.allowed_extensions, AllowedExtensions::All) {
                counts.excluded += 1;
                continue;
            }
        }

        entries.push(entry);
    }

    Ok((sort_by(entries, params.sort_mode, params.sort_direction), counts))
}

fn sort_by(list: Vec<DirEntry>, sort_mode: SortMode, sort_direction: SortDirection) -> Vec<DirEntry> {
    let mut dirs = vec![];
    let mut files = vec![];
    for entry in list {
        if entry.is_dir {
            dirs.push(entry);
        } else {
            files.push(entry);
        }
    }

    match sort_mode {
        SortMode::Alphabetical => {
            dirs.sort_by_key(|e| e.name.to_lowercase());
            files.sort_by_key(|e| e.name.to_lowercase());
        }
        SortMode::FileSize => {
            dirs.sort_by_key(|e| e.name.to_lowercase());
            files.sort_by_key(|e| e.len);
        }
        SortMode::ModificationDate => {
            dirs.sort_by_key(|e| e.name.to_lowercase());
            files.sort_by_key(|e| e.modified);
        }
    }

    if sort_direction == SortDirection::Ascending {
        dirs.reverse();
        files.reverse();
    }

    dirs.extend(files);
    dirs
}
