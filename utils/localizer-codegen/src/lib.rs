// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Build-time translation codegen shared by Slint apps and headless services.
//!
//! [`localizer`] emits a `mod tr` (its `TrId` enum, phf-backed string tables, and lookup helpers)
//! from a crate's `i18n/*.json`. A Slint app also gets a `tr.slint` and the `init_tr!` macro that
//! wires the Slint `TR`/`TR2` globals; a headless service (see [`compile_service_translations`])
//! skips both and defines `TrId` itself, so it only needs `phf`, `serde_json`, and `anyhow`. Both
//! paths run the same generator, so a string shared by a service and a Slint app produces the same
//! `TrId` variant and renders identically.

use std::path::PathBuf;

pub mod generated_file;
pub mod localizer;
pub mod source;

pub use localizer::{build_translations, generate_empty_translations, TranslationOptions};

/// Generate a headless service's localized strings from its `i18n/*.json` into `$OUT_DIR/tr.rs`, a
/// self-contained `mod tr` with no Slint. Call this from the service's `build.rs`, then
/// `include!(concat!(env!("OUT_DIR"), "/tr.rs"))` at the crate root to get `tr::TrId`,
/// `tr::lookup_id`, `tr::set_locale_str`, and friends. The crate must depend on `phf`, and be
/// registered in `localizer.json` so `just localize` (re)generates its catalog.
pub fn compile_service_translations() {
    let out_dir =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo for build scripts"));
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"));
    let i18n_dir = manifest_dir.join("i18n");

    let options = TranslationOptions { slint: false, include_time_localization: false };
    // gen_dir is only used for the Slint output, which a service does not emit.
    build_translations(&i18n_dir, &out_dir, &out_dir, &options).expect("generating service translations");
}
