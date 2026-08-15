// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{args::Args, build::FirmwarePaths},
    std::{
        env, fs,
        io::{self, Write},
        path::{Path, PathBuf},
        process::Command,
        thread,
        time::Duration,
    },
};

/// Summary of what happened during the git release operations
#[derive(Debug, Clone)]
pub struct ReleaseSummary {
    pub branch_created: bool,
    pub files_copied: bool,
    pub changes_committed: bool,
    pub branch_pushed: bool,
    pub pr_status: PrStatus,
}

#[derive(Debug, Clone)]
pub enum PrStatus {
    Created,
    AlreadyExists,
    GhNotAvailable,
}

/// Handle all git operations for the release
pub fn handle_release(
    args: &Args,
    firmware_paths: &FirmwarePaths,
    stdout: &mut impl Write,
) -> Result<ReleaseSummary, Error> {
    // Get the current directory (keyOS directory)
    let new_version_str = args.version.to_string();

    let keyos_dir = env::current_dir().map_err(Error::GetCurrentDir)?;
    writeln!(stdout, "Debug: keyOS directory is {}", keyos_dir.display()).map_err(Error::Stdout)?;

    // Ensure the KeyOS-Releases repo exists
    let releases_repo = keyos_dir.parent().ok_or(Error::NoParentDir)?.join("KeyOS-Releases");

    if !releases_repo.exists() {
        return Err(Error::ReleasesRepoNotFound(releases_repo));
    }

    // Change to releases repo directory
    env::set_current_dir(&releases_repo).map_err(Error::ChangeDir)?;

    // Make sure we're up to date with the remote before creating worktrees/branches
    writeln!(stdout, "Fetching latest from origin...").map_err(Error::Stdout)?;
    let _ = Command::new("git").args(["fetch", "--all", "--prune"]).status();

    // We will operate in a temporary worktree, so do not switch the primary checkout's branch
    // Note: We don't check for unstaged changes in the main repo because we use a worktree

    // Check if branch already exists and delete it automatically
    if branch_exists(&new_version_str)? {
        writeln!(stdout, "Branch {} already exists, deleting it...", new_version_str)
            .map_err(Error::Stdout)?;

        // Remove any worktrees that are using this branch to avoid deletion errors
        remove_worktrees_for_branch(&new_version_str, stdout)?;
        // Prune any stale worktree references (ignore errors)
        let _ = Command::new("git").args(["worktree", "prune"]).status();

        // Now delete the local branch and remote branch (ignore errors if they don't exist)
        let _ = run_git_command(&["branch", "-D", &new_version_str]);
        writeln!(stdout, "Deleting remote branch if it exists...").map_err(Error::Stdout)?;
        let _ = run_git_command_silent(&["push", "origin", "--delete", &new_version_str]);
    }

    // Delete any tags with the same name to avoid push ambiguity
    if tag_exists(&new_version_str)? {
        writeln!(stdout, "Tag {} exists, deleting it...", new_version_str).map_err(Error::Stdout)?;
        let _ = run_git_command(&["tag", "-d", &new_version_str]);
        writeln!(stdout, "Deleting remote tag if it exists...").map_err(Error::Stdout)?;
        let _ = run_git_command_silent(&["push", "origin", "--delete", &format!("refs/tags/{}", new_version_str)]);
    }

    // Create a short-lived worktree for the release branch so the primary checkout is unaffected
    let worktrees_dir = releases_repo.join(".worktrees");
    fs::create_dir_all(&worktrees_dir).map_err(Error::CreateDir)?;
    let worktree_path = worktrees_dir.join(&new_version_str);

    // If a directory remains at the previous worktree path, remove it (best-effort)
    if worktree_path.exists() {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force", &worktree_path.to_string_lossy()])
            .status();
        let _ = fs::remove_dir_all(&worktree_path);
    }

    writeln!(
        stdout,
        "Creating temporary worktree for branch {} at {}...",
        new_version_str,
        worktree_path.display()
    )
    .map_err(Error::Stdout)?;
    let status = Command::new("git")
        .args(["worktree", "add", "-B", &new_version_str, &worktree_path.to_string_lossy(), "origin/main"])
        .status()
        .map_err(Error::GitCommand)?;
    if !status.success() {
        return Err(Error::GitFailed(format!(
            "git worktree add -B {} {} origin/main",
            new_version_str,
            worktree_path.display()
        )));
    }

    // Switch into the worktree root for all subsequent operations
    env::set_current_dir(&worktree_path).map_err(Error::ChangeDir)?;

    // Create release directory
    let release_dir = Path::new(&new_version_str);
    if release_dir.exists() {
        writeln!(stdout, "WARNING: Release directory {} already exists", release_dir.display())
            .map_err(Error::Stdout)?;

        let mut input = String::new();
        print!("Do you want to overwrite it? (y/n): ");
        io::stdout().flush().map_err(Error::Stdout)?;
        io::stdin().read_line(&mut input).map_err(Error::UserInput)?;

        if input.trim() != "y" {
            return Err(Error::UserAborted);
        }

        writeln!(stdout, "Overwriting existing release directory...").map_err(Error::Stdout)?;
        fs::remove_dir_all(release_dir).map_err(Error::RemoveDir)?;
    }

    fs::create_dir_all(release_dir).map_err(Error::CreateDir)?;

    // Copy firmware files
    writeln!(stdout, "Copying firmware files and apps to release directory...").map_err(Error::Stdout)?;
    copy_firmware_files(&keyos_dir, firmware_paths, release_dir, stdout)?;

    // Add files to git
    writeln!(stdout, "Adding new files to the branch...").map_err(Error::Stdout)?;
    run_git_command(&["add", &new_version_str])?;

    // Check if there are changes to commit
    if !has_staged_changes()? {
        writeln!(
            stdout,
            "No changes to commit. This could mean the files are identical to the previous version."
        )
        .map_err(Error::Stdout)?;

        let mut input = String::new();
        print!("Do you want to force push the branch anyway? (y/n): ");
        io::stdout().flush().map_err(Error::Stdout)?;
        io::stdin().read_line(&mut input).map_err(Error::UserInput)?;

        if input.trim() != "y" {
            return Err(Error::UserAborted);
        }
    } else {
        // Commit the changes
        writeln!(stdout, "Committing changes...").map_err(Error::Stdout)?;
        ring_terminal_bell(stdout, "git commit (watch for passphrase prompt)")?;
        let commit_msg =
            format!("Add release {} (bootloader, recovery, main firmware, apps, blassets)", new_version_str);
        run_git_command(&["commit", "-m", &commit_msg])?;
    }

    // Push the branch
    writeln!(stdout, "Pushing branch to GitHub...").map_err(Error::Stdout)?;
    run_git_command(&["push", "-u", "origin", &new_version_str, "--force"])?;

    // Create GitHub PR if gh CLI is available
    let pr_status = create_github_pr(&new_version_str, stdout)?;

    // Cleanup: return to releases repo and remove the temporary worktree
    env::set_current_dir(&releases_repo).map_err(Error::ChangeDir)?;
    writeln!(stdout, "Removing temporary worktree at {}...", worktree_path.display())
        .map_err(Error::Stdout)?;
    stdout.flush().map_err(Error::Stdout)?;

    // Diagnostics to identify which step hangs
    writeln!(stdout, "Worktree path exists before removal: {}", worktree_path.exists())
        .map_err(Error::Stdout)?;
    writeln!(stdout, "fs::remove_dir_all arg: {}", worktree_path.display()).map_err(Error::Stdout)?;
    writeln!(stdout, "Attempting fs::remove_dir_all(...) cleanup...").map_err(Error::Stdout)?;
    match fs::remove_dir_all(&worktree_path) {
        Ok(()) => writeln!(stdout, "fs::remove_dir_all completed successfully").map_err(Error::Stdout)?,
        Err(e) => writeln!(stdout, "fs::remove_dir_all failed: {} (continuing)", e).map_err(Error::Stdout)?,
    }

    writeln!(stdout, "Running 'git worktree prune'...").map_err(Error::Stdout)?;
    match Command::new("git").args(["worktree", "prune"]).status() {
        Ok(status) => {
            writeln!(stdout, "git worktree prune exit status: {:?}", status).map_err(Error::Stdout)?
        }
        Err(e) => writeln!(stdout, "Failed to execute git worktree prune: {}", e).map_err(Error::Stdout)?,
    }

    writeln!(stdout, "Cleanup finished.").map_err(Error::Stdout)?;

    Ok(ReleaseSummary {
        branch_created: true,
        files_copied: true,
        changes_committed: true,
        branch_pushed: true,
        pr_status,
    })
}

fn copy_firmware_files(
    keyos_dir: &Path,
    firmware_paths: &FirmwarePaths,
    release_dir: &Path,
    stdout: &mut impl Write,
) -> Result<(), Error> {
    writeln!(
        stdout,
        "Debug: Copying signed bootloader, recovery, and main firmware to {}",
        release_dir.display()
    )
    .map_err(Error::Stdout)?;

    // Copy firmware binaries (signed bootloader and recovery are always required)
    // Note: We copy boot.cip (signed bootloader), NOT boot.bin
    // boot.bin contains unencrypted EXTRA_ENTROPY and must never be uploaded to the repository
    fs::copy(keyos_dir.join(&firmware_paths.bootloader_cip), release_dir.join("boot.cip"))
        .map_err(Error::CopyFile)?;
    fs::copy(keyos_dir.join(&firmware_paths.recovery), release_dir.join("recovery.bin"))
        .map_err(Error::CopyFile)?;

    // Create keyos directory and copy app.bin into it
    let keyos_release_dir = release_dir.join("keyos");
    fs::create_dir_all(&keyos_release_dir).map_err(Error::CreateDir)?;
    fs::copy(keyos_dir.join(&firmware_paths.app), keyos_release_dir.join("app.bin"))
        .map_err(Error::CopyFile)?;

    // Copy apps directory if it exists (into keyos/apps)
    if let Some(apps_dir) = &firmware_paths.apps_dir {
        writeln!(stdout, "Debug: Copying apps directory to {}", keyos_release_dir.display())
            .map_err(Error::Stdout)?;
        copy_dir_all(keyos_dir.join(apps_dir), keyos_release_dir.join("apps"))?;
    } else {
        writeln!(
            stdout,
            "Warning: Apps directory not found at {}",
            keyos_dir.join("target/armv7a-unknown-xous-elf/release/keyos/apps").display()
        )
        .map_err(Error::Stdout)?;
    }

    // Sideload bundles are release inputs, but they must stay outside keyos/apps so Factory,
    // Recovery, and OTA images do not install them as built-in apps.
    let sideload_release_dir = release_dir.join("sideload-apps");
    writeln!(
        stdout,
        "Debug: Copying Foundation sideload app bundles to {}",
        sideload_release_dir.display()
    )
    .map_err(Error::Stdout)?;
    copy_dir_all(
        keyos_dir.join(&firmware_paths.sideload_apps_dir),
        sideload_release_dir,
    )?;

    // Copy blassets directory (only .raw files, not source PNGs)
    writeln!(stdout, "Debug: Copying bootloader assets (.raw files only) to {}", release_dir.display())
        .map_err(Error::Stdout)?;
    copy_files_filtered(
        keyos_dir.join(&firmware_paths.blassets_dir),
        release_dir.join("blassets"),
        Some("raw"),
    )?;

    if let Some(common_assets_boot_dir) = &firmware_paths.common_assets_boot_dir {
        writeln!(stdout, "Debug: copying common boot asset files to {}", release_dir.display())
            .map_err(Error::Stdout)?;

        copy_files_filtered(keyos_dir.join(common_assets_boot_dir), release_dir.join("common-boot"), None)?;
    } else {
        writeln!(
            stdout,
            "Warning: Common boot assets directory not found at {}",
            keyos_dir.join("target/armv7a-unknown-xous-elf/release/common-boot").display()
        )
        .map_err(Error::Stdout)?;
    }

    if let Some(common_assets_dir) = &firmware_paths.common_assets_dir {
        writeln!(stdout, "Debug: copying common asset files to {}", keyos_release_dir.display())
            .map_err(Error::Stdout)?;

        copy_files_filtered(keyos_dir.join(common_assets_dir), keyos_release_dir.join("common"), None)?;
    } else {
        writeln!(
            stdout,
            "Warning: Common assets directory not found at {}",
            keyos_dir.join("target/armv7a-unknown-xous-elf/release/common").display()
        )
        .map_err(Error::Stdout)?;
    }

    Ok(())
}

fn run_git_command(args: &[&str]) -> Result<(), Error> {
    let status = Command::new("git").args(args).status().map_err(Error::GitCommand)?;

    if !status.success() {
        return Err(Error::GitFailed(args.join(" ")));
    }

    Ok(())
}

fn run_git_command_silent(args: &[&str]) -> Result<(), Error> {
    let output = Command::new("git")
        .args(args)
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(Error::GitCommand)?;

    if !output.success() {
        return Err(Error::GitFailed(args.join(" ")));
    }

    Ok(())
}

fn branch_exists(branch_name: &str) -> Result<bool, Error> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", branch_name])
        .output()
        .map_err(Error::GitCommand)?;

    Ok(output.status.success())
}

fn tag_exists(tag_name: &str) -> Result<bool, Error> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/tags/{}", tag_name)])
        .output()
        .map_err(Error::GitCommand)?;

    Ok(output.status.success())
}

fn has_staged_changes() -> Result<bool, Error> {
    let output =
        Command::new("git").args(["diff", "--cached", "--quiet"]).status().map_err(Error::GitCommand)?;

    // git diff --cached --quiet returns 0 if no changes, 1 if changes
    Ok(!output.success())
}

fn remove_worktrees_for_branch(branch_name: &str, stdout: &mut impl Write) -> Result<(), Error> {
    // Discover any worktrees that are using this branch and remove them so the branch can be deleted
    writeln!(stdout, "Checking for worktrees using branch {}...", branch_name).map_err(Error::Stdout)?;

    let output =
        Command::new("git").args(["worktree", "list", "--porcelain"]).output().map_err(Error::GitCommand)?;

    if !output.status.success() {
        return Err(Error::GitFailed("git worktree list --porcelain".to_string()));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut current_path: Option<String> = None;
    let mut targets: Vec<String> = Vec::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            current_path = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("branch ") {
            let br = rest.trim();
            let short = br.strip_prefix("refs/heads/").unwrap_or(br);
            if short == branch_name {
                if let Some(p) = current_path.clone() {
                    targets.push(p);
                }
            }
        } else if line.trim().is_empty() {
            current_path = None;
        }
    }

    for path in targets {
        writeln!(stdout, "Removing worktree at {} for branch {}...", path, branch_name)
            .map_err(Error::Stdout)?;
        let status = Command::new("git")
            .args(["worktree", "remove", "--force", &path])
            .status()
            .map_err(Error::GitCommand)?;
        if !status.success() {
            writeln!(stdout, "Warning: failed to remove worktree at {} (continuing)", path)
                .map_err(Error::Stdout)?;
        }
    }

    Ok(())
}

fn create_github_pr(version: &str, stdout: &mut impl Write) -> Result<PrStatus, Error> {
    // Check if gh CLI is available
    let gh_available =
        Command::new("gh").arg("--version").output().map(|output| output.status.success()).unwrap_or(false);

    if !gh_available {
        writeln!(
            stdout,
            "GitHub CLI (gh) not found. To create a PR, install gh or create it manually on GitHub."
        )
        .map_err(Error::Stdout)?;
        writeln!(
            stdout,
            "To install gh: brew install gh (macOS) or https://cli.github.com/manual/installation"
        )
        .map_err(Error::Stdout)?;
        writeln!(stdout, "Then create a PR by running: gh pr create --title \"Release {}\" --body \"This PR adds release {} (bootloader, recovery, main firmware, apps, blassets)\" --base main --head \"{}\"",
                version, version, version).map_err(Error::Stdout)?;
        return Ok(PrStatus::GhNotAvailable);
    }

    writeln!(stdout, "Creating GitHub Pull Request...").map_err(Error::Stdout)?;

    // Check if PR already exists
    let pr_exists = Command::new("gh")
        .args(["pr", "view", version, "--json", "state", "--jq", ".state"])
        .output()
        .map(|output| output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "OPEN")
        .unwrap_or(false);

    if pr_exists {
        writeln!(stdout, "Pull Request for branch {} already exists.", version).map_err(Error::Stdout)?;

        let mut input = String::new();
        print!("Do you want to close the existing PR and create a new one? (y/n): ");
        io::stdout().flush().map_err(Error::Stdout)?;
        io::stdin().read_line(&mut input).map_err(Error::UserInput)?;

        if input.trim() == "y" {
            writeln!(stdout, "Closing existing PR...").map_err(Error::Stdout)?;
            let close_status =
                Command::new("gh").args(["pr", "close", version]).status().map_err(Error::GitCommand)?;

            if !close_status.success() {
                return Err(Error::GitFailed("gh pr close".to_string()));
            }

            writeln!(stdout, "Creating new Pull Request...").map_err(Error::Stdout)?;
            create_new_pr(version, stdout)?;
            Ok(PrStatus::Created)
        } else {
            writeln!(stdout, "Keeping existing PR.").map_err(Error::Stdout)?;
            Ok(PrStatus::AlreadyExists)
        }
    } else {
        create_new_pr(version, stdout)?;
        Ok(PrStatus::Created)
    }
}

fn create_new_pr(version: &str, stdout: &mut impl Write) -> Result<(), Error> {
    let title = format!("Release {}", version);
    let body =
        format!("This PR adds release {} (bootloader, recovery, main firmware, apps, blassets)", version);

    let status = Command::new("gh")
        .args(["pr", "create", "--title", &title, "--body", &body, "--base", "main", "--head", version])
        .status()
        .map_err(Error::GitCommand)?;

    if status.success() {
        writeln!(stdout, "Pull Request created successfully!").map_err(Error::Stdout)?;
    } else {
        return Err(Error::GitFailed("gh pr create".to_string()));
    }

    Ok(())
}

fn ring_terminal_bell(stdout: &mut impl Write, action: &str) -> Result<(), Error> {
    writeln!(stdout, "Ringing bell before {}...", action).map_err(Error::Stdout)?;
    for _ in 0..5 {
        write!(stdout, "\x07").map_err(Error::Stdout)?;
        stdout.flush().map_err(Error::Stdout)?;
        thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> Result<(), Error> {
    fs::create_dir_all(&dst).map_err(Error::CreateDir)?;

    for entry in fs::read_dir(src).map_err(Error::ReadDir)? {
        let entry = entry.map_err(Error::ReadDir)?;
        let ty = entry.file_type().map_err(Error::ReadDir)?;

        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.as_ref().join(entry.file_name())).map_err(Error::CopyFile)?;
        }
    }

    Ok(())
}

fn copy_files_filtered(
    src: impl AsRef<Path>,
    dst: impl AsRef<Path>,
    filter_ext: Option<&str>,
) -> Result<(), Error> {
    fs::create_dir_all(&dst).map_err(Error::CreateDir)?;

    for entry in fs::read_dir(src).map_err(Error::ReadDir)? {
        let entry = entry.map_err(Error::ReadDir)?;
        let ty = entry.file_type().map_err(Error::ReadDir)?;

        if ty.is_dir() {
            // Recursively copy subdirectories (like fonts/)
            copy_files_filtered(entry.path(), dst.as_ref().join(entry.file_name()), filter_ext)?;
        } else {
            // Only copy .raw files, skip .png files and other source files
            if let Some(extension) = entry.path().extension() {
                if let Some(filter_ext) = filter_ext {
                    if extension != filter_ext {
                        continue;
                    }
                }

                fs::copy(entry.path(), dst.as_ref().join(entry.file_name())).map_err(Error::CopyFile)?;
            }
        }
    }

    Ok(())
}

#[derive(Debug)]
pub enum Error {
    GetCurrentDir(std::io::Error),
    NoParentDir,
    ReleasesRepoNotFound(PathBuf),
    ChangeDir(std::io::Error),
    GitCommand(std::io::Error),
    GitFailed(String),
    UserInput(std::io::Error),
    UserAborted,
    RemoveDir(std::io::Error),
    CreateDir(std::io::Error),
    CopyFile(std::io::Error),
    ReadDir(std::io::Error),
    Stdout(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::GetCurrentDir(e) => write!(f, "failed to get current directory: {}", e),
            Error::NoParentDir => write!(f, "current directory has no parent"),
            Error::ReleasesRepoNotFound(path) => {
                write!(f, "KeyOS-Releases repository not found at {}", path.display())
            }
            Error::ChangeDir(e) => write!(f, "failed to change directory: {}", e),
            Error::GitCommand(e) => write!(f, "failed to execute git command: {}", e),
            Error::GitFailed(cmd) => write!(f, "git command failed: {}", cmd),
            Error::UserInput(e) => write!(f, "failed to read user input: {}", e),
            Error::UserAborted => write!(f, "operation aborted by user"),
            Error::RemoveDir(e) => write!(f, "failed to remove directory: {}", e),
            Error::CreateDir(e) => write!(f, "failed to create directory: {}", e),
            Error::CopyFile(e) => write!(f, "failed to copy file: {}", e),
            Error::ReadDir(e) => write!(f, "failed to read directory: {}", e),
            Error::Stdout(e) => write!(f, "failed to write to stdout: {}", e),
        }
    }
}

impl std::error::Error for Error {}
