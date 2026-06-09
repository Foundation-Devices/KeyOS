// SPDX-FileCopyrightText: 2024-2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! File picker (select file) navigation request and response formats.

#[derive(Debug, Copy, Clone, Eq, PartialEq, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub enum Location {
    Internal,
    Airlock,
    External,
}

#[derive(Debug, Clone, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub enum AllowedLocations {
    All,
    Specific(Vec<Location>),
}

impl AllowedLocations {
    pub fn specific<T: IntoIterator<Item = Location>>(locations: T) -> Self {
        Self::Specific(locations.into_iter().collect::<Vec<_>>())
    }

    pub fn contains(&self, location: Location) -> bool {
        match self {
            Self::All => true,
            Self::Specific(locations) => locations.contains(&location),
        }
    }

    pub fn len(&self) -> Option<usize> {
        match self {
            Self::All => None,
            Self::Specific(locations) => Some(locations.len()),
        }
    }

    pub fn is_empty(&self) -> bool { self.len().map(|len| len == 0).unwrap_or(false) }
}

#[derive(Debug, Clone, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub enum AllowedExtensions {
    All,
    Specific(Vec<String>),
}

impl AllowedExtensions {
    pub fn specific<S: AsRef<str>, T: IntoIterator<Item = S>>(extensions: T) -> Self {
        Self::Specific(extensions.into_iter().map(|s| s.as_ref().to_string()).collect::<Vec<_>>())
    }

    pub fn contains<S: AsRef<str>>(&self, extension: S) -> bool {
        match self {
            Self::All => true,
            Self::Specific(extensions) => extensions.contains(&extension.as_ref().to_string()),
        }
    }
}

/// Options for the file picker navigation request.
///
/// Example to only allow accessing `.bin` files in `External` location:
///
/// ```rust
/// # use navigation::api::filepicker::{SelectFileOptions, AllowedLocations, AllowedExtensions, Location};
/// let options = SelectFileOptions::default()
///     .with_start_location(Location::External)
///     .with_allowed_locations(AllowedLocations::specific(&[Location::External]))
///     .with_allowed_extensions(AllowedExtensions::specific(&["bin"]));
/// ```
#[derive(Debug, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct SelectFileOptions {
    start_location: Location,
    locations: AllowedLocations,
    extensions: AllowedExtensions,
    hidden_allowed: bool,
    dirs_allowed: bool,
    search_allowed: bool,
    dir_selection_mode: bool,
    multiple_selection_mode: bool,
}

impl Default for SelectFileOptions {
    fn default() -> Self {
        Self {
            start_location: Location::Internal,
            locations: AllowedLocations::All,
            extensions: AllowedExtensions::All,
            hidden_allowed: true,
            search_allowed: true,
            dirs_allowed: true,
            dir_selection_mode: false,
            multiple_selection_mode: false,
        }
    }
}

impl SelectFileOptions {
    /// Enable directory selection mode.
    /// Doesn't show the files, only directories and allows navigating into them.
    /// This is disabled by default.
    /// Enabling this also enables `dirs_allowed`.
    pub fn with_dir_selection_mode(self, dir_selection_mode: bool) -> Self {
        Self { dir_selection_mode, dirs_allowed: self.dirs_allowed || dir_selection_mode, ..self }
    }

    /// Allow viewing and navigating directories.
    /// This is enabled by default.
    /// Disabling this has no effect if `dir_selection_mode` is enabled.
    pub fn with_dirs_allowed(self, allow_directories: bool) -> Self {
        Self { dirs_allowed: allow_directories || self.dir_selection_mode, ..self }
    }

    /// Display the search bar and activates search menu.
    pub fn with_search_allowed(self, allow_search: bool) -> Self {
        Self { search_allowed: allow_search, ..self }
    }

    /// Display the search bar and activates search menu.
    /// This is disabled by default.
    /// Enabling this will disable `dir_selection_mode`.
    pub fn with_multiple_selection_mode(self, multiple_selection_mode: bool) -> Self {
        let dir_selection_mode = self.dir_selection_mode && !multiple_selection_mode;
        Self { multiple_selection_mode, dir_selection_mode, ..self }
    }

    /// Sets whether hidden files are allowed to be shown.
    pub fn with_hidden_allowed(self, hidden_allowed: bool) -> Self { Self { hidden_allowed, ..self } }

    pub fn with_start_location(self, start_location: Location) -> Self { Self { start_location, ..self } }

    pub fn with_allowed_locations(self, locations: AllowedLocations) -> Self { Self { locations, ..self } }

    pub fn with_allowed_extensions(self, extensions: AllowedExtensions) -> Self {
        Self { extensions, ..self }
    }

    pub fn search_allowed(&self) -> bool { self.search_allowed }

    pub fn dirs_allowed(&self) -> bool { self.dirs_allowed }

    pub fn hidden_allowed(&self) -> bool { self.hidden_allowed }

    pub fn dir_selection_mode(&self) -> bool { self.dir_selection_mode }

    pub fn multiple_selection_mode(&self) -> bool { self.multiple_selection_mode }

    pub fn start_location(&self) -> Location { self.start_location }

    pub fn allowed_locations(&self) -> &AllowedLocations { &self.locations }

    pub fn allowed_extensions(&self) -> &AllowedExtensions { &self.extensions }

    pub fn from_slice(data: &[u8]) -> Option<Self> {
        let Ok(archived) = rkyv::access::<ArchivedSelectFileOptions, rkyv::rancor::Error>(data) else {
            return None;
        };
        rkyv::deserialize::<Self, rkyv::rancor::Error>(archived).ok()
    }

    pub fn serialize(&self) -> Vec<u8> { rkyv::to_bytes::<rkyv::rancor::Error>(self).unwrap().to_vec() }
}

#[derive(Debug, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct SelectFileResult {
    pub files: Vec<(String, Location)>,
}

impl SelectFileResult {
    pub fn new(files: Vec<(String, Location)>) -> Self { Self { files } }

    pub fn files(&self) -> &[(String, Location)] { &self.files }

    pub fn from_slice(data: &[u8]) -> Option<Self> {
        let Ok(archived) = rkyv::access::<ArchivedSelectFileResult, rkyv::rancor::Error>(data) else {
            return None;
        };
        rkyv::deserialize::<Self, rkyv::rancor::Error>(archived).ok()
    }

    pub fn serialize(&self) -> Vec<u8> { rkyv::to_bytes::<rkyv::rancor::Error>(self).unwrap().to_vec() }
}
