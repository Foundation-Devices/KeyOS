// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::config::{boxed_err, selected_targets, Config, Result};
use crate::package::target_archive_name;
use crate::release::validated_sdk_version;
use crate::util;

const HANDOFF_FORMAT_VERSION: u32 = 1;
const HANDOFF_MANIFEST_NAME: &str = "foundation-sdk-handoff.toml";
const MAX_MANIFEST_SIZE: u64 = 64 * 1024;

#[derive(Clone, Debug)]
pub struct ZipArgs {
    pub selector: String,
    pub destination: Option<PathBuf>,
    pub output_dir: PathBuf,
}

impl ZipArgs {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut output_dir = PathBuf::from("dist");
        let mut positional = Vec::new();
        let mut iter = raw.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--output-dir" => output_dir = PathBuf::from(next_value(&mut iter, "--output-dir")?),
                other if other.starts_with('-') => {
                    return Err(boxed_err(format!("unsupported zip option: {other}")));
                }
                _ => positional.push(arg),
            }
        }

        if positional.is_empty() || positional.len() > 2 {
            return Err(boxed_err("usage: cargo xtask zip <SELECTOR> [DESTINATION]"));
        }

        Ok(Self {
            selector: positional.remove(0),
            destination: positional.pop().map(PathBuf::from),
            output_dir,
        })
    }
}

#[derive(Clone, Debug)]
pub struct UnzipArgs {
    pub archive: PathBuf,
    pub output_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SyncArgs {
    pub selector: String,
    pub address: String,
    pub destination: String,
    pub output_dir: PathBuf,
}

impl SyncArgs {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut output_dir = PathBuf::from("dist");
        let mut positional = Vec::new();
        let mut iter = raw.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--output-dir" => output_dir = PathBuf::from(next_value(&mut iter, "--output-dir")?),
                other if other.starts_with('-') => {
                    return Err(boxed_err(format!("unsupported sync option: {other}")));
                }
                _ => positional.push(arg),
            }
        }

        if positional.len() != 3 {
            return Err(boxed_err(
                "usage: cargo xtask sync <SELECTOR> <ADDRESS> <DESTINATION> [--output-dir <PATH>]",
            ));
        }

        Ok(Self {
            selector: positional.remove(0),
            address: positional.remove(0),
            destination: positional.remove(0),
            output_dir,
        })
    }
}

impl UnzipArgs {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut output_dir = PathBuf::from("dist");
        let mut archive = None;
        let mut iter = raw.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--output-dir" => output_dir = PathBuf::from(next_value(&mut iter, "--output-dir")?),
                other if other.starts_with('-') => {
                    return Err(boxed_err(format!("unsupported unzip option: {other}")));
                }
                _ if archive.is_none() => archive = Some(PathBuf::from(arg)),
                _ => return Err(boxed_err("usage: cargo xtask unzip <ARCHIVE> [--output-dir <PATH>]")),
            }
        }

        Ok(Self {
            archive: archive
                .ok_or_else(|| boxed_err("usage: cargo xtask unzip <ARCHIVE> [--output-dir <PATH>]"))?,
            output_dir,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HandoffManifest {
    format_version: u32,
    sdk_version: String,
    targets: Vec<String>,
    archives: Vec<String>,
}

#[derive(Debug)]
struct InspectedHandoff {
    manifest: HandoffManifest,
}

struct StagedArchive {
    name: String,
    file: NamedTempFile,
    identical: bool,
}

pub fn zip(root: &Path, config: &Config, args: &ZipArgs) -> Result<()> {
    let version = validated_sdk_version(root, config, None)?;
    let targets = selected_targets(config, std::slice::from_ref(&args.selector))?;
    let output_dir = util::absolute_path(root, &args.output_dir);
    let archives = target_archive_names(&version, &targets);

    let sources = archives
        .iter()
        .map(|name| {
            let path = output_dir.join(name);
            if !path.is_file() {
                return Err(boxed_err(format!(
                    "missing packaged build archive: {} (run `just build {}` first)",
                    path.display(),
                    args.selector
                )));
            }
            Ok((name.clone(), path))
        })
        .collect::<Result<Vec<_>>>()?;

    let generated_name = format!("foundation-sdk-{version}-{}-handoff.zip", args.selector);
    let requested_destination = args.destination.as_deref().unwrap_or(&output_dir);
    let destination = resolve_zip_destination(root, requested_destination, &generated_name)?;

    if destination.exists() && !confirm_overwrite(std::slice::from_ref(&destination))? {
        println!("Kept existing handoff ZIP: {}", destination.display());
        return Ok(());
    }

    let manifest =
        HandoffManifest { format_version: HANDOFF_FORMAT_VERSION, sdk_version: version, targets, archives };
    write_handoff_zip(&destination, &manifest, &sources)?;

    println!("Wrote SDK build handoff: {}", destination.display());
    for target in &manifest.targets {
        println!("  {target}");
    }
    Ok(())
}

pub fn unzip(root: &Path, config: &Config, args: &UnzipArgs) -> Result<()> {
    let version = validated_sdk_version(root, config, None)?;
    let archive = util::absolute_path(root, &args.archive);
    if !archive.is_file() {
        return Err(boxed_err(format!("handoff ZIP does not exist: {}", archive.display())));
    }

    let inspected = inspect_handoff_zip(&archive, config, &version)?;
    let output_dir = util::absolute_path(root, &args.output_dir);
    fs::create_dir_all(&output_dir)?;
    let mut staged = stage_archives(&archive, &output_dir, &inspected.manifest.archives)?;

    println!("Handoff contains SDK {} builds:", inspected.manifest.sdk_version);
    for target in &inspected.manifest.targets {
        println!("  {target}");
    }

    let replacements = staged
        .iter()
        .filter(|archive| !archive.identical && output_dir.join(&archive.name).exists())
        .map(|archive| output_dir.join(&archive.name))
        .collect::<Vec<_>>();
    if !replacements.is_empty() && !confirm_overwrite(&replacements)? {
        println!("No build archives were imported.");
        return Ok(());
    }

    let mut imported = 0;
    let mut unchanged = 0;
    for staged_archive in staged.drain(..) {
        if staged_archive.identical {
            unchanged += 1;
            continue;
        }

        let destination = output_dir.join(&staged_archive.name);
        staged_archive.file.persist(&destination)?;
        imported += 1;
    }

    println!(
        "Imported {imported} build archive{} into {}{}.",
        if imported == 1 { "" } else { "s" },
        output_dir.display(),
        if unchanged == 0 { String::new() } else { format!(" ({unchanged} already identical)") }
    );
    println!("Ready to run:");
    println!("  just finalize {} {}", config.sdk.keyos_version, inspected.manifest.targets.join(" "));
    Ok(())
}

pub fn sync(root: &Path, config: &Config, args: &SyncArgs) -> Result<()> {
    let version = validated_sdk_version(root, config, None)?;
    let targets = selected_targets(config, std::slice::from_ref(&args.selector))?;
    let output_dir = util::absolute_path(root, &args.output_dir);
    let archives = target_archive_names(&version, &targets);
    let sources = archives
        .iter()
        .map(|name| {
            let path = output_dir.join(name);
            if !path.is_file() {
                return Err(boxed_err(format!(
                    "missing packaged build archive: {} (run `just build {}` first)",
                    path.display(),
                    args.selector
                )));
            }
            Ok(path)
        })
        .collect::<Result<Vec<_>>>()?;
    let remote = scp_destination(&args.address, &args.destination)?;

    println!("Sending SDK {version} build archives to {remote}:");
    for source in &sources {
        println!("  {}", source.file_name().unwrap_or_default().to_string_lossy());
    }

    let mut command = scp_command(&sources, &remote)?;
    util::run_command(&mut command, false)?;
    println!("Synced {} build archives to {remote}.", sources.len());
    Ok(())
}

fn scp_destination(address: &str, destination: &str) -> Result<String> {
    if address.is_empty() || address.starts_with('-') || address.chars().any(char::is_whitespace) {
        return Err(boxed_err(
            "SCP address must be a non-empty SSH host without whitespace and must not start with '-'",
        ));
    }
    if destination.is_empty() || destination.contains(['\r', '\n']) {
        return Err(boxed_err("SCP destination must be a non-empty remote path on one line"));
    }

    let separator = if destination.ends_with('/') { "" } else { "/" };
    Ok(format!("{address}:{destination}{separator}"))
}

fn scp_command(sources: &[PathBuf], remote: &str) -> Result<Command> {
    if let Some(source) =
        sources.iter().find(|source| source.as_os_str().as_encoded_bytes().starts_with(b"-"))
    {
        return Err(boxed_err(format!("SCP source path must not start with '-': {}", source.display())));
    }
    if remote.starts_with('-') {
        return Err(boxed_err("SCP remote destination must not start with '-'"));
    }

    let mut command = Command::new("scp");
    command.args(sources).arg(remote);
    Ok(command)
}

fn resolve_zip_destination(root: &Path, requested: &Path, generated_name: &str) -> Result<PathBuf> {
    let requested = util::absolute_path(root, requested);
    if requested.is_dir() {
        return Ok(requested.join(generated_name));
    }

    let has_zip_extension = requested
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"));
    if !has_zip_extension {
        return Err(boxed_err(format!(
            "ZIP destination must be an existing directory or a .zip file path: {}",
            requested.display()
        )));
    }
    let parent = requested.parent().ok_or_else(|| {
        boxed_err(format!("ZIP destination has no parent directory: {}", requested.display()))
    })?;
    if !parent.is_dir() {
        return Err(boxed_err(format!(
            "ZIP destination directory does not exist (is the USB drive mounted?): {}",
            parent.display()
        )));
    }
    Ok(requested)
}

fn write_handoff_zip(
    destination: &Path,
    manifest: &HandoffManifest,
    sources: &[(String, PathBuf)],
) -> Result<()> {
    let parent = destination.parent().ok_or_else(|| {
        boxed_err(format!("ZIP destination has no parent directory: {}", destination.display()))
    })?;
    let temporary = NamedTempFile::new_in(parent)?;
    let mut writer = ZipWriter::new(temporary);

    let manifest_text = toml::to_string(manifest)?;
    writer.start_file(HANDOFF_MANIFEST_NAME, stored_file_options())?;
    writer.write_all(manifest_text.as_bytes())?;

    for (name, source) in sources {
        writer.start_file(name, stored_file_options())?;
        let mut input = File::open(source)?;
        io::copy(&mut input, &mut writer)?;
    }

    let temporary = writer.finish()?;
    temporary.as_file().sync_all()?;
    temporary.persist(destination)?;
    Ok(())
}

fn stored_file_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true)
        .unix_permissions(0o644)
}

fn inspect_handoff_zip(path: &Path, config: &Config, expected_version: &str) -> Result<InspectedHandoff> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    let mut entries = BTreeSet::new();

    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name();
        if !entry.is_file() || Path::new(name).file_name() != Some(OsStr::new(name)) {
            return Err(boxed_err(format!("handoff ZIP contains a non-file or nested path: {name}")));
        }
        if entry.compression() != CompressionMethod::Stored {
            return Err(boxed_err(format!(
                "handoff ZIP entry uses unsupported compression (expected stored): {name}"
            )));
        }
        if !entries.insert(name.to_string()) {
            return Err(boxed_err(format!("handoff ZIP contains duplicate entry: {name}")));
        }
    }

    if !entries.contains(HANDOFF_MANIFEST_NAME) {
        return Err(boxed_err(format!("handoff ZIP is missing {HANDOFF_MANIFEST_NAME}")));
    }

    let mut manifest_entry = archive.by_name(HANDOFF_MANIFEST_NAME)?;
    if manifest_entry.size() > MAX_MANIFEST_SIZE {
        return Err(boxed_err("handoff manifest is unexpectedly large"));
    }
    let mut manifest_text = String::new();
    manifest_entry.read_to_string(&mut manifest_text)?;
    let manifest: HandoffManifest = toml::from_str(&manifest_text)?;
    drop(manifest_entry);

    if manifest.format_version != HANDOFF_FORMAT_VERSION {
        return Err(boxed_err(format!(
            "unsupported handoff format version {}; expected {HANDOFF_FORMAT_VERSION}",
            manifest.format_version
        )));
    }
    if manifest.sdk_version != expected_version {
        return Err(boxed_err(format!(
            "handoff SDK version {} does not match this checkout's SDK version {expected_version}",
            manifest.sdk_version
        )));
    }
    if manifest.targets.is_empty() {
        return Err(boxed_err("handoff ZIP contains no SDK targets"));
    }

    let target_set = manifest.targets.iter().collect::<BTreeSet<_>>();
    if target_set.len() != manifest.targets.len() {
        return Err(boxed_err("handoff manifest contains duplicate targets"));
    }
    for target in &manifest.targets {
        if !config.targets.triples.contains(target) {
            return Err(boxed_err(format!("handoff contains unconfigured SDK target: {target}")));
        }
    }

    let expected_archives = target_archive_names(&manifest.sdk_version, &manifest.targets);
    if manifest.archives != expected_archives {
        return Err(boxed_err("handoff manifest archive list does not match its included targets"));
    }
    let expected_entries =
        std::iter::once(HANDOFF_MANIFEST_NAME.to_string()).chain(expected_archives).collect::<BTreeSet<_>>();
    if entries != expected_entries {
        return Err(boxed_err("handoff ZIP contents do not match its manifest"));
    }

    Ok(InspectedHandoff { manifest })
}

fn stage_archives(path: &Path, output_dir: &Path, names: &[String]) -> Result<Vec<StagedArchive>> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    let mut staged = Vec::new();

    for name in names {
        let destination = output_dir.join(name);
        if destination.exists() && !destination.is_file() {
            return Err(boxed_err(format!(
                "build archive destination is not a file: {}",
                destination.display()
            )));
        }

        let mut entry = archive.by_name(name)?;
        let mut file = NamedTempFile::new_in(output_dir)?;
        io::copy(&mut entry, file.as_file_mut())?;
        file.as_file_mut().flush()?;
        let identical = destination.is_file() && files_equal(file.path(), &destination)?;
        staged.push(StagedArchive { name: name.clone(), file, identical });
    }

    Ok(staged)
}

fn files_equal(first: &Path, second: &Path) -> Result<bool> {
    if first.metadata()?.len() != second.metadata()?.len() {
        return Ok(false);
    }

    let mut first = BufReader::new(File::open(first)?);
    let mut second = BufReader::new(File::open(second)?);
    let mut first_buffer = [0_u8; 64 * 1024];
    let mut second_buffer = [0_u8; 64 * 1024];

    loop {
        let first_len = first.read(&mut first_buffer)?;
        let second_len = second.read(&mut second_buffer)?;
        if first_len != second_len || first_buffer[..first_len] != second_buffer[..second_len] {
            return Ok(false);
        }
        if first_len == 0 {
            return Ok(true);
        }
    }
}

fn target_archive_names(version: &str, targets: &[String]) -> Vec<String> {
    targets.iter().map(|target| target_archive_name(version, target)).collect()
}

fn confirm_overwrite(paths: &[PathBuf]) -> Result<bool> {
    println!("The following files already exist:");
    for path in paths {
        println!("  {}", path.display());
    }
    print!("Overwrite {}? [y/N] ", if paths.len() == 1 { "it" } else { "them" });
    io::stdout().flush()?;

    let mut response = String::new();
    if io::stdin().read_line(&mut response)? == 0 {
        return Err(boxed_err("overwrite confirmation requires interactive input"));
    }
    Ok(matches!(response.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    iter.next().ok_or_else(|| boxed_err(format!("missing value for {flag}")))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{
        inspect_handoff_zip, scp_command, scp_destination, unzip, write_handoff_zip, zip, HandoffManifest,
        SyncArgs, UnzipArgs, ZipArgs, HANDOFF_FORMAT_VERSION,
    };
    use crate::config::{Config, TargetsConfig};

    const VERSION: &str = "1.2.3";
    const TARGET: &str = "x86_64-unknown-linux-gnu";

    #[test]
    fn zip_args_accept_selector_and_usb_destination() {
        let args = ZipArgs::parse(vec!["linux-all".into(), "/media/usb".into()]).unwrap();
        assert_eq!(args.selector, "linux-all");
        assert_eq!(args.destination, Some(PathBuf::from("/media/usb")));
        assert_eq!(args.output_dir, PathBuf::from("dist"));
    }

    #[test]
    fn unzip_args_require_one_archive() {
        let args = UnzipArgs::parse(vec!["/media/usb/builds.zip".into()]).unwrap();
        assert_eq!(args.archive, PathBuf::from("/media/usb/builds.zip"));
        assert!(UnzipArgs::parse(Vec::new()).is_err());
        assert!(UnzipArgs::parse(vec!["one.zip".into(), "two.zip".into()]).is_err());
    }

    #[test]
    fn sync_args_accept_address_destination_and_selector() {
        let args = SyncArgs::parse(vec![
            "linux-all".into(),
            "ken@macbook.local".into(),
            "/Users/ken/foundation/KeyOS/sdk/dist".into(),
        ])
        .unwrap();
        assert_eq!(args.selector, "linux-all");
        assert_eq!(args.address, "ken@macbook.local");
        assert_eq!(args.destination, "/Users/ken/foundation/KeyOS/sdk/dist");
        assert_eq!(args.output_dir, PathBuf::from("dist"));
    }

    #[test]
    fn scp_command_sends_each_archive_to_the_remote_directory() {
        let sources = [PathBuf::from("/tmp/linux-arm.tar.gz"), PathBuf::from("/tmp/linux-x86.tar.gz")];
        let remote = scp_destination("ken@macbook.local", "/Users/ken/sdk/dist").unwrap();
        let command = scp_command(&sources, &remote).unwrap();

        assert_eq!(command.get_program(), "scp");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["/tmp/linux-arm.tar.gz", "/tmp/linux-x86.tar.gz", "ken@macbook.local:/Users/ken/sdk/dist/"]
        );
    }

    #[test]
    fn scp_destination_rejects_ambiguous_addresses_and_multiline_paths() {
        assert!(scp_destination("", "/Users/ken/sdk/dist").is_err());
        assert!(scp_destination("-Fconfig", "/Users/ken/sdk/dist").is_err());
        assert!(scp_destination("ken@macbook local", "/Users/ken/sdk/dist").is_err());
        assert!(scp_destination("ken@macbook.local", "/Users/ken/sdk\ndist").is_err());
    }

    #[test]
    fn scp_command_rejects_option_like_paths() {
        let remote = "ken@macbook.local:/Users/ken/sdk/dist/";
        assert!(scp_command(&[PathBuf::from("-archive.tar.gz")], remote).is_err());
        assert!(scp_command(&[PathBuf::from("archive.tar.gz")], "-Fconfig").is_err());
    }

    #[test]
    fn handoff_round_trip_transfers_only_target_archives() {
        let source = tempfile::tempdir().unwrap();
        write_workspace_versions(source.path());
        let source_dist = source.path().join("dist");
        fs::create_dir(&source_dist).unwrap();
        fs::write(source_dist.join(format!("foundation-sdk-{VERSION}-{TARGET}.tar.gz")), b"target").unwrap();

        let usb = tempfile::tempdir().unwrap();
        let config = test_config();
        zip(
            source.path(),
            &config,
            &ZipArgs {
                selector: TARGET.to_string(),
                destination: Some(usb.path().to_path_buf()),
                output_dir: PathBuf::from("dist"),
            },
        )
        .unwrap();
        let handoff = usb.path().join(format!("foundation-sdk-{VERSION}-{TARGET}-handoff.zip"));
        let inspected = inspect_handoff_zip(&handoff, &config, VERSION).unwrap();
        assert_eq!(inspected.manifest.targets, [TARGET]);
        assert_eq!(inspected.manifest.archives, [format!("foundation-sdk-{VERSION}-{TARGET}.tar.gz")]);

        let destination = tempfile::tempdir().unwrap();
        write_workspace_versions(destination.path());
        let destination_dist = destination.path().join("dist");
        fs::create_dir(&destination_dist).unwrap();
        fs::write(
            destination_dist.join(format!("foundation-sdk-{VERSION}-common.tar.gz")),
            b"destination-common",
        )
        .unwrap();
        unzip(
            destination.path(),
            &config,
            &UnzipArgs { archive: handoff, output_dir: PathBuf::from("dist") },
        )
        .unwrap();

        // Re-importing an unchanged handoff must preserve the existing files
        // without asking for overwrite confirmation.
        let handoff = usb.path().join(format!("foundation-sdk-{VERSION}-{TARGET}-handoff.zip"));
        unzip(
            destination.path(),
            &config,
            &UnzipArgs { archive: handoff, output_dir: PathBuf::from("dist") },
        )
        .unwrap();

        assert_eq!(
            fs::read(destination.path().join(format!("dist/foundation-sdk-{VERSION}-common.tar.gz")))
                .unwrap(),
            b"destination-common"
        );
        assert_eq!(
            fs::read(destination.path().join(format!("dist/foundation-sdk-{VERSION}-{TARGET}.tar.gz")))
                .unwrap(),
            b"target"
        );
    }

    #[test]
    fn handoff_inspection_rejects_files_not_declared_by_its_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let target_name = format!("foundation-sdk-{VERSION}-{TARGET}.tar.gz");
        let target = temp.path().join(&target_name);
        let unexpected = temp.path().join("unexpected.txt");
        fs::write(&target, b"target").unwrap();
        fs::write(&unexpected, b"unexpected").unwrap();

        let handoff = temp.path().join("handoff.zip");
        let manifest = HandoffManifest {
            format_version: HANDOFF_FORMAT_VERSION,
            sdk_version: VERSION.to_string(),
            targets: vec![TARGET.to_string()],
            archives: vec![target_name.clone()],
        };
        write_handoff_zip(
            &handoff,
            &manifest,
            &[(target_name, target), ("unexpected.txt".into(), unexpected)],
        )
        .unwrap();

        let error = inspect_handoff_zip(&handoff, &test_config(), VERSION).unwrap_err();
        assert!(error.to_string().contains("contents do not match"));
    }

    fn test_config() -> Config {
        let mut config = Config::default();
        config.sdk.version = VERSION.to_string();
        config.targets = TargetsConfig { triples: vec![TARGET.to_string()], overrides: Default::default() };
        config
    }

    fn write_workspace_versions(root: &Path) {
        fs::create_dir_all(root.join("crates/cli")).unwrap();
        let manifest = format!("[workspace.package]\nversion = \"{VERSION}\"\n");
        fs::write(root.join("Cargo.toml"), &manifest).unwrap();
        fs::write(root.join("crates/cli/Cargo.toml"), manifest).unwrap();
    }
}
