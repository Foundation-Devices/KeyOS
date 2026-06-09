// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Plugin i18n helpers

use std::collections::HashMap;

/// i18n support for plugins
pub struct PluginI18n {
    current_locale: String,
    embedded: HashMap<String, HashMap<String, String>>,
    overrides: HashMap<String, String>,
}

impl PluginI18n {
    /// Create with embedded translations
    pub fn new(plugin_name: &str, embedded: HashMap<String, HashMap<String, String>>) -> Self {
        let current_locale = Self::detect_locale();
        let overrides = Self::load_overrides(plugin_name, &current_locale);

        Self { current_locale, embedded, overrides }
    }

    /// Get a translated string
    pub fn t(&self, key: &str) -> String {
        // Check overrides first
        if let Some(value) = self.overrides.get(key) {
            return value.clone();
        }

        // Check embedded for current locale
        if let Some(translations) = self.embedded.get(&self.current_locale) {
            if let Some(value) = translations.get(key) {
                return value.clone();
            }
        }

        // Fall back to English
        if let Some(translations) = self.embedded.get("en") {
            if let Some(value) = translations.get(key) {
                return value.clone();
            }
        }

        // Return key if not found
        key.to_string()
    }

    fn detect_locale() -> String {
        std::env::var("FOUNDATION_LANG")
            .or_else(|_| std::env::var("LANG"))
            .map(|l| l.split('.').next().unwrap_or("en").to_string())
            .unwrap_or_else(|_| "en".to_string())
    }

    fn load_overrides(plugin_name: &str, locale: &str) -> HashMap<String, String> {
        let path = dirs::data_dir().map(|d| {
            d.join("foundation")
                .join("locales")
                .join("plugins")
                .join(plugin_name)
                .join(format!("{}.json", locale))
        });

        path.and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}
