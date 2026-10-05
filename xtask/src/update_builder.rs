// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::Context;
use update_image::{Action, ReleaseManifest, Transaction, Version};

/// build an unsigned update tar from two signed firmware trees.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// version before the update, checked against the base firmware header.
    pub base_version: String,
    /// signed base tree containing keyos/.
    pub base: PathBuf,
    /// version after the update, checked against the target firmware header.
    pub new_version: String,
    /// signed target tree containing keyos/.
    pub new: PathBuf,
    /// release label.
    #[arg(long, default_value = "KeyOS Release")]
    pub label: String,
    /// flag indicating whether this release is mandatory.
    #[arg(long)]
    pub mandatory: bool,
    /// flag indicating whether a reboot is required after this release.
    #[arg(long)]
    pub reboot_required: bool,
    /// output path for the unsigned update tar. parent directories are created.
    #[arg(short, long, default_value = "release.tar")]
    pub out: PathBuf,
    /// overwrite an existing output file.
    #[arg(long)]
    pub force: bool,
    /// encoder strategy: normal or forward. forward avoids backward and
    /// overlapping source reads to work around the 1.4.0 cache bug.
    #[arg(long, default_value = "normal")]
    pub strategy: update_image::patch::Strategy,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    fn files(dir: &Path, root: &Path, paths: &mut BTreeSet<PathBuf>) -> anyhow::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                files(&entry.path(), root, paths)?;
            } else if kind.is_file() {
                paths.insert(entry.path().strip_prefix(root)?.to_owned());
            }
        }
        Ok(())
    }

    let date = chrono::DateTime::from_timestamp(crate::utils::GIT_TIMESTAMP.parse()?, 0)
        .context("source timestamp is out of range")?
        .date_naive()
        .to_string();
    let base_version = ota_patch_version(&args.base_version)?;
    let new_version = ota_patch_version(&args.new_version)?;
    let old_header = Version::parse(&base_version).map_err(anyhow::Error::msg)?;
    let new_header = Version::parse(&new_version).map_err(anyhow::Error::msg)?;
    let format = crate::release_generator::patch_format(old_header);
    anyhow::ensure!(args.force || !args.out.exists(), "output {} already exists", args.out.display());
    let base = args.base.join("keyos");
    let new = args.new.join("keyos");
    for (tree, version) in [(&base, &args.base_version), (&new, &args.new_version)] {
        let path = tree.join("app.bin");
        let mut bytes = [0; 0x800];
        fs::File::open(&path)
            .with_context(|| format!("open {}", path.display()))?
            .read_exact(&mut bytes)
            .with_context(|| format!("read firmware header from {}", path.display()))?;
        let header = cosign2::Header::parse_unverified(&bytes, bytes.len(), false)
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("invalid firmware header in {}", path.display()))?
            .with_context(|| format!("missing firmware header in {}", path.display()))?;
        anyhow::ensure!(
            crate::parse_keyos_version(header.version()).map_err(anyhow::Error::msg)?
                == crate::parse_keyos_version(version).map_err(anyhow::Error::msg)?,
            "{}: requested version {version}, firmware header version {}",
            path.display(),
            header.version()
        );
    }
    let mut old_files = BTreeSet::new();
    let mut new_files = BTreeSet::new();
    files(&base, &base, &mut old_files)?;
    files(&new, &new, &mut new_files)?;
    let staging = tempfile::tempdir()?;
    let patches = staging.path().join("patch");
    fs::create_dir(&patches)?;
    let mut actions = Vec::new();
    for path in old_files.union(&new_files) {
        let device_path = path.to_str().context("update paths must be UTF8")?.to_owned();
        let patch_file = format!("keyos/{device_path}");
        let output = patches.join(&patch_file);
        let action = if !new_files.contains(path) {
            Action::Delete { path: device_path }
        } else if !old_files.contains(path) {
            fs::create_dir_all(output.parent().unwrap())?;
            fs::copy(new.join(path), &output)?;
            Action::Add { source: patch_file, dest: device_path }
        } else {
            let old = fs::read(base.join(path))?;
            let new = fs::read(new.join(path))?;
            if old == new {
                continue;
            }
            println!("building {patch_file}");
            fs::create_dir_all(output.parent().unwrap())?;
            update_image::patch::build(
                &old,
                old_header,
                &new,
                new_header,
                format,
                args.strategy,
                fs::File::create(&output)?,
            )
            .with_context(|| format!("building {patch_file}"))?;
            Action::Patch {
                patch_file,
                patch_source: device_path,
                base_version: base_version.clone(),
                new_version: new_version.clone(),
            }
        };
        actions.push(action);
    }
    let manifest = ReleaseManifest {
        label: args.label,
        mandatory: args.mandatory,
        reboot_required: args.reboot_required,
        date,
        transactions: vec![Transaction::new(actions)],
    };
    let manifest_path = staging.path().join("manifest.json");
    fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
    if let Some(parent) = args.out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut tar = tar::Builder::new(fs::File::create(&args.out)?);
    tar.mode(tar::HeaderMode::Deterministic);
    append_tree(&mut tar, &patches, Path::new("patch"))?;
    tar.append_path_with_name(manifest_path, "manifest.json")?;
    tar.finish()?;
    println!("wrote {}", args.out.display());
    Ok(())
}

fn append_tree(tar: &mut tar::Builder<fs::File>, source: &Path, destination: &Path) -> std::io::Result<()> {
    tar.append_dir(destination, source)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let destination = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            append_tree(tar, &entry.path(), &destination)?;
        } else {
            // fixed permissions keep host executable bits out of the archive
            let bytes = fs::read(entry.path())?;
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            tar.append_data(&mut header, destination, bytes.as_slice())?;
        }
    }
    Ok(())
}

fn ota_patch_version(input: &str) -> anyhow::Result<String> {
    let version = crate::parse_keyos_version(input).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(version.build.is_empty(), "update versions may not contain build metadata");
    let mut wire = format!("v{}.{}.{}", version.major, version.minor, version.patch);
    if !version.pre.is_empty() {
        let pre = version.pre.as_str();
        let (number, offset, maximum) = if let Some(number) = pre.strip_prefix("alpha") {
            (number, 128, 126)
        } else if let Some(number) = pre.strip_prefix("beta") {
            (number, 0, 127)
        } else {
            anyhow::bail!("update prereleases must use alphaN or betaN");
        };
        let sequence: u8 = number.parse()?;
        anyhow::ensure!(
            sequence > 0 && sequence <= maximum && number == sequence.to_string(),
            "invalid update prerelease: {pre}"
        );
        wire.push_str(&format!("b{}", sequence + offset));
    }
    Ok(wire)
}
