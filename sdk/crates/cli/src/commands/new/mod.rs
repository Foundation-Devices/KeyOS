// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! New project scaffolding command
//!
//! Creates a new KeyOS application project with the necessary structure.

mod template;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use dialoguer::{Input, Select};
use foundation_core::SdkRoot;
use template::TemplateProcessor;

use crate::sdk_mapping::{
    ensure_project_sdk_mapping, project_sdk_keyos_root_path, project_sdk_root_path, project_sdk_ui_root_path,
};

/// Application configuration from user prompts
struct PromptConfig {
    name: String,
    friendly_app_name: String,
    launcher_app_name: String,
    description: String,
    publisher_name: String,
    contact_email: String,
    support_url: String,
    icon: String,
    app_id: String,
    version: String,
    min_keyos_version: String,
    selected_theme_id: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ThemeFileEntry {
    id: Option<String>,
    name: Option<String>,
    sort_order: Option<i64>,
}

#[derive(Debug, Clone)]
struct ThemeEntry {
    id: String,
    display_name: String,
    sort_order: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitInitStatus {
    Initialized,
    Unavailable,
    Failed,
}

/// Execute the new command to create a new project
pub fn execute(name: Option<&str>, template: Option<&str>, no_git: bool) -> Result<()> {
    println!("Let's create a new KeyOS application!");
    println!();

    let sdk = SdkRoot::discover()
        .context("Could not locate the Foundation SDK root. Run this command from the SDK checkout or unpacked bundle.")?;

    // Select template if not provided
    let selected_template = match template {
        Some(t) => t.to_string(),
        None => {
            let available_templates = template::list_available_templates(Some(&sdk));

            if available_templates.is_empty() {
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

            let items: Vec<String> =
                available_templates.iter().map(|(name, desc)| format!("{} - {}", name, desc)).collect();

            let selection =
                Select::new().with_prompt("Select a template").items(&items).default(0).interact()?;

            available_templates[selection].0.clone()
        }
    };

    let available_themes = load_available_themes(&sdk)?;
    let theme_items: Vec<&str> = available_themes.iter().map(|theme| theme.display_name.as_str()).collect();
    let selected_theme_index =
        Select::new().with_prompt("Select a starting theme:").items(&theme_items).default(0).interact()?;
    let selected_theme =
        available_themes.get(selected_theme_index).cloned().context("selected theme index out of range")?;

    // Get the project name - prompt if not provided
    let project_name = match name {
        Some(n) => n.trim().to_string(),
        None => Input::<String>::new().with_prompt("Enter the app name").interact_text()?.trim().to_string(),
    };

    if project_name.is_empty() {
        anyhow::bail!("Error: Project name cannot be empty");
    }

    // Project name becomes a directory and a Cargo package name; reject anything
    // that could escape the working directory or break Cargo's identifier rules.
    if project_name.chars().any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        || project_name.contains("..")
    {
        anyhow::bail!(
            "Error: Project name '{}' contains invalid characters; use only letters, digits, hyphens, and underscores",
            project_name
        );
    }

    // Cargo package names must begin with a letter or underscore (`cargo build`
    // would otherwise fail on the scaffolded project), so reject a leading digit,
    // hyphen, or any other non-identifier-start character at `foundation new` time.
    if !project_name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        anyhow::bail!("Error: Project name '{}' must begin with a letter or underscore", project_name);
    }

    // Prompt for all app configuration fields. Per-template defaults come from
    // the selected template's [variables] in template.toml — no template-specific
    // text baked into this command.
    let template_vars = template::read_template_variables(&selected_template, Some(&sdk));
    let template_default = |key: &str, fallback: &str| -> String {
        template_vars
            .get(key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| fallback.to_string())
    };

    let friendly_default = template_default("friendly_app_name", &project_name);
    let friendly_app_name = Input::<String>::new()
        .with_prompt("Enter the app's friendly name")
        .with_initial_text(&friendly_default)
        .interact_text()?
        .trim()
        .to_string();

    let launcher_default = template_default("launcher_app_name", &friendly_app_name);
    let launcher_app_name = Input::<String>::new()
        .with_prompt("Enter the app's launcher name")
        .with_initial_text(&launcher_default)
        .interact_text()?
        .trim()
        .to_string();

    let description_default = template_default("description", "A new KeyOS application");
    let description = Input::<String>::new()
        .with_prompt("Enter the app description")
        .with_initial_text(&description_default)
        .interact_text()?
        .trim()
        .to_string();

    let publisher_name = Input::<String>::new()
        .with_prompt("Publisher name")
        .allow_empty(true)
        .interact_text()?
        .trim()
        .to_string();

    let contact_email = Input::<String>::new()
        .with_prompt("Contact email")
        .allow_empty(true)
        .interact_text()?
        .trim()
        .to_string();

    let support_url = Input::<String>::new()
        .with_prompt("Support website URL")
        .allow_empty(true)
        .interact_text()?
        .trim()
        .to_string();

    let icon_default = template_default("icon", "resources/icon.svg");
    let icon = Input::<String>::new()
        .with_prompt("Enter the icon path")
        .with_initial_text(&icon_default)
        .interact_text()?
        .trim()
        .to_string();

    let app_id = Input::<String>::new()
        .with_prompt("Enter app ID (or press ENTER to generate a random ID)")
        .allow_empty(true)
        .interact_text()?
        .trim()
        .to_string();

    let app_id = if app_id.is_empty() { generate_random_app_id() } else { app_id };

    let version_default = template_default("version", "0.1.0");
    let version = Input::<String>::new()
        .with_prompt("Enter the app version")
        .with_initial_text(&version_default)
        .interact_text()?
        .trim()
        .to_string();

    let min_keyos_version = Input::<String>::new()
        .with_prompt("Enter the minimum KeyOS version")
        .with_initial_text("1.0.0")
        .interact_text()?
        .trim()
        .to_string();

    let config = PromptConfig {
        name: project_name.clone(),
        friendly_app_name,
        launcher_app_name,
        description,
        publisher_name,
        contact_email,
        support_url,
        icon,
        app_id,
        version,
        min_keyos_version,
        selected_theme_id: selected_theme.id,
    };

    println!();
    println!("Creating new KeyOS application: {}", project_name);

    // Create project directory
    let project_path = PathBuf::from(&project_name);

    if project_path.exists() {
        anyhow::bail!("Error: Directory '{}' already exists", project_name);
    }

    fs::create_dir_all(&project_path)
        .with_context(|| format!("Error: Failed to create directory '{}'", project_name))?;

    // Process template to create project structure
    apply_template(&project_path, &selected_template, &config, &sdk)?;

    println!("✓ Created {}/", project_name);
    println!("✓ Created project structure from template");
    println!("✓ Created app-config.toml");

    if !no_git {
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

    println!("\nNext steps:");
    println!("  cd {}", project_name);
    println!("  foundation build");

    Ok(())
}

/// Apply a template to create the project structure
fn apply_template(
    project_path: &PathBuf,
    template_name: &str,
    config: &PromptConfig,
    sdk: &SdkRoot,
) -> Result<()> {
    // Build template variables map
    let mut variables = HashMap::new();
    variables.insert("app_name".to_string(), config.name.clone());
    variables.insert("friendly_app_name".to_string(), config.friendly_app_name.clone());
    variables.insert("launcher_app_name".to_string(), config.launcher_app_name.clone());
    variables.insert("description".to_string(), config.description.clone());
    variables.insert("publisher_name".to_string(), config.publisher_name.clone());
    variables.insert("contact_email".to_string(), config.contact_email.clone());
    variables.insert("support_url".to_string(), config.support_url.clone());
    variables.insert("icon".to_string(), config.icon.clone());
    variables.insert("app_id".to_string(), config.app_id.clone());
    variables.insert("version".to_string(), config.version.clone());
    variables.insert("min_keyos_version".to_string(), config.min_keyos_version.clone());

    // `sdk_keyos_root` is the preferred variable for bundled templates. The
    // other SDK path variables remain part of the template API for external
    // SDK/user templates that already consume them.
    variables.insert("sdk_root".to_string(), project_sdk_root_path().to_string());
    variables.insert("sdk_keyos_root".to_string(), project_sdk_keyos_root_path().to_string());
    variables.insert("sdk_ui_root".to_string(), project_sdk_ui_root_path().to_string());
    variables.insert("sdk_path".to_string(), project_sdk_keyos_root_path().to_string());
    variables.insert("selected_theme_id".to_string(), config.selected_theme_id.clone());

    // Get template path
    let template_path = template::get_template_path(template_name, Some(sdk));
    let template_files_path = template_path.join("files");

    if !template_files_path.exists() {
        anyhow::bail!("Error: Template '{}' not found at {}", template_name, template_path.display());
    }

    // Process template
    let processor = TemplateProcessor::new(variables);
    processor
        .process_directory(&template_files_path, project_path)
        .with_context(|| format!("Failed to apply template '{}'", template_name))?;
    ensure_project_sdk_mapping(project_path, sdk)?;
    crate::commands::themes::write_editable_app_theme(
        &config.selected_theme_id,
        sdk,
        project_path,
        &project_path.join("resources").join("theme.json"),
        &config.friendly_app_name,
    )?;

    Ok(())
}

fn load_available_themes(sdk: &SdkRoot) -> Result<Vec<ThemeEntry>> {
    let theme_dir = sdk.keyos_root().join("sdk").join("crates").join("foundation-themes").join("themes");
    let mut entries = fs::read_dir(&theme_dir)
        .with_context(|| format!("Failed to read built-in themes at {}", theme_dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    let mut themes = Vec::new();
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        let contents = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read theme file {}", path.display()))?;
        let parsed: ThemeFileEntry = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse theme file {}", path.display()))?;
        let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("theme");

        themes.push(ThemeEntry {
            id: parsed.id.unwrap_or_else(|| normalize_identifier(stem)),
            display_name: parsed.name.unwrap_or_else(|| prettify_identifier(stem)),
            // Convention: themes with no explicit sort_order land at the end of the
            // built-in block (sort_order <= 999 reserved) but before any future
            // out-of-tree additions.
            sort_order: parsed.sort_order.unwrap_or(1000),
        });
    }

    themes.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.display_name.to_ascii_lowercase().cmp(&b.display_name.to_ascii_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });

    if themes.is_empty() {
        anyhow::bail!("No themes found in {}", theme_dir.display());
    }

    Ok(themes)
}

fn normalize_identifier(value: &str) -> String {
    let mut ident = String::new();
    let mut last_was_underscore = true;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            if ident.is_empty() && ch.is_ascii_digit() {
                ident.push('_');
            }
            ident.push(ch.to_ascii_lowercase());
            last_was_underscore = false;
        } else if !last_was_underscore {
            ident.push('_');
            last_was_underscore = true;
        }
    }
    while ident.ends_with('_') {
        ident.pop();
    }
    if ident.is_empty() {
        "theme".to_string()
    } else {
        ident
    }
}

fn prettify_identifier(value: &str) -> String {
    normalize_identifier(value)
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let Some(first) = chars.next() else { return String::new() };
            format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn initialize_git_repo(project_path: &PathBuf) -> GitInitStatus {
    if !is_git_available() {
        return GitInitStatus::Unavailable;
    }

    let status = Command::new("git").arg("init").current_dir(project_path).status();

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
    use std::time::{SystemTime, UNIX_EPOCH};

    use foundation_core::SdkRoot;

    use super::{apply_template, initialize_git_repo, is_git_available, GitInitStatus, PromptConfig};
    use crate::sdk_mapping::{project_sdk_keyos_root_path, project_sdk_root_path, project_sdk_ui_root_path};
    use crate::slint_codegen::{prepare_project_for_build, project_sdk_ui_root, UI_LIBRARY_PATH_ENV};

    #[test]
    fn apply_template_uses_project_sdk_keyos_root_variable() {
        let sdk_root = make_sdk_root("template-sdk");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let project_root = make_temp_dir("scaffold-project");
        let project_path = project_root.join("demo-app");

        fs::create_dir_all(&project_path).unwrap();
        let config = PromptConfig {
            name: "demo-app".to_string(),
            friendly_app_name: "Demo App".to_string(),
            launcher_app_name: "Demo".to_string(),
            description: "Demo app".to_string(),
            publisher_name: "Demo Publisher".to_string(),
            contact_email: "support@example.com".to_string(),
            support_url: "https://example.com".to_string(),
            icon: "resources/icon.svg".to_string(),
            app_id: "0x00112233445566778899aabbccddeeff".to_string(),
            version: "0.1.0".to_string(),
            min_keyos_version: "1.0.0".to_string(),
            selected_theme_id: "default_theme".to_string(),
        };

        apply_template(&project_path, "default-app", &config, &sdk).unwrap();

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
        assert!(project_path.join("resources").join("theme.json").exists());
        assert!(!project_path.join("theme").join("theme.json").exists());
        let cargo_lock = fs::read_to_string(project_path.join("Cargo.lock")).unwrap();
        assert!(cargo_lock.contains("name = \"demo-app\""));
        assert!(cargo_lock.contains("version = \"0.1.0\""));
        let agents = fs::read_to_string(project_path.join("AGENTS.md")).unwrap();
        assert!(agents.contains("# Demo App Agent Guide"));
        assert!(agents.contains("sdk/docs/foundation-cli.md"));
        assert!(!agents.contains("{{friendly_app_name}}"));
        assert!(project_path.join("app-config.toml").exists());
        assert!(project_path.join("permission_templates.toml").exists());
        assert!(project_path.join("resources").join("icon.svg").exists());

        cleanup(&project_root);
        cleanup(sdk_root.parent().unwrap());
    }

    #[test]
    fn apply_template_keeps_sdk_path_variables_for_external_templates() {
        let sdk_root = make_sdk_root("compat-template-sdk");
        let template_dir =
            sdk_root.join("crates").join("cli").join("templates").join("compat-vars").join("files");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(
            template_dir.join("paths.txt"),
            "root={{sdk_root}}\nkeyos={{sdk_keyos_root}}\nui={{sdk_ui_root}}\npath={{sdk_path}}\n",
        )
        .unwrap();

        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let project_root = make_temp_dir("compat-scaffold-project");
        let project_path = project_root.join("demo-app");

        fs::create_dir_all(&project_path).unwrap();
        let config = PromptConfig {
            name: "demo-app".to_string(),
            friendly_app_name: "Demo App".to_string(),
            launcher_app_name: "Demo".to_string(),
            description: "Demo app".to_string(),
            publisher_name: "Demo Publisher".to_string(),
            contact_email: "support@example.com".to_string(),
            support_url: "https://example.com".to_string(),
            icon: "resources/icon.svg".to_string(),
            app_id: "0x00112233445566778899aabbccddeeff".to_string(),
            version: "0.1.0".to_string(),
            min_keyos_version: "1.0.0".to_string(),
            selected_theme_id: "default_theme".to_string(),
        };

        apply_template(&project_path, "compat-vars", &config, &sdk).unwrap();

        let paths = fs::read_to_string(project_path.join("paths.txt")).unwrap();
        assert_eq!(
            paths,
            format!(
                "root={}\nkeyos={}\nui={}\npath={}\n",
                project_sdk_root_path(),
                project_sdk_keyos_root_path(),
                project_sdk_ui_root_path(),
                project_sdk_keyos_root_path()
            )
        );
        assert!(project_path.join(project_sdk_keyos_root_path()).exists());

        cleanup(&project_root);
        cleanup(sdk_root.parent().unwrap());
    }

    // Heavy integration test: scaffolds an app and `cargo check`s it against the
    // real SDK. Requires (1) generated base themes in ~/.foundation/themes/rust
    // - `foundation themes build` or the helper below provides these - and (2) a
    // nightly cargo (the SDK workspace uses the `trim-paths` feature). Ignored by
    // default because a nested `cargo test` under nix can't reliably self-provision
    // either precondition; run explicitly with `--ignored` in a provisioned env.
    #[test]
    #[ignore = "requires ~/.foundation/themes/rust populated and nightly cargo; run with --ignored"]
    fn scaffolded_default_app_compiles_with_generated_theme_module() {
        let sdk = SdkRoot::discover_from(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        let project_root = make_temp_dir("scaffold-compile");
        let project_path = project_root.join("demo-app");
        let home = project_root.join("home");

        fs::create_dir_all(&project_path).unwrap();
        let config = PromptConfig {
            name: "demo-app".to_string(),
            friendly_app_name: "Demo App".to_string(),
            launcher_app_name: "Demo".to_string(),
            description: "Demo app".to_string(),
            publisher_name: "Demo Publisher".to_string(),
            contact_email: "support@example.com".to_string(),
            support_url: "https://example.com".to_string(),
            icon: "resources/icon.svg".to_string(),
            app_id: "0x00112233445566778899aabbccddeeff".to_string(),
            version: "0.1.0".to_string(),
            min_keyos_version: "1.0.0".to_string(),
            selected_theme_id: "default_theme".to_string(),
        };

        apply_template(&project_path, "default-app", &config, &sdk).unwrap();
        prepare_project_for_build(&project_path, &sdk).unwrap();

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

        cleanup(&project_root);
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
            rust_dir.join("default_theme.rs").exists(),
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
        let project_root = make_temp_dir("scaffold-multi-page-compile");
        let project_path = project_root.join("demo-app");
        let home = project_root.join("home");

        fs::create_dir_all(&project_path).unwrap();
        let config = PromptConfig {
            name: "demo-app".to_string(),
            friendly_app_name: "Demo App".to_string(),
            launcher_app_name: "Demo".to_string(),
            description: "Demo app".to_string(),
            publisher_name: "Demo Publisher".to_string(),
            contact_email: "support@example.com".to_string(),
            support_url: "https://example.com".to_string(),
            icon: "resources/icon.svg".to_string(),
            app_id: "0x00112233445566778899aabbccddeeff".to_string(),
            version: "0.1.0".to_string(),
            min_keyos_version: "1.0.0".to_string(),
            selected_theme_id: "default_theme".to_string(),
        };

        apply_template(&project_path, "multi-page-app", &config, &sdk).unwrap();
        prepare_project_for_build(&project_path, &sdk).unwrap();

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

        cleanup(&project_root);
    }

    #[test]
    fn initialize_git_repo_runs_git_init_when_available() {
        if !is_git_available() {
            return;
        }

        let temp_root = make_temp_dir("git-init");
        let project_root = temp_root.join("project");
        fs::create_dir_all(&project_root).unwrap();

        let status = initialize_git_repo(&project_root);

        assert_eq!(status, GitInitStatus::Initialized);
        assert!(project_root.join(".git").is_dir());

        cleanup(&temp_root);
    }

    fn make_sdk_root(label: &str) -> PathBuf {
        let repo_root = make_temp_dir(label);
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

        root
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

    fn make_temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("foundation-new-{label}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn cleanup(path: &Path) { let _ = fs::remove_dir_all(path); }
}

/// Generate a random 16-byte app ID in hex format
fn generate_random_app_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Use current time and a simple random approach to generate 16 bytes
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();

    // Generate 16 bytes using the timestamp and some simple manipulation
    let mut bytes = [0u8; 16];
    let now_bytes = now.to_le_bytes();

    // Fill the first 8 bytes with timestamp
    bytes[..8].copy_from_slice(&now_bytes[..8]);

    // Fill the remaining 8 bytes with a different transformation of timestamp
    let now2 = now.wrapping_mul(0x123456789ABCDEF);
    let now2_bytes = now2.to_le_bytes();
    bytes[8..].copy_from_slice(&now2_bytes[..8]);

    // Convert to hex string
    format!("0x{}", bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>())
}
