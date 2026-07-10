// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Nix development environment command

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use foundation_core::SdkRoot;
use tempfile::TempDir;

/// SDK version embedded in the CLI
const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");
const SDK_USER_FLAKE_SOURCE: &str = "nix/sdk-user-flake.nix";
const SDK_USER_FLAKE_LOCK_SOURCE: &str = "nix/sdk-user-flake.lock";

/// Execute the develop command
pub fn execute() -> Result<()> {
    // Check if Nix is installed
    println!("Checking for Nix installation...");
    if !is_nix_installed() {
        eprintln!("Nix is not installed");
        eprintln!("Please install Nix from: https://docs.determinate.systems/determinate-nix/");
        anyhow::bail!("Nix not installed");
    }

    println!("Locating Foundation SDK root...");
    let sdk = SdkRoot::discover().context("Could not locate the Foundation SDK root. Run this command from an SDK checkout or unpacked SDK bundle, or set FOUNDATION_SDK_ROOT.")?;

    println!("Foundation SDK CLI version: {}", SDK_VERSION);
    println!();

    // Only show trust prompt warning if config is not already trusted
    if !is_flake_config_trusted() {
        println!("Note: You may be prompted to trust configuration settings. Answer 'y' to both questions.");
        println!();
    }

    // Set up a clean interactive shell environment with a Foundation-specific rc file in ~/.foundation.
    let shell = SupportedShell::detect()?;
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let shell_config_dir = home.join(".foundation");
    fs::create_dir_all(&shell_config_dir)?;

    let shell_config_path = shell.config_path(&shell_config_dir);
    // Atomic create — if another `foundation develop` raced us to create the file,
    // treat AlreadyExists as success and leave the existing contents in place.
    match OpenOptions::new().write(true).create_new(true).open(&shell_config_path) {
        Ok(mut file) => {
            file.write_all(shell.default_config().as_bytes())?;
            println!(
                "{}",
                format!(
                    "Created foundation shell config at {}. You can add your aliases and customizations there.",
                    shell_config_path.display()
                )
            );
            println!();
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }

    // Run nix develop with a supported interactive shell and a Foundation-specific config directory.
    // We still allow system zsh config so terminals keep their normal line-editor bindings.
    println!("Using SDK root: {}", sdk.root().display());
    println!();

    let temp_flake = MinimalFlakeDir::create(sdk.root())?;
    let mut command = Command::new("nix");
    command
        .arg("develop")
        .arg(format!("path:{}", temp_flake.path().display()))
        .arg("--no-write-lock-file")
        .arg("-c");
    shell.configure_command(&mut command, &shell_config_dir);
    let status = command
        .env("FOUNDATION_SDK_ROOT", sdk.root())
        .env("FOUNDATION_SDK_BIN", sdk.root().join("bin"))
        .status()
        .context("Failed to start Nix development shell")?;

    if !status.success() {
        anyhow::bail!("Nix development shell exited with error");
    }

    Ok(())
}

struct MinimalFlakeDir {
    dir: TempDir,
}

impl MinimalFlakeDir {
    fn create(sdk_root: &Path) -> Result<Self> {
        // nix resolves the flake path through symlinks, so root the temp dir at a
        // canonicalized base (e.g. macOS /tmp -> /private/tmp) or nix rejects it.
        let temp_root = env::temp_dir().canonicalize().unwrap_or_else(|_| env::temp_dir());
        let dir = tempfile::Builder::new()
            .prefix("foundation-sdk-user-flake-")
            .tempdir_in(temp_root)
            .context("Failed to create temporary Foundation SDK flake directory")?;
        let path = dir.path();

        let flake_source = sdk_root.join(SDK_USER_FLAKE_SOURCE);
        let (flake_source, lock_path) = if flake_source.exists() {
            (flake_source, sdk_root.join(SDK_USER_FLAKE_LOCK_SOURCE))
        } else {
            (sdk_root.join("flake.nix"), sdk_root.join("flake.lock"))
        };

        fs::copy(&flake_source, path.join("flake.nix")).with_context(|| {
            format!("Failed to prepare temporary Foundation SDK flake source from {}", flake_source.display())
        })?;

        if lock_path.exists() {
            fs::copy(lock_path, path.join("flake.lock"))
                .context("Failed to copy Foundation SDK flake.lock into temporary flake source")?;
        }

        Ok(Self { dir })
    }

    fn path(&self) -> &Path { self.dir.path() }
}

/// Check if Nix is installed
fn is_nix_installed() -> bool {
    Command::new("nix").arg("--version").output().map(|output| output.status.success()).unwrap_or(false)
}

/// Check if Nix flake config is already trusted
/// Returns true if accept-flake-config is enabled, or if the user has previously
/// accepted the flake's settings (stored in ~/.local/share/nix/trusted-settings.json)
fn is_flake_config_trusted() -> bool {
    // Check if accept-flake-config is enabled (auto-trusts all flake configs)
    let output = Command::new("nix").args(["config", "show", "accept-flake-config"]).output();

    if let Ok(output) = output {
        let value = String::from_utf8_lossy(&output.stdout);
        if value.trim() == "true" {
            return true;
        }
    }

    // Check if the user has previously trusted these settings via the nix prompt
    let trusted_settings_path = dirs::data_local_dir().map(|d| d.join("nix").join("trusted-settings.json"));

    if let Some(path) = trusted_settings_path {
        if let Ok(contents) = fs::read_to_string(&path) {
            if let Ok(settings) = serde_json::from_str::<serde_json::Value>(&contents) {
                let substituters_trusted = settings
                    .get("extra-substituters")
                    .and_then(|v| v.as_object())
                    .map(|obj| {
                        obj.iter()
                            .any(|(k, v)| k.contains("nix-community.cachix.org") && v.as_bool() == Some(true))
                    })
                    .unwrap_or(false);

                let keys_trusted = settings
                    .get("extra-trusted-public-keys")
                    .and_then(|v| v.as_object())
                    .map(|obj| {
                        obj.iter().any(|(k, v)| {
                            k.contains("nix-community.cachix.org-1:") && v.as_bool() == Some(true)
                        })
                    })
                    .unwrap_or(false);

                if substituters_trusted && keys_trusted {
                    return true;
                }
            }
        }
    }

    false
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SupportedShell {
    Zsh(PathBuf),
    Bash(PathBuf),
}

impl SupportedShell {
    fn detect() -> Result<Self> {
        if let Some(shell) = env::var_os("SHELL").and_then(|value| shell_from_env_value(&value)) {
            return Ok(shell);
        }

        if let Some(path) = find_shell_in_path("zsh") {
            return Ok(Self::Zsh(path));
        }

        if let Some(path) = find_shell_in_path("bash") {
            return Ok(Self::Bash(path));
        }

        anyhow::bail!("foundation develop requires either zsh or bash to be installed");
    }

    fn config_path(&self, config_dir: &Path) -> PathBuf {
        match self {
            Self::Zsh(_) => config_dir.join(".zshrc"),
            Self::Bash(_) => config_dir.join(".bashrc"),
        }
    }

    fn default_config(&self) -> &'static str {
        match self {
            Self::Zsh(_) => {
                concat!(
                    "# Foundation development shell configuration\n",
                    "# Add your aliases and customizations here\n",
                    "\n",
                    "# Prompt: (foundation) ~/path %\n",
                    "PROMPT='(foundation) %~ %# '\n",
                    "\n",
                    "# Keep common terminal arrow-key history bindings working even without\n",
                    "# user-specific zsh configuration.\n",
                    "bindkey -e\n",
                    "bindkey '^[[A' up-line-or-history\n",
                    "bindkey '^[[B' down-line-or-history\n",
                    "bindkey '^[OA' up-line-or-history\n",
                    "bindkey '^[OB' down-line-or-history\n",
                    "\n",
                    "# Examples:\n",
                    "#   alias ll='ls -laG'\n",
                )
            }
            Self::Bash(_) => {
                concat!(
                    "# Foundation development shell configuration\n",
                    "# Add your aliases and customizations here\n",
                    "\n",
                    "# Prompt: (foundation) ~/path $ \n",
                    "PS1='(foundation) \\w \\\\$ '\n",
                    "\n",
                    "# Examples:\n",
                    "#   alias ll='ls -la'\n",
                )
            }
        }
    }

    fn configure_command(&self, command: &mut Command, config_dir: &Path) {
        match self {
            Self::Zsh(path) => {
                command.arg(path).arg("-i").env("ZDOTDIR", config_dir);
            }
            Self::Bash(path) => {
                command
                    .arg(path)
                    .arg("--noprofile")
                    .arg("--rcfile")
                    .arg(config_dir.join(".bashrc"))
                    .arg("-i");
            }
        }
    }
}

fn shell_from_env_value(value: &OsStr) -> Option<SupportedShell> {
    let candidate = PathBuf::from(value);
    let shell_name = candidate.file_name()?.to_str()?;

    match shell_name {
        "zsh" => resolve_shell_path(&candidate, "zsh").map(SupportedShell::Zsh),
        "bash" => resolve_shell_path(&candidate, "bash").map(SupportedShell::Bash),
        _ => None,
    }
}

fn resolve_shell_path(candidate: &Path, shell_name: &str) -> Option<PathBuf> {
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }

    find_shell_in_path(shell_name)
}

fn find_shell_in_path(shell_name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).map(|entry| entry.join(shell_name)).find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::{shell_from_env_value, MinimalFlakeDir, SupportedShell};
    use crate::test_support::make_temp_dir;

    #[test]
    fn ignores_unsupported_shell_env_values() {
        assert!(shell_from_env_value("/bin/fish".as_ref()).is_none());
    }

    #[test]
    fn zsh_default_config_includes_arrow_key_bindings() {
        let config = SupportedShell::Zsh("/bin/zsh".into()).default_config();

        assert!(config.contains("bindkey -e"));
        assert!(config.contains("bindkey '^[[A' up-line-or-history"));
        assert!(config.contains("bindkey '^[[B' down-line-or-history"));
        assert!(config.contains("bindkey '^[OA' up-line-or-history"));
        assert!(config.contains("bindkey '^[OB' down-line-or-history"));
    }

    #[test]
    fn zsh_shell_uses_foundation_zdotdir_without_disabling_global_rcs() {
        let shell = SupportedShell::Zsh("/bin/zsh".into());
        let mut command = Command::new("nix");
        shell.configure_command(&mut command, Path::new("/tmp/foundation-shell"));

        let args = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let envs = command
            .get_envs()
            .map(|(key, value)| {
                (key.to_string_lossy().into_owned(), value.map(|value| value.to_string_lossy().into_owned()))
            })
            .collect::<Vec<_>>();

        assert!(args.contains(&"/bin/zsh".to_string()));
        assert!(args.contains(&"-i".to_string()));
        assert!(!args.contains(&"--no-globalrcs".to_string()));
        assert!(envs
            .iter()
            .any(|(key, value)| { key == "ZDOTDIR" && value.as_deref() == Some("/tmp/foundation-shell") }));
    }

    #[test]
    fn minimal_flake_prefers_standalone_user_flake() {
        let sdk_root_dir = make_temp_dir("sdk-root");
        let sdk_root = sdk_root_dir.path();
        fs::create_dir_all(sdk_root.join("nix")).unwrap();
        fs::write(sdk_root.join("flake.nix"), "maintainer flake").unwrap();
        fs::write(sdk_root.join("flake.lock"), "maintainer lock").unwrap();
        fs::write(sdk_root.join("nix").join("sdk-user-flake.nix"), "user flake").unwrap();
        fs::write(sdk_root.join("nix").join("sdk-user-flake.lock"), "user lock").unwrap();

        let temp_flake = MinimalFlakeDir::create(sdk_root).unwrap();

        assert_eq!(fs::read_to_string(temp_flake.path().join("flake.nix")).unwrap(), "user flake");
        assert_eq!(fs::read_to_string(temp_flake.path().join("flake.lock")).unwrap(), "user lock");
        assert!(!temp_flake.path().join("nix").exists());
    }
}
