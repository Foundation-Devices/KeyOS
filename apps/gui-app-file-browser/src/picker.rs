// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use slint_keyos_platform::gui_server_api::navigation::filepicker::{AllowedExtensions, AllowedLocations};

use crate::{
    fsutils::list::ListingParams,
    location::{LocationKey, LocationMap},
    path::FsPath,
    state::ListingState,
};

#[derive(Clone, Default)]
pub struct PickerLocationState {
    pub path: FsPath,
    pub allowed: bool,
}

#[derive(Clone)]
pub struct PickerOptions {
    pub allowed_extensions: AllowedExtensions,
    pub allow_dirs: bool,
    pub allow_hidden: bool,
    pub dir_selection_mode: bool,
}

#[derive(Clone)]
pub struct PickerState {
    pub current: LocationKey,
    pub locations: LocationMap<PickerLocationState>,
    pub listing: ListingState,
    pub options: PickerOptions,
}

impl PickerState {
    pub fn new(options: PickerOptions, allowed: LocationMap<bool>, start: LocationKey) -> Self {
        let locations = LocationMap::from_fn(|key| PickerLocationState {
            allowed: *allowed.get(key),
            ..Default::default()
        });
        PickerState { current: start, locations, listing: ListingState::new(), options }
    }

    pub fn allowed_map_from_request(allowed: &AllowedLocations) -> LocationMap<bool> {
        let mut map = LocationMap::from_fn(|_| false);
        match allowed {
            AllowedLocations::All => {
                for key in LocationKey::ALL.iter().copied() {
                    map.insert(key, true);
                }
            }
            AllowedLocations::Specific(locations) => {
                for location in locations {
                    let key = LocationKey::from_nav(*location);
                    map.insert(key, true);
                }
            }
        }
        map
    }

    pub fn allowed_order(&self) -> Vec<LocationKey> {
        LocationKey::ALL.into_iter().filter(|key| self.locations.get(*key).allowed).collect::<Vec<_>>()
    }

    /// `user_show_hidden` is the persisted user preference. It is ANDed with
    /// `options.allow_hidden` so a caller that hard-denies hidden files
    /// (e.g. image viewer) always wins; otherwise the user toggle decides.
    pub fn listing_params(&self, user_show_hidden: bool) -> ListingParams {
        let key = self.current;
        let location = key.to_fs();
        let path = self.locations.get(key).path.to_string();
        ListingParams {
            location,
            path,
            allowed_extensions: self.options.allowed_extensions.clone(),
            sort_mode: crate::SortMode::Alphabetical,
            sort_direction: crate::SortDirection::Descending,
            search_query: None,
            show_hidden: self.options.allow_hidden && user_show_hidden,
            allow_dirs: self.options.allow_dirs,
        }
    }
}
