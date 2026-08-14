// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Shell completions generation command

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Command, CommandFactory};
use clap_complete::{generate, Shell};
#[cfg(feature = "experimental-plugins")]
use foundation_plugins::install::PluginInstaller;

use crate::cli::Cli;

#[derive(Args)]
pub struct CompletionsArgs {
    /// Shell type (bash, zsh, fish, powershell)
    #[arg(value_parser = clap::builder::PossibleValuesParser::new(["bash", "zsh", "fish", "powershell"]))]
    pub shell: String,

    /// Print completions to stdout instead of installing them
    #[arg(long, conflicts_with = "install")]
    pub stdout: bool,

    /// Retained for compatibility now that installation is the default
    #[arg(long, hide = true)]
    pub install: bool,
}

/// Execute the completions command
pub fn execute(args: &CompletionsArgs) -> Result<()> {
    let shell_type = parse_shell(&args.shell)?;

    if args.stdout {
        generate_completions(shell_type)?;
    } else {
        install_completions(shell_type)?;
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

    let mut cmd = Cli::command();
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
    let mut cmd = Cli::command();
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
    let zsh_configured = shell == Shell::Zsh
        && match setup_zsh() {
            Ok(()) => true,
            Err(error) => {
                eprintln!("Could not update ~/.zshrc automatically: {error:#}");
                false
            }
        };

    // Print shell-specific instructions
    let instructions = match shell {
        Shell::Bash => {
            // Detect platform for bash
            #[cfg(target_os = "macos")]
            {
                "Completions have been enabled in ~/.bash_profile.\nRestart your shell or run: source ~/.bash_profile"
                    .to_string()
            }
            #[cfg(not(target_os = "macos"))]
            {
                if completion_dir.ends_with(".bash_completion.d") {
                    format!(
                        "Add this line to ~/.bashrc, then restart your shell:\nsource {}",
                        shell_quote(&completion_file)
                    )
                } else {
                    "To enable completions, restart your shell".to_string()
                }
            }
        }
        Shell::Zsh => {
            if zsh_configured {
                "Completions have been enabled in ~/.zshrc.\nRestart your shell or run: source ~/.zshrc"
                    .to_string()
            } else {
                "Add these lines to ~/.zshrc, then restart your shell:\n\
                 fpath=(~/.zsh/completions $fpath)\n\
                 autoload -Uz compinit && compinit"
                    .to_string()
            }
        }
        Shell::Fish => "To enable completions, restart your shell".to_string(),
        Shell::PowerShell => format!(
            "Add this line to your PowerShell profile, then restart your shell:\n. {}",
            powershell_quote(&completion_file)
        ),
        _ => "To enable completions, restart your shell".to_string(),
    };
    eprintln!("{}", instructions);

    Ok(())
}

#[cfg(feature = "experimental-plugins")]
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

#[cfg(not(feature = "experimental-plugins"))]
fn add_installed_plugin_subcommands(_cmd: &mut Command) {}

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

fn shell_quote(path: &Path) -> String { format!("'{}'", path.display().to_string().replace('\'', "'\\''")) }

fn powershell_quote(path: &Path) -> String { format!("'{}'", path.display().to_string().replace('\'', "''")) }

/// Set up bash completions on macOS by adding source line to ~/.bash_profile
#[cfg(target_os = "macos")]
fn setup_bash_macos(completion_file: &std::path::Path) -> Result<()> {
    use std::io::{Read, Write};

    let home = dirs::home_dir().context("Could not determine home directory")?;

    let bash_profile = home.join(".bash_profile");
    let source_line = format!("source {}", shell_quote(completion_file));

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

    let mut content = String::new();
    if zshrc.exists() {
        fs::File::open(&zshrc)?.read_to_string(&mut content)?;

        let fpath_position = uncommented_position(&content, fpath_line);
        let compinit_position = uncommented_position(&content, compinit_line);
        if matches!((fpath_position, compinit_position), (Some(fpath), Some(compinit)) if fpath < compinit) {
            return Ok(());
        }

        // compinit only discovers completion directories already in fpath. If a
        // shell framework configured it first, move or insert our entry before it.
        if compinit_position.is_some() {
            if let Some(fpath_position) = fpath_position {
                if let Some(mut removal_range) = standalone_line_range(&content, fpath_position, fpath_line) {
                    if removal_range.start > 0 {
                        let previous_line = containing_line_range(&content, removal_range.start - 1);
                        if content[previous_line.clone()].trim() == "# Foundation CLI completions" {
                            removal_range.start = previous_line.start;
                        }
                    }
                    content.replace_range(removal_range, "");
                }
            }

            let compinit_position = uncommented_position(&content, compinit_line)
                .context("Could not relocate compinit while updating ~/.zshrc")?;
            let line_start = containing_line_range(&content, compinit_position).start;
            let configuration = format!("# Foundation CLI completions\n{fpath_line}\n");
            content.insert_str(line_start, &configuration);
            atomic_write(&zshrc, content.as_bytes())?;
            eprintln!("Added completion configuration to {}", zshrc.display());
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
    if uncommented_position(&content, fpath_line).is_none() {
        writeln!(file, "{}", fpath_line)?;
    }
    if uncommented_position(&content, compinit_line).is_none() {
        writeln!(file, "{}", compinit_line)?;
    }

    eprintln!("Added completion configuration to {}", zshrc.display());

    Ok(())
}

fn containing_line_range(content: &str, position: usize) -> std::ops::Range<usize> {
    let start = content[..position].rfind('\n').map_or(0, |position| position + 1);
    let end = content[position..].find('\n').map_or(content.len(), |offset| position + offset + 1);
    start..end
}

fn uncommented_position(content: &str, expected: &str) -> Option<usize> {
    content.match_indices(expected).find_map(|(position, _)| {
        let line_start = content[..position].rfind('\n').map_or(0, |position| position + 1);
        (!content[line_start..position].trim_start().starts_with('#')).then_some(position)
    })
}

fn standalone_line_range(content: &str, position: usize, expected: &str) -> Option<std::ops::Range<usize>> {
    let range = containing_line_range(content, position);
    (content[range.clone()].trim() == expected).then_some(range)
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    use std::io::Write;

    let path = fs::canonicalize(path).with_context(|| format!("Failed to resolve {}", path.display()))?;
    let parent = path.parent().context("Could not determine shell config directory")?;
    let permissions = fs::metadata(&path)?.permissions();
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("Failed to create temporary file in {}", parent.display()))?;
    temp.as_file().set_permissions(permissions)?;
    temp.write_all(content)?;
    temp.as_file().sync_all()?;
    temp.persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to update {}", path.display()))?;
    Ok(())
}
