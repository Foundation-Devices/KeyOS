// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Building an archive from a staged bundle directory.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{MANIFEST_FILE, MAX_BUNDLE_BYTES};

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("cannot write the archive to {path}: it is inside the app bundle {dir}, whose files it packs")]
    OutputInsideBundle { path: PathBuf, dir: PathBuf },

    #[error("cannot write the archive to {0}: it is a symlink, and the write would land on its target")]
    OutputIsSymlink(PathBuf),

    #[error("app bundle unpacks to {stream_bytes} bytes, over the {MAX_BUNDLE_BYTES} an install accepts")]
    TooLarge { stream_bytes: u64 },

    #[error("could not read {path}: {source:?}")]
    Read { path: PathBuf, source: io::Error },

    #[error("could not write {path}: {source:?}")]
    Write { path: PathBuf, source: io::Error },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackReport {
    pub archive_path: PathBuf,
    pub entries: usize,
    /// Total size of the bundle files that went in.
    pub bundle_bytes: u64,
    /// Size of the archive itself, after compression.
    pub archive_bytes: u64,
}

/// Pack a staged bundle directory into a single gzip-compressed archive at `archive_path`,
/// creating the directories that path names.
///
/// `hashed_files` are the bundle-relative names the manifest's `fileHashes` covers, which is every
/// file the bundle holds bar the manifest itself. The archive carries those and the manifest, so
/// it holds exactly what the signature covers.
pub fn pack_bundle(
    bundle_dir: &Path,
    archive_path: &Path,
    hashed_files: &[String],
) -> Result<PackReport, PackError> {
    reject_output_inside_bundle(bundle_dir, archive_path)?;

    // Sorted, so the archive does not depend on the order the caller happens to list them in.
    let mut names: Vec<&str> = hashed_files.iter().map(String::as_str).collect();
    names.sort_unstable();
    let entries: Vec<Entry> = std::iter::once(MANIFEST_FILE)
        .chain(names)
        .map(|name| Entry { name: name.to_string(), source: bundle_dir.join(name) })
        .collect();

    // Before anything is created: an archive written and then refused leaves a complete .app on
    // disk beside the error, which reads as packed. Counted as the reader will see it, framing
    // included, or a bundle just under the cap packs and is then cut short on device: a 512-byte
    // header and up to 511 of padding per entry, rounded up to a conservative kibibyte, plus the
    // archive's trailer.
    let mut bundle_bytes = 0u64;
    let mut stream_bytes = 1024u64;
    for entry in &entries {
        let size = fs::metadata(&entry.source)
            .map_err(|source| PackError::Read { path: entry.source.clone(), source })?
            .len();
        bundle_bytes = bundle_bytes.saturating_add(size);
        // A name over the 100 bytes a tar header holds gets a GNU long-name record of its own,
        // which is another header and another block.
        let framing = if entry.name.len() > 100 { 2048 } else { 1024 };
        stream_bytes = stream_bytes.saturating_add(size).saturating_add(framing);
    }
    if stream_bytes > MAX_BUNDLE_BYTES {
        return Err(PackError::TooLarge { stream_bytes });
    }

    fs::create_dir_all(output_parent(archive_path))
        .map_err(|source| PackError::Write { path: archive_path.to_path_buf(), source })?;
    let archive = fs::File::create(archive_path)
        .map_err(|source| PackError::Write { path: archive_path.to_path_buf(), source })?;
    let encoder = flate2::write::GzEncoder::new(archive, flate2::Compression::default());
    let encoder = write_entries(encoder, &entries, archive_path)?;
    encoder.finish().map_err(|source| PackError::Write { path: archive_path.to_path_buf(), source })?;

    let archive_bytes = fs::metadata(archive_path)
        .map_err(|source| PackError::Read { path: archive_path.to_path_buf(), source })?
        .len();

    Ok(PackReport {
        archive_path: archive_path.to_path_buf(),
        entries: entries.len(),
        bundle_bytes,
        archive_bytes,
    })
}

/// One file to write, as its archive entry name and where to read it from.
struct Entry {
    name: String,
    source: PathBuf,
}

/// Write every entry as a tar stream, returning the writer.
fn write_entries<W: Write>(writer: W, entries: &[Entry], archive_path: &Path) -> Result<W, PackError> {
    let mut builder = tar::Builder::new(writer);

    for entry in entries {
        let data = fs::read(&entry.source)
            .map_err(|source| PackError::Read { path: entry.source.clone(), source })?;

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        // Carry nothing from the packing host: the same bundle must produce the same archive
        // bytes wherever it is packed (see REPRODUCIBILITY.md).
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        builder
            .append_data(&mut header, &entry.name, data.as_slice())
            .map_err(|source| PackError::Write { path: archive_path.to_path_buf(), source })?;
    }

    // No directory entries for `resources/`: an unpacker creates parents as it writes, and the
    // fewer entry types the archive uses, the less a reader has to accept.
    builder.into_inner().map_err(|source| PackError::Write { path: archive_path.to_path_buf(), source })
}

/// Refuse an output path inside the bundle being packed, or one that is a symlink.
///
/// Creating the archive truncates whatever is at that path, before the entries are read, so an
/// output naming a bundle file destroys it and packs the empty remains under its name. A symlink
/// is refused wherever it sits, since the create follows it.
fn reject_output_inside_bundle(bundle_dir: &Path, archive_path: &Path) -> Result<(), PackError> {
    if fs::symlink_metadata(archive_path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(PackError::OutputIsSymlink(archive_path.to_path_buf()));
    }

    let (Ok(bundle), Some(parent)) =
        (fs::canonicalize(bundle_dir), existing_ancestor(output_parent(archive_path)))
    else {
        return Ok(());
    };

    if parent.starts_with(&bundle) {
        return Err(PackError::OutputInsideBundle {
            path: archive_path.to_path_buf(),
            dir: bundle_dir.to_path_buf(),
        });
    }
    Ok(())
}

/// The directory an archive is written into. A bare file name has an empty parent rather than
/// none, and canonicalizing that fails, so it resolves to the working directory.
fn output_parent(archive_path: &Path) -> &Path {
    archive_path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

/// Resolve `dir` by the nearest ancestor of it that exists. The directories an output names are
/// created rather than required, so resolving `dir` itself would place an output nowhere until
/// after the very directories that must not be created inside a bundle already were.
fn existing_ancestor(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .map(|ancestor| if ancestor.as_os_str().is_empty() { Path::new(".") } else { ancestor })
        .find_map(|ancestor| fs::canonicalize(ancestor).ok())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::ELF_FILE;

    /// A bundle holding every file the format carries, and a directory to write archives into.
    struct Fixture {
        bundle: tempfile::TempDir,
        out: tempfile::TempDir,
        hashed_files: Vec<String>,
    }

    impl Fixture {
        fn new() -> Self {
            let bundle = tempfile::tempdir().unwrap();
            let dir = bundle.path();
            fs::write(dir.join(MANIFEST_FILE), b"signed manifest").unwrap();
            fs::create_dir_all(dir.join("resources/images")).unwrap();
            let hashed_files = ["app.elf", "icon.bin", "icon-dark.bin", "resources/images/logo.bin"];
            for file in hashed_files {
                fs::write(dir.join(file), file.as_bytes()).unwrap();
            }
            Self {
                bundle,
                out: tempfile::tempdir().unwrap(),
                hashed_files: hashed_files.map(str::to_string).into(),
            }
        }

        fn bundle_dir(&self) -> &Path { self.bundle.path() }

        fn archive_path(&self, name: &str) -> PathBuf { self.out.path().join(name) }

        fn pack_to(&self, archive_path: &Path) -> Result<PackReport, PackError> {
            pack_bundle(self.bundle_dir(), archive_path, &self.hashed_files)
        }

        fn pack(&self) -> Result<PackReport, PackError> { self.pack_to(&self.archive_path("example.app")) }
    }

    fn entry_names(archive: &Path) -> Vec<String> {
        let bytes = fs::read(archive).unwrap();
        tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(bytes)))
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().display().to_string())
            .collect()
    }

    #[test]
    fn packs_the_manifest_and_what_it_hashes_with_the_manifest_first() {
        let fixture = Fixture::new();

        let report = fixture.pack().unwrap();

        assert_eq!(report.entries, 5);
        assert_eq!(
            entry_names(&report.archive_path),
            ["manifest.json", "app.elf", "icon-dark.bin", "icon.bin", "resources/images/logo.bin"]
        );
    }

    /// The caller's order must not reach the archive, or two hosts listing the same bundle
    /// differently would pack it to different bytes.
    #[test]
    fn packing_the_same_bundle_twice_gives_the_same_bytes() {
        let fixture = Fixture::new();
        let first = fixture.archive_path("first.app");
        let second = fixture.archive_path("second.app");

        fixture.pack_to(&first).unwrap();
        let mut reversed = fixture.hashed_files.clone();
        reversed.reverse();
        pack_bundle(fixture.bundle_dir(), &second, &reversed).unwrap();

        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
    }

    /// Creating the archive truncates what is at that path, and the entries are read afterwards:
    /// the binary would be destroyed and packed as nothing.
    #[test]
    fn an_archive_written_over_a_bundle_file_is_rejected() {
        let fixture = Fixture::new();
        let elf = fixture.bundle_dir().join(ELF_FILE);

        let error = fixture.pack_to(&elf).unwrap_err();

        assert!(matches!(error, PackError::OutputInsideBundle { .. }), "{error}");
        assert_eq!(fs::read(&elf).unwrap(), ELF_FILE.as_bytes(), "the bundle is untouched");
    }

    /// The directories an output names are created, so the guard has to place the output before
    /// they exist: resolving the output's own parent would refuse nothing until the directory it
    /// was refusing to create inside the bundle already existed.
    #[test]
    fn an_output_directory_is_created_but_never_inside_the_bundle() {
        let fixture = Fixture::new();

        let nested = fixture.archive_path("dist/example.app");
        fixture.pack_to(&nested).unwrap();
        assert!(nested.is_file());

        let inside = fixture.bundle_dir().join("resources/dist/example.app");
        let error = fixture.pack_to(&inside).unwrap_err();

        assert!(matches!(error, PackError::OutputInsideBundle { .. }), "{error}");
        assert!(!fixture.bundle_dir().join("resources/dist").exists(), "the refusal created nothing");
    }

    /// `--out app.elf` is the natural way to write it, and a bare name has an empty parent rather
    /// than none: canonicalizing that fails, which would skip the guard entirely.
    #[test]
    fn a_bare_output_name_resolves_to_the_working_directory() {
        assert_eq!(output_parent(Path::new("example.app")), Path::new("."));
        assert_eq!(output_parent(Path::new("out/example.app")), Path::new("out"));
        assert_eq!(output_parent(Path::new("/tmp/example.app")), Path::new("/tmp"));
    }

    /// The create follows a symlink, so an output symlinked at a bundle file would truncate the
    /// source while its parent sits innocently outside the bundle.
    #[cfg(unix)]
    #[test]
    fn an_output_that_is_a_symlink_is_rejected() {
        let fixture = Fixture::new();
        let out = fixture.archive_path("example.app");
        std::os::unix::fs::symlink(fixture.bundle_dir().join(ELF_FILE), &out).unwrap();

        let error = fixture.pack_to(&out).unwrap_err();

        assert!(matches!(error, PackError::OutputIsSymlink(_)), "{error}");
        assert_eq!(fs::read(fixture.bundle_dir().join(ELF_FILE)).unwrap(), ELF_FILE.as_bytes());
    }
}
