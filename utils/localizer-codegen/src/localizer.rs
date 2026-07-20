// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Context;
use serde_json::Value;

use crate::generated_file::GeneratedFile;
use crate::source::{uwriteln, Source};

/// Controls what the translation codegen emits, so one implementation serves both a Slint app and a
/// headless service.
#[derive(Debug, Clone, Copy)]
pub struct TranslationOptions {
    /// A Slint app: the `TrId` enum comes from the generated `tr.slint` (referenced as
    /// `crate::TrId`), the `init_tr!` macro wiring the Slint `TR`/`TR2` globals is emitted, and the
    /// phf maps resolve through `slint_keyos_platform::phf`. When false this is a headless service:
    /// the generated `mod tr` defines its own `TrId` enum, no Slint globals are referenced, and the
    /// phf maps resolve through the service's own `phf` dependency.
    pub slint: bool,
    /// Emit the duration/relative-time formatting helpers (only meaningful for Slint apps that pull
    /// in the `common.time` strings).
    pub include_time_localization: bool,
}

impl TranslationOptions {
    /// The crate path of the `TrId` enum in the generated code: a Slint app takes it from the
    /// compiled `tr.slint`, a service defines its own inside `mod tr`.
    fn trid_path(&self) -> &'static str {
        if self.slint {
            "crate::TrId"
        } else {
            "TrId"
        }
    }

    /// The crate path of `phf`: a Slint app reaches it re-exported from `slint_keyos_platform`, a
    /// service depends on `phf` directly.
    fn phf_path(&self) -> &'static str {
        if self.slint {
            "slint_keyos_platform::phf"
        } else {
            "phf"
        }
    }
}

pub fn build_translations(
    translations_dir: &Path,
    out_dir: &Path,
    gen_dir: &Path,
    options: &TranslationOptions,
) -> anyhow::Result<()> {
    let languages = get_languages_from_dir(translations_dir)?;

    let all_translations = load_all_translations(translations_dir, &languages)?;

    let output_path = out_dir.join("tr.rs");
    generate_rust_code(&all_translations, &output_path, options)?;
    if options.slint {
        generate_slint_tr(&all_translations, gen_dir)?;
    }

    for lang in &languages {
        let file_path = translations_dir.join(format!("{}.json", lang));
        println!("cargo:rerun-if-changed={}", file_path.display());
    }

    Ok(())
}

fn get_languages_from_dir(translations_dir: &Path) -> anyhow::Result<Vec<String>> {
    if !translations_dir.exists() {
        return Err(anyhow::anyhow!("Translation directory {} does not exist", translations_dir.display()));
    }

    let languages: Vec<String> = fs::read_dir(translations_dir)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                path.file_stem()?.to_str().map(String::from)
            } else {
                None
            }
        })
        .collect();

    if languages.is_empty() {
        return Err(anyhow::anyhow!("No translation files found in {}", translations_dir.display()));
    }

    Ok(languages)
}

fn load_all_translations(
    translations_dir: &Path,
    languages: &[String],
) -> anyhow::Result<BTreeMap<String, BTreeMap<String, String>>> {
    let mut all_translations = BTreeMap::new();

    for lang in languages {
        let file_path = translations_dir.join(format!("{}.json", lang));
        let content =
            fs::read_to_string(&file_path).with_context(|| format!("Failed to read {:?}", file_path))?;
        let json: Value =
            serde_json::from_str(&content).with_context(|| format!("Failed to parse {:?}", file_path))?;

        let mut translations = BTreeMap::new();
        let obj = json.as_object().ok_or_else(|| anyhow::anyhow!("json should be an object"))?;

        for (key, value) in obj.iter() {
            extract_translations(&mut translations, key, value);
        }

        all_translations.insert(lang.clone(), translations);
    }

    Ok(all_translations)
}

fn extract_translations(translations: &mut BTreeMap<String, String>, prefix: &str, json: &Value) {
    match json {
        Value::String(s) => {
            // It's a string value - just insert it
            translations.insert(prefix.to_string(), s.clone());
        }
        Value::Object(obj) => {
            // It's an object - recurse into it
            for (key, value) in obj {
                let full_key = if prefix.is_empty() { key.clone() } else { format!("{}.{}", prefix, key) };
                extract_translations(translations, &full_key, value);
            }
        }
        _ => {
            // Ignore other types (null, bool, number, array)
        }
    }
}

fn generate_rust_code(
    all_translations: &BTreeMap<String, BTreeMap<String, String>>,
    output_path: &Path,
    options: &TranslationOptions,
) -> anyhow::Result<()> {
    let trid = options.trid_path();
    let phf = options.phf_path();

    let keys: Vec<&String> = all_translations
        .get("en")
        .or_else(|| all_translations.values().next())
        .map(|translations| translations.keys().collect())
        .unwrap();

    let mut src = Source::default();

    uwriteln!(
        src,
        "
        // AUTO-GENERATED FILE - DO NOT EDIT
        #[allow(unused)]
        mod tr {{

        static LOCALE: std::sync::RwLock<&'static str> = std::sync::RwLock::new(\"en\");
    "
    );

    // A Slint app takes `TrId` from the compiled `tr.slint`; a headless service has no Slint, so
    // the module carries its own copy of the enum.
    if !options.slint {
        uwriteln!(src, "#[derive(Debug, Clone, Copy, PartialEq, Eq)]");
        uwriteln!(src, "pub enum TrId {{");
        for key in &keys {
            uwriteln!(src, "{},", key_to_pascal_case(key));
        }
        uwriteln!(src, "}}");
        uwriteln!(src, "");
    }

    for (lang, translations) in all_translations {
        let mut map_builder = phf_codegen::Map::new();

        for (key, value) in translations {
            map_builder.entry(key, &format!("\"{}\"", escape_string(value)));
        }

        uwriteln!(
            src,
            "static {}_TRANSLATIONS: {phf}::Map<&'static str, &'static str> = {};",
            lang.to_uppercase(),
            map_builder.build().to_string().replace("::phf::", &format!("{phf}::"))
        );
        uwriteln!(src, "");
    }

    uwriteln!(src, "impl {trid} {{");
    uwriteln!(src, "pub fn as_str(self) -> &'static str {{");
    uwriteln!(src, "match self {{");
    for key in &keys {
        let enum_variant = key_to_pascal_case(key);
        uwriteln!(src, "{trid}::{enum_variant} => \"{key}\",");
    }
    uwriteln!(src, "}}");
    uwriteln!(src, "}}");
    uwriteln!(src, "}}");
    uwriteln!(src, "");

    uwriteln!(
        src,
        "
        pub fn set_locale(locale: &'static str) {{
            let mut current_locale = LOCALE.write().unwrap();
            *current_locale = locale;
        }}

        pub fn get_locale() -> &'static str {{
            &LOCALE.read().unwrap()
        }}
    "
    );

    // Set the active locale from a runtime string (e.g. a request's locale field), matching it
    // against the known catalog locales so `set_locale` still receives a `'static`. An unknown
    // locale leaves the current one untouched.
    uwriteln!(src, "pub fn set_locale_str(locale: &str) {{");
    uwriteln!(src, "let matched: Option<&'static str> = match locale {{");
    for lang in all_translations.keys() {
        uwriteln!(src, "\"{lang}\" => Some(\"{lang}\"),");
    }
    uwriteln!(src, "_ => None,");
    uwriteln!(src, "}};");
    uwriteln!(src, "if let Some(locale) = matched {{ set_locale(locale); }}");
    uwriteln!(src, "}}");
    uwriteln!(src, "");

    // The per-locale match lives here; the process-global lookups below delegate to it against the
    // current `LOCALE`, and callers with a request-scoped locale use the `_in` variants directly.
    uwriteln!(src, "pub fn try_lookup_in(locale: &str, id: &str) -> Option<&'static str> {{");
    uwriteln!(src, "match locale {{");
    for lang in all_translations.keys() {
        uwriteln!(src, "\"{lang}\" => {}_TRANSLATIONS.get(id).copied(),", lang.to_uppercase());
    }
    uwriteln!(src, "_ => None,");
    uwriteln!(src, "}}");
    uwriteln!(src, "}}");
    uwriteln!(src, "");

    uwriteln!(
        src,
        "
        pub fn try_lookup(id: &str) -> Option<&'static str> {{
            try_lookup_in(get_locale(), id)
        }}

        pub fn lookup(id: &str) -> String {{
            try_lookup(id).unwrap_or(id).to_string()
        }}

        pub fn lookup_id(id: {trid}) -> &'static str {{
            try_lookup(id.as_str()).unwrap()
        }}
    "
    );

    // Resolve against an explicit locale without touching the process-global `LOCALE`, so a request
    // that carries its own locale need not mutate shared state (and concurrent requests in different
    // locales can't clobber each other). An unknown locale, or a key missing from it, falls back to
    // English.
    uwriteln!(src, "pub fn lookup_id_in(locale: &str, id: {trid}) -> &'static str {{");
    uwriteln!(
        src,
        "try_lookup_in(locale, id.as_str()).or_else(|| try_lookup_in(\"en\", id.as_str())).unwrap()"
    );
    uwriteln!(src, "}}");
    uwriteln!(src, "");

    if options.include_time_localization {
        uwriteln!(
            src,
            "
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum DurationStringSize {{
            Short,
            Medium,
            Full,
        }}

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum DurationUnit {{
            Week,
            Day,
            Hour,
            Minute,
            Second,
        }}

        const TR_WEEK_SECS: u64 = 7 * TR_DAY_SECS;
        const TR_DAY_SECS: u64 = 24 * TR_HOUR_SECS;
        const TR_HOUR_SECS: u64 = 60 * TR_MINUTE_SECS;
        const TR_MINUTE_SECS: u64 = 60;

        const TR_UNITS_ORDERING: &[(u64, DurationUnit)] = &[
            (TR_WEEK_SECS, DurationUnit::Week),
            (TR_DAY_SECS, DurationUnit::Day),
            (TR_HOUR_SECS, DurationUnit::Hour),
            (TR_MINUTE_SECS, DurationUnit::Minute),
            (1, DurationUnit::Second),
        ];

        pub fn lookup_duration_unit(unit: DurationUnit, size: DurationStringSize, plural: bool) -> &'static str {{
            let id = match unit {{
                DurationUnit::Week => match size {{
                    DurationStringSize::Short => crate::TrId::CommonTimeWeekShort,
                    DurationStringSize::Medium => if plural {{ crate::TrId::CommonTimeWeeksMed }} else {{ crate::TrId::CommonTimeWeekMed }},
                    DurationStringSize::Full => if plural {{ crate::TrId::CommonTimeWeeksFull }} else {{ crate::TrId::CommonTimeWeekFull }},
                }}
                DurationUnit::Day => match size {{
                    DurationStringSize::Short => crate::TrId::CommonTimeDayShort,
                    DurationStringSize::Medium => if plural {{ crate::TrId::CommonTimeDaysMed }} else {{ crate::TrId::CommonTimeDayMed }},
                    DurationStringSize::Full => if plural {{ crate::TrId::CommonTimeDaysFull }} else {{ crate::TrId::CommonTimeDayFull }},
                }}
                DurationUnit::Hour => match size {{
                    DurationStringSize::Short => crate::TrId::CommonTimeHourShort,
                    DurationStringSize::Medium => if plural {{ crate::TrId::CommonTimeHoursMed }} else {{ crate::TrId::CommonTimeHourMed }},
                    DurationStringSize::Full => if plural {{ crate::TrId::CommonTimeHoursFull }} else {{ crate::TrId::CommonTimeHourFull }},
                }}
                DurationUnit::Minute => match size {{
                    DurationStringSize::Short => crate::TrId::CommonTimeMinuteShort,
                    DurationStringSize::Medium => if plural {{ crate::TrId::CommonTimeMinutesMed }} else {{ crate::TrId::CommonTimeMinuteMed }},
                    DurationStringSize::Full => if plural {{ crate::TrId::CommonTimeMinutesFull }} else {{ crate::TrId::CommonTimeMinuteFull }},
                }}
                DurationUnit::Second => match size {{
                    DurationStringSize::Short => crate::TrId::CommonTimeSecondShort,
                    DurationStringSize::Medium => if plural {{ crate::TrId::CommonTimeSecondsMed }} else {{ crate::TrId::CommonTimeSecondMed }},
                    DurationStringSize::Full => if plural {{ crate::TrId::CommonTimeSecondsFull }} else {{ crate::TrId::CommonTimeSecondFull }},
                }}
            }};

            lookup_id(id)
        }}

        // Parse a duration into time units that can be translated
        pub fn parse_duration(duration: std::time::Duration) -> Vec<(u64, DurationUnit)> {{
            let mut secs = duration.as_secs();

            // For each unit, find the number of whole units,
            // and continue work on the remainder
            TR_UNITS_ORDERING
                .iter()
                .map(|(secs_per, unit)| {{
                    let unit_count = secs / secs_per;
                    secs = secs % secs_per;
                    (unit_count, *unit)
                }}).collect::<Vec<(u64, DurationUnit)>>()
        }}

        // Display the only largest denomination of time since the timestamp,
        // rounding seconds up to ~1 minute
        pub fn format_duration(duration: std::time::Duration) -> String {{
            let (count, unit, plural) = parse_duration(duration)
                .into_iter()
                // Skip units with 0 whole counts, drop seconds
                .filter(|(count, unit)| *count != 0 && *unit != DurationUnit::Second)
                // Get first and largest non-zero count and unit
                .next()
                .map(|(count, unit)| (count.to_string(), unit, count != 1))
                // If filtering 0s and seconds removed everything, round up to 1 min
                .unwrap_or((String::from(\"1\"), DurationUnit::Minute, false));

            format!(\"{{}} {{}}\", count, lookup_duration_unit(unit, DurationStringSize::Medium, plural))
        }}

        "
        );
    }

    // The `init_tr!` macro wires the Slint `TR`/`TR2` globals, so it only makes sense for a Slint
    // app. A headless service has no Slint runtime and just closes `mod tr` here.
    if options.slint {
        // TODO: we can remove crate::TR config once all apps have migrated
        uwriteln!(
            src,
            "
        #[macro_export]
        macro_rules! init_tr {{
            ($ui:expr) => {{
                $ui.global::<crate::TR>().on_lookup(move |id| crate::tr::lookup(id.as_str()).into());
                $ui.global::<crate::TR>().on_format(move |id, args| {{
                    let translated = crate::tr::try_lookup(&id).unwrap_or(&id);
                    let args = slint_keyos_platform::slint::Model::iter(&args).collect::<Vec<_>>();
                    slint_keyos_platform::i18n::replace_placeholders(translated, &args).into()
                }});

                $ui.global::<crate::TR2>().on_lookup(move |id| {{
                    crate::tr::lookup_id(id).into()
                }});
                $ui.global::<crate::TR2>().on_lookup_str(move |id| crate::tr::lookup(id.as_str()).into());
                $ui.global::<crate::TR2>().on_format(move |id, args| {{
                    let translated = crate::tr::try_lookup(id.as_str()).unwrap();
                    let args = slint_keyos_platform::slint::Model::iter(&args).collect::<Vec<_>>();
                    slint_keyos_platform::i18n::replace_placeholders(translated, &args).into()
                }});
            }};
        }}
        "
        );
    }

    uwriteln!(src, "}}");

    std::fs::write(output_path, String::from(src))?;
    Ok(())
}

fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('\u{2028}', "\\n")
        .replace('\u{2026}', "...")
        .replace('\'', "'")
}

fn generate_slint_tr(
    all_translations: &BTreeMap<String, BTreeMap<String, String>>,
    gen_dir: &Path,
) -> anyhow::Result<()> {
    let keys = all_translations
        .get("en")
        .or_else(|| all_translations.values().next())
        .map(|translations| translations.keys().collect::<Vec<_>>())
        .unwrap();

    let mut src = Source::default();

    uwriteln!(src, "// AUTO-GENERATED FILE - DO NOT EDIT");
    uwriteln!(src, "");
    uwriteln!(src, "export enum TrId {{");
    for key in &keys {
        let enum_variant = key_to_pascal_case(key);
        uwriteln!(src, "{},", enum_variant);
    }
    uwriteln!(src, "}}");
    uwriteln!(src, "");

    uwriteln!(src, "export global TR2 {{");
    uwriteln!(src, "pure callback lookup(id: TrId) -> string;");
    uwriteln!(src, "pure callback lookup-str(id: string) -> string;");
    uwriteln!(src, "pure callback format(id: TrId, args: [string]) -> string;");
    uwriteln!(src, "}}");
    uwriteln!(src, "");

    GeneratedFile { path: PathBuf::from("tr.slint"), content: src.into() }.write(gen_dir)?;

    Ok(())
}

// "common.button.ok" -> "CommonButtonOk"
fn key_to_pascal_case(key: &str) -> String {
    key.split('.')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

/// generate an empty translations file for apps that don't have translations
pub fn generate_empty_translations(out_dir: &Path) -> anyhow::Result<()> {
    let output_path = out_dir.join("tr.rs");
    let mut src = Source::default();

    uwriteln!(
        src,
        "
        // AUTO-GENERATED FILE - DO NOT EDIT

        #[macro_export]
        macro_rules! init_tr {{
            ($ui:expr) => {{}};
        }}
    "
    );

    std::fs::write(output_path, String::from(src))?;

    Ok(())
}
