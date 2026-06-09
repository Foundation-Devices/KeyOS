// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Shell completions generation command

use std::fs;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Command;
use clap_complete::{generate, Shell};
use foundation_plugins::install::PluginInstaller;

/// Execute the completions command
pub fn execute(shell: &str, install: bool) -> Result<()> {
    // Parse shell type
    let shell_type = parse_shell(shell)?;

    if install {
        install_completions(shell_type)?;
    } else {
        generate_completions(shell_type)?;
    }

    Ok(())
}

/// Parse shell string into Shell enum
fn parse_shell(shell: &str) -> Result<Shell> {
    match shell.to_lowercase().as_str() {
        "bash" => Ok(Shell::Bash),
        "zsh" => Ok(Shell::Zsh),
        "fish" => Ok(Shell::Fish),
        "powershell" | "pwsh" => Ok(Shell::PowerShell),
        _ => anyhow::bail!("Unsupported shell: {}. Supported shells: bash, zsh, fish, powershell", shell),
    }
}

/// Generate completions to stdout
fn generate_completions(shell: Shell) -> Result<()> {
    let shell_name = format!("{:?}", shell).to_lowercase();
    eprintln!("Generating shell completions for {}...", shell_name);

    let mut cmd = crate::cli::build_cli();
    add_installed_plugin_subcommands(&mut cmd);
    generate(shell, &mut cmd, "foundation", &mut io::stdout());

    eprintln!();
    eprintln!("Completion script written to stdout");
    eprintln!("To use these completions, save the output to a file and source it in your shell config");

    Ok(())
}

/// Install completions to the appropriate directory for the shell
fn install_completions(shell: Shell) -> Result<()> {
    let shell_name = format!("{:?}", shell).to_lowercase();
    eprintln!("Installing completions for {}...", shell_name);

    let completion_dir = get_completion_dir(shell)?;

    // Create completion directory if it doesn't exist
    fs::create_dir_all(&completion_dir)
        .with_context(|| format!("Failed to create completion directory: {}", completion_dir.display()))?;

    // Generate completion file path
    let completion_file = completion_dir.join(get_completion_filename(shell));

    // Generate completions to a string
    let mut buffer = Vec::new();
    let mut cmd = crate::cli::build_cli();
    add_installed_plugin_subcommands(&mut cmd);
    generate(shell, &mut cmd, "foundation", &mut buffer);

    // Write to file
    fs::write(&completion_file, buffer)
        .with_context(|| format!("Failed to write completion file: {}", completion_file.display()))?;

    eprintln!("Completions installed to {}", completion_file.display());
    eprintln!();

    // For bash on macOS, add source line to ~/.bash_profile
    #[cfg(target_os = "macos")]
    if shell == Shell::Bash {
        setup_bash_macos(&completion_file)?;
    }

    // For zsh, add fpath configuration to ~/.zshrc
    if shell == Shell::Zsh {
        setup_zsh()?;
    }

    // Print shell-specific instructions
    let instructions = match shell {
        Shell::Bash => {
            // Detect platform for bash
            #[cfg(target_os = "macos")]
            {
                "Completions have been enabled in ~/.bash_profile.\nRestart your shell or run: source ~/.bash_profile"
            }
            #[cfg(not(target_os = "macos"))]
            {
                "To enable completions, restart your shell or run: source ~/.bashrc"
            }
        }
        Shell::Zsh => {
            "Completions have been enabled in ~/.zshrc.\nRestart your shell or run: source ~/.zshrc"
        }
        Shell::Fish => "To enable completions, restart your shell",
        Shell::PowerShell => "To enable completions, restart your shell",
        _ => "To enable completions, restart your shell",
    };
    eprintln!("{}", instructions);

    Ok(())
}

fn add_installed_plugin_subcommands(cmd: &mut Command) {
    let installer = PluginInstaller::new();
    let installed = installer.list_installed().unwrap_or_default();
    let known: std::collections::HashSet<String> =
        cmd.get_subcommands().map(|subcommand| subcommand.get_name().to_string()).collect();

    // Replace+take pattern lets us extend the original `cmd` without cloning the
    // whole Command graph just to chain subcommand() calls.
    let mut augmented = std::mem::replace(cmd, Command::new("_placeholder"));
    for plugin in installed {
        if !known.contains(&plugin) {
            let leaked: &'static str = Box::leak(plugin.into_boxed_str());
            augmented = augmented.subcommand(Command::new(leaked));
        }
    }
    *cmd = augmented;
}

/// Get the completion directory for the given shell
fn get_completion_dir(shell: Shell) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;

    let dir = match shell {
        Shell::Bash => {
            // Try ~/.local/share/bash-completion/completions first (user-local)
            // Fall back to ~/.bash_completion.d if that doesn't work
            let local_dir = home.join(".local/share/bash-completion/completions");
            if local_dir.exists() || local_dir.parent().map(|p| p.exists()).unwrap_or(false) {
                local_dir
            } else {
                home.join(".bash_completion.d")
            }
        }
        Shell::Zsh => {
            // Use ~/.zsh/completions or create it
            home.join(".zsh/completions")
        }
        Shell::Fish => {
            // Fish standard config directory
            home.join(".config/fish/completions")
        }
        Shell::PowerShell => {
            // PowerShell profile directory
            #[cfg(target_os = "windows")]
            {
                home.join("Documents/PowerShell/Completions")
            }
            #[cfg(not(target_os = "windows"))]
            {
                home.join(".config/powershell/completions")
            }
        }
        _ => anyhow::bail!("Unsupported shell for installation"),
    };

    Ok(dir)
}

/// Get the completion filename for the given shell
fn get_completion_filename(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => "foundation",
        Shell::Zsh => "_foundation",
        Shell::Fish => "foundation.fish",
        Shell::PowerShell => "foundation.ps1",
        _ => "foundation",
    }
}

/// Set up bash completions on macOS by adding source line to ~/.bash_profile
#[cfg(target_os = "macos")]
fn setup_bash_macos(completion_file: &std::path::Path) -> Result<()> {
    use std::io::{Read, Write};

    let home = dirs::home_dir().context("Could not determine home directory")?;

    let bash_profile = home.join(".bash_profile");
    let source_line = format!("source {}", completion_file.display());

    // Check if the source line already exists
    if bash_profile.exists() {
        let mut content = String::new();
        fs::File::open(&bash_profile)?.read_to_string(&mut content)?;

        if content.contains(&source_line) {
            // Already configured
            return Ok(());
        }
    }

    // Append the source line to ~/.bash_profile
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&bash_profile)
        .with_context(|| format!("Failed to open {}", bash_profile.display()))?;

    writeln!(file, "\n# Foundation CLI completions")?;
    writeln!(file, "{}", source_line)?;

    eprintln!("Added source line to {}", bash_profile.display());

    Ok(())
}

/// Set up zsh completions by adding fpath configuration to ~/.zshrc
fn setup_zsh() -> Result<()> {
    use std::io::{Read, Write};

    let home = dirs::home_dir().context("Could not determine home directory")?;

    let zshrc = home.join(".zshrc");
    let fpath_line = "fpath=(~/.zsh/completions $fpath)";
    let compinit_line = "autoload -Uz compinit && compinit";

    // Check if already configured
    let mut content = String::new();
    if zshrc.exists() {
        fs::File::open(&zshrc)?.read_to_string(&mut content)?;

        if content.contains(fpath_line) && content.contains(compinit_line) {
            // Already configured
            return Ok(());
        }
    }

    // Append the configuration to ~/.zshrc
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&zshrc)
        .with_context(|| format!("Failed to open {}", zshrc.display()))?;

    writeln!(file, "\n# Foundation CLI completions")?;
    if !content.contains(fpath_line) {
        writeln!(file, "{}", fpath_line)?;
    }
    if !content.contains(compinit_line) {
        writeln!(file, "{}", compinit_line)?;
    }

    eprintln!("Added completion configuration to {}", zshrc.display());

    Ok(())
}
