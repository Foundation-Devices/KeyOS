// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! New project scaffolding command
//!
//! Creates a new KeyOS application project with the necessary structure.

mod template;

use std::collections::HashMap;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::Args;
use dialoguer::{Input, Select};
use foundation_core::{validate_display_app_name, AppId, SdkRoot};
use template::TemplateProcessor;

use crate::cargo_support::is_development_environment_active;

/// Template scaffolded when `--template` is omitted.
const DEFAULT_TEMPLATE: &str = "default-app";
/// The one built-in theme every app theme inherits from.
const BASE_THEME_ID: &str = "base_theme";
/// Template-owned app theme, which may contain app-specific overrides.
const APP_THEME_PATH: &str = "resources/theme.json";

#[derive(Args)]
pub struct NewArgs {
    /// Name of the application
    pub name: String,

    /// Project template to use
    #[arg(short, long, value_name = "TEMPLATE")]
    pub template: Option<String>,

    /// Friendly app name shown to users
    #[arg(long, value_name = "NAME")]
    pub friendly_name: Option<String>,

    /// Launcher app name
    #[arg(long, value_name = "NAME")]
    pub launcher_name: Option<String>,

    /// App description
    #[arg(long, value_name = "TEXT")]
    pub description: Option<String>,

    /// Publisher or company name
    #[arg(long, value_name = "NAME")]
    pub publisher_name: Option<String>,

    /// Contact email address
    #[arg(long, value_name = "EMAIL")]
    pub contact_email: Option<String>,

    /// Support website URL
    #[arg(long, value_name = "URL")]
    pub support_url: Option<String>,

    /// App ID; a random one is generated when omitted
    #[arg(long, value_name = "ID")]
    pub app_id: Option<String>,

    /// App version
    #[arg(long, value_name = "VERSION")]
    pub app_version: Option<String>,

    /// Minimum required KeyOS version
    #[arg(long, value_name = "VERSION")]
    pub min_keyos_version: Option<String>,

    /// Don't initialize a git repository
    #[arg(long)]
    pub no_git: bool,
}

use crate::sdk_mapping::{
    ensure_project_sdk_mapping, project_sdk_keyos_root_path, project_sdk_root_path, project_sdk_ui_root_path,
};

const DEFAULT_GIT_BRANCH: &str = "main";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitInitStatus {
    Initialized,
    Unavailable,
    Failed,
}

/// Execute the new command to create a new project
pub fn execute(args: &NewArgs) -> Result<()> {
    let sdk = SdkRoot::discover()
        .context("Could not locate the Foundation SDK root. Run this command from the SDK checkout or unpacked bundle.")?;
    let parent = std::env::current_dir().context("Could not determine the current directory")?;
    create_project(args, &sdk, &parent)
}

/// Scaffold a new project under `parent` using `sdk`. Prompt-backed fields fall
/// back to interactive input only when unsupplied and stdin is a terminal.
fn create_project(args: &NewArgs, sdk: &SdkRoot, parent: &Path) -> Result<()> {
    println!("Let's create a new KeyOS application!");
    println!();

    let template = select_template(args, sdk)?;

    let project_name = args.name.trim().to_string();
    validate_project_name(&project_name)?;

    let variables = collect_variables(args, sdk, &template, &project_name)?;

    println!();
    println!("Creating new KeyOS application: {}", project_name);

    let project_path = parent.join(&project_name);
    write_project_files(&project_path, &template, variables, sdk)?;

    println!("✓ Created {}/", project_name);
    println!("✓ Created project structure from template");
    println!("✓ Created app-config.toml");

    if !args.no_git {
        match initialize_git_repo(&project_path) {
            GitInitStatus::Initialized => println!("✓ Initialized Git repository"),
            GitInitStatus::Unavailable => {
                println!("Note: Git is not installed, so repository initialization was skipped")
            }
            GitInitStatus::Failed => {
                println!("Note: Git repository initialization failed, but project scaffolding completed")
            }
        }
    }

    println!("\n{}", next_steps(&project_name, is_development_environment_active()));

    Ok(())
}

fn next_steps(project_name: &str, in_nix_environment: bool) -> String {
    let enter_environment = if in_nix_environment { "" } else { "  foundation develop\n" };

    format!(
        "Next steps:\n{enter_environment}  cd {project_name}\n\nThen test in the simulator:\n  foundation sim\n\nOr test on connected hardware:\n  foundation sideload"
    )
}

/// Pick the template: the `--template` value, an interactive menu, or
/// `default-app` when there is no terminal.
fn select_template(args: &NewArgs, sdk: &SdkRoot) -> Result<String> {
    if let Some(template) = args.template.as_deref() {
        return Ok(template.to_string());
    }

    let available = template::list_available_templates(Some(sdk));
    if available.is_empty() {
        eprintln!("Error: No templates found");
        eprintln!();
        eprintln!("Templates should be installed to:");
        if let Some(home) = dirs::home_dir() {
            eprintln!("  - {}", home.join(".foundation").join("templates").display());
        }
        eprintln!();
        eprintln!("For development, templates can also be in:");
        eprintln!("  - ./templates (current directory)");
        eprintln!();
        eprintln!("Copy the templates from the foundation-cli source to one of these locations.");
        anyhow::bail!("No templates available");
    }

    let default_index = available.iter().position(|(name, _)| name == DEFAULT_TEMPLATE).unwrap_or(0);

    if interactive() {
        let items: Vec<String> =
            available.iter().map(|(name, desc)| format!("{} - {}", name, desc)).collect();
        let selection =
            Select::new().with_prompt("Select a template").items(&items).default(default_index).interact()?;
        Ok(available[selection].0.clone())
    } else {
        Ok(available[default_index].0.clone())
    }
}

/// Reject a project name that cannot double as a directory and a Cargo package
/// name.
fn validate_project_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("Error: Project name cannot be empty");
    }

    // Reject anything that could escape the working directory or break Cargo's identifier rules.
    if name.chars().any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) || name.contains("..") {
        anyhow::bail!(
            "Error: Project name '{}' contains invalid characters; use only letters, digits, hyphens, and underscores",
            name
        );
    }

    // Cargo package names must begin with a letter or underscore, or `cargo build`
    // fails on the scaffolded project.
    if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        anyhow::bail!("Error: Project name '{}' must begin with a letter or underscore", name);
    }

    Ok(())
}

/// Gather every template variable, prompting for the app fields that were not
/// passed as flags. Per-template defaults come from the template's [variables]
/// in template.toml.
fn collect_variables(
    args: &NewArgs,
    sdk: &SdkRoot,
    template: &str,
    project_name: &str,
) -> Result<HashMap<String, String>> {
    let template_vars = template::read_template_variables(template, Some(sdk));
    let template_default = |key: &str, fallback: &str| -> String {
        template_vars
            .get(key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| fallback.to_string())
    };

    // Fields without a prompt; the prompted ones follow in question order.
    let mut variables = HashMap::new();
    variables.insert("app_name".to_string(), project_name.to_string());
    variables.insert("icon".to_string(), template_default("icon", "resources/icon.svg"));
    // Keep the old template variable for external templates, but make its
    // value deterministic now that Base Theme is the only built-in parent.
    variables.insert("selected_theme_id".to_string(), BASE_THEME_ID.to_string());

    prompt_field(
        &mut variables,
        "friendly_app_name",
        "Enter the app's friendly name",
        args.friendly_name.as_deref(),
        &template_default("friendly_app_name", &project_name.replace('_', " ")),
        |value| {
            validate_display_app_name("friendly-app-name", value)?;
            Ok(())
        },
    )?;

    // launcher's default derives from the just-resolved friendly name.
    let launcher_default = template_default("launcher_app_name", &variables["friendly_app_name"]);
    prompt_field(
        &mut variables,
        "launcher_app_name",
        "Enter the app's launcher name",
        args.launcher_name.as_deref(),
        &launcher_default,
        |value| {
            validate_display_app_name("launcher-app-name", value)?;
            Ok(())
        },
    )?;

    prompt_field(
        &mut variables,
        "description",
        "Enter the app description",
        args.description.as_deref(),
        &template_default("description", "A new KeyOS application"),
        non_empty,
    )?;
    prompt_field(
        &mut variables,
        "publisher_name",
        "Publisher name",
        args.publisher_name.as_deref(),
        "",
        no_validation,
    )?;
    prompt_field(
        &mut variables,
        "contact_email",
        "Contact email",
        args.contact_email.as_deref(),
        "",
        no_validation,
    )?;
    prompt_field(
        &mut variables,
        "support_url",
        "Support website URL",
        args.support_url.as_deref(),
        "",
        no_validation,
    )?;

    prompt_field(
        &mut variables,
        "app_id",
        "Enter app ID (or press ENTER to generate a random ID)",
        args.app_id.as_deref(),
        &generate_random_app_id(),
        valid_app_id,
    )?;

    prompt_field(
        &mut variables,
        "version",
        "Enter the app version",
        args.app_version.as_deref(),
        &template_default("version", "0.1.0"),
        |value| valid_version("version", value),
    )?;
    prompt_field(
        &mut variables,
        "min_keyos_version",
        "Enter the minimum KeyOS version",
        args.min_keyos_version.as_deref(),
        &template_default("min_keyos_version", "1.0.0"),
        |value| valid_version("min-keyos-version", value),
    )?;

    Ok(variables)
}

/// Create the project directory and populate it from the template. `variables`
/// must already hold `friendly_app_name`, used as the app theme's display name.
fn write_project_files(
    project_path: &Path,
    template: &str,
    mut variables: HashMap<String, String>,
    sdk: &SdkRoot,
) -> Result<()> {
    if project_path.exists() {
        anyhow::bail!("Error: Directory '{}' already exists", project_path.display());
    }
    fs::create_dir_all(project_path)
        .with_context(|| format!("Error: Failed to create directory '{}'", project_path.display()))?;

    // `sdk_keyos_root` is the preferred variable for bundled templates. The
    // other SDK path variables remain part of the template API for external
    // SDK/user templates that already consume them.
    variables.insert("sdk_root".to_string(), project_sdk_root_path().to_string());
    variables.insert("sdk_keyos_root".to_string(), project_sdk_keyos_root_path().to_string());
    variables.insert("sdk_ui_root".to_string(), project_sdk_ui_root_path().to_string());
    variables.insert("sdk_path".to_string(), project_sdk_keyos_root_path().to_string());

    let template_path = template::get_template_path(template, Some(sdk));
    let template_files_path = template_path.join("files");
    if !template_files_path.exists() {
        anyhow::bail!("Error: Template '{}' not found at {}", template, template_path.display());
    }

    // friendly_app_name is read back before the processor consumes the map; it
    // feeds the app-theme scaffolding.
    let friendly_app_name = variables["friendly_app_name"].clone();

    let processor = TemplateProcessor::new(variables);
    processor
        .process_directory(&template_files_path, project_path)
        .with_context(|| format!("Failed to apply template '{}'", template))?;
    ensure_project_sdk_mapping(project_path, sdk)?;
    // Normalize the template-owned app theme in place. Reading the generated
    // file back preserves any sparse token or component overrides provided by
    // the template; templates without one get an empty child of Base Theme.
    crate::commands::themes::write_editable_app_theme(
        APP_THEME_PATH,
        sdk,
        project_path,
        &project_path.join(APP_THEME_PATH),
        &friendly_app_name,
        Some(BASE_THEME_ID),
    )?;

    Ok(())
}

/// Prompt for a field and record it in `variables[key]`. A supplied value, or
/// the `default` used when there is no terminal, skips the prompt and is
/// validated once; the interactive prompt (pre-filled with `default`) re-asks
/// until `validate` accepts. `validate` also decides whether empty is allowed.
fn prompt_field(
    variables: &mut HashMap<String, String>,
    key: &str,
    prompt: &str,
    provided: Option<&str>,
    default: &str,
    validate: impl Fn(&str) -> Result<()>,
) -> Result<()> {
    let value = match provided {
        Some(value) => {
            let value = value.trim().to_string();
            validate(&value)?;
            value
        }
        None if !interactive() => {
            let value = default.trim().to_string();
            validate(&value)?;
            value
        }
        None => {
            let mut initial = default.to_string();
            loop {
                let value = Input::<String>::new()
                    .with_prompt(prompt)
                    .with_initial_text(&initial)
                    .allow_empty(true)
                    .interact_text()?
                    .trim()
                    .to_string();
                match validate(&value) {
                    Ok(()) => break value,
                    Err(error) => {
                        eprintln!("{error}");
                        initial = value;
                    }
                }
            }
        }
    };
    variables.insert(key.to_string(), value);
    Ok(())
}

/// True when stdin is a terminal we can prompt on.
fn interactive() -> bool { std::io::stdin().is_terminal() }

fn no_validation(_: &str) -> Result<()> { Ok(()) }

fn non_empty(value: &str) -> Result<()> {
    if value.is_empty() {
        anyhow::bail!("must not be empty");
    }
    Ok(())
}

// AppConfig types app-id and the versions, so a value that doesn't parse
// scaffolds a project that no later command can load.
fn valid_app_id(value: &str) -> Result<()> {
    AppId::from_hex(value)?;
    Ok(())
}

fn valid_version(field: &str, value: &str) -> Result<()> {
    semver::Version::parse(value).map_err(|error| anyhow::anyhow!("{field}: {error}"))?;
    Ok(())
}

fn initialize_git_repo(project_path: &PathBuf) -> GitInitStatus {
    if !is_git_available() {
        return GitInitStatus::Unavailable;
    }

    let status = Command::new("git")
        .args(["init", "--initial-branch", DEFAULT_GIT_BRANCH])
        .current_dir(project_path)
        .status();

    match status {
        Ok(status) if status.success() => GitInitStatus::Initialized,
        Ok(_) | Err(_) => GitInitStatus::Failed,
    }
}

fn is_git_available() -> bool {
    Command::new("git").arg("--version").output().map(|output| output.status.success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use foundation_core::{AppConfig, SdkRoot};

    use super::{
        create_project, initialize_git_repo, is_git_available, next_steps, GitInitStatus, NewArgs,
        APP_THEME_PATH, DEFAULT_GIT_BRANCH,
    };
    use crate::sdk_mapping::{project_sdk_keyos_root_path, project_sdk_root_path, project_sdk_ui_root_path};
    use crate::slint_codegen::{prepare_project_for_build, project_sdk_ui_root, UI_LIBRARY_PATH_ENV};
    use crate::test_support::make_temp_dir;

    #[test]
    fn create_project_scaffolds_default_app() {
        let (_sdk_dir, sdk_root) = make_sdk_root("template-sdk");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let parent_dir = make_temp_dir("scaffold-project");
        let parent = parent_dir.path();

        create_project(&sample_args("demo-app"), &sdk, parent).unwrap();

        let project_path = parent.join("demo-app");
        let cargo_toml = fs::read_to_string(project_path.join("Cargo.toml")).unwrap();
        assert!(cargo_toml.contains(project_sdk_keyos_root_path()));
        assert!(!cargo_toml.contains(&sdk.keyos_root().display().to_string()));
        assert!(!cargo_toml.contains("{{sdk_keyos_root}}"));
        assert!(cargo_toml.contains("[workspace]"));
        assert!(cargo_toml.contains(r#"exclude = [".foundation-sdk"]"#));
        assert!(project_path.join(project_sdk_keyos_root_path()).exists());
        let theme_rs = fs::read_to_string(project_path.join("src").join("theme.rs")).unwrap();
        assert!(theme_rs.contains("slint_keyos_platform::settings::use_api!("));
        assert!(theme_rs.contains("slint_keyos_platform::settings,"));
        assert!(theme_rs.contains("slint_keyos_platform::server"));
        assert!(theme_rs.contains("settings_permissions::settings::messages::SubscribeSystemTheme"));
        assert!(theme_rs.contains("settings_permissions::settings::global::SystemTheme"));
        // The configured app theme is generated as app_theme and applied via apply_theme!.
        assert!(theme_rs.contains("foundation_themes::include_theme!(app_theme);"));
        assert!(theme_rs.contains("foundation_themes::apply_theme!(ui, app_theme::theme(), scheme);"));
        assert!(!theme_rs.contains("{{selected_theme_id}}"));
        let app_slint = fs::read_to_string(project_path.join("ui").join("app.slint")).unwrap();
        assert!(app_slint.contains("size: ControlSize.lg;"));
        assert!(!app_slint.contains("PrimaryAction"));
        assert!(!app_slint.contains("ButtonVariant"));
        assert!(!app_slint.contains("background: Theme.palette-primary;"));
        assert!(!app_slint.contains(r#"font-family: "Montserrat";"#));
        assert!(!app_slint.contains("font-weight: 600;"));
        assert!(!app_slint.contains("spacing: 20px;"));
        assert!(!app_slint.contains("border-radius: 20px;"));
        assert!(app_slint.contains("font-family: Theme.font-primary;"));
        assert!(app_slint.contains("spacing: Theme.spacing-xl;"));
        assert!(app_slint.contains("border-radius: Theme.radius-default;"));
        assert!(app_slint.contains("preferred-width: UISize.screen-width;"));
        assert!(app_slint.contains("preferred-height: UISize.screen-height;"));
        let app_theme_path = project_path.join("resources").join("theme.json");
        assert!(app_theme_path.exists());
        let app_theme: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(app_theme_path).unwrap()).unwrap();
        assert_eq!(app_theme["parent"].as_str(), Some("base_theme"));
        assert!(app_theme.get("tokens").is_none(), "base theme must remain inherited, not flattened");
        assert!(!project_path.join("theme").join("theme.json").exists());
        let cargo_lock = fs::read_to_string(project_path.join("Cargo.lock")).unwrap();
        assert!(cargo_lock.contains("name = \"demo-app\""));
        assert!(cargo_lock.contains("version = \"0.1.0\""));
        let agents = fs::read_to_string(project_path.join("AGENTS.md")).unwrap();
        assert!(agents.contains("# Demo App Agent Guide"));
        assert!(agents.contains("sdk/docs/foundation-cli.md"));
        assert!(!agents.contains("{{friendly_app_name}}"));
        assert!(project_path.join("permission_templates.toml").exists());
        assert!(project_path.join("resources").join("icon.svg").exists());
        assert!(project_path.join("resources").join("icon-dark.svg").exists());
        assert!(!project_path.join("i18n").exists());
        AppConfig::load(&project_path.join("app-config.toml")).unwrap().validate_icon(&project_path).unwrap();

        // Supplied args flow all the way into the generated config.
        let config = fs::read_to_string(project_path.join("app-config.toml")).unwrap();
        let cargo_toml = fs::read_to_string(project_path.join("Cargo.toml")).unwrap();
        assert!(config.contains(r#"app-name = "demo-app""#));
        assert!(config.contains(r#"friendly-app-name = "Demo App""#));
        assert!(config.contains(r#"description = "Demo app""#));
        assert!(!config.lines().any(|line| line.trim_start().starts_with("version =")));
        assert!(cargo_toml.contains(r#"version = "0.1.0""#));
        assert!(config.contains("0x00112233445566778899aabbccddeeff"));
    }

    #[test]
    fn other_builtin_templates_use_theme_tokens() {
        let (_sdk_dir, sdk_root) = make_sdk_root("other-template-sdk");
        let sdk = SdkRoot::from_root(sdk_root).unwrap();

        let multi_parent = make_temp_dir("multi-page-token-scaffold");
        let mut multi_args = sample_args("multi-demo");
        multi_args.template = Some("multi-page-app".to_string());
        create_project(&multi_args, &sdk, multi_parent.path()).unwrap();
        let multi_path = multi_parent.path().join("multi-demo");
        let multi_app = fs::read_to_string(multi_path.join("ui").join("app.slint")).unwrap();
        let main_page = fs::read_to_string(multi_path.join("ui").join("pages").join("page.slint")).unwrap();
        let second_page =
            fs::read_to_string(multi_path.join("ui").join("pages").join("second").join("page.slint"))
                .unwrap();
        assert!(multi_app.contains("preferred-width: UISize.screen-width;"));
        assert!(multi_app.contains("preferred-height: UISize.screen-height;"));
        assert!(main_page.contains("padding-left: Theme.spacing-xl;"));
        assert!(main_page.contains("font-family: Theme.font-primary;"));
        assert!(main_page.contains("border-radius: Theme.radius-default;"));
        assert!(second_page.contains("font-size: Theme.font-size-title;"));
        assert!(!main_page.contains("PageAction"));
        assert!(!second_page.contains("PageAction"));
        assert!(!main_page.contains("\"Montserrat\""));
        assert!(!second_page.contains("\"Montserrat\""));
        assert!(multi_path.join("resources").join("icon-dark.svg").exists());
        assert!(!multi_path.join("i18n").exists());
        AppConfig::load(&multi_path.join("app-config.toml")).unwrap().validate_icon(&multi_path).unwrap();

        let kitchen_parent = make_temp_dir("kitchen-sink-token-scaffold");
        let mut kitchen_args = sample_args("kitchen-demo");
        kitchen_args.template = Some("kitchen-sink".to_string());
        create_project(&kitchen_args, &sdk, kitchen_parent.path()).unwrap();
        let kitchen_app =
            fs::read_to_string(kitchen_parent.path().join("kitchen-demo").join("ui").join("app.slint"))
                .unwrap();
        assert!(kitchen_app.contains("preferred-width: UISize.screen-width;"));
        assert!(kitchen_app.contains("padding-left: Theme.spacing-xl;"));
        assert!(kitchen_app.contains(r#"Images.icon("settings", Theme.icon-size-md)"#));
        assert!(kitchen_app.contains("border-width: Theme.border-width-sm;"));
        assert!(kitchen_app.contains("border-radius: Theme.radius-sm;"));
        assert!(!kitchen_app.contains("#ffffff"));
        assert!(!kitchen_app.contains("spacing: 20px;"));
        let kitchen_path = kitchen_parent.path().join("kitchen-demo");
        assert!(kitchen_path.join("resources").join("icon-dark.svg").exists());
        AppConfig::load(&kitchen_path.join("app-config.toml")).unwrap().validate_icon(&kitchen_path).unwrap();
    }

    #[test]
    fn next_steps_enters_development_environment_when_needed() {
        assert_eq!(
            next_steps("demo-app", false),
            "Next steps:\n  foundation develop\n  cd demo-app\n\nThen test in the simulator:\n  foundation sim\n\nOr test on connected hardware:\n  foundation sideload"
        );
    }

    #[test]
    fn next_steps_skips_development_environment_when_already_active() {
        assert_eq!(
            next_steps("demo-app", true),
            "Next steps:\n  cd demo-app\n\nThen test in the simulator:\n  foundation sim\n\nOr test on connected hardware:\n  foundation sideload"
        );
    }

    #[test]
    fn create_project_rejects_invalid_app_ids_and_versions_from_flags() {
        let (_sdk_dir, sdk_root) = make_sdk_root("reject-flags-sdk");
        let sdk = SdkRoot::from_root(sdk_root).unwrap();

        let cases = [
            ("empty-app-id", NewArgs { app_id: Some(String::new()), ..sample_args("demo-app") }),
            ("non-hex-app-id", NewArgs { app_id: Some("hello".to_string()), ..sample_args("demo-app") }),
            ("short-app-id", NewArgs { app_id: Some("0xdeadbeef".to_string()), ..sample_args("demo-app") }),
            (
                "bad-version",
                NewArgs { app_version: Some("not-a-version".to_string()), ..sample_args("demo-app") },
            ),
            (
                "bad-min-keyos-version",
                NewArgs { min_keyos_version: Some("banana".to_string()), ..sample_args("demo-app") },
            ),
        ];

        for (label, args) in cases {
            let parent_dir = make_temp_dir(label);
            let parent = parent_dir.path();

            assert!(create_project(&args, &sdk, parent).is_err(), "{label} was accepted");
            // Validation runs before scaffolding, so a rejected run leaves nothing behind.
            assert!(!parent.join("demo-app").exists(), "{label} left a project directory");
        }
    }

    #[test]
    fn create_project_keeps_sdk_path_variables_for_external_templates() {
        let (_sdk_dir, sdk_root) = make_sdk_root("compat-template-sdk");
        let template_dir =
            sdk_root.join("crates").join("cli").join("templates").join("compat-vars").join("files");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(
            template_dir.join("paths.txt"),
            "root={{sdk_root}}\nkeyos={{sdk_keyos_root}}\nui={{sdk_ui_root}}\npath={{sdk_path}}\ntheme={{selected_theme_id}}\n",
        )
        .unwrap();

        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let project_root_dir = make_temp_dir("compat-scaffold-project");
        let project_root = project_root_dir.path();

        let mut args = sample_args("demo-app");
        args.template = Some("compat-vars".to_string());
        create_project(&args, &sdk, project_root).unwrap();

        let project_path = project_root.join("demo-app");
        let paths = fs::read_to_string(project_path.join("paths.txt")).unwrap();
        assert_eq!(
            paths,
            format!(
                "root={}\nkeyos={}\nui={}\npath={}\ntheme=base_theme\n",
                project_sdk_root_path(),
                project_sdk_keyos_root_path(),
                project_sdk_ui_root_path(),
                project_sdk_keyos_root_path()
            )
        );
        assert!(project_path.join(project_sdk_keyos_root_path()).exists());
    }

    #[test]
    fn create_project_locks_base_theme_and_preserves_template_overrides() {
        let (_sdk_dir, sdk_root) = make_sdk_root("theme-template-sdk");
        let template_dir = sdk_root
            .join("crates")
            .join("cli")
            .join("templates")
            .join("theme-overrides")
            .join("files")
            .join("resources");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(
            template_dir.join("theme.json"),
            r#"{
  "id": "template_theme",
  "name": "Template Theme",
  "parent": "obsolete_theme",
  "tokens": {
    "spacing": {
      "md": 18.0
    }
  },
  "components": {
    "button": {
      "variantProps": {
        "primary": {
          "normal": {
            "background": "color.danger"
          }
        }
      }
    }
  }
}
"#,
        )
        .unwrap();

        let sdk = SdkRoot::from_root(sdk_root).unwrap();
        let project_root_dir = make_temp_dir("theme-template-project");
        let project_root = project_root_dir.path();
        let mut args = sample_args("demo-app");
        args.template = Some("theme-overrides".to_string());

        create_project(&args, &sdk, project_root).unwrap();

        let app_theme: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(project_root.join("demo-app").join(APP_THEME_PATH)).unwrap(),
        )
        .unwrap();
        assert_eq!(app_theme["id"], "app_theme");
        assert_eq!(app_theme["name"], "Demo App");
        assert_eq!(app_theme["parent"], "base_theme");
        assert_eq!(app_theme["tokens"]["spacing"]["md"], 18.0);
        assert_eq!(
            app_theme["components"]["button"]["variantProps"]["primary"]["normal"]["background"],
            "color.danger"
        );
    }

    // Heavy integration test: scaffolds an app and `cargo check`s it against the
    // real SDK. Requires (1) generated base themes in ~/.foundation/themes/rust
    // (`foundation themes build` or the helper below provides these) and (2) a
    // nightly cargo (the SDK workspace uses the `trim-paths` feature). Ignored by
    // default because a nested `cargo test` under nix can't reliably self-provision
    // either precondition; run explicitly with `--ignored` in a provisioned env.
    #[test]
    #[ignore = "requires ~/.foundation/themes/rust populated and nightly cargo; run with --ignored"]
    fn scaffolded_default_app_compiles_with_generated_theme_module() {
        let sdk = SdkRoot::discover_from(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        let project_root_dir = make_temp_dir("scaffold-compile");
        let project_root = project_root_dir.path();
        let home = project_root.join("home");

        create_project(&sample_args("demo-app"), &sdk, project_root).unwrap();
        let project_path = project_root.join("demo-app");
        prepare_project_for_build(&project_path, &sdk, &semver::Version::new(0, 1, 0)).unwrap();

        // The scaffolded app's build.rs resolves FOUNDATION_THEMES_RUST_DIR,
        // falling back to <home>/.foundation/themes/rust, which is exactly where
        // `foundation themes build` writes in real use. Generate the base
        // themes there so the compile check mirrors a real environment.
        generate_base_themes_into_home(&sdk, &home);

        let output = Command::new("cargo")
            .arg("check")
            .current_dir(&project_path)
            .env("HOME", &home)
            .env(
                "FOUNDATION_THEMES_RUST_DIR",
                project_path.join("target").join("foundation").join("themes").join("rust"),
            )
            .env(UI_LIBRARY_PATH_ENV, project_sdk_ui_root(&project_path))
            .output()
            .unwrap();
        if !output.status.success() {
            panic!(
                "generated app failed to compile\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    /// Generate the SDK's base theme JSON into `<home>/.foundation/themes/rust`,
    /// the default location the template build.rs falls back to. Used by the
    /// scaffold compile tests (which build through a nix shell that scrubs the
    /// FOUNDATION_THEMES_RUST_DIR override, leaving only the fallback path).
    fn generate_base_themes_into_home(sdk: &SdkRoot, home: &Path) {
        let rust_dir = home.join(".foundation").join("themes").join("rust");
        let json_dir = sdk.keyos_root().join("sdk").join("crates").join("foundation-themes").join("themes");
        let manifest =
            sdk.keyos_root().join("sdk").join("crates").join("foundation-themes").join("Cargo.toml");
        let gen = Command::new("cargo")
            .arg("run")
            .arg("--quiet")
            .arg("--manifest-path")
            .arg(&manifest)
            .arg("--bin")
            .arg("foundation-theme-compiler")
            .arg("--")
            .arg("--json-dir")
            .arg(&json_dir)
            .arg("--rust-dir")
            .arg(&rust_dir)
            // Pin HOME so a nix-wrapped `cargo run` doesn't write to a build
            // sandbox home. The scaffolded app's build.rs falls back to
            // $HOME/.foundation/themes/rust, so both must agree.
            .env("HOME", home)
            .output()
            .unwrap();
        assert!(gen.status.success(), "theme compiler failed:\n{}", String::from_utf8_lossy(&gen.stderr));
        assert!(
            rust_dir.join("base_theme.rs").exists(),
            "theme compiler did not write to {}",
            rust_dir.display()
        );
    }

    // See scaffolded_default_app_compiles_with_generated_theme_module for why
    // this heavy integration test is ignored by default.
    #[test]
    #[ignore = "requires ~/.foundation/themes/rust populated and nightly cargo; run with --ignored"]
    fn scaffolded_multi_page_app_compiles_with_light_template_theme() {
        let sdk = SdkRoot::discover_from(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        let project_root_dir = make_temp_dir("scaffold-multi-page-compile");
        let project_root = project_root_dir.path();
        let home = project_root.join("home");

        let mut args = sample_args("demo-app");
        args.template = Some("multi-page-app".to_string());
        create_project(&args, &sdk, project_root).unwrap();
        let project_path = project_root.join("demo-app");
        prepare_project_for_build(&project_path, &sdk, &semver::Version::new(0, 1, 0)).unwrap();

        let theme_rs = fs::read_to_string(project_path.join("src").join("theme.rs")).unwrap();
        assert!(theme_rs.contains("foundation_themes::include_theme!(app_theme);"));
        assert!(theme_rs.contains("foundation_themes::apply_theme!(ui, app_theme::theme(), scheme);"));
        assert!(!project_path.join("theme").join("theme.json").exists());

        generate_base_themes_into_home(&sdk, &home);

        let output = Command::new("cargo")
            .arg("check")
            .current_dir(&project_path)
            .env("HOME", &home)
            .env(
                "FOUNDATION_THEMES_RUST_DIR",
                project_path.join("target").join("foundation").join("themes").join("rust"),
            )
            .env(UI_LIBRARY_PATH_ENV, project_sdk_ui_root(&project_path))
            .output()
            .unwrap();
        if !output.status.success() {
            panic!(
                "generated multi-page app failed to compile\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    // See scaffolded_default_app_compiles_with_generated_theme_module for why
    // this heavy integration test is ignored by default.
    #[test]
    #[ignore = "requires ~/.foundation/themes/rust populated and nightly cargo; run with --ignored"]
    fn scaffolded_kitchen_sink_compiles_with_generated_theme_module() {
        let sdk = SdkRoot::discover_from(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        let project_root_dir = make_temp_dir("scaffold-kitchen-sink-compile");
        let project_root = project_root_dir.path();
        let home = project_root.join("home");

        let mut args = sample_args("demo-app");
        args.template = Some("kitchen-sink".to_string());
        create_project(&args, &sdk, project_root).unwrap();
        let project_path = project_root.join("demo-app");
        prepare_project_for_build(&project_path, &sdk, &semver::Version::new(0, 1, 0)).unwrap();

        let theme_rs = fs::read_to_string(project_path.join("src").join("theme.rs")).unwrap();
        assert!(theme_rs.contains("foundation_themes::include_theme!(app_theme);"));
        assert!(theme_rs.contains("foundation_themes::apply_theme!(ui, app_theme::theme(), scheme);"));

        generate_base_themes_into_home(&sdk, &home);

        let output = Command::new("cargo")
            .arg("check")
            .current_dir(&project_path)
            .env("HOME", &home)
            .env(
                "FOUNDATION_THEMES_RUST_DIR",
                project_path.join("target").join("foundation").join("themes").join("rust"),
            )
            .env(UI_LIBRARY_PATH_ENV, project_sdk_ui_root(&project_path))
            .output()
            .unwrap();
        if !output.status.success() {
            panic!(
                "generated kitchen-sink app failed to compile\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn initialize_git_repo_runs_git_init_when_available() {
        if !is_git_available() {
            return;
        }

        let temp_root_dir = make_temp_dir("git-init");
        let temp_root = temp_root_dir.path();
        let project_root = temp_root.join("project");
        fs::create_dir_all(&project_root).unwrap();

        let status = initialize_git_repo(&project_root);

        assert_eq!(status, GitInitStatus::Initialized);
        assert!(project_root.join(".git").is_dir());
        assert_eq!(git_branch(&project_root), DEFAULT_GIT_BRANCH);
    }

    fn git_branch(project_root: &Path) -> String {
        let output = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(project_root)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn sample_args(name: &str) -> NewArgs {
        NewArgs {
            name: name.to_string(),
            template: Some("default-app".to_string()),
            friendly_name: Some("Demo App".to_string()),
            launcher_name: Some("Demo".to_string()),
            description: Some("Demo app".to_string()),
            publisher_name: Some("Demo Publisher".to_string()),
            contact_email: Some("support@example.com".to_string()),
            support_url: Some("https://example.com".to_string()),
            app_id: Some("0x00112233445566778899aabbccddeeff".to_string()),
            app_version: Some("0.1.0".to_string()),
            min_keyos_version: Some("1.0.0".to_string()),
            no_git: true,
        }
    }

    fn make_sdk_root(label: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = make_temp_dir(label);
        let repo_root = dir.path();
        fs::write(repo_root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let root = repo_root.join("sdk");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("flake.nix"), "{}").unwrap();
        fs::write(root.join("sdk-build.toml"), "").unwrap();
        fs::create_dir_all(root.join("crates").join("cli").join("templates")).unwrap();

        for template in ["default-app", "multi-page-app", "kitchen-sink"] {
            copy_dir(
                &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates").join(template),
                &root.join("crates").join("cli").join("templates").join(template),
            );
        }

        // create_project reads the built-in themes; seed the base one so
        // theme lookup by id resolves.
        let themes_dir = root.join("crates").join("foundation-themes").join("themes");
        fs::create_dir_all(&themes_dir).unwrap();
        fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("foundation-themes")
                .join("themes")
                .join("base_theme.json"),
            themes_dir.join("base_theme.json"),
        )
        .unwrap();

        (dir, root)
    }

    fn copy_dir(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            if source_path.is_dir() {
                copy_dir(&source_path, &destination_path);
            } else {
                fs::copy(&source_path, &destination_path).unwrap();
            }
        }
    }
}

/// Generate a 16-byte app ID as a `0x`-prefixed hex string. Derived from the
/// nanosecond timestamp, not cryptographically random.
fn generate_random_app_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();

    let mut bytes = [0u8; 16];
    let now_bytes = now.to_le_bytes();
    bytes[..8].copy_from_slice(&now_bytes[..8]);

    // Second half is a mix of the same timestamp, not fresh entropy.
    let now2 = now.wrapping_mul(0x123456789ABCDEF);
    let now2_bytes = now2.to_le_bytes();
    bytes[8..].copy_from_slice(&now2_bytes[..8]);

    format!("0x{}", bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>())
}
