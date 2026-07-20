// SPDX-FileCopyrightText: 2024-2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde::Deserialize;
use serde_json::Value;

#[derive(Parser)]
#[command(
    name = "localizer",
    version = "1.0",
    author = "Ken Carpenter <ken@foundation.xyz>",
    about = "Fetches translations from Localazy and outputs JSON files for each app.\nAlso generates a .slint lookup() function to enable viewing English messages in VSCode previews."
)]
struct LocalizerArgs {
    /// Specifies the config file
    #[arg(short, long, value_name = "FILE")]
    config: PathBuf,

    /// Check for missing translation keys in Slint files
    #[arg(long)]
    check: bool,
}

#[derive(Debug, Deserialize)]
struct LocalizerConfig {
    sources: String,
    apps: Vec<AppConfig>,
}

#[derive(Debug, Deserialize, Clone)]
struct AppConfig {
    name: String,
    path: String,
    include: Vec<String>,
}

fn main() -> Result<()> {
    let args = LocalizerArgs::parse();

    let config = read_config(&args.config)?;

    if args.check {
        return check_translations(config);
    }

    // Validate includes against the english key set (hard-error on bad patterns)
    let available_keys = load_available_keys(&config.sources)
        .with_context(|| format!("Failed to load keys from '{}'", config.sources))?;
    validate_includes(&config.apps, &available_keys)?;

    let apps = &config.apps;

    let mut app_translations: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>> = BTreeMap::new();

    println!("=========================================================================================");
    println!("Generating JSON translation files for build system");
    println!("=========================================================================================");

    // Iterate over entries in the sources directory - each folder represents a language code
    for entry in fs::read_dir(&config.sources)
        .with_context(|| format!("Failed to read sources directory '{}'", config.sources))?
    {
        let entry = entry?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let translations_path = path.join("figma.json");
        if !translations_path.exists() {
            eprintln!("No figma.json found in {}", path.display());
            continue;
        }

        let lang_code = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("Invalid language directory name: {}", path.display()))?
            .to_string();

        let translation_content = fs::read_to_string(&translations_path)
            .with_context(|| format!("Failed to read {}", translations_path.display()))?;
        let translation_json: Value = serde_json::from_str(&translation_content)
            .with_context(|| format!("Failed to parse {}", translations_path.display()))?;
        let flattened_translations = flatten_translation_json(&translation_json);

        for app in apps {
            let app_translation = app_translations.entry(app.name.clone()).or_insert_with(BTreeMap::new);
            let lang_translations = app_translation.entry(lang_code.clone()).or_insert_with(BTreeMap::new);

            for (full_key, value) in &flattened_translations {
                if include_key(full_key, &app.include) {
                    let stripped_key = strip_app_prefix(full_key, &app.name);
                    lang_translations.insert(stripped_key, value.clone());
                }
            }
        }
    }

    // Output JSON files for each app
    for app in apps {
        let translations_dir = Path::new(&app.path).join("i18n");
        fs::create_dir_all(&translations_dir)
            .with_context(|| format!("Failed to create translations dir {}", translations_dir.display()))?;

        if let Some(app_trans) = app_translations.get(&app.name) {
            for (lang_code, translations) in app_trans {
                let json_output: serde_json::Map<String, Value> =
                    translations.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();

                let output_path = translations_dir.join(format!("{}.json", lang_code));
                let json_string = serde_json::to_string_pretty(&Value::Object(json_output))?;
                fs::write(&output_path, json_string)
                    .with_context(|| format!("Failed to write {}", output_path.display()))?;

                println!("  {} -> {}", app.name, output_path.display());
            }
        }
    }

    Ok(())
}

fn read_config(path: &Path) -> Result<LocalizerConfig> {
    let config_content =
        fs::read_to_string(path).with_context(|| format!("Failed to read config {}", path.display()))?;
    let mut config: LocalizerConfig = serde_json::from_str(&config_content)
        .with_context(|| format!("Failed to parse config {}", path.display()))?;
    for app in &mut config.apps {
        app.include.push(app.name.clone())
    }
    Ok(config)
}

fn validate_includes(apps: &[AppConfig], available_keys: &HashSet<String>) -> Result<()> {
    for app in apps {
        for prefix in &app.include {
            // The app name is auto-added as an include for the app's own namespace; a service that
            // only consumes shared namespaces has none of its own, so a name matching nothing is
            // fine. Declared includes must still match.
            if prefix == &app.name {
                continue;
            }
            if available_keys.iter().any(|k| k.starts_with(prefix)) {
                continue;
            }

            anyhow::bail!(
                "App '{}' include prefix '{}' does not match any translation keys",
                app.name,
                prefix
            );
        }
    }

    Ok(())
}

/// flattens a translation JSON value into a BTreeMap with dotted keys
fn flatten_translation_json(json: &Value) -> BTreeMap<String, String> {
    fn flatten_json(json: &Value, prefix: &str, result: &mut BTreeMap<String, String>) {
        if let Some(obj) = json.as_object() {
            for (key, value) in obj {
                let full_key = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };

                if let Some(str_value) = value.as_str() {
                    result.insert(full_key, str_value.to_string());
                } else {
                    flatten_json(value, &full_key, result);
                }
            }
        }
    }

    let mut flattened = BTreeMap::new();
    flatten_json(json, "", &mut flattened);
    flattened
}

fn strip_app_prefix(full_key: &str, app_name: &str) -> String {
    let prefix = format!("{}.", app_name);
    if let Some(stripped) = full_key.strip_prefix(&prefix) {
        stripped.to_string()
    } else {
        full_key.to_string()
    }
}

/// Decide whether a key is included for an app.
///
/// A key matches if it starts with any of the include patterns.
fn include_key(key: &str, include: &[String]) -> bool {
    include.iter().any(|pattern| key.starts_with(pattern))
}

fn check_translations(config: LocalizerConfig) -> anyhow::Result<()> {
    let missing_translations = {
        let mut tr = find_broken_translations(&config)?.into_iter().collect::<Vec<_>>();
        tr.sort_by(|(a, _), (b, _)| a.cmp(b));
        tr
    };
    if missing_translations.is_empty() {
        println!("✅ All translation keys valid");
        return Ok(());
    }

    let total_missing: usize = missing_translations.iter().map(|(_app, missing)| missing.len()).sum();

    println!("Translation Check Summary");
    println!("========================");
    println!("Total missing: {}", total_missing);
    println!();

    println!("App Summary:");

    let max_app_name_len = missing_translations.iter().map(|(name, _)| name.len()).max().unwrap_or(0);

    for (app_name, missing) in &missing_translations {
        let missing_count = missing.len();
        println!("  {:<width$} - {:>2} missing", app_name, missing_count, width = max_app_name_len);
    }
    println!();

    println!("Missing Keys:");
    println!("=======================");
    println!();

    for (app_name, app_missing) in &missing_translations {
        if !app_missing.is_empty() {
            println!("➡️ {} ({} missing)", app_name, app_missing.len());

            for missing in app_missing {
                println!("□ {}", missing.key);

                for usage in &missing.usages {
                    let simplified_path = simplify_file_path(&usage.file, app_name);
                    println!("  └─ {}:{}", simplified_path, usage.line + 1);
                }
                println!();
            }
        }
    }

    println!("Total missing keys: {}", total_missing);
    std::process::exit(1);
}

#[derive(Debug, Clone)]
struct TranslationUsage {
    key: String,
    file: PathBuf,
    line: usize,
}

#[derive(Debug)]
struct MissingTranslation {
    key: String,
    usages: Vec<TranslationUsage>,
}

fn find_broken_translations(
    config: &LocalizerConfig,
) -> anyhow::Result<BTreeMap<String, Vec<MissingTranslation>>> {
    let apps = &config.apps;
    let mut result: BTreeMap<String, Vec<MissingTranslation>> = BTreeMap::new();

    for app in apps {
        let app_name = app.name.as_str();
        let app_path = app.path.as_str();
        let ui_dir = Path::new(app_path).join("ui");

        if !ui_dir.exists() {
            continue;
        }

        let translations_path = Path::new(app_path).join("i18n/en.json");
        if !translations_path.exists() {
            anyhow::bail!("No en.json found for app '{}' at {}", app_name, translations_path.display());
        }

        let content = fs::read_to_string(&translations_path)?;
        let translations: BTreeMap<String, String> = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

        let usages = scan_slint_files(&ui_dir)?;

        let mut usages_by_key: BTreeMap<String, Vec<TranslationUsage>> = BTreeMap::new();
        for usage in usages {
            usages_by_key.entry(usage.key.clone()).or_insert_with(Vec::new).push(usage);
        }

        let mut missing_translations = Vec::new();

        for (key, usages) in usages_by_key {
            if !translations.contains_key(&key) {
                missing_translations.push(MissingTranslation { key, usages });
            }
        }

        if !missing_translations.is_empty() {
            result.insert(app.name.clone(), missing_translations);
        }
    }

    Ok(result)
}

fn load_available_keys(sources: &str) -> std::io::Result<HashSet<String>> {
    let translations_path = Path::new(sources).join("en/figma.json");
    let content = fs::read_to_string(&translations_path)?;
    let json: Value = serde_json::from_str(&content)?;
    let keys = flatten_translation_json(&json).into_keys().collect();
    Ok(keys)
}

fn scan_slint_files(ui_dir: &Path) -> std::io::Result<Vec<TranslationUsage>> {
    use regex::Regex;
    use walkdir::WalkDir;

    let lookup_re = Regex::new(r#"TR\.lookup\s*\(\s*"([^"]+)"\s*\)"#).unwrap();
    let format_re = Regex::new(r#"TR\.format\s*\(\s*"([^"]+)"\s*,"#).unwrap();

    let mut usages = Vec::new();

    for entry in WalkDir::new(ui_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "slint"))
    {
        let path = entry.path();
        let content = fs::read_to_string(path)?;

        for (line_num, line) in content.lines().enumerate() {
            // Find TR.lookup calls
            for captures in lookup_re.captures_iter(line) {
                let key = captures.get(1).unwrap().as_str().to_string();
                usages.push(TranslationUsage { key, file: path.to_path_buf(), line: line_num });
            }

            // Find TR.format calls
            for captures in format_re.captures_iter(line) {
                let key = captures.get(1).unwrap().as_str().to_string();
                usages.push(TranslationUsage { key, file: path.to_path_buf(), line: line_num });
            }
        }
    }

    Ok(usages)
}

fn simplify_file_path(file_path: &Path, app_name: &str) -> String {
    let path_str = file_path.to_string_lossy();

    // Remove common prefixes to make paths shorter and more readable
    let prefixes_to_remove = [
        &format!("os/gui-app-{}/ui/", app_name),
        &format!("apps/gui-app-{}/ui/", app_name),
        "os/gui-app-",
        "apps/gui-app-",
        "/ui/",
    ];

    let mut simplified = path_str.to_string();
    for prefix in &prefixes_to_remove {
        if let Some(pos) = simplified.find(prefix) {
            simplified = simplified[pos + prefix.len()..].to_string();
            break;
        }
    }

    // If we still have a long path, try to extract just the meaningful part
    if simplified.len() > 50 {
        if let Some(ui_pos) = simplified.find("/ui/") {
            simplified = simplified[ui_pos + 4..].to_string();
        }
    }

    simplified
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flatten_translation_json() {
        let json = serde_json::json!({
            "Label": "BlueWallet",
            "Time": "15",
            "authenticator": {
                "archivedCodes": {
                    "noArchivedCodes": "No archived codes",
                    "title": "Archive"
                },
                "main": {
                    "title": "2FA Codes",
                    "searchTextFiller": "Search..."
                }
            },
            "common": {
                "button": {
                    "ok": "OK",
                    "cancel": "Cancel"
                }
            }
        });

        let flattened = flatten_translation_json(&json);

        assert_eq!(flattened.len(), 8);

        assert_eq!(flattened.get("Label"), Some(&"BlueWallet".to_string()));
        assert_eq!(flattened.get("Time"), Some(&"15".to_string()));
        assert_eq!(
            flattened.get("authenticator.archivedCodes.noArchivedCodes"),
            Some(&"No archived codes".to_string())
        );
        assert_eq!(flattened.get("authenticator.archivedCodes.title"), Some(&"Archive".to_string()));
        assert_eq!(flattened.get("authenticator.main.title"), Some(&"2FA Codes".to_string()));
        assert_eq!(flattened.get("authenticator.main.searchTextFiller"), Some(&"Search...".to_string()));
        assert_eq!(flattened.get("common.button.ok"), Some(&"OK".to_string()));
        assert_eq!(flattened.get("common.button.cancel"), Some(&"Cancel".to_string()));
    }
}
