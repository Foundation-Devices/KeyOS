// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand};
use ignore::gitignore::GitignoreBuilder;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use walkdir::WalkDir;

const EXIT_SUCCESS: i32 = 0;
const EXIT_VERIFY_FAILED: i32 = 1;
const EXIT_INVALID: i32 = 2;
const EXIT_GIT: i32 = 3;
const EXIT_IO: i32 = 4;
const EXIT_GPG: i32 = 5;
const DEFAULT_PUSH_REMOTE: &str = "origin";
const DEFAULT_PUSH_BRANCH: &str = "main";

#[derive(Debug)]
struct AppError {
    code: i32,
    message: String,
}

impl AppError {
    fn new(code: i32, message: impl Into<String>) -> Self { Self { code, message: message.into() } }

    fn verify(message: impl Into<String>) -> Self { Self::new(EXIT_VERIFY_FAILED, message) }

    fn invalid(message: impl Into<String>) -> Self { Self::new(EXIT_INVALID, message) }

    fn git(message: impl Into<String>) -> Self { Self::new(EXIT_GIT, message) }

    fn io(message: impl Into<String>) -> Self { Self::new(EXIT_IO, message) }

    fn gpg(message: impl Into<String>) -> Self { Self::new(EXIT_GPG, message) }
}

type AppResult<T> = Result<T, AppError>;

#[derive(Parser, Debug)]
#[command(
    name = "repo-sync",
    about = "Publish release snapshots from a source repo to a destination repo.",
    version
)]
struct Cli {
    /// Config file path
    #[arg(short = 'c', long = "config", default_value = ".repo-sync.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Initialize a new destination repo for release syncing
    Init(InitArgs),
    /// Sync a release tag to the destination repo
    Sync(SyncArgs),
    /// Verify destination repo matches source repo at a given tag
    Verify(VerifyArgs),
    /// List synced releases in the destination repo
    List(ListArgs),
}

#[derive(Args, Debug)]
struct InitArgs {
    /// Path where the destination repo will be created
    #[arg(long, alias = "public")]
    destination: PathBuf,
    /// Optional remote URL to configure as 'origin'
    #[arg(long)]
    remote: Option<String>,
}

#[derive(Args, Debug)]
struct SyncArgs {
    /// Path to source repo
    #[arg(long, alias = "private")]
    source: Option<PathBuf>,
    /// Path to destination repo
    #[arg(long, alias = "public")]
    destination: Option<PathBuf>,
    /// Release tag to sync (e.g. v1.2.3)
    #[arg(long)]
    tag: String,
    /// GPG key ID to sign commits and tags
    #[arg(long = "gpg-key")]
    gpg_key: Option<String>,
    /// Optional exclude file path
    #[arg(long)]
    exclude: Option<PathBuf>,
    /// Commit message template
    #[arg(long)]
    message: Option<String>,
    /// Force-push to destination remote after sync
    #[arg(long)]
    push: bool,
    /// Show what would change without modifying anything
    #[arg(long)]
    dry_run: bool,
    /// Verbose output
    #[arg(short = 'v', long)]
    verbose: bool,
}

#[derive(Args, Debug)]
struct VerifyArgs {
    /// Path to source repo
    #[arg(long, alias = "private")]
    source: Option<PathBuf>,
    /// Path to destination repo
    #[arg(long, alias = "public")]
    destination: Option<PathBuf>,
    /// Release tag to verify
    #[arg(long)]
    tag: String,
    /// Optional exclude file path
    #[arg(long)]
    exclude: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ListArgs {
    /// Path to destination repo
    #[arg(long, alias = "public")]
    destination: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct RepoSyncConfig {
    #[serde(default)]
    repo: RepoConfig,
    #[serde(default)]
    sync: SyncConfig,
    #[serde(default)]
    signing: SigningConfig,
    #[serde(default)]
    author: AuthorConfig,
}

#[derive(Debug, Default, Deserialize)]
struct RepoConfig {
    #[serde(alias = "private")]
    source: Option<String>,
    #[serde(alias = "public")]
    destination: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SyncConfig {
    exclude: Option<String>,
    message: Option<String>,
    push: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct SigningConfig {
    key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AuthorConfig {
    name: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Clone)]
struct Author {
    name: String,
    email: String,
}

#[derive(Debug, Clone)]
struct SigningContext {
    gpg_program: String,
    key: Option<String>,
}

#[derive(Debug)]
struct SyncOptions {
    source_repo: PathBuf,
    destination_repo: PathBuf,
    tag: String,
    gpg_key_override: Option<String>,
    exclude_file: Option<PathBuf>,
    message_template: String,
    push: bool,
    dry_run: bool,
    verbose: bool,
}

#[derive(Debug)]
struct VerifyOptions {
    source_repo: PathBuf,
    destination_repo: PathBuf,
    tag: String,
    exclude_file: Option<PathBuf>,
}

#[derive(Debug)]
struct ListOptions {
    destination_repo: PathBuf,
}

#[derive(Debug)]
struct RewriteCommit {
    old_sha: String,
    message: String,
    tags: Vec<String>,
}

fn main() {
    let cli = Cli::parse();
    let result = run(cli);
    match result {
        Ok(()) => std::process::exit(EXIT_SUCCESS),
        Err(err) => {
            eprintln!("{}", err.message);
            std::process::exit(err.code);
        }
    }
}

fn run(cli: Cli) -> AppResult<()> {
    match cli.command {
        Commands::Init(args) => run_init(args),
        Commands::Sync(args) => {
            let (config, config_base) = load_config(&cli.config)?;
            run_sync(args, &config, &config_base)
        }
        Commands::Verify(args) => {
            let (config, config_base) = load_config(&cli.config)?;
            run_verify(args, &config, &config_base)
        }
        Commands::List(args) => {
            let (config, config_base) = load_config(&cli.config)?;
            run_list(args, &config, &config_base)
        }
    }
}

fn run_init(args: InitArgs) -> AppResult<()> {
    let destination_repo = absolutize_from_cwd(&args.destination)?;

    if destination_repo.exists() {
        if !destination_repo.is_dir() {
            return Err(AppError::invalid(format!(
                "Destination path is not a directory: {}",
                destination_repo.display()
            )));
        }
        if is_git_repo(&destination_repo) {
            return Err(AppError::invalid(format!(
                "Directory already contains a git repository: {}",
                destination_repo.display()
            )));
        }
    } else {
        fs::create_dir_all(&destination_repo).map_err(|e| {
            AppError::io(format!(
                "Failed to create destination directory '{}': {e}",
                destination_repo.display()
            ))
        })?;
    }

    let mut init_cmd = Command::new("git");
    init_cmd.arg("init").arg("--initial-branch").arg("main").arg(&destination_repo);
    run_command(
        init_cmd,
        EXIT_GIT,
        &format!("Failed to initialize git repository at '{}'", destination_repo.display()),
    )?;

    if let Some(remote) = args.remote {
        git_output(
            &destination_repo,
            ["remote", "add", "origin", remote.as_str()],
            EXIT_GIT,
            "Failed to configure origin remote",
        )?;
    }

    println!("Initialized destination repository at {}", destination_repo.display());
    Ok(())
}

fn run_sync(args: SyncArgs, config: &RepoSyncConfig, config_base: &Path) -> AppResult<()> {
    let options = resolve_sync_options(args, config, config_base)?;

    validate_tag_with_confirmation(&options.tag)?;
    ensure_git_repo(&options.source_repo, "source")?;
    ensure_git_repo(&options.destination_repo, "destination")?;
    ensure_tag_exists(&options.source_repo, &options.tag, "source")?;
    if options.push {
        ensure_push_remote_configured(&options.destination_repo)?;
    }

    let signing = resolve_signing_context(
        options.gpg_key_override.clone(),
        &config.signing.key,
        &options.destination_repo,
    )?;
    let author = resolve_author(config, &options.destination_repo)?;

    if options.verbose {
        eprintln!(
            "Reminder: for GitHub 'Verified', ensure author.email matches the GPG key UID and the key is uploaded to GitHub."
        );
    }

    let snapshot_temp = create_temp_dir("repo-sync-source-snapshot")?;
    let source_snapshot = snapshot_temp.path().join("snapshot");
    fs::create_dir_all(&source_snapshot).map_err(|e| {
        AppError::io(format!(
            "Failed to prepare source snapshot directory '{}': {e}",
            source_snapshot.display()
        ))
    })?;
    export_snapshot(&options.source_repo, &options.tag, &source_snapshot)?;
    if let Some(exclude_file) = &options.exclude_file {
        apply_excludes(&source_snapshot, exclude_file)?;
    }

    let main_commits = main_commits(&options.destination_repo)?;
    let existing_tag_commit = resolve_tag_commit(&options.destination_repo, &options.tag)?;
    let existing_index = if let Some(commit) = existing_tag_commit {
        Some(main_commits.iter().position(|sha| sha == &commit).ok_or_else(|| {
            AppError::git(format!("Tag '{}' exists in destination repo but is not on 'main'", options.tag))
        })?)
    } else {
        None
    };

    if options.dry_run {
        println!("Dry run only. No changes will be made.");
        println!("source: {}", options.source_repo.display());
        println!("destination: {}", options.destination_repo.display());
        println!("tag: {}", options.tag);
        println!(
            "mode: {}",
            if existing_index.is_some() {
                "re-sync (replace existing release and rewrite following commits)"
            } else {
                "new sync (append release commit)"
            }
        );
        println!(
            "exclude: {}",
            options
                .exclude_file
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "none".to_string())
        );
        println!(
            "signing-key: {}",
            signing.key.as_deref().map(ToString::to_string).unwrap_or_else(|| "default GPG key".to_string())
        );
        println!("push: {}", options.push);
        return Ok(());
    }

    ensure_clean_worktree(&options.destination_repo)?;
    checkout_main_branch(&options.destination_repo)?;

    let commit_message = render_message_template(&options.message_template, &options.tag);
    if let Some(index) = existing_index {
        rewrite_release_history(
            &options.destination_repo,
            &options.tag,
            &source_snapshot,
            &commit_message,
            &author,
            &signing,
            &main_commits,
            index,
        )?;
    } else {
        append_release_commit(
            &options.destination_repo,
            &options.tag,
            &source_snapshot,
            &commit_message,
            &author,
            &signing,
        )?;
    }

    if options.push {
        push_destination_main_and_tags(&options.destination_repo)?;
    }

    println!("Synced {}", options.tag);
    Ok(())
}

fn run_verify(args: VerifyArgs, config: &RepoSyncConfig, config_base: &Path) -> AppResult<()> {
    let options = resolve_verify_options(args, config, config_base)?;

    validate_tag_with_confirmation(&options.tag)?;
    ensure_git_repo(&options.source_repo, "source")?;
    ensure_git_repo(&options.destination_repo, "destination")?;
    ensure_tag_exists(&options.source_repo, &options.tag, "source")?;
    ensure_tag_exists(&options.destination_repo, &options.tag, "destination")?;

    let source_temp = create_temp_dir("repo-sync-verify-source")?;
    let source_snapshot = source_temp.path().join("snapshot");
    fs::create_dir_all(&source_snapshot).map_err(|e| {
        AppError::io(format!("Failed to prepare source verify snapshot '{}': {e}", source_snapshot.display()))
    })?;
    export_snapshot(&options.source_repo, &options.tag, &source_snapshot)?;
    if let Some(exclude_file) = &options.exclude_file {
        apply_excludes(&source_snapshot, exclude_file)?;
    }

    let destination_temp = create_temp_dir("repo-sync-verify-destination")?;
    let destination_snapshot = destination_temp.path().join("snapshot");
    fs::create_dir_all(&destination_snapshot).map_err(|e| {
        AppError::io(format!(
            "Failed to prepare destination verify snapshot '{}': {e}",
            destination_snapshot.display()
        ))
    })?;
    export_snapshot(&options.destination_repo, &options.tag, &destination_snapshot)?;

    let source_manifest = hash_manifest(&source_snapshot)?;
    let destination_manifest = hash_manifest(&destination_snapshot)?;

    let mut missing_in_destination = Vec::new();
    let mut extra_in_destination = Vec::new();
    let mut content_mismatch = Vec::new();

    for (path, source_hash) in &source_manifest {
        match destination_manifest.get(path) {
            Some(destination_hash) if destination_hash == source_hash => {}
            Some(_) => content_mismatch.push(path.clone()),
            None => missing_in_destination.push(path.clone()),
        }
    }
    for path in destination_manifest.keys() {
        if !source_manifest.contains_key(path) {
            extra_in_destination.push(path.clone());
        }
    }

    if missing_in_destination.is_empty() && extra_in_destination.is_empty() && content_mismatch.is_empty() {
        println!("Verification succeeded for {}", options.tag);
        return Ok(());
    }

    eprintln!("Verification failed for {}.", options.tag);
    for path in missing_in_destination {
        eprintln!("missing in destination: {path}");
    }
    for path in extra_in_destination {
        eprintln!("extra in destination: {path}");
    }
    for path in content_mismatch {
        eprintln!("content mismatch: {path}");
    }

    Err(AppError::verify("Verification failed"))
}

fn run_list(args: ListArgs, config: &RepoSyncConfig, config_base: &Path) -> AppResult<()> {
    let options = resolve_list_options(args, config, config_base)?;
    ensure_git_repo(&options.destination_repo, "destination")?;

    let commits = main_commits(&options.destination_repo)?;
    for commit in commits {
        let tags = tags_for_commit(&options.destination_repo, &commit)?;
        if tags.is_empty() {
            continue;
        }
        let date = commit_date(&options.destination_repo, &commit)?;
        for tag in tags {
            println!("{tag} {commit} {date}");
        }
    }
    Ok(())
}

fn resolve_sync_options(
    args: SyncArgs,
    config: &RepoSyncConfig,
    config_base: &Path,
) -> AppResult<SyncOptions> {
    let source_repo = if let Some(source_path) = args.source {
        absolutize_from_cwd(&source_path)?
    } else if let Some(source_path) = &config.repo.source {
        absolutize_from_config(source_path, config_base)
    } else {
        absolutize_from_cwd(Path::new("."))?
    };

    let destination_repo = if let Some(destination_path) = args.destination {
        absolutize_from_cwd(&destination_path)?
    } else if let Some(destination_path) = &config.repo.destination {
        absolutize_from_config(destination_path, config_base)
    } else {
        return Err(AppError::invalid(
            "Missing required setting: destination repo path (--destination or [repo].destination in config)",
        ));
    };

    let exclude_file = if let Some(exclude) = args.exclude {
        Some(resolve_exclude_path(&source_repo, &exclude))
    } else if let Some(exclude) = &config.sync.exclude {
        Some(resolve_exclude_path(&source_repo, Path::new(exclude)))
    } else {
        None
    };

    let message_template =
        args.message.or_else(|| config.sync.message.clone()).unwrap_or_else(|| "Release {tag}".to_string());
    let push = if args.push { true } else { config.sync.push.unwrap_or(false) };

    Ok(SyncOptions {
        source_repo,
        destination_repo,
        tag: args.tag,
        gpg_key_override: args.gpg_key,
        exclude_file,
        message_template,
        push,
        dry_run: args.dry_run,
        verbose: args.verbose,
    })
}

fn resolve_verify_options(
    args: VerifyArgs,
    config: &RepoSyncConfig,
    config_base: &Path,
) -> AppResult<VerifyOptions> {
    let source_repo = if let Some(source_path) = args.source {
        absolutize_from_cwd(&source_path)?
    } else if let Some(source_path) = &config.repo.source {
        absolutize_from_config(source_path, config_base)
    } else {
        absolutize_from_cwd(Path::new("."))?
    };

    let destination_repo = if let Some(destination_path) = args.destination {
        absolutize_from_cwd(&destination_path)?
    } else if let Some(destination_path) = &config.repo.destination {
        absolutize_from_config(destination_path, config_base)
    } else {
        return Err(AppError::invalid(
            "Missing required setting: destination repo path (--destination or [repo].destination in config)",
        ));
    };

    let exclude_file = if let Some(exclude) = args.exclude {
        Some(resolve_exclude_path(&source_repo, &exclude))
    } else if let Some(exclude) = &config.sync.exclude {
        Some(resolve_exclude_path(&source_repo, Path::new(exclude)))
    } else {
        None
    };

    Ok(VerifyOptions { source_repo, destination_repo, tag: args.tag, exclude_file })
}

fn resolve_list_options(
    args: ListArgs,
    config: &RepoSyncConfig,
    config_base: &Path,
) -> AppResult<ListOptions> {
    let destination_repo = if let Some(destination_path) = args.destination {
        absolutize_from_cwd(&destination_path)?
    } else if let Some(destination_path) = &config.repo.destination {
        absolutize_from_config(destination_path, config_base)
    } else {
        return Err(AppError::invalid(
            "Missing required setting: destination repo path (--destination or [repo].destination in config)",
        ));
    };

    Ok(ListOptions { destination_repo })
}

fn load_config(path: &Path) -> AppResult<(RepoSyncConfig, PathBuf)> {
    let cwd = std::env::current_dir()
        .map_err(|e| AppError::io(format!("Failed to read current working directory: {e}")))?;
    let config_path = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
    let default_config_path = cwd.join(".repo-sync.toml");

    if !config_path.exists() {
        if config_path == default_config_path {
            return Ok((RepoSyncConfig::default(), cwd));
        }
        return Err(AppError::invalid(format!("Config file not found: {}", config_path.display())));
    }

    let contents = fs::read_to_string(&config_path).map_err(|e| {
        AppError::invalid(format!("Failed to read config file '{}': {e}", config_path.display()))
    })?;
    let config: RepoSyncConfig = toml::from_str(&contents).map_err(|e| {
        AppError::invalid(format!("Failed to parse config file '{}': {e}", config_path.display()))
    })?;

    let config_base = config_path.parent().map(Path::to_path_buf).unwrap_or_else(|| cwd.clone());

    Ok((config, config_base))
}

fn resolve_author(config: &RepoSyncConfig, destination_repo: &Path) -> AppResult<Author> {
    let name = if let Some(name) = &config.author.name {
        name.clone()
    } else {
        git_config_get(destination_repo, "user.name")?.ok_or_else(|| {
            AppError::invalid(
                "Missing author.name: set [author].name in config or configure user.name in the destination repo",
            )
        })?
    };

    let email = if let Some(email) = &config.author.email {
        email.clone()
    } else {
        git_config_get(destination_repo, "user.email")?.ok_or_else(|| {
            AppError::invalid(
                "Missing author.email: set [author].email in config or configure user.email in the destination repo",
            )
        })?
    };

    Ok(Author { name, email })
}

fn resolve_signing_context(
    cli_key: Option<String>,
    config_key: &Option<String>,
    destination_repo: &Path,
) -> AppResult<SigningContext> {
    let gpg_program = find_gpg_program()?;
    let key = if let Some(cli_key) = cli_key {
        Some(cli_key)
    } else if let Some(config_key) = config_key {
        Some(config_key.clone())
    } else if let Some(repo_key) = git_config_get(destination_repo, "user.signingkey")? {
        Some(repo_key)
    } else {
        None
    };

    validate_signing_key(&gpg_program, key.as_deref())?;
    Ok(SigningContext { gpg_program, key })
}

fn find_gpg_program() -> AppResult<String> {
    for candidate in ["gpg", "gpg2"] {
        let output = Command::new(candidate).arg("--version").output();
        if let Ok(output) = output
            && output.status.success()
        {
            return Ok(candidate.to_string());
        }
    }
    Err(AppError::gpg("Could not find gpg or gpg2 in PATH"))
}

fn validate_signing_key(gpg_program: &str, key: Option<&str>) -> AppResult<()> {
    let mut cmd = Command::new(gpg_program);
    cmd.arg("--list-secret-keys").arg("--with-colons");
    if let Some(key) = key {
        cmd.arg(key);
    }

    let output = run_command(cmd, EXIT_GPG, "Failed to query GPG secret keys")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let has_secret_key = stdout.lines().any(|line| line.starts_with("sec:"));
    if !has_secret_key {
        if let Some(key) = key {
            return Err(AppError::gpg(format!("No usable GPG secret key found for '{key}'")));
        }
        return Err(AppError::gpg("No usable GPG secret key found and no default signing key is configured"));
    }

    Ok(())
}

fn ensure_git_repo(repo: &Path, label: &str) -> AppResult<()> {
    if !repo.exists() {
        return Err(AppError::invalid(format!("{label} repo path does not exist: {}", repo.display())));
    }
    if !repo.is_dir() {
        return Err(AppError::invalid(format!("{label} repo path is not a directory: {}", repo.display())));
    }
    if !is_git_repo(repo) {
        return Err(AppError::invalid(format!(
            "{label} repo is not a valid git repository: {}",
            repo.display()
        )));
    }
    Ok(())
}

fn is_git_repo(path: &Path) -> bool {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(path).arg("rev-parse").arg("--is-inside-work-tree");
    cmd.output().map(|output| output.status.success()).unwrap_or(false)
}

fn ensure_tag_exists(repo: &Path, tag: &str, label: &str) -> AppResult<()> {
    let tag_ref = format!("refs/tags/{tag}^{{}}");
    if !git_succeeds(repo, ["rev-parse", "--verify", "--quiet", tag_ref.as_str()]) {
        return Err(AppError::invalid(format!("Tag '{tag}' does not exist in {label} repo")));
    }
    Ok(())
}

fn validate_tag_with_confirmation(tag: &str) -> AppResult<()> {
    if is_semver_tag(tag) {
        return Ok(());
    }

    eprintln!("Warning: Tag '{tag}' does not follow the expected vMAJOR.MINOR.PATCH format.");
    if !io::stdin().is_terminal() {
        return Err(AppError::invalid("Aborted due to non-standard tag format in non-interactive mode"));
    }

    eprint!("Continue with non-standard tag format? [y/N] ");
    io::stderr().flush().map_err(|e| AppError::io(format!("Failed to flush prompt: {e}")))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| AppError::io(format!("Failed to read confirmation input: {e}")))?;
    let normalized = input.trim().to_ascii_lowercase();
    if normalized == "y" || normalized == "yes" {
        Ok(())
    } else {
        Err(AppError::invalid("Aborted due to non-standard tag format"))
    }
}

fn is_semver_tag(tag: &str) -> bool {
    if !tag.starts_with('v') {
        return false;
    }

    let mut parts = tag[1..].split('.');
    let major = parts.next();
    let minor = parts.next();
    let patch = parts.next();
    if parts.next().is_some() {
        return false;
    }

    for part in [major, minor, patch] {
        let Some(value) = part else { return false };
        if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }

    true
}

fn export_snapshot(repo: &Path, rev: &str, destination: &Path) -> AppResult<()> {
    let mut archive_cmd = git_command(repo);
    archive_cmd.arg("archive").arg("--format=tar").arg(rev).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = archive_cmd.spawn().map_err(|e| {
        AppError::git(format!(
            "Failed to export snapshot from '{}' at revision '{}': {e}",
            repo.display(),
            rev
        ))
    })?;
    let archive_stdout = child.stdout.take().ok_or_else(|| {
        AppError::git(format!(
            "Failed to export snapshot from '{}' at revision '{}': git archive stdout was not captured",
            repo.display(),
            rev
        ))
    })?;

    let mut archive = tar::Archive::new(archive_stdout);
    let unpack_error = archive
        .unpack(destination)
        .err()
        .map(|e| AppError::io(format!("Failed to unpack archive to '{}': {e}", destination.display())));
    drop(archive);

    let output = child.wait_with_output().map_err(|e| {
        AppError::git(format!(
            "Failed to export snapshot from '{}' at revision '{}': {e}",
            repo.display(),
            rev
        ))
    })?;
    if !output.status.success() {
        return Err(AppError::git(format!(
            "Failed to export snapshot from '{}' at revision '{}': {}",
            repo.display(),
            rev,
            command_error_detail(&output)
        )));
    }
    if let Some(err) = unpack_error {
        return Err(err);
    }

    Ok(())
}

fn append_release_commit(
    destination_repo: &Path,
    tag: &str,
    source_snapshot: &Path,
    commit_message: &str,
    author: &Author,
    signing: &SigningContext,
) -> AppResult<()> {
    replace_working_tree(destination_repo, source_snapshot)?;
    let _sha = commit_signed(destination_repo, commit_message, author, signing)?;
    delete_tag_if_exists(destination_repo, tag)?;
    create_signed_tag(destination_repo, tag, signing)?;
    Ok(())
}

fn rewrite_release_history(
    destination_repo: &Path,
    tag: &str,
    source_snapshot: &Path,
    replacement_message: &str,
    author: &Author,
    signing: &SigningContext,
    main_commits: &[String],
    replace_index: usize,
) -> AppResult<()> {
    let replaced_sha = &main_commits[replace_index];
    let mut replacement_tags = tags_for_commit(destination_repo, replaced_sha)?;
    if !replacement_tags.iter().any(|t| t == tag) {
        replacement_tags.push(tag.to_string());
    }

    let mut rewrite_tail = Vec::new();
    for sha in main_commits.iter().skip(replace_index + 1) {
        rewrite_tail.push(RewriteCommit {
            old_sha: sha.clone(),
            message: commit_message(destination_repo, sha)?,
            tags: tags_for_commit(destination_repo, sha)?,
        });
    }

    let mut tags_to_recreate = BTreeSet::new();
    for release_tag in &replacement_tags {
        tags_to_recreate.insert(release_tag.clone());
    }
    for commit in &rewrite_tail {
        for release_tag in &commit.tags {
            tags_to_recreate.insert(release_tag.clone());
        }
    }

    let parent = if replace_index > 0 { Some(main_commits[replace_index - 1].clone()) } else { None };

    let branch_suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| AppError::io(format!("System clock error: {e}")))?
        .as_millis();
    let rewrite_branch = format!("repo-sync-rewrite-{branch_suffix}");

    if let Some(parent_sha) = parent {
        git_output(
            destination_repo,
            ["checkout", "-B", rewrite_branch.as_str(), parent_sha.as_str()],
            EXIT_GIT,
            "Failed to create rewrite branch from parent commit",
        )?;
    } else {
        git_output(
            destination_repo,
            ["checkout", "--orphan", rewrite_branch.as_str()],
            EXIT_GIT,
            "Failed to create orphan rewrite branch",
        )?;
        clear_working_tree(destination_repo)?;
    }

    for release_tag in &tags_to_recreate {
        delete_tag_if_exists(destination_repo, release_tag)?;
    }

    replace_working_tree(destination_repo, source_snapshot)?;
    let _replacement_sha = commit_signed(destination_repo, replacement_message, author, signing)?;
    for release_tag in &replacement_tags {
        create_signed_tag(destination_repo, release_tag, signing)?;
    }

    for commit in rewrite_tail {
        let snapshot_temp = create_temp_dir("repo-sync-rewrite-snapshot")?;
        let old_snapshot = snapshot_temp.path().join("snapshot");
        fs::create_dir_all(&old_snapshot).map_err(|e| {
            AppError::io(format!(
                "Failed to create rewrite snapshot directory '{}': {e}",
                old_snapshot.display()
            ))
        })?;
        export_snapshot(destination_repo, &commit.old_sha, &old_snapshot)?;
        replace_working_tree(destination_repo, &old_snapshot)?;
        let _new_sha = commit_signed(destination_repo, &commit.message, author, signing)?;
        for release_tag in commit.tags {
            create_signed_tag(destination_repo, &release_tag, signing)?;
        }
    }

    let new_tip = current_head_sha(destination_repo)?;
    git_output(
        destination_repo,
        ["branch", "-f", "main", new_tip.as_str()],
        EXIT_GIT,
        "Failed to move main branch to rewritten history",
    )?;
    git_output(destination_repo, ["checkout", "main"], EXIT_GIT, "Failed to check out main after rewrite")?;
    git_output(
        destination_repo,
        ["branch", "-D", rewrite_branch.as_str()],
        EXIT_GIT,
        "Failed to delete temporary rewrite branch",
    )?;

    Ok(())
}

fn ensure_clean_worktree(repo: &Path) -> AppResult<()> {
    let status = git_text(
        repo,
        ["status", "--porcelain"],
        EXIT_GIT,
        "Failed to inspect destination repo worktree status",
    )?;
    if !status.trim().is_empty() {
        return Err(AppError::git(format!("Destination repo has uncommitted changes: {}", repo.display())));
    }
    Ok(())
}

fn checkout_main_branch(repo: &Path) -> AppResult<()> {
    if current_head_ref(repo)?.as_deref() == Some("main") {
        return Ok(());
    }

    if git_succeeds(repo, ["show-ref", "--verify", "--quiet", "refs/heads/main"]) {
        git_output(repo, ["checkout", "main"], EXIT_GIT, "Failed to check out main branch")?;
    } else {
        git_output(repo, ["checkout", "--orphan", "main"], EXIT_GIT, "Failed to create orphan main branch")?;
    }
    Ok(())
}

fn ensure_push_remote_configured(repo: &Path) -> AppResult<()> {
    let output = git_output_allow_failure(
        repo,
        ["remote", "get-url", DEFAULT_PUSH_REMOTE],
        EXIT_GIT,
        &format!("Failed to check remote '{DEFAULT_PUSH_REMOTE}' configuration"),
    )?;
    if output.status.success() {
        return Ok(());
    }

    let repo_display = repo.display();
    Err(AppError::invalid(format!(
        "--push requires remote '{DEFAULT_PUSH_REMOTE}' in destination repo '{repo_display}'. \
Configure it with `git -C {repo_display} remote add {DEFAULT_PUSH_REMOTE} <url>` \
or rerun `repo-sync init --destination {repo_display} --remote <url>`."
    )))
}

fn push_destination_main_and_tags(repo: &Path) -> AppResult<()> {
    let main_refspec = format!("refs/heads/{DEFAULT_PUSH_BRANCH}:refs/heads/{DEFAULT_PUSH_BRANCH}");
    git_output(
        repo,
        ["push", "--force", DEFAULT_PUSH_REMOTE, main_refspec.as_str()],
        EXIT_GIT,
        "Failed to force-push destination main branch",
    )?;
    git_output(
        repo,
        ["push", "--force", DEFAULT_PUSH_REMOTE, "--tags"],
        EXIT_GIT,
        "Failed to force-push destination tags",
    )?;
    Ok(())
}

fn resolve_tag_commit(repo: &Path, tag: &str) -> AppResult<Option<String>> {
    let tag_ref = format!("refs/tags/{tag}^{{}}");
    if !git_succeeds(repo, ["rev-parse", "--verify", "--quiet", tag_ref.as_str()]) {
        return Ok(None);
    }

    let ref_name = format!("refs/tags/{tag}");
    let sha = git_text(
        repo,
        ["rev-list", "-n", "1", ref_name.as_str()],
        EXIT_GIT,
        &format!("Failed to resolve commit for tag '{tag}'"),
    )?;
    if sha.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(sha.trim().to_string()))
    }
}

fn main_commits(repo: &Path) -> AppResult<Vec<String>> {
    if !git_succeeds(repo, ["rev-parse", "--verify", "--quiet", "main^{commit}"]) {
        return Ok(Vec::new());
    }
    let output =
        git_text(repo, ["rev-list", "--reverse", "main"], EXIT_GIT, "Failed to list commits on main")?;
    Ok(output.lines().map(str::trim).filter(|line| !line.is_empty()).map(ToString::to_string).collect())
}

fn tags_for_commit(repo: &Path, commit: &str) -> AppResult<Vec<String>> {
    let output = git_text(
        repo,
        ["tag", "--points-at", commit],
        EXIT_GIT,
        &format!("Failed to list tags for commit '{commit}'"),
    )?;
    Ok(output.lines().map(str::trim).filter(|line| !line.is_empty()).map(ToString::to_string).collect())
}

fn commit_message(repo: &Path, commit: &str) -> AppResult<String> {
    let output = git_output(
        repo,
        ["show", "-s", "--format=%B", commit],
        EXIT_GIT,
        &format!("Failed to read commit message for '{commit}'"),
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn commit_date(repo: &Path, commit: &str) -> AppResult<String> {
    Ok(git_text(
        repo,
        ["show", "-s", "--format=%cI", commit],
        EXIT_GIT,
        &format!("Failed to read commit date for '{commit}'"),
    )?
    .trim()
    .to_string())
}

fn current_head_sha(repo: &Path) -> AppResult<String> {
    Ok(git_text(repo, ["rev-parse", "HEAD"], EXIT_GIT, "Failed to resolve HEAD SHA")?.trim().to_string())
}

fn current_head_ref(repo: &Path) -> AppResult<Option<String>> {
    let output = git_output_allow_failure(
        repo,
        ["symbolic-ref", "--short", "--quiet", "HEAD"],
        EXIT_GIT,
        "Failed to resolve current branch",
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        Ok(None)
    } else {
        Ok(Some(branch))
    }
}

fn git_config_get(repo: &Path, key: &str) -> AppResult<Option<String>> {
    let output = git_output_allow_failure(
        repo,
        ["config", "--get", key],
        EXIT_GIT,
        &format!("Failed to read git config key '{key}'"),
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn replace_working_tree(repo: &Path, snapshot: &Path) -> AppResult<()> {
    clear_working_tree(repo)?;
    copy_tree(snapshot, repo)?;
    git_output(repo, ["add", "-A"], EXIT_GIT, "Failed to stage updated destination snapshot")?;
    Ok(())
}

fn clear_working_tree(repo: &Path) -> AppResult<()> {
    let read_dir = fs::read_dir(repo).map_err(|e| {
        AppError::io(format!("Failed to read destination repo directory '{}': {e}", repo.display()))
    })?;
    for entry in read_dir {
        let entry = entry.map_err(|e| AppError::io(format!("Failed to read directory entry: {e}")))?;
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        remove_path(&path).map_err(|e| {
            AppError::io(format!("Failed to remove path '{}' while replacing snapshot: {e}", path.display()))
        })?;
    }
    Ok(())
}

fn copy_tree(source: &Path, dest_root: &Path) -> AppResult<()> {
    for entry in WalkDir::new(source).min_depth(1).follow_links(false) {
        let entry = entry.map_err(|e| AppError::io(format!("Failed while walking snapshot tree: {e}")))?;
        let rel = entry.path().strip_prefix(source).map_err(|e| {
            AppError::io(format!(
                "Failed to compute relative path '{}' from '{}': {e}",
                entry.path().display(),
                source.display()
            ))
        })?;
        let dest = dest_root.join(rel);

        if entry.file_type().is_dir() {
            fs::create_dir_all(&dest).map_err(|e| {
                AppError::io(format!(
                    "Failed to create directory '{}' in destination repo: {e}",
                    dest.display()
                ))
            })?;
            continue;
        }

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                AppError::io(format!("Failed to create parent directory '{}': {e}", parent.display()))
            })?;
        }

        if entry.file_type().is_file() {
            fs::copy(entry.path(), &dest).map_err(|e| {
                AppError::io(format!(
                    "Failed to copy '{}' to '{}': {e}",
                    entry.path().display(),
                    dest.display()
                ))
            })?;
        } else if entry.file_type().is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(entry.path()).map_err(|e| {
                    AppError::io(format!("Failed to read symlink '{}': {e}", entry.path().display()))
                })?;
                symlink(&target, &dest).map_err(|e| {
                    AppError::io(format!(
                        "Failed to recreate symlink '{}' -> '{}': {e}",
                        dest.display(),
                        target.display()
                    ))
                })?;
            }
            #[cfg(not(unix))]
            {
                return Err(AppError::io(format!(
                    "Symlink '{}' is not supported on this platform",
                    entry.path().display()
                )));
            }
        }
    }

    Ok(())
}

fn apply_excludes(snapshot_root: &Path, exclude_file: &Path) -> AppResult<()> {
    if !exclude_file.exists() {
        return Err(AppError::io(format!("Exclude file does not exist: {}", exclude_file.display())));
    }

    let mut builder = GitignoreBuilder::new(snapshot_root);
    builder.add(exclude_file);
    let matcher = builder.build().map_err(|e| {
        AppError::invalid(format!("Failed to parse exclude file '{}': {e}", exclude_file.display()))
    })?;

    let mut to_remove = Vec::new();
    for entry in WalkDir::new(snapshot_root).min_depth(1).follow_links(false) {
        let entry = entry.map_err(|e| AppError::io(format!("Failed while walking snapshot tree: {e}")))?;
        let rel = entry.path().strip_prefix(snapshot_root).map_err(|e| {
            AppError::io(format!(
                "Failed to compute relative path '{}' from snapshot root '{}': {e}",
                entry.path().display(),
                snapshot_root.display()
            ))
        })?;
        if matcher.matched_path_or_any_parents(rel, entry.file_type().is_dir()).is_ignore() {
            to_remove.push(entry.path().to_path_buf());
        }
    }

    to_remove.sort_by_key(|path| Reverse(path_depth(snapshot_root, path)));
    for path in to_remove {
        match remove_path(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(AppError::io(format!(
                    "Failed to remove excluded path '{}': {err}",
                    path.display()
                )))
            }
        }
    }

    Ok(())
}

fn hash_manifest(root: &Path) -> AppResult<BTreeMap<String, String>> {
    let mut manifest = BTreeMap::new();

    for entry in WalkDir::new(root).min_depth(1).follow_links(false) {
        let entry = entry
            .map_err(|e| AppError::io(format!("Failed while walking directory '{}': {e}", root.display())))?;
        if entry.file_type().is_dir() {
            continue;
        }

        let rel = entry.path().strip_prefix(root).map_err(|e| {
            AppError::io(format!(
                "Failed to compute relative path '{}' from '{}': {e}",
                entry.path().display(),
                root.display()
            ))
        })?;
        let rel_key = normalize_rel_path(rel);

        if entry.file_type().is_file() {
            let hash = sha256_file(entry.path())?;
            manifest.insert(rel_key, format!("file:{hash}"));
        } else if entry.file_type().is_symlink() {
            let hash = sha256_symlink(entry.path())?;
            manifest.insert(rel_key, format!("symlink:{hash}"));
        }
    }

    Ok(manifest)
}

fn sha256_file(path: &Path) -> AppResult<String> {
    let mut file = File::open(path)
        .map_err(|e| AppError::io(format!("Failed to open file '{}' for hashing: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = file.read(&mut buf).map_err(|e| {
            AppError::io(format!("Failed to read file '{}' for hashing: {e}", path.display()))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex_encode(hasher.finalize().as_slice()))
}

fn sha256_symlink(path: &Path) -> AppResult<String> {
    let target = fs::read_link(path)
        .map_err(|e| AppError::io(format!("Failed to read symlink '{}': {e}", path.display())))?;
    let mut hasher = Sha256::new();
    #[cfg(unix)]
    {
        hasher.update(target.as_os_str().as_bytes());
    }
    #[cfg(not(unix))]
    {
        hasher.update(target.to_string_lossy().as_bytes());
    }
    Ok(hex_encode(hasher.finalize().as_slice()))
}

fn commit_signed(repo: &Path, message: &str, author: &Author, signing: &SigningContext) -> AppResult<String> {
    let mut cmd = git_command(repo);
    cmd.arg("-c").arg("commit.gpgsign=true").arg("-c").arg(format!("gpg.program={}", signing.gpg_program));
    if let Some(key) = &signing.key {
        cmd.arg("-c").arg(format!("user.signingkey={key}"));
    }

    cmd.arg("commit").arg("--allow-empty").arg("-S").arg("-F").arg("-");
    cmd.env("GIT_AUTHOR_NAME", &author.name)
        .env("GIT_AUTHOR_EMAIL", &author.email)
        .env("GIT_COMMITTER_NAME", &author.name)
        .env("GIT_COMMITTER_EMAIL", &author.email)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child =
        cmd.spawn().map_err(|e| AppError::gpg(format!("Failed to spawn signed commit command: {e}")))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(message.as_bytes())
            .map_err(|e| AppError::gpg(format!("Failed to write commit message to git: {e}")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| AppError::gpg(format!("Failed waiting for signed commit command: {e}")))?;
    if !output.status.success() {
        return Err(AppError::gpg(format!(
            "Failed to create signed commit: {}",
            command_error_detail(&output)
        )));
    }

    current_head_sha(repo)
}

fn create_signed_tag(repo: &Path, tag: &str, signing: &SigningContext) -> AppResult<()> {
    let message = format!("Release {tag}");
    let mut cmd = git_command(repo);
    cmd.arg("-c").arg(format!("gpg.program={}", signing.gpg_program));
    if let Some(key) = &signing.key {
        cmd.arg("-c").arg(format!("user.signingkey={key}"));
    }

    cmd.arg("tag").arg("-s").arg(tag).arg("-m").arg(message);
    if let Some(key) = &signing.key {
        cmd.arg("-u").arg(key);
    }
    run_command(cmd, EXIT_GPG, &format!("Failed to create signed tag '{tag}'"))?;
    Ok(())
}

fn delete_tag_if_exists(repo: &Path, tag: &str) -> AppResult<()> {
    let tag_ref = format!("refs/tags/{tag}");
    if !git_succeeds(repo, ["rev-parse", "--verify", "--quiet", tag_ref.as_str()]) {
        return Ok(());
    }
    git_output(repo, ["tag", "-d", tag], EXIT_GIT, &format!("Failed to delete existing tag '{tag}'"))?;
    Ok(())
}

fn create_temp_dir(prefix: &str) -> AppResult<TempDir> {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .map_err(|e| AppError::io(format!("Failed to create temporary directory: {e}")))
}

fn git_command(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo);
    cmd
}

fn git_succeeds<I, S>(repo: &Path, args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = git_command(repo);
    cmd.args(args);
    cmd.output().map(|output| output.status.success()).unwrap_or(false)
}

fn git_output<I, S>(repo: &Path, args: I, code: i32, context: &str) -> AppResult<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = git_command(repo);
    cmd.args(args);
    run_command(cmd, code, context)
}

fn git_text<I, S>(repo: &Path, args: I, code: i32, context: &str) -> AppResult<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = git_output(repo, args, code, context)?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn git_output_allow_failure<I, S>(repo: &Path, args: I, code: i32, context: &str) -> AppResult<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = git_command(repo);
    cmd.args(args);
    run_command_allow_failure(cmd, code, context)
}

fn run_command(mut cmd: Command, code: i32, context: &str) -> AppResult<Output> {
    let output = cmd.output().map_err(|e| AppError::new(code, format!("{context}: {e}")))?;
    if !output.status.success() {
        return Err(AppError::new(code, format!("{context}: {}", command_error_detail(&output))));
    }
    Ok(output)
}

fn run_command_allow_failure(mut cmd: Command, code: i32, context: &str) -> AppResult<Output> {
    cmd.output().map_err(|e| AppError::new(code, format!("{context}: {e}")))
}

fn command_error_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("command exited with status {}", output.status)
    }
}

fn resolve_exclude_path(source_repo: &Path, raw: &Path) -> PathBuf {
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        source_repo.join(raw)
    }
}

fn absolutize_from_cwd(path: &Path) -> AppResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        let cwd = std::env::current_dir()
            .map_err(|e| AppError::io(format!("Failed to read current working directory: {e}")))?;
        Ok(cwd.join(path))
    }
}

fn absolutize_from_config(raw: &str, config_base: &Path) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        config_base.join(path)
    }
}

fn remove_path(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn path_depth(root: &Path, path: &Path) -> usize {
    path.strip_prefix(root).map(|relative| relative.components().count()).unwrap_or(0)
}

fn normalize_rel_path(path: &Path) -> String { path.to_string_lossy().replace('\\', "/") }

fn render_message_template(template: &str, tag: &str) -> String { template.replace("{tag}", tag) }

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(hex_nibble(byte >> 4));
        out.push(hex_nibble(byte & 0x0f));
    }
    out
}

fn hex_nibble(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'a' + (value - 10)) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_test_command(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git command should run in tests");
        assert!(output.status.success(), "git {:?} failed: {}", args, command_error_detail(&output));
    }

    fn git_test_commit(repo: &Path, message: &str) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("-c")
            .arg("commit.gpgsign=false")
            .arg("commit")
            .arg("-m")
            .arg(message)
            .output()
            .expect("git commit should run in tests");
        assert!(output.status.success(), "git commit failed: {}", command_error_detail(&output));
    }

    fn create_test_repo(files: &[(&str, &str)]) -> TempDir {
        let repo_dir = create_temp_dir("repo-sync-test-repo").expect("temp repo should be created");
        git_test_command(repo_dir.path(), &["init", "-q"]);
        git_test_command(repo_dir.path(), &["config", "user.name", "Repo Sync Test"]);
        git_test_command(repo_dir.path(), &["config", "user.email", "repo-sync-test@example.com"]);

        for (path, contents) in files {
            let file_path = repo_dir.path().join(path);
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).expect("test repo parent dir should be created");
            }
            fs::write(&file_path, contents).expect("test repo file should be written");
        }

        git_test_command(repo_dir.path(), &["add", "."]);
        git_test_commit(repo_dir.path(), "test snapshot");
        repo_dir
    }

    fn create_snapshot_destination() -> (TempDir, PathBuf) {
        let snapshot_temp =
            create_temp_dir("repo-sync-test-snapshot").expect("temp snapshot dir should be created");
        let destination = snapshot_temp.path().join("snapshot");
        fs::create_dir_all(&destination).expect("snapshot destination should be created");
        (snapshot_temp, destination)
    }

    #[test]
    fn semver_validation_accepts_expected_tags() {
        assert!(is_semver_tag("v0.0.0"));
        assert!(is_semver_tag("v1.2.3"));
        assert!(is_semver_tag("v10.42.7"));
    }

    #[test]
    fn semver_validation_rejects_non_conforming_tags() {
        assert!(!is_semver_tag("1.2.3"));
        assert!(!is_semver_tag("v1.2"));
        assert!(!is_semver_tag("v1.2.3.4"));
        assert!(!is_semver_tag("v1.2.x"));
        assert!(!is_semver_tag("release-v1.2.3"));
    }

    #[test]
    fn message_template_replaces_tag_placeholder() {
        assert_eq!(render_message_template("Release {tag}", "v1.2.3"), "Release v1.2.3");
    }

    #[test]
    fn export_snapshot_preserves_repo_file_named_like_temp_archive() {
        let repo_dir = create_test_repo(&[
            (".repo-sync-snapshot.tar", "repo-owned tarball contents"),
            ("docs/readme.txt", "hello"),
        ]);
        let (_snapshot_temp, destination) = create_snapshot_destination();

        export_snapshot(repo_dir.path(), "HEAD", &destination).expect("snapshot export should succeed");

        assert_eq!(
            fs::read_to_string(destination.join(".repo-sync-snapshot.tar"))
                .expect("repo file with archive name should be extracted"),
            "repo-owned tarball contents"
        );
        assert_eq!(
            fs::read_to_string(destination.join("docs/readme.txt")).expect("other files should be extracted"),
            "hello"
        );
    }

    #[test]
    fn export_snapshot_does_not_leave_temp_archive_in_destination() {
        let repo_dir = create_test_repo(&[("regular.txt", "hello")]);
        let (_snapshot_temp, destination) = create_snapshot_destination();

        export_snapshot(repo_dir.path(), "HEAD", &destination).expect("snapshot export should succeed");

        assert_eq!(
            fs::read_to_string(destination.join("regular.txt")).expect("regular file should be extracted"),
            "hello"
        );
        assert!(
            !destination.join(".repo-sync-snapshot.tar").exists(),
            "temporary archive should not be created inside the extraction directory"
        );
    }
}
