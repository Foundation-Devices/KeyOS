// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! CLI command building and parsing

use clap::{Arg, ArgAction, Command};

pub const CMD_NEW: &str = "new";
pub const CMD_DEVELOP: &str = "develop";
pub const CMD_EXIT: &str = "exit";
pub const CMD_BUILD: &str = "build";
pub const CMD_CLEAN: &str = "clean";
pub const CMD_SIDELOAD: &str = "sideload";
pub const CMD_SIM: &str = "sim";
pub const CMD_CERT: &str = "cert";
pub const CMD_THEME: &str = "theme";
pub const CMD_THEMES: &str = "themes";
pub const CMD_DOCTOR: &str = "doctor";
pub const CMD_PREVIEW: &str = "preview";
pub const CMD_LOGS: &str = "logs";
pub const CMD_PLUGIN: &str = "plugin";
pub const CMD_COMPLETIONS: &str = "completions";
pub const CMD_CERT_GEN: &str = "gen";
pub const CMD_CERT_PRINT: &str = "print";
pub const CMD_THEMES_BUILD: &str = "build";
pub const CMD_THEMES_LIST: &str = "list";
pub const CMD_THEMES_NEW: &str = "new";
pub const CMD_PLUGIN_INSTALL: &str = "install";
pub const CMD_PLUGIN_UNINSTALL: &str = "uninstall";
pub const CMD_PLUGIN_SEARCH: &str = "search";

pub fn builtin_top_level_names() -> Vec<String> {
    vec![
        CMD_NEW.to_string(),
        CMD_DEVELOP.to_string(),
        CMD_EXIT.to_string(),
        CMD_BUILD.to_string(),
        CMD_CLEAN.to_string(),
        CMD_SIDELOAD.to_string(),
        CMD_SIM.to_string(),
        CMD_CERT.to_string(),
        CMD_THEME.to_string(),
        CMD_THEMES.to_string(),
        CMD_DOCTOR.to_string(),
        CMD_PREVIEW.to_string(),
        CMD_LOGS.to_string(),
        CMD_PLUGIN.to_string(),
        CMD_COMPLETIONS.to_string(),
    ]
}

/// Build the clap Command with localized descriptions
pub fn build_cli() -> Command {
    let mut cmd = Command::new("foundation")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Foundation CLI for KeyOS app development")
        .arg_required_else_help(true)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .disable_help_subcommand(true)
        .subcommand(build_new_command())
        .subcommand(build_develop_command())
        .subcommand(build_exit_command())
        .subcommand(build_build_command())
        .subcommand(build_clean_command())
        .subcommand(build_sideload_command())
        .subcommand(build_sim_command())
        .subcommand(build_cert_command())
        .subcommand(build_theme_command())
        .subcommand(build_themes_command())
        .subcommand(build_doctor_command())
        .subcommand(build_preview_command())
        .subcommand(build_logs_command())
        .subcommand(build_plugin_command())
        .subcommand(build_completions_command());

    let help_arg =
        Arg::new("help").long("help").short('h').help("Print help").action(ArgAction::Help).global(true);

    cmd = cmd.arg(help_arg);
    cmd
}

fn build_new_command() -> Command {
    Command::new(CMD_NEW)
        .about("Create a new KeyOS application project")
        .long_about("Scaffolds a new KeyOS application with the necessary structure and configuration files")
        .arg(Arg::new("name").help("Name of the application").required(false).index(1))
        .arg(
            Arg::new("template")
                .long("template")
                .short('t')
                .help("Project template to use")
                .value_name("TEMPLATE"),
        )
        .arg(
            Arg::new("no-git")
                .long("no-git")
                .help("Don't initialize a git repository")
                .action(ArgAction::SetTrue),
        )
}

fn build_develop_command() -> Command {
    Command::new(CMD_DEVELOP).about("Enter the KeyOS development environment").long_about(
        "Starts a Nix development shell with all required tools and dependencies for KeyOS app development",
    )
}

fn build_exit_command() -> Command {
    Command::new(CMD_EXIT)
        .about("Clean up the development environment")
        .long_about("Removes SDK-related Nix state, clears cache directories, and runs garbage collection to free up disk space")
}

fn build_build_command() -> Command {
    Command::new(CMD_BUILD)
        .about("Build the application")
        .long_about("Compiles the KeyOS application and prepares it for deployment")
        .arg(
            Arg::new("release")
                .long("release")
                .short('r')
                .help("Build in release mode with optimizations")
                .action(ArgAction::SetTrue),
        )
}

fn build_clean_command() -> Command {
    Command::new(CMD_CLEAN)
        .about("Remove generated build and theme files")
        .long_about("Removes the app's generated build and theme artifacts: the target/ directory (cargo output, the generated target/foundation UI/resources/theme files, and target/keyos hardware bundles), the generated manifest.toml, and the ui/ui SDK UI mapping. Authored source is left untouched")
}

fn build_cert_command() -> Command {
    Command::new(CMD_CERT)
        .about("Manage signing certificates")
        .long_about("Generate and inspect publisher certificates used for Foundation app signing")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(build_cert_gen_command())
        .subcommand(build_cert_print_command())
}

fn build_theme_command() -> Command {
    Command::new(CMD_THEME)
        .about("Open the app theme editor")
        .long_about("Opens the visual theme editor for the current app's configured theme JSON")
}

fn build_themes_command() -> Command {
    Command::new(CMD_THEMES)
        .about("Manage app themes")
        .long_about("Seed, generate, list, and scaffold KeyOS app themes in ~/.foundation/themes")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new(CMD_THEMES_BUILD)
                .about("Generate theme Rust from JSON")
                .long_about("Seed base themes and compile every theme JSON in ~/.foundation/themes/json into Rust under ~/.foundation/themes/rust"),
        )
        .subcommand(
            Command::new(CMD_THEMES_LIST)
                .about("List available themes")
                .long_about("List the theme JSON files in ~/.foundation/themes/json"),
        )
        .subcommand(
            Command::new(CMD_THEMES_NEW)
                .about("Create a new theme from a base")
                .long_about("Copy a base theme to a new editable JSON in ~/.foundation/themes/json and regenerate Rust")
                .arg(
                    Arg::new("name")
                        .help("Name of the new theme")
                        .required(true)
                        .index(1),
                )
                .arg(
                    Arg::new("from")
                        .long("from")
                        .help("Base theme to inherit from (default: default_theme)")
                        .value_name("BASE")
                        .default_value("default_theme"),
                ),
        )
}

fn build_cert_gen_command() -> Command {
    Command::new(CMD_CERT_GEN)
        .about("Generate signing keys and an X.509 publisher certificate")
        .long_about("Generates a secp256k1 keypair, a self-signed X.509 code-signing certificate, and cosign2 configuration for Foundation app signing")
        .arg(Arg::new("name").help("Name for the signing certificate and key files").required(false).index(1))
        .arg(
            Arg::new("publisher-name")
                .long("publisher-name")
                .help("Publisher or company name to embed in the certificate"),
        )
        .arg(
            Arg::new("contact-email")
                .long("contact-email")
                .help("Contact email address to embed in the certificate"),
        )
        .arg(
            Arg::new("support-url")
                .long("support-url")
                .help("Support website URL to embed in the certificate"),
        )
}

fn build_cert_print_command() -> Command {
    Command::new(CMD_CERT_PRINT)
        .about("Print an X.509 publisher certificate")
        .long_about("Uses OpenSSL to print the contents of a stored publisher certificate for inspection")
        .arg(
            Arg::new("name")
                .help(
                    "Publisher identity name to print (defaults to the current app publisher when available)",
                )
                .required(false)
                .index(1),
        )
}

fn build_sideload_command() -> Command {
    Command::new(CMD_SIDELOAD)
        .about("Build, sign, upload, and launch on hardware over usb-debug")
        .long_about("Builds and signs the application, uploads it to a connected Passport Prime over usb-debug, and launches it through passport-drive MCP by default")
        .arg(
            Arg::new("release")
                .long("release")
                .short('r')
                .help("Build in release mode with optimizations before sideloading")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("no-run")
                .long("no-run")
                .help("Upload the app bundle to the device but do not launch it afterward")
                .action(ArgAction::SetTrue),
        )
}

fn build_sim_command() -> Command {
    Command::new(CMD_SIM)
        .about("Build and run in the simulator")
        .long_about("Builds the KeyOS application and runs it in the KeyOS simulator")
}

fn build_doctor_command() -> Command {
    Command::new(CMD_DOCTOR)
        .about("Check development environment")
        .long_about("Verifies that all required tools and dependencies are properly installed and configured")
}

fn build_preview_command() -> Command {
    Command::new(CMD_PREVIEW)
        .about("Preview UI in foundation-slint-viewer")
        .long_about("Opens the application UI in foundation-slint-viewer for quick preview without a full hardware build")
        .arg(Arg::new("file").help("Slint file to preview (defaults to ui/app.slint)").required(false).index(1))
        .arg(
            Arg::new("include-path")
                .short('I')
                .help("Include path for other .slint files or images")
                .action(clap::ArgAction::Append),
        )
        .arg(Arg::new("style").long("style").help("The style name ('native' or 'fluent')"))
        .arg(Arg::new("component").long("component").help("The name of the component to view"))
        .arg(Arg::new("backend").long("backend").help("The rendering backend"))
        .arg(
            Arg::new("auto-reload")
                .long("auto-reload")
                .help("Automatically watch the file system, and reload when it changes")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(Arg::new("load-data").long("load-data").help("Load properties from a JSON file ('-' for stdin)"))
        .arg(Arg::new("save-data").long("save-data").help("Store property values in a JSON file at exit ('-' for stdout)"))
        .arg(
            Arg::new("on")
                .long("on")
                .help("Specify callback handler: <callback> <handler>")
                .num_args(2)
                .action(clap::ArgAction::Append),
        )
        .arg(Arg::new("i18n-dir").long("i18n-dir").help("Path to the i18n directory containing JSON translation files"))
        .arg(Arg::new("locale").long("locale").help("Set the locale for custom translations (e.g., 'en', 'es')"))
}

fn build_logs_command() -> Command {
    Command::new(CMD_LOGS)
        .about("Open the Passport USB log viewer")
        .long_about("Launches foundation-keyos-log-viewer and automatically connects it to the Passport currently attached over USB")
        .arg(
            Arg::new("timeout")
                .long("timeout")
                .short('t')
                .help("Reconnect timeout in seconds before retrying USB discovery (default: 3)")
                .value_name("SECONDS")
                .value_parser(clap::value_parser!(u64))
                .default_value("3"),
        )
}

fn build_plugin_command() -> Command {
    Command::new(CMD_PLUGIN)
        .about("Manage Foundation CLI plugins")
        .long_about("Search, install, and uninstall Foundation CLI plugins")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(build_plugin_install_command())
        .subcommand(build_plugin_uninstall_command())
        .subcommand(build_plugin_search_command())
}

fn build_plugin_install_command() -> Command {
    Command::new(CMD_PLUGIN_INSTALL)
        .about("Install a Foundation plugin")
        .long_about("Downloads and installs a Foundation CLI plugin from the registry or GitHub")
        .arg(
            Arg::new("plugin")
                .help("Plugin name or repository (e.g., 'foo' or 'user/repo')")
                .required(true)
                .index(1),
        )
}

fn build_plugin_uninstall_command() -> Command {
    Command::new(CMD_PLUGIN_UNINSTALL)
        .about("Uninstall a Foundation plugin")
        .long_about("Removes an installed Foundation CLI plugin")
        .arg(Arg::new("plugin").help("Plugin name to uninstall").required(true).index(1))
}

fn build_plugin_search_command() -> Command {
    Command::new(CMD_PLUGIN_SEARCH)
        .about("Search for Foundation plugins")
        .long_about("Searches the plugin registry for available plugins")
        .arg(Arg::new("query").help("Search query").required(true).index(1))
}

fn build_completions_command() -> Command {
    Command::new(CMD_COMPLETIONS)
        .about("Generate shell completions")
        .long_about("Generates shell completion scripts for bash, zsh, fish, or PowerShell")
        .arg(
            Arg::new("shell")
                .help("Shell type (bash, zsh, fish, powershell)")
                .required(true)
                .index(1)
                .value_parser(["bash", "zsh", "fish", "powershell"]),
        )
        .arg(
            Arg::new("install")
                .long("install")
                .help("Install completions to the standard location for the shell")
                .action(ArgAction::SetTrue),
        )
}

/// Print custom help message with localized command names.
pub fn print_help() {
    println!("Foundation CLI for KeyOS app development");
    println!();
    println!("{}: foundation <COMMAND>", "Usage");
    println!();
    println!("{}:", "Commands");
    println!("  {}        {}", CMD_NEW, "Create a new KeyOS application project");
    println!("  {}    {}", CMD_DEVELOP, "Enter the KeyOS development environment");
    println!("  {}       {}", CMD_EXIT, "Clean up the development environment");
    println!("  {}      {}", CMD_BUILD, "Build the application");
    println!("  {}      {}", CMD_CLEAN, "Remove generated build and theme files");
    println!("  {}   {}", CMD_SIDELOAD, "Build, sign, copy, and launch on hardware over USB");
    println!("  {}        {}", CMD_SIM, "Build and run in the simulator");
    println!("  {}   {}", CMD_CERT, "Manage signing certificates");
    println!("  {}      {}", CMD_THEME, "Open the app theme editor");
    println!("  {}     {}", CMD_THEMES, "Manage app themes");
    println!("  {}     {}", CMD_DOCTOR, "Check development environment");
    println!("  {}    {}", CMD_PREVIEW, "Preview UI in foundation-slint-viewer");
    println!("  {}       {}", CMD_LOGS, "Open the Passport USB log viewer");
    println!("  {}     {}", CMD_PLUGIN, "Manage Foundation CLI plugins");
    println!("  {} {}", CMD_COMPLETIONS, "Generate shell completions");
    println!();
    println!("{}:", "Options");
    println!("  -h, --help     {}", "Print help");
    println!("  -V, --version  {}", "Print version");
}
