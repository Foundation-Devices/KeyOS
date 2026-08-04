// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! The KeyOS app archive: one file carrying one app bundle, so an app can be installed from the
//! local filesystem instead of over usb-debug.
//!
//! An archive is a gzip-wrapped tar stream whose entries are the bundle's files at the archive
//! root: `manifest.json` first, then every file it hashes, sorted by name.
//!
//! ```text
//! manifest.json
//! app.elf
//! icon-dark.bin    optional
//! icon.bin         optional
//! resources/...    optional, any depth
//! ```
//!
//! `manifest.json` comes first because tar is read in a single pass: a reader learns the app id
//! and the declared permissions, and can reject the archive, before writing megabytes to flash.
//!
//! The archive carries no signature of its own. `manifest.json` is cosign2-signed and its
//! `fileHashes` cover every other file in the bundle, so trust is decided from the unpacked
//! bundle. Nothing outside the signed manifest may influence where an app lands or what it may
//! do: an SDK app and a Flux app produce the same archive, and which one it is follows from the
//! servers the manifest declares.

#[cfg(feature = "pack")]
mod pack;
#[cfg(feature = "unpack")]
mod unpack;

#[cfg(feature = "pack")]
pub use pack::{pack_bundle, PackError, PackReport};
#[cfg(feature = "unpack")]
pub use unpack::decode;

/// Extension of an app archive, without the dot.
///
/// A single dot-segment, not `tar.gz`: the file picker matches only the segment after the last
/// dot, so a two-segment extension would offer the user every gzip file on the drive.
pub const ARCHIVE_EXTENSION: &str = "app";

/// The signed manifest, and the archive's first entry.
pub const MANIFEST_FILE: &str = "manifest.json";

/// The app binary.
pub const ELF_FILE: &str = "app.elf";

/// The app's launcher icon.
pub const ICON_FILE: &str = "icon.bin";

/// The icon's optional dark-theme variant.
pub const ICON_DARK_FILE: &str = "icon-dark.bin";

/// Directory prefix of the app's runtime resources.
pub const RESOURCES_DIR: &str = "resources";

/// The most an archive may unpack to.
///
/// The one cap the format carries. It bounds a gzip bomb, which is the only thing a reader cannot
/// simply fail on: everything else about a malformed archive surfaces as an install error.
pub const MAX_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;

/// The conventional file name for an app's archive, e.g. `my-app.app`.
pub fn archive_file_name(app_name: &str) -> String { format!("{app_name}.{ARCHIVE_EXTENSION}") }
