// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Verifies and uploads one packaged KeyOS SDK documentation release to GitHub.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::Args;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::{
    builder::project_root,
    docs_api::{
        acquire_docs_bundle_lock, file_sha256, read_bundle_manifest, sdk_docs_bundle_dir,
        update_tree_entry_header, BundleManifest,
    },
};

#[cfg(test)]
const MANIFEST_PATH: &str = "target/sdk-docs/api/bundle-manifest.json";
const REPOSITORY: &str = "Foundation-Devices/KeyOS-Releases-private";
const REPOSITORY_API: &str = "repos/Foundation-Devices/KeyOS-Releases-private";
const DRAFT_ASSET_LOOKUP_ATTEMPTS: usize = 3;
const DRAFT_ASSET_LOOKUP_DELAY: Duration = Duration::from_millis(100);

#[derive(Args, Debug)]
pub struct DocsPublishArgs {
    /// KeyOS-Releases-private tag, including a tagged draft, or the title of its untagged draft.
    #[arg(value_name = "RELEASE_TAG")]
    release_tag: Option<String>,
    /// Verify the docs release without uploading.
    #[arg(long)]
    dry_run: bool,
    /// Replace existing ZIP and checksum assets.
    #[arg(long)]
    replace: bool,
}

#[derive(Debug)]
struct GeneratedRelease {
    docs_version: String,
    release_tag: String,
    archive: PathBuf,
    checksum: PathBuf,
    archive_name: String,
    checksum_name: String,
}

#[derive(Debug, PartialEq, Eq)]
struct PublishOutcome {
    summary: String,
    docs_site_command: String,
}

trait ReleaseHost {
    fn release_assets(&mut self, release_tag: &str) -> Result<BTreeSet<String>>;
    fn upload(&mut self, release_tag: &str, archive: &Path, checksum: &Path, replace: bool) -> Result<()>;
}

#[derive(Default)]
struct GitHubCli {
    resolved_releases: BTreeMap<String, ResolvedRelease>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedRelease {
    id: u64,
    /// A draft without a tag must be addressed through its release ID.
    tag_name: Option<String>,
}

#[derive(Deserialize)]
struct ApiRelease {
    id: u64,
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    draft: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct ApiReleaseAsset {
    id: u64,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AssetRename {
    id: u64,
    from: String,
    to: String,
}

fn select_draft_by_reference(
    releases: &[ApiRelease],
    release_reference: &str,
) -> Result<Option<ResolvedRelease>> {
    let tagged_matches = releases
        .iter()
        .filter(|release| release.draft && release.tag_name == release_reference)
        .collect::<Vec<_>>();
    match tagged_matches.as_slice() {
        [] => {}
        [release] => return Ok(Some(ResolvedRelease { id: release.id, tag_name: None })),
        _ => bail!("multiple GitHub drafts use tag '{release_reference}'"),
    }

    let title_matches = releases
        .iter()
        .filter(|release| {
            release.draft && release.tag_name.is_empty() && release.name.as_deref() == Some(release_reference)
        })
        .collect::<Vec<_>>();
    match title_matches.as_slice() {
        [] => Ok(None),
        [release] => Ok(Some(ResolvedRelease { id: release.id, tag_name: None })),
        _ => bail!("multiple untagged GitHub drafts are titled '{release_reference}'"),
    }
}

fn percent_encode_query_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

fn asset_ids(assets: Vec<ApiReleaseAsset>) -> BTreeMap<String, u64> {
    assets.into_iter().map(|asset| (asset.name, asset.id)).collect()
}

fn uploaded_asset_id(assets: &[ApiReleaseAsset], name: &str) -> Option<u64> {
    assets.iter().find(|asset| asset.name == name).map(|asset| asset.id)
}

fn retry_asset_lookup(
    asset_name: &str,
    attempts: usize,
    mut list_assets: impl FnMut() -> Result<BTreeMap<String, u64>>,
    mut pause: impl FnMut(),
) -> Result<Option<u64>> {
    debug_assert!(attempts > 0);
    let mut last_error = None;
    for attempt in 0..attempts {
        match list_assets() {
            Ok(assets) => {
                if let Some(asset_id) = assets.get(asset_name) {
                    return Ok(Some(*asset_id));
                }
                if attempt + 1 == attempts {
                    return Ok(None);
                }
            }
            Err(error) => {
                if attempt + 1 == attempts {
                    return Err(error);
                }
                last_error = Some(error);
            }
        }
        pause();
    }
    Err(last_error.expect("a retry loop with no result must have an error"))
}

fn github_cli_error(context: &str, error: std::io::Error) -> anyhow::Error {
    if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied) {
        anyhow!("GitHub CLI is unavailable; run this command through nix develop .#build")
    } else {
        anyhow!("{context}: {error}")
    }
}

fn run_gh(command: &mut Command, context: &str) -> Result<Output> {
    command.output().map_err(|error| github_cli_error(context, error))
}

fn github_not_found(output: &Output) -> bool {
    let detail = command_error_detail(&output.stdout, &output.stderr).to_ascii_lowercase();
    detail.contains("not found") || detail.contains("404")
}

fn cleanup_partial_draft_upload_assets(
    uploaded: &[ApiReleaseAsset],
    attempted_names: &[String],
    mut delete_asset: impl FnMut(u64) -> Result<()>,
    mut cleanup_remaining: impl FnMut(&[&str]) -> Result<()>,
) -> Result<()> {
    let mut fallback_names = attempted_names.to_vec();
    let mut errors = Vec::new();
    for asset in uploaded {
        match delete_asset(asset.id) {
            Ok(()) => fallback_names.retain(|name| name != &asset.name),
            Err(error) => errors.push(format!("asset {}: {error:#}", asset.name)),
        }
    }
    let fallback_names = fallback_names.iter().map(String::as_str).collect::<Vec<_>>();
    if !fallback_names.is_empty() {
        if let Err(error) = cleanup_remaining(&fallback_names) {
            errors.push(format!("remaining assets: {error:#}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("partial draft-upload cleanup failed for {}", errors.join(", "));
    }
}

fn apply_asset_renames(
    renames: &[AssetRename],
    mut rename: impl FnMut(u64, &str) -> Result<()>,
    verify: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let mut attempted = Vec::new();
    let result = (|| {
        for operation in renames {
            // Include the current operation in rollback in case GitHub applied the rename
            // but its response was lost before the client could confirm it.
            attempted.push(operation);
            rename(operation.id, &operation.to)?;
        }
        verify()
    })();
    let Err(error) = result else { return Ok(()) };

    let mut rollback_errors = Vec::new();
    for operation in attempted.into_iter().rev() {
        if let Err(rollback_error) = rename(operation.id, &operation.from) {
            rollback_errors
                .push(format!("asset {} back to {}: {rollback_error:#}", operation.id, operation.from));
        }
    }
    if rollback_errors.is_empty() {
        Err(error).context("GitHub asset update failed and completed renames were rolled back")
    } else {
        bail!(
            "GitHub asset update failed: {error:#}; rollback also failed for {}",
            rollback_errors.join(", ")
        )
    }
}

impl GitHubCli {
    fn release_upload_command(release_tag: &str, files: &[&Path]) -> Command {
        let mut command = Command::new("gh");
        command.args(["release", "upload", release_tag]);
        for file in files {
            command.arg(file);
        }
        command.args(["--repo", REPOSITORY]);
        command
    }

    fn release_delete_asset_command(release_tag: &str, asset_name: &str) -> Command {
        let mut command = Command::new("gh");
        command.args(["release", "delete-asset", release_tag, asset_name, "--repo", REPOSITORY, "--yes"]);
        command
    }

    fn draft_upload_command(release_id: u64, file: &Path) -> Result<Command> {
        let asset_name = file_name(file)?;
        let endpoint = format!(
            "https://uploads.github.com/{REPOSITORY_API}/releases/{release_id}/assets?name={}",
            percent_encode_query_component(&asset_name),
        );
        let mut command = Command::new("gh");
        command.args([
            "api",
            "--hostname",
            "github.com",
            "--method",
            "POST",
            "--header",
            "Content-Type: application/octet-stream",
            "--input",
        ]);
        command.arg(file);
        command.arg(&endpoint);
        Ok(command)
    }

    fn upload_files(&self, release: &ResolvedRelease, files: &[&Path]) -> Result<Vec<ApiReleaseAsset>> {
        if let Some(release_tag) = &release.tag_name {
            let mut command = Self::release_upload_command(release_tag, files);
            let status = command.status().map_err(|error| {
                github_cli_error(&format!("GitHub upload failed for release '{release_tag}'"), error)
            })?;
            if !status.success() {
                bail!("GitHub upload failed for release '{release_tag}'");
            }
            return Ok(Vec::new());
        }

        let mut uploaded = Vec::new();
        for (index, file) in files.iter().enumerate() {
            match self.upload_file_to_untagged_draft(release.id, file) {
                Ok(asset) => uploaded.push(asset),
                Err(error) => {
                    let cleanup = self.cleanup_partial_draft_upload(release, &uploaded, &files[..=index]);
                    return match cleanup {
                        Ok(()) => Err(error),
                        Err(cleanup_error) => Err(error.context(format!(
                            "also failed to remove partially uploaded draft assets: {cleanup_error:#}"
                        ))),
                    };
                }
            }
        }
        Ok(uploaded)
    }

    fn upload_file_to_untagged_draft(&self, release_id: u64, file: &Path) -> Result<ApiReleaseAsset> {
        let mut command = Self::draft_upload_command(release_id, file)?;
        let output = run_gh(&mut command, &format!("GitHub upload failed for untagged draft {release_id}"))?;
        if !output.status.success() {
            bail!(
                "GitHub upload failed for untagged draft {release_id}: {}",
                command_error_detail(&output.stdout, &output.stderr)
            );
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|_| anyhow!("GitHub returned malformed uploaded asset metadata"))
    }

    fn cleanup_partial_draft_upload(
        &self,
        release: &ResolvedRelease,
        uploaded: &[ApiReleaseAsset],
        attempted_files: &[&Path],
    ) -> Result<()> {
        let attempted_names =
            attempted_files.iter().map(|file| file_name(file)).collect::<Result<Vec<_>>>()?;
        cleanup_partial_draft_upload_assets(
            uploaded,
            &attempted_names,
            |asset_id| self.delete_asset(asset_id),
            |asset_names| self.cleanup_assets(release, asset_names),
        )
    }

    fn release_asset_ids(&self, release: &ResolvedRelease) -> Result<BTreeMap<String, u64>> {
        let endpoint = format!("{REPOSITORY_API}/releases/{}/assets?per_page=100", release.id);
        let mut command = Command::new("gh");
        command.args(["api", "--paginate", &endpoint]);
        let output = run_gh(&mut command, &format!("cannot inspect GitHub release {}", release.id))?;
        if !output.status.success() {
            bail!(
                "cannot inspect GitHub release {}: {}",
                release.id,
                command_error_detail(&output.stdout, &output.stderr)
            );
        }
        let assets: Vec<ApiReleaseAsset> = serde_json::from_slice(&output.stdout)
            .map_err(|_| anyhow!("GitHub returned malformed release metadata"))?;
        Ok(asset_ids(assets))
    }

    fn delete_asset(&self, asset_id: u64) -> Result<()> {
        self.run_api_mutation("DELETE", &format!("{REPOSITORY_API}/releases/assets/{asset_id}"), None)
    }

    fn delete_asset_by_name(&self, release: &ResolvedRelease, asset_name: &str) -> Result<()> {
        let Some(release_tag) = &release.tag_name else {
            let asset_id = retry_asset_lookup(
                asset_name,
                DRAFT_ASSET_LOOKUP_ATTEMPTS,
                || self.release_asset_ids(release),
                || std::thread::sleep(DRAFT_ASSET_LOOKUP_DELAY),
            )?;
            return match asset_id {
                Some(asset_id) => self.delete_asset(asset_id),
                None => Ok(()),
            };
        };
        let mut command = Self::release_delete_asset_command(release_tag, asset_name);
        let output = run_gh(
            &mut command,
            &format!("could not delete staged GitHub release asset '{asset_name}' from '{release_tag}'"),
        )?;
        if !output.status.success() && !github_not_found(&output) {
            bail!(
                "could not delete staged GitHub release asset '{asset_name}' from '{release_tag}': {}",
                command_error_detail(&output.stdout, &output.stderr)
            );
        }
        Ok(())
    }

    fn rename_asset(&self, asset_id: u64, name: &str) -> Result<()> {
        self.run_api_mutation("PATCH", &format!("{REPOSITORY_API}/releases/assets/{asset_id}"), Some(name))
    }

    fn rename_asset_checked(&self, release: &ResolvedRelease, asset_id: u64, name: &str) -> Result<()> {
        let Err(error) = self.rename_asset(asset_id, name) else { return Ok(()) };
        match self.release_asset_ids(release) {
            Ok(assets) if assets.get(name) == Some(&asset_id) => Ok(()),
            Ok(_) => Err(error),
            Err(inspect_error) => bail!(
                "GitHub asset rename failed: {error:#}; could not determine whether it completed: {inspect_error:#}"
            ),
        }
    }

    fn run_api_mutation(&self, method: &str, endpoint: &str, name: Option<&str>) -> Result<()> {
        let mut command = Command::new("gh");
        command.args(["api", "--method", method, endpoint]);
        if let Some(name) = name {
            command.args(["-f", &format!("name={name}")]);
        }
        let output = run_gh(&mut command, "GitHub asset update failed")?;
        if !output.status.success() {
            bail!("GitHub asset update failed: {}", command_error_detail(&output.stdout, &output.stderr));
        }
        Ok(())
    }

    fn release_by_tag(&self, release_tag: &str) -> Result<Option<ResolvedRelease>> {
        let endpoint = format!("{REPOSITORY_API}/releases/tags/{release_tag}");
        let mut command = Command::new("gh");
        command.args(["api", &endpoint]);
        let output = run_gh(&mut command, &format!("cannot inspect GitHub release '{release_tag}'"))?;
        if output.status.success() {
            let release: ApiRelease = serde_json::from_slice(&output.stdout)
                .map_err(|_| anyhow!("GitHub returned malformed release metadata"))?;
            return Ok(Some(ResolvedRelease { id: release.id, tag_name: Some(release.tag_name) }));
        }

        if github_not_found(&output) {
            Ok(None)
        } else {
            let detail = command_error_detail(&output.stdout, &output.stderr);
            bail!("cannot inspect GitHub release '{release_tag}': {detail}");
        }
    }

    fn draft_by_reference(&self, release_reference: &str) -> Result<Option<ResolvedRelease>> {
        let endpoint = format!("{REPOSITORY_API}/releases?per_page=100");
        let mut command = Command::new("gh");
        command.args(["api", "--paginate", &endpoint]);
        let output = run_gh(&mut command, "cannot list GitHub releases")?;
        if !output.status.success() {
            bail!("cannot list GitHub releases: {}", command_error_detail(&output.stdout, &output.stderr));
        }
        let releases: Vec<ApiRelease> = serde_json::from_slice(&output.stdout)
            .map_err(|_| anyhow!("GitHub returned malformed release metadata"))?;
        select_draft_by_reference(&releases, release_reference)
    }

    fn resolve_release(&self, release_tag: &str) -> Result<ResolvedRelease> {
        if let Some(release) = self.release_by_tag(release_tag)? {
            return Ok(release);
        }
        if let Some(release) = self.draft_by_reference(release_tag)? {
            return Ok(release);
        }
        bail!(
            "cannot read GitHub release '{release_tag}': no published release or draft has that tag, and no untagged draft has that title"
        );
    }

    fn resolved_release(&self, release_tag: &str) -> Result<&ResolvedRelease> {
        self.resolved_releases
            .get(release_tag)
            .with_context(|| format!("GitHub release '{release_tag}' was not resolved before upload"))
    }

    fn cleanup_assets(&self, release: &ResolvedRelease, asset_names: &[&str]) -> Result<()> {
        cleanup_staged_assets(
            asset_names,
            || self.release_asset_ids(release),
            |asset_id| self.delete_asset(asset_id),
            |asset_name| self.delete_asset_by_name(release, asset_name),
        )
    }

    fn uploaded_asset_id(
        &self,
        release: &ResolvedRelease,
        uploaded_assets: &[ApiReleaseAsset],
        name: &str,
    ) -> Result<u64> {
        if let Some(asset_id) = uploaded_asset_id(uploaded_assets, name) {
            return Ok(asset_id);
        }
        retry_asset_lookup(
            name,
            DRAFT_ASSET_LOOKUP_ATTEMPTS,
            || self.release_asset_ids(release),
            || std::thread::sleep(DRAFT_ASSET_LOOKUP_DELAY),
        )?
        .with_context(|| format!("GitHub did not retain staged asset {name}"))
    }

    fn replace_files(&self, release: &ResolvedRelease, archive: &Path, checksum: &Path) -> Result<()> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| anyhow!("system clock is before the Unix epoch"))?
            .as_nanos();
        let parent = archive.parent().context("generated docs archive has no parent directory")?;
        let archive_name = file_name(archive)?;
        let checksum_name = file_name(checksum)?;
        let staging_dir = parent.join(format!("docs-publish-stage-{}-{nonce}", std::process::id()));
        fs::create_dir(&staging_dir)
            .map_err(|error| anyhow!("cannot create replacement staging directory: {error}"))?;

        let staged_archive_name = format!("{archive_name}.pending-{nonce}");
        let staged_checksum_name = format!("{checksum_name}.pending-{nonce}");
        let staged_archive = staging_dir.join(&staged_archive_name);
        let staged_checksum = staging_dir.join(&staged_checksum_name);
        let staging_result: Result<()> = (|| {
            fs::copy(archive, &staged_archive)
                .map_err(|error| anyhow!("cannot stage replacement archive: {error}"))?;
            fs::copy(checksum, &staged_checksum)
                .map_err(|error| anyhow!("cannot stage replacement checksum: {error}"))?;
            Ok(())
        })();
        if let Err(error) = staging_result {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(error);
        }

        let staged_names = [staged_archive_name.as_str(), staged_checksum_name.as_str()];
        let mut staged_assets_promoted = false;
        let result: Result<()> = (|| {
            // Uploading unique names first keeps the currently published pair intact if either
            // upload fails. Once both staging assets exist, only metadata operations remain.
            let assets = self.release_asset_ids(release)?;
            let uploaded_assets = self.upload_files(release, &[&staged_archive, &staged_checksum])?;
            let staged_archive_id =
                self.uploaded_asset_id(release, &uploaded_assets, &staged_archive_name)?;
            let staged_checksum_id =
                self.uploaded_asset_id(release, &uploaded_assets, &staged_checksum_name)?;

            let mut previous_assets = Vec::new();
            let mut renames = Vec::new();
            for name in [&archive_name, &checksum_name] {
                if let Some(asset_id) = assets.get(name) {
                    let previous_name = format!("{name}.previous-{nonce}");
                    renames.push(AssetRename { id: *asset_id, from: name.clone(), to: previous_name });
                    previous_assets.push(*asset_id);
                }
            }
            renames.extend([
                AssetRename {
                    id: staged_archive_id,
                    from: staged_archive_name.clone(),
                    to: archive_name.clone(),
                },
                AssetRename {
                    id: staged_checksum_id,
                    from: staged_checksum_name.clone(),
                    to: checksum_name.clone(),
                },
            ]);
            apply_asset_renames(
                &renames,
                |asset_id, name| self.rename_asset_checked(release, asset_id, name),
                || {
                    let published = self.release_asset_ids(release)?;
                    ensure!(
                        published.get(&archive_name) == Some(&staged_archive_id)
                            && published.get(&checksum_name) == Some(&staged_checksum_id),
                        "GitHub did not expose both replacement assets under their final names"
                    );
                    Ok(())
                },
            )?;
            staged_assets_promoted = true;
            for asset_id in previous_assets {
                self.delete_asset(asset_id)?;
            }
            Ok(())
        })();
        let result = match result {
            Ok(()) => Ok(()),
            Err(error) if staged_assets_promoted => Err(error),
            Err(error) => match self.cleanup_assets(release, &staged_names) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(error
                    .context(format!("also failed to remove unpromoted staged assets: {cleanup_error:#}"))),
            },
        };
        let _ = fs::remove_dir_all(&staging_dir);
        result.with_context(|| {
            format!(
                "safe replacement did not complete; inspect GitHub for pending/previous assets related to {archive_name} and {checksum_name}"
            )
        })
    }
}

fn cleanup_staged_assets(
    staged_names: &[&str],
    list_assets: impl FnOnce() -> Result<BTreeMap<String, u64>>,
    mut delete_asset: impl FnMut(u64) -> Result<()>,
    mut delete_asset_by_name: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let assets = list_assets().ok();
    let mut errors = Vec::new();
    for name in staged_names {
        let result = match assets.as_ref().and_then(|assets| assets.get(*name)) {
            Some(asset_id) => delete_asset(*asset_id),
            // A metadata response may be transiently unavailable or lag a successful upload.
            // Delete the assets it did identify by ID, then retry the remaining names.
            None => delete_asset_by_name(name),
        };
        if let Err(error) = result {
            errors.push(format!("{name}: {error:#}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("could not remove staged GitHub release assets: {}", errors.join(", "));
    }
}

impl ReleaseHost for GitHubCli {
    fn release_assets(&mut self, release_tag: &str) -> Result<BTreeSet<String>> {
        let release = self.resolve_release(release_tag)?;
        let assets = self.release_asset_ids(&release)?.into_keys().collect();
        self.resolved_releases.insert(release_tag.to_owned(), release);
        Ok(assets)
    }

    fn upload(&mut self, release_tag: &str, archive: &Path, checksum: &Path, replace: bool) -> Result<()> {
        let release = self.resolved_release(release_tag)?.clone();
        if replace {
            self.replace_files(&release, archive, checksum)
        } else {
            self.upload_files(&release, &[archive, checksum])?;
            Ok(())
        }
    }
}

pub fn run(args: DocsPublishArgs) -> Result<()> {
    let mut github = GitHubCli::default();
    let outcome = publish_from_root(&project_root(), &args, &mut github)?;
    println!("{}", outcome.summary);
    if args.dry_run {
        println!("After a successful publish, run this in Docs-Site:");
    } else {
        println!("Next, in Docs-Site:");
    }
    println!("  {}", outcome.docs_site_command);
    Ok(())
}

fn publish_from_root(
    root: &Path,
    args: &DocsPublishArgs,
    release_host: &mut impl ReleaseHost,
) -> Result<PublishOutcome> {
    // The archive and checksum are verified from the shared output directory.
    // Keep its lock until the upload finishes so another docs-api invocation
    // cannot replace those files after verification.
    let _docs_bundle_lock = acquire_docs_bundle_lock(root)?;
    let release = load_generated_release(root, args.release_tag.as_deref())?;
    let existing = release_host.release_assets(&release.release_tag)?;
    let collisions = [release.archive_name.as_str(), release.checksum_name.as_str()]
        .into_iter()
        .filter(|name| existing.contains(*name))
        .collect::<BTreeSet<_>>();
    if !collisions.is_empty() && !args.replace {
        let names = collisions.into_iter().collect::<Vec<_>>().join(", ");
        bail!("refusing to replace published release asset(s): {names}; pass --replace to overwrite them");
    }

    let summary = if args.dry_run {
        let action = if collisions.is_empty() { "upload to" } else { "replace existing assets in" };
        format!("Verified SDK docs {}; would {action} {}", release.docs_version, release.release_tag)
    } else {
        release_host.upload(
            &release.release_tag,
            &release.archive,
            &release.checksum,
            args.replace && !collisions.is_empty(),
        )?;
        let action = if collisions.is_empty() { "Published" } else { "Replaced" };
        format!(
            "{action} SDK docs {} in KeyOS-Releases-private tag {}",
            release.docs_version, release.release_tag
        )
    };

    let release_tag = if release.release_tag == release.docs_version {
        String::new()
    } else {
        format!(" {}", release.release_tag)
    };
    let replace = if args.replace { " --replace" } else { "" };
    let docs_site_command =
        format!("nix develop --command npm run docs:add -- {}{release_tag}{replace}", release.docs_version);
    Ok(PublishOutcome { summary, docs_site_command })
}

fn load_generated_release(root: &Path, release_tag: Option<&str>) -> Result<GeneratedRelease> {
    let bundle_dir = sdk_docs_bundle_dir(root);
    let manifest_path = bundle_dir.join("bundle-manifest.json");
    let manifest = read_bundle_manifest(&manifest_path)?;
    if manifest.current_keyos_version.is_empty() {
        bail!("generated manifest has no valid currentKeyosVersion");
    }
    let docs_version =
        require_keyos_version("generated currentKeyosVersion", &manifest.current_keyos_version)?;

    if manifest.versions.len() != 1 {
        bail!("generated docs release must contain exactly one KeyOS version");
    }
    if manifest.versions[0].keyos_version != docs_version {
        bail!("generated docs release does not contain its current KeyOS version");
    }
    if manifest.versions[0].path != format!("v{docs_version}/") {
        bail!("generated docs release has an invalid current KeyOS version path");
    }
    if manifest.versions[0].source_dirty {
        bail!(
            "generated docs were built from uncommitted source changes; commit the source and rebuild before publishing"
        );
    }
    ensure!(
        !manifest.versions[0].source_revision.is_empty(),
        "generated docs manifest has no source revision"
    );
    ensure!(
        !manifest.versions[0].generator_revision.is_empty(),
        "generated docs manifest has no generator revision"
    );

    let release_tag = require_keyos_version("GitHub release tag", release_tag.unwrap_or(&docs_version))?;
    let archive_name = format!("keyos-sdk-docs-v{docs_version}.zip");
    let checksum_name = format!("{archive_name}.sha256");
    let archive = root.join("target").join(&archive_name);
    let checksum = root.join("target").join(&checksum_name);
    verify_checksum(&archive, &checksum)?;
    verify_archive_bundle(&archive, &bundle_dir, &manifest)?;

    Ok(GeneratedRelease { docs_version, release_tag, archive, checksum, archive_name, checksum_name })
}

fn require_keyos_version(label: &str, value: &str) -> Result<String> {
    if Version::parse(value).is_err() {
        bail!("{label} is not valid SemVer: {value:?}");
    }
    if value.matches('.').count() != 2 {
        bail!("{label} must contain exactly two periods for RecoveryOS compatibility: {value:?}");
    }
    Ok(value.to_owned())
}

fn verify_checksum(archive: &Path, checksum: &Path) -> Result<()> {
    if !archive.is_file() || !checksum.is_file() {
        bail!("generated docs artifact is missing: {} or {}", archive.display(), checksum.display());
    }
    let contents = fs::read_to_string(checksum)
        .map_err(|error| anyhow!("cannot read generated checksum {}: {error}", checksum.display()))?;
    let lines = contents.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if lines.len() != 1 {
        bail!("malformed generated checksum: {}", checksum.display());
    }
    let fields = lines[0].split_whitespace().collect::<Vec<_>>();
    let archive_name = archive.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if fields.len() != 2 || fields[1].strip_prefix('*').unwrap_or(fields[1]) != archive_name {
        bail!("generated checksum does not name {archive_name}");
    }
    let actual = file_sha256(archive)?;
    if !constant_time_checksum_matches(fields[0], &actual) {
        bail!("generated checksum does not match {}", archive.display());
    }
    Ok(())
}

fn verify_archive_bundle(
    archive: &Path,
    loose_bundle_dir: &Path,
    loose_manifest: &BundleManifest,
) -> Result<()> {
    let file = fs::File::open(archive)
        .with_context(|| format!("opening generated docs archive {}", archive.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("reading generated docs archive {}", archive.display()))?;
    let mut manifest_entries = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).context("reading generated docs archive entry")?;
        if entry.name() == "bundle-manifest.json" {
            manifest_entries += 1;
        }
    }
    ensure!(manifest_entries == 1, "generated docs archive must contain one bundle-manifest.json");

    let archived_manifest: BundleManifest = {
        let mut entry = archive
            .by_name("bundle-manifest.json")
            .context("generated docs archive has no readable bundle-manifest.json")?;
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents).context("reading archived bundle-manifest.json")?;
        serde_json::from_slice(&contents).context("parsing archived bundle-manifest.json")?
    };
    ensure!(
        &archived_manifest == loose_manifest,
        "generated docs archive manifest does not match the loose bundle manifest"
    );
    let version = &archived_manifest.versions[0];
    let actual_tree = archive_tree_sha256(&mut archive, &version.path)?;
    ensure!(
        constant_time_checksum_matches(&version.tree_sha256, &actual_tree),
        "generated docs archive version tree does not match its manifest digest"
    );
    ensure!(
        archive_file_hashes(&mut archive)? == loose_bundle_file_hashes(loose_bundle_dir)?,
        "generated docs archive contents do not match the loose bundle output"
    );
    Ok(())
}

fn archive_tree_sha256(archive: &mut ZipArchive<fs::File>, prefix: &str) -> Result<String> {
    let mut paths = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).context("reading generated docs archive entry")?;
        if entry.is_dir() || !entry.name().starts_with(prefix) {
            continue;
        }
        let relative = entry.name()[prefix.len()..].to_owned();
        ensure!(
            !relative.is_empty()
                && !relative.starts_with('/')
                && !relative.contains("\\")
                && !relative.split('/').any(|part| part == ".."),
            "generated docs archive has an invalid version-tree path"
        );
        paths.push(relative);
    }
    paths.sort_by(|left, right| Path::new(left).cmp(Path::new(right)));
    ensure!(!paths.is_empty(), "generated docs archive has an empty version tree");
    ensure!(
        paths.windows(2).all(|pair| pair[0] != pair[1]),
        "generated docs archive has duplicate version-tree paths"
    );

    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    for relative in paths {
        let mut entry = archive
            .by_name(&format!("{prefix}{relative}"))
            .context("reading generated docs version-tree entry")?;
        update_tree_entry_header(&mut hasher, &relative, entry.size());
        loop {
            let read = entry.read(&mut buffer).context("reading generated docs version-tree contents")?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn archive_file_hashes(archive: &mut ZipArchive<fs::File>) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).context("reading generated docs archive entry")?;
        if entry.is_dir() {
            continue;
        }
        let path = normalized_archive_path(entry.name())?;
        let digest = reader_sha256(&mut entry)?;
        ensure!(
            files.insert(path.clone(), digest).is_none(),
            "generated docs archive has duplicate file path {path}"
        );
    }
    Ok(files)
}

fn loose_bundle_file_hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    ensure!(root.is_dir(), "generated docs bundle is missing: {}", root.display());

    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).with_context(|| format!("reading {}", directory.display()))? {
            let entry = entry.context("reading generated docs bundle entry")?;
            let path = entry.path();
            let file_type = entry.file_type().context("reading generated docs bundle entry type")?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                let relative =
                    path.strip_prefix(root).context("generated docs bundle file is outside its root")?;
                let path = normalized_archive_path(&relative.to_string_lossy())?;
                let digest = file_sha256(&entry.path())?;
                ensure!(
                    files.insert(path.clone(), digest).is_none(),
                    "generated docs bundle has duplicate file path {path}"
                );
            } else {
                bail!("generated docs bundle has unsupported entry {}", path.display());
            }
        }
    }
    Ok(files)
}

fn normalized_archive_path(path: &str) -> Result<String> {
    ensure!(
        !path.is_empty()
            && !path.starts_with('/')
            && !path.contains('\\')
            && !path.split('/').any(|part| part.is_empty() || part == "." || part == ".."),
        "generated docs archive has an invalid file path"
    );
    Ok(path.to_owned())
}

fn reader_sha256(reader: &mut impl Read) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).context("reading generated docs archive contents")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
fn hash_tree_entries(entries: &[(String, Vec<u8>)]) -> String {
    let mut hasher = Sha256::new();
    for (path, contents) in entries {
        update_tree_entry_header(&mut hasher, path, contents.len() as u64);
        hasher.update(contents);
    }
    hex::encode(hasher.finalize())
}

fn constant_time_checksum_matches(provided: &str, actual: &str) -> bool {
    if provided.len() != actual.len() {
        return false;
    }
    provided
        .bytes()
        .zip(actual.bytes())
        .fold(0_u8, |difference, (left, right)| difference | (left.to_ascii_lowercase() ^ right))
        == 0
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .with_context(|| format!("invalid generated docs asset name: {}", path.display()))
}

fn command_error_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    if stderr.trim().is_empty() {
        stdout.trim().to_owned()
    } else {
        stderr.trim().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str, version: &str) -> Self {
            Self::with_tree_entries(name, version, vec![("index.html".to_owned(), b"docs archive".to_vec())])
        }

        fn with_tree_entries(name: &str, version: &str, mut tree_entries: Vec<(String, Vec<u8>)>) -> Self {
            let root =
                project_root().join(format!("target/xtask-docs-publish-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let bundle_dir = root.join("target/sdk-docs/api");
            fs::create_dir_all(&bundle_dir).unwrap();
            let version_path = format!("v{version}/");
            tree_entries.sort_by(|left, right| Path::new(&left.0).cmp(Path::new(&right.0)));
            let manifest = serde_json::json!({
                "schemaVersion": 1,
                "sdkVersion": "1.0.0",
                "currentKeyosVersion": version,
                "defaultKeyosVersion": version,
                "versions": [{
                    "keyosVersion": version,
                    "path": version_path,
                    "sourceRevision": "test",
                    "generatorRevision": "test",
                    "sourceDirty": false,
                    "treeSha256": hash_tree_entries(&tree_entries),
                    "crates": [],
                }],
            });
            let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
            fs::write(root.join(MANIFEST_PATH), &manifest_bytes).unwrap();
            write_test_loose_bundle(&bundle_dir, &version_path, &tree_entries, &[]);
            let archive_name = format!("keyos-sdk-docs-v{version}.zip");
            let archive = root.join("target").join(&archive_name);
            let mut archive_entries = tree_entries.clone();
            archive_entries.reverse();
            write_test_archive(&archive, &manifest_bytes, &version_path, &archive_entries);
            write_test_checksum(&archive);
            Self(root)
        }
    }

    fn write_test_loose_bundle(
        bundle_dir: &Path,
        version_path: &str,
        tree_entries: &[(String, Vec<u8>)],
        root_entries: &[(String, Vec<u8>)],
    ) {
        for (relative, contents) in root_entries {
            let path = bundle_dir.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        for (relative, contents) in tree_entries {
            let path = bundle_dir.join(version_path).join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    fn write_test_archive(
        path: &Path,
        manifest: &[u8],
        version_path: &str,
        tree_entries: &[(String, Vec<u8>)],
    ) {
        write_test_archive_with_root(path, manifest, version_path, tree_entries, &[])
    }

    fn write_test_archive_with_root(
        path: &Path,
        manifest: &[u8],
        version_path: &str,
        tree_entries: &[(String, Vec<u8>)],
        root_entries: &[(String, Vec<u8>)],
    ) {
        use std::io::Write;

        let mut archive = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let options =
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        archive.start_file("bundle-manifest.json", options).unwrap();
        archive.write_all(manifest).unwrap();
        for (relative, contents) in root_entries {
            archive.start_file(relative, options).unwrap();
            archive.write_all(contents).unwrap();
        }
        for (relative, contents) in tree_entries {
            archive.start_file(format!("{version_path}{relative}"), options).unwrap();
            archive.write_all(contents).unwrap();
        }
        archive.finish().unwrap();
    }

    fn write_test_checksum(archive: &Path) {
        let archive_name = archive.file_name().unwrap().to_string_lossy();
        let digest = file_sha256(archive).unwrap();
        fs::write(archive.with_extension("zip.sha256"), format!("{digest}  {archive_name}\n")).unwrap();
    }

    impl Drop for Fixture {
        fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); }
    }

    #[derive(Default)]
    struct MockReleaseHost {
        assets: BTreeSet<String>,
        inspected_tags: Vec<String>,
        uploads: Vec<(String, PathBuf, PathBuf, bool)>,
    }

    struct LockCheckingReleaseHost {
        root: PathBuf,
        lock_was_held_during_upload: bool,
    }

    impl ReleaseHost for LockCheckingReleaseHost {
        fn release_assets(&mut self, _: &str) -> Result<BTreeSet<String>> { Ok(BTreeSet::new()) }

        fn upload(&mut self, _: &str, _: &Path, _: &Path, _: bool) -> Result<()> {
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(self.root.join("target/docs-api.lock"))?;
            self.lock_was_held_during_upload =
                matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock));
            Ok(())
        }
    }

    impl ReleaseHost for MockReleaseHost {
        fn release_assets(&mut self, release_tag: &str) -> Result<BTreeSet<String>> {
            self.inspected_tags.push(release_tag.to_owned());
            Ok(self.assets.clone())
        }

        fn upload(
            &mut self,
            release_tag: &str,
            archive: &Path,
            checksum: &Path,
            replace: bool,
        ) -> Result<()> {
            self.uploads.push((release_tag.to_owned(), archive.to_owned(), checksum.to_owned(), replace));
            Ok(())
        }
    }

    fn args(release_tag: Option<&str>, dry_run: bool, replace: bool) -> DocsPublishArgs {
        DocsPublishArgs { release_tag: release_tag.map(str::to_owned), dry_run, replace }
    }

    #[test]
    fn keyos_versions_are_recoveryos_compatible() {
        for version in ["1.4.0", "1.4.0-alpha1", "1.4.0-beta2"] {
            assert_eq!(require_keyos_version("version", version).unwrap(), version);
        }
        for version in ["1.4.0-alpha.1", "1.4.0+build.1"] {
            let error = require_keyos_version("version", version).unwrap_err().to_string();
            assert!(error.contains("exactly two periods"), "unexpected error: {error}");
        }
        assert!(require_keyos_version("version", "1.4").unwrap_err().to_string().contains("SemVer"));
    }

    #[test]
    fn generated_release_validates_manifest_and_checksum() {
        let fixture = Fixture::new("validation", "1.4.0-beta2");
        let release = load_generated_release(&fixture.0, Some("1.4.0")).unwrap();
        assert_eq!(release.docs_version, "1.4.0-beta2");
        assert_eq!(release.release_tag, "1.4.0");

        fs::write(&release.archive, b"tampered docs archive").unwrap();
        let error = load_generated_release(&fixture.0, None).unwrap_err().to_string();
        assert!(error.contains("generated checksum does not match"), "unexpected error: {error}");
    }

    #[test]
    fn generated_release_rejects_an_archive_with_a_stale_manifest() {
        let fixture = Fixture::new("stale-archive-manifest", "1.4.0");
        let manifest_path = fixture.0.join(MANIFEST_PATH);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["versions"][0]["sourceRevision"] = "new-source".into();
        fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

        let error = load_generated_release(&fixture.0, None).unwrap_err().to_string();
        assert!(error.contains("archive manifest does not match"), "unexpected error: {error}");
    }

    #[test]
    fn generated_release_verifies_the_archived_version_tree() {
        let fixture = Fixture::new("stale-archive-tree", "1.4.0");
        let archive = fixture.0.join("target/keyos-sdk-docs-v1.4.0.zip");
        let manifest = fs::read(fixture.0.join(MANIFEST_PATH)).unwrap();
        write_test_archive(
            &archive,
            &manifest,
            "v1.4.0/",
            &[("index.html".to_owned(), b"different docs".to_vec())],
        );
        write_test_checksum(&archive);

        let error = load_generated_release(&fixture.0, None).unwrap_err().to_string();
        assert!(error.contains("version tree does not match"), "unexpected error: {error}");
    }

    #[test]
    fn generated_release_uses_path_component_order_for_the_archived_version_tree() {
        let fixture = Fixture::with_tree_entries(
            "archive-path-order",
            "1.4.0",
            vec![
                ("src-files.js".to_owned(), b"rustdoc root asset".to_vec()),
                ("src/lib.rs.html".to_owned(), b"rustdoc source page".to_vec()),
            ],
        );

        load_generated_release(&fixture.0, None).unwrap();
    }

    #[test]
    fn generated_release_rejects_stale_root_bundle_files() {
        let fixture = Fixture::new("stale-archive-root", "1.4.0");
        let bundle_dir = fixture.0.join("target/sdk-docs/api");
        fs::write(bundle_dir.join("version-selector.js"), "new selector").unwrap();
        let archive = fixture.0.join("target/keyos-sdk-docs-v1.4.0.zip");
        let manifest = fs::read(bundle_dir.join("bundle-manifest.json")).unwrap();
        write_test_archive_with_root(
            &archive,
            &manifest,
            "v1.4.0/",
            &[("index.html".to_owned(), b"docs archive".to_vec())],
            &[("version-selector.js".to_owned(), b"old selector".to_vec())],
        );
        write_test_checksum(&archive);

        let error = load_generated_release(&fixture.0, None).unwrap_err().to_string();
        assert!(error.contains("contents do not match"), "unexpected error: {error}");
    }

    #[test]
    fn generated_release_requires_one_matching_manifest_version() {
        let fixture = Fixture::new("manifest-version", "1.4.0");
        fs::write(
            fixture.0.join(MANIFEST_PATH),
            br#"{
                "schemaVersion": 1,
                "sdkVersion": "1.0.0",
                "currentKeyosVersion": "1.4.0",
                "defaultKeyosVersion": "1.4.0",
                "versions": [{
                    "keyosVersion": "1.3.0",
                    "path": "v1.3.0/",
                    "sourceRevision": "test",
                    "generatorRevision": "test",
                    "sourceDirty": false,
                    "treeSha256": "test",
                    "crates": []
                }]
            }"#,
        )
        .unwrap();

        let error = load_generated_release(&fixture.0, None).unwrap_err().to_string();
        assert!(error.contains("does not contain its current KeyOS version"), "unexpected error: {error}");
    }

    #[test]
    fn generated_release_rejects_dirty_source_provenance() {
        let fixture = Fixture::new("dirty-source", "1.4.0");
        let manifest_path = fixture.0.join(MANIFEST_PATH);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["versions"][0]["sourceDirty"] = true.into();
        let manifest = serde_json::to_vec_pretty(&manifest).unwrap();
        fs::write(&manifest_path, &manifest).unwrap();
        let archive = fixture.0.join("target/keyos-sdk-docs-v1.4.0.zip");
        write_test_archive(
            &archive,
            &manifest,
            "v1.4.0/",
            &[("index.html".to_owned(), b"docs archive".to_vec())],
        );
        write_test_checksum(&archive);

        let error = load_generated_release(&fixture.0, None).unwrap_err().to_string();
        assert!(error.contains("uncommitted source changes"), "unexpected error: {error}");
    }

    #[test]
    fn dry_run_inspects_assets_without_uploading() {
        let fixture = Fixture::new("dry-run", "1.4.0");
        let mut host = MockReleaseHost::default();
        let outcome = publish_from_root(&fixture.0, &args(None, true, false), &mut host).unwrap();

        assert_eq!(host.inspected_tags, ["1.4.0"]);
        assert!(host.uploads.is_empty());
        assert_eq!(outcome.summary, "Verified SDK docs 1.4.0; would upload to 1.4.0");
        assert_eq!(outcome.docs_site_command, "nix develop --command npm run docs:add -- 1.4.0");
    }

    #[test]
    fn upload_keeps_the_generated_docs_locked_after_verification() {
        let fixture = Fixture::new("upload-lock", "1.4.0");
        let mut host =
            LockCheckingReleaseHost { root: fixture.0.clone(), lock_was_held_during_upload: false };

        publish_from_root(&fixture.0, &args(None, false, false), &mut host).unwrap();

        assert!(host.lock_was_held_during_upload);
    }

    #[test]
    fn collisions_require_replace_and_use_safe_replacement() {
        let fixture = Fixture::new("replace", "1.4.0");
        let assets = BTreeSet::from([
            "keyos-sdk-docs-v1.4.0.zip".to_owned(),
            "keyos-sdk-docs-v1.4.0.zip.sha256".to_owned(),
        ]);
        let mut host = MockReleaseHost { assets: assets.clone(), ..Default::default() };
        let error =
            publish_from_root(&fixture.0, &args(None, false, false), &mut host).unwrap_err().to_string();
        assert!(error.contains("refusing to replace published release asset(s)"));
        assert!(host.uploads.is_empty());

        let mut host = MockReleaseHost { assets, ..Default::default() };
        let outcome = publish_from_root(&fixture.0, &args(Some("1.4.1"), false, true), &mut host).unwrap();
        assert_eq!(host.inspected_tags, ["1.4.1"]);
        assert_eq!(host.uploads.len(), 1);
        assert!(host.uploads[0].3);
        assert_eq!(outcome.summary, "Replaced SDK docs 1.4.0 in KeyOS-Releases-private tag 1.4.1");
        assert_eq!(
            outcome.docs_site_command,
            "nix develop --command npm run docs:add -- 1.4.0 1.4.1 --replace"
        );
    }

    #[test]
    fn replace_without_collisions_uses_a_normal_upload() {
        let fixture = Fixture::new("replace-without-collision", "1.4.0");
        let mut host = MockReleaseHost::default();

        let outcome = publish_from_root(&fixture.0, &args(None, false, true), &mut host).unwrap();

        assert_eq!(host.uploads.len(), 1);
        assert!(!host.uploads[0].3);
        assert_eq!(outcome.summary, "Published SDK docs 1.4.0 in KeyOS-Releases-private tag 1.4.0");
        assert!(outcome.docs_site_command.ends_with(" --replace"));
    }

    #[test]
    fn github_release_metadata_contains_asset_names() {
        let assets: Vec<ApiReleaseAsset> =
            serde_json::from_slice(br#"[{"id":1,"name":"docs.zip"},{"id":2,"name":"docs.zip.sha256"}]"#)
                .unwrap();
        assert_eq!(
            asset_ids(assets),
            BTreeMap::from([("docs.zip".to_owned(), 1), ("docs.zip.sha256".to_owned(), 2)])
        );
        assert!(
            serde_json::from_slice::<Vec<ApiReleaseAsset>>(br#"[{"id":1,"url":"missing-name"}]"#).is_err()
        );
    }

    #[test]
    fn confirmed_upload_asset_ids_are_reused() {
        let assets = [
            ApiReleaseAsset { id: 7, name: "docs.zip.pending".to_owned() },
            ApiReleaseAsset { id: 8, name: "docs.zip.sha256.pending".to_owned() },
        ];

        assert_eq!(uploaded_asset_id(&assets, "docs.zip.pending"), Some(7));
        assert_eq!(uploaded_asset_id(&assets, "missing"), None);
    }

    #[test]
    fn github_upload_never_clobbers_existing_assets() {
        let archive = Path::new("/tmp/docs.pending");
        let checksum = Path::new("/tmp/docs.sha256.pending");
        let command = GitHubCli::release_upload_command("1.4.0-beta2", &[archive, checksum]);
        let args = command.get_args().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>();

        assert_eq!(
            args,
            [
                "release",
                "upload",
                "1.4.0-beta2",
                "/tmp/docs.pending",
                "/tmp/docs.sha256.pending",
                "--repo",
                REPOSITORY,
            ]
        );
        assert!(!args.iter().any(|arg| arg == "--clobber"));
    }

    #[test]
    fn github_uploads_to_an_untagged_draft_by_release_id() {
        let command =
            GitHubCli::draft_upload_command(374250059, Path::new("/tmp/keyos-sdk-docs.zip")).unwrap();
        let args = command.get_args().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>();

        assert_eq!(
            args,
            [
                "api",
                "--hostname",
                "github.com",
                "--method",
                "POST",
                "--header",
                "Content-Type: application/octet-stream",
                "--input",
                "/tmp/keyos-sdk-docs.zip",
                "https://uploads.github.com/repos/Foundation-Devices/KeyOS-Releases-private/releases/374250059/assets?name=keyos-sdk-docs.zip",
            ]
        );
    }

    #[test]
    fn draft_upload_percent_encodes_asset_names() {
        let encoded = percent_encode_query_component("keyos-sdk-docs-v1.4.0+build.zip");
        assert_eq!(encoded, "keyos-sdk-docs-v1.4.0%2Bbuild.zip");
    }

    #[test]
    fn draft_title_is_a_safe_release_fallback() {
        let releases = vec![
            ApiRelease {
                id: 1,
                tag_name: "1.4.0-beta1".to_owned(),
                name: Some("1.4.0-beta1".to_owned()),
                draft: false,
            },
            ApiRelease { id: 2, tag_name: String::new(), name: Some("1.4.0-beta2".to_owned()), draft: true },
        ];

        assert_eq!(
            select_draft_by_reference(&releases, "1.4.0-beta2").unwrap(),
            Some(ResolvedRelease { id: 2, tag_name: None })
        );
        assert_eq!(select_draft_by_reference(&releases, "1.4.0-beta3").unwrap(), None);
    }

    #[test]
    fn tagged_draft_is_resolved_by_tag_before_untagged_title() {
        let releases = [
            ApiRelease {
                id: 1,
                tag_name: "1.4.0-beta2".to_owned(),
                name: Some("release title".to_owned()),
                draft: true,
            },
            ApiRelease { id: 2, tag_name: String::new(), name: Some("1.4.0-beta2".to_owned()), draft: true },
        ];

        assert_eq!(
            select_draft_by_reference(&releases, "1.4.0-beta2").unwrap(),
            Some(ResolvedRelease { id: 1, tag_name: None })
        );
    }

    #[test]
    fn duplicate_draft_titles_are_rejected() {
        let releases = [
            ApiRelease { id: 1, tag_name: String::new(), name: Some("1.4.0-beta2".to_owned()), draft: true },
            ApiRelease { id: 2, tag_name: String::new(), name: Some("1.4.0-beta2".to_owned()), draft: true },
        ];

        let error = select_draft_by_reference(&releases, "1.4.0-beta2").unwrap_err().to_string();
        assert!(error.contains("multiple untagged GitHub drafts"));
    }

    #[test]
    fn github_cleanup_deletes_staged_assets_by_name() {
        let command = GitHubCli::release_delete_asset_command("1.4.0", "docs.zip.pending-1");
        let args = command.get_args().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>();

        assert_eq!(
            args,
            ["release", "delete-asset", "1.4.0", "docs.zip.pending-1", "--repo", REPOSITORY, "--yes"]
        );
    }

    #[test]
    fn failed_staged_asset_lookup_cleans_up_by_name() {
        use std::cell::RefCell;

        let deleted = RefCell::new(Vec::new());
        cleanup_staged_assets(
            &["docs.zip.pending-1", "docs.zip.sha256.pending-1"],
            || bail!("injected transient release metadata failure"),
            |_| panic!("cleanup must not require asset IDs after metadata lookup fails"),
            |name| {
                deleted.borrow_mut().push(name.to_owned());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(deleted.into_inner(), ["docs.zip.pending-1", "docs.zip.sha256.pending-1"]);
    }

    #[test]
    fn incomplete_staged_asset_listing_cleans_every_asset() {
        use std::cell::RefCell;

        let deleted_ids = RefCell::new(Vec::new());
        let deleted_names = RefCell::new(Vec::new());
        let error = cleanup_staged_assets(
            &["docs.zip.pending-1", "docs.zip.sha256.pending-1"],
            || Ok(BTreeMap::from([("docs.zip.sha256.pending-1".to_owned(), 2)])),
            |id| {
                deleted_ids.borrow_mut().push(id);
                Ok(())
            },
            |name| {
                deleted_names.borrow_mut().push(name.to_owned());
                bail!("injected missing asset")
            },
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("docs.zip.pending-1"));
        assert_eq!(deleted_names.into_inner(), ["docs.zip.pending-1"]);
        assert_eq!(deleted_ids.into_inner(), [2]);
    }

    #[test]
    fn partial_untagged_upload_rolls_back_confirmed_assets() {
        use std::cell::RefCell;

        let deleted_ids = RefCell::new(Vec::new());
        let fallback_names = RefCell::new(Vec::new());
        cleanup_partial_draft_upload_assets(
            &[ApiReleaseAsset { id: 7, name: "docs.zip".to_owned() }],
            &["docs.zip".to_owned(), "docs.zip.sha256".to_owned()],
            |id| {
                deleted_ids.borrow_mut().push(id);
                Ok(())
            },
            |names| {
                fallback_names.borrow_mut().extend(names.iter().map(|name| (*name).to_owned()));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(deleted_ids.into_inner(), [7]);
        assert_eq!(fallback_names.into_inner(), ["docs.zip.sha256"]);
    }

    #[test]
    fn partial_untagged_upload_only_cleans_attempted_names() {
        use std::cell::RefCell;

        let fallback_names = RefCell::new(Vec::new());
        cleanup_partial_draft_upload_assets(
            &[],
            &["docs.zip".to_owned()],
            |_| panic!("no upload was confirmed"),
            |names| {
                fallback_names.borrow_mut().extend(names.iter().map(|name| (*name).to_owned()));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(fallback_names.into_inner(), ["docs.zip"]);
    }

    #[test]
    fn draft_asset_cleanup_retries_a_lagging_listing() {
        use std::cell::Cell;

        let attempts = Cell::new(0);
        let pauses = Cell::new(0);
        let asset_id = retry_asset_lookup(
            "docs.zip.pending-1",
            3,
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 1 {
                    Ok(BTreeMap::new())
                } else {
                    Ok(BTreeMap::from([("docs.zip.pending-1".to_owned(), 7)]))
                }
            },
            || pauses.set(pauses.get() + 1),
        )
        .unwrap();

        assert_eq!(asset_id, Some(7));
        assert_eq!(attempts.get(), 2);
        assert_eq!(pauses.get(), 1);
    }

    #[test]
    fn missing_github_cli_has_an_actionable_error() {
        let error = github_cli_error(
            "cannot inspect GitHub release",
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        assert!(error.to_string().contains("run this command through nix develop .#build"));
    }

    #[test]
    fn failed_asset_rename_rolls_back_completed_renames() {
        use std::cell::{Cell, RefCell};

        let names = RefCell::new(BTreeMap::from([
            (1, "docs.zip".to_owned()),
            (2, "docs.zip.sha256".to_owned()),
            (3, "docs.zip.pending".to_owned()),
        ]));
        let calls = Cell::new(0);
        let renames = [
            AssetRename { id: 1, from: "docs.zip".to_owned(), to: "docs.zip.previous".to_owned() },
            AssetRename {
                id: 2,
                from: "docs.zip.sha256".to_owned(),
                to: "docs.zip.sha256.previous".to_owned(),
            },
            AssetRename { id: 3, from: "docs.zip.pending".to_owned(), to: "docs.zip".to_owned() },
        ];

        let error = apply_asset_renames(
            &renames,
            |id, name| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    names.borrow_mut().insert(id, name.to_owned());
                    bail!("injected rename failure");
                }
                names.borrow_mut().insert(id, name.to_owned());
                Ok(())
            },
            || Ok(()),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("rolled back"));
        assert_eq!(names.borrow().get(&1).map(String::as_str), Some("docs.zip"));
        assert_eq!(names.borrow().get(&2).map(String::as_str), Some("docs.zip.sha256"));
        assert_eq!(names.borrow().get(&3).map(String::as_str), Some("docs.zip.pending"));
    }

    #[test]
    fn failed_asset_verification_rolls_back_every_rename() {
        use std::cell::RefCell;

        let names =
            RefCell::new(BTreeMap::from([(1, "docs.zip".to_owned()), (2, "docs.zip.pending".to_owned())]));
        let renames = [
            AssetRename { id: 1, from: "docs.zip".to_owned(), to: "docs.zip.previous".to_owned() },
            AssetRename { id: 2, from: "docs.zip.pending".to_owned(), to: "docs.zip".to_owned() },
        ];

        apply_asset_renames(
            &renames,
            |id, name| {
                names.borrow_mut().insert(id, name.to_owned());
                Ok(())
            },
            || bail!("injected verification failure"),
        )
        .unwrap_err();

        assert_eq!(names.borrow().get(&1).map(String::as_str), Some("docs.zip"));
        assert_eq!(names.borrow().get(&2).map(String::as_str), Some("docs.zip.pending"));
    }
}
