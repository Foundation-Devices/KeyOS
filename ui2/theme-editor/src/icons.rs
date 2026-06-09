// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct IconRegistry {
    names: Vec<String>,
    images: HashMap<String, slint::Image>,
    empty_image: slint::Image,
}

impl IconRegistry {
    pub fn load_from_dir(dir: &Path) -> Self {
        let mut names = vec!["none".to_string()];
        let mut images = HashMap::new();
        let empty_image = slint::Image::default();

        if let Ok(entries) = fs::read_dir(dir) {
            let mut svg_paths: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "svg"))
                .collect();
            svg_paths.sort();

            for path in svg_paths {
                let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                if let Ok(image) = slint::Image::load_from_path(&path) {
                    let name = stem.to_string();
                    names.push(name.clone());
                    images.insert(name, image);
                }
            }
        }

        Self { names, images, empty_image }
    }

    pub fn filter(&self, query: &str) -> Vec<slint::SharedString> {
        let trimmed = query.trim().to_ascii_lowercase();
        let names = if trimmed.is_empty() {
            self.names.iter().collect::<Vec<_>>()
        } else {
            self.names.iter().filter(|name| name.to_ascii_lowercase().contains(&trimmed)).collect::<Vec<_>>()
        };

        names.into_iter().cloned().map(Into::into).collect()
    }

    pub fn image(&self, name: &str) -> slint::Image {
        if name.is_empty() || name == "none" {
            return self.empty_image.clone();
        }

        self.images.get(name).cloned().unwrap_or_else(|| self.empty_image.clone())
    }
}
