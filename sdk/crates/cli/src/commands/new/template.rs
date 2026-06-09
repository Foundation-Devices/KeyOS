// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Template processing for new project scaffolding

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use foundation_core::SdkRoot;
use include_dir::{include_dir, Dir};

/// Embedded templates directory
static EMBEDDED_TEMPLATES: Dir = include_dir!("$CARGO_MANIFEST_DIR/templates");

/// Template processor that replaces {{variable}} with actual values
pub struct TemplateProcessor {
    variables: HashMap<String, String>,
}

impl TemplateProcessor {
    /// Create a new template processor with the given variables
    pub fn new(variables: HashMap<String, String>) -> Self { Self { variables } }

    /// Process a template string, replacing all {{variable}} placeholders.
    ///
    /// Each substituted value is escaped for `format` first. Every templated
    /// file embeds these values inside double-quoted string literals
    /// (`description = "{{description}}"`, `title: "{{friendly_app_name}}"`,
    /// etc.), so a prompted value containing a quote, backslash, or newline
    /// would otherwise terminate the string early and produce a file that no
    /// longer parses. Escaping per target format keeps the generated project
    /// valid regardless of what the user typed at the `foundation new` prompts.
    pub fn process_string(&self, template: &str, format: EscapeFormat) -> String {
        let mut result = template.to_string();

        for (key, value) in &self.variables {
            let placeholder = format!("{{{{{}}}}}", key);
            result = result.replace(&placeholder, &format.escape(value));
        }

        result
    }

    /// Process a template file and write it to the destination
    pub fn process_file(&self, source: &Path, dest: &Path) -> Result<()> {
        let content = fs::read_to_string(source)
            .with_context(|| format!("Failed to read template file: {}", source.display()))?;

        let format = EscapeFormat::for_path(dest);
        let processed = self.process_string(&content, format);

        // Ensure parent directory exists
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        fs::write(dest, processed).with_context(|| format!("Failed to write file: {}", dest.display()))?;

        Ok(())
    }

    /// Copy a directory recursively, processing all files
    pub fn process_directory(&self, source: &Path, dest: &Path) -> Result<()> {
        if !source.is_dir() {
            anyhow::bail!("Source is not a directory: {}", source.display());
        }

        fs::create_dir_all(dest)
            .with_context(|| format!("Failed to create directory: {}", dest.display()))?;

        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let source_path = entry.path();
            let file_name = entry.file_name();
            let dest_path = dest.join(&file_name);

            if file_type.is_dir() {
                // Recursively process subdirectories
                self.process_directory(&source_path, &dest_path)?;
            } else if file_type.is_file() {
                // Check if it's a text file that should be processed
                if should_process_file(&file_name) {
                    self.process_file(&source_path, &dest_path)?;
                } else {
                    // Just copy binary/resource files
                    fs::copy(&source_path, &dest_path)
                        .with_context(|| format!("Failed to copy file: {}", source_path.display()))?;
                }
            }
        }

        Ok(())
    }
}

/// Determine if a file should be processed as a template or just copied
fn should_process_file(file_name: &std::ffi::OsStr) -> bool {
    let name = file_name.to_string_lossy();

    // Process these file types as templates
    matches!(name.rsplit('.').next().unwrap_or(""), "rs" | "toml" | "json" | "slint" | "txt" | "md" | "lock")
}

/// How to escape a substituted value so it stays valid inside a double-quoted
/// string literal of the target file's format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscapeFormat {
    /// TOML basic strings (`.toml`).
    Toml,
    /// JSON strings (`.json`).
    Json,
    /// Rust / Slint double-quoted string literals (`.rs`, `.slint`).
    RustLike,
    /// Plain text — no structure to break (`.txt`, `.md`, `.lock`, unknown).
    None,
}

impl EscapeFormat {
    fn for_path(path: &Path) -> Self {
        match path.extension().and_then(|ext| ext.to_str()).unwrap_or("") {
            "toml" => EscapeFormat::Toml,
            "json" => EscapeFormat::Json,
            "rs" | "slint" => EscapeFormat::RustLike,
            _ => EscapeFormat::None,
        }
    }

    /// Escape `value` for embedding inside a double-quoted string literal of
    /// this format. The common terminators (`"`, `\`, newline, CR, tab) escape
    /// identically across TOML/JSON/Rust/Slint; only the fallback for other
    /// control characters differs (JSON/TOML use `\uXXXX`, Rust/Slint `\u{X}`).
    pub fn escape(self, value: &str) -> String {
        if self == EscapeFormat::None {
            return value.to_string();
        }

        let mut out = String::with_capacity(value.len());
        for c in value.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                // Remaining C0 control characters can't appear literally in any
                // of these string literals; emit a format-appropriate escape.
                c if (c as u32) < 0x20 => match self {
                    EscapeFormat::Json | EscapeFormat::Toml => out.push_str(&format!("\\u{:04x}", c as u32)),
                    _ => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
                },
                c => out.push(c),
            }
        }
        out
    }
}

/// Extract embedded templates to the user's data directory
fn extract_embedded_templates() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;

    let templates_dir = home.join(".foundation").join("templates");

    // Create templates directory if it doesn't exist
    fs::create_dir_all(&templates_dir)
        .with_context(|| format!("Failed to create templates directory: {}", templates_dir.display()))?;

    // Extract all files in the root of the templates directory (like README.md)
    for file in EMBEDDED_TEMPLATES.files() {
        let file_path = templates_dir.join(file.path().file_name().unwrap());
        fs::write(&file_path, file.contents())
            .with_context(|| format!("Failed to write file: {}", file_path.display()))?;
    }

    // Extract each template subdirectory
    for template_dir in EMBEDDED_TEMPLATES.dirs() {
        if let Some(template_name) = template_dir.path().file_name() {
            let dest_path = templates_dir.join(template_name);
            extract_dir(template_dir, &dest_path)?;
        }
    }

    Ok(templates_dir)
}

/// Recursively extract a Dir to the filesystem
fn extract_dir(dir: &Dir, dest: &Path) -> Result<()> {
    // Create destination directory
    fs::create_dir_all(dest).with_context(|| format!("Failed to create directory: {}", dest.display()))?;

    // Extract all files directly at this level
    for file in dir.files() {
        // Get just the file name, not the full path
        if let Some(file_name) = file.path().file_name() {
            let file_path = dest.join(file_name);

            // Write file contents
            fs::write(&file_path, file.contents())
                .with_context(|| format!("Failed to write file: {}", file_path.display()))?;
        }
    }

    // Extract all subdirectories
    for subdir in dir.dirs() {
        // Get just the directory name, not the full path
        if let Some(dir_name) = subdir.path().file_name() {
            let subdir_path = dest.join(dir_name);
            extract_dir(subdir, &subdir_path)?;
        }
    }

    Ok(())
}

/// Get the path to a template directory
/// This will extract embedded templates to the user's data directory if needed
pub fn get_template_path(template_name: &str, sdk: Option<&SdkRoot>) -> PathBuf {
    if let Some(sdk) = sdk {
        for template_root in sdk.template_roots() {
            let candidate = template_root.join(template_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // Fallback for standalone development mode
    let dev_path = PathBuf::from("templates").join(template_name);
    if dev_path.exists() {
        return dev_path;
    }

    // Extract embedded templates to user's data directory
    if let Ok(templates_dir) = extract_embedded_templates() {
        let template_path = templates_dir.join(template_name);
        if template_path.exists() {
            return template_path;
        }
    }

    // Default fallback
    PathBuf::from("templates").join(template_name)
}

/// Get list of available templates
pub fn list_available_templates(sdk: Option<&SdkRoot>) -> Vec<(String, String)> {
    if let Some(sdk) = sdk {
        for template_root in sdk.template_roots() {
            if template_root.exists() {
                let templates = list_templates_from_fs(&template_root);
                if !templates.is_empty() {
                    return templates;
                }
            }
        }
    }

    let mut templates = Vec::new();

    // Use embedded templates directly
    for template_dir in EMBEDDED_TEMPLATES.dirs() {
        if let Some(template_name) = template_dir.path().file_name().and_then(|n| n.to_str()) {
            // Look for template.toml file in this template directory
            let toml_path = format!("{}/template.toml", template_name);
            if let Some(toml_file) = EMBEDDED_TEMPLATES.get_file(&toml_path) {
                let description = read_embedded_template_description(toml_file.contents_utf8())
                    .unwrap_or_else(|| "No description".to_string());
                templates.push((template_name.to_string(), description));
            }
        }
    }

    // Sort templates alphabetically
    templates.sort_by(|a, b| a.0.cmp(&b.0));
    templates
}

fn list_templates_from_fs(template_root: &Path) -> Vec<(String, String)> {
    let mut templates = Vec::new();
    let read_dir = match fs::read_dir(template_root) {
        Ok(read_dir) => read_dir,
        Err(_) => return templates,
    };

    for entry in read_dir.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }

        let template_name = entry.file_name().to_string_lossy().to_string();
        let template_toml = entry.path().join("template.toml");
        let description = fs::read_to_string(&template_toml)
            .ok()
            .and_then(|contents| read_embedded_template_description(Some(&contents)))
            .unwrap_or_else(|| "No description".to_string());
        templates.push((template_name, description));
    }

    templates.sort_by(|a, b| a.0.cmp(&b.0));
    templates
}

/// Read template description from embedded template.toml contents.
/// Uses the `toml` crate so quoted-string escapes, multi-line strings, and
/// nested tables can't break us silently.
#[derive(serde::Deserialize)]
struct TemplateMeta {
    description: Option<String>,
    #[serde(default)]
    variables: HashMap<String, String>,
}

fn read_embedded_template_description(contents: Option<&str>) -> Option<String> {
    let contents = contents?;
    let meta: TemplateMeta = toml::from_str(contents).ok()?;
    meta.description
}

/// Read a template's default `[variables]` from its template.toml, used to
/// pre-fill the `foundation new` prompts so per-template defaults live with the
/// template rather than being special-cased in the command. Resolves on-disk
/// SDK template roots first, then the embedded copy; empty map if not found.
pub fn read_template_variables(template_name: &str, sdk: Option<&SdkRoot>) -> HashMap<String, String> {
    if let Some(sdk) = sdk {
        for template_root in sdk.template_roots() {
            let toml_path = template_root.join(template_name).join("template.toml");
            if let Ok(contents) = fs::read_to_string(&toml_path) {
                if let Ok(meta) = toml::from_str::<TemplateMeta>(&contents) {
                    return meta.variables;
                }
            }
        }
    }

    let toml_path = format!("{}/template.toml", template_name);
    if let Some(meta) = EMBEDDED_TEMPLATES
        .get_file(&toml_path)
        .and_then(|f| f.contents_utf8())
        .and_then(|contents| toml::from_str::<TemplateMeta>(contents).ok())
    {
        return meta.variables;
    }

    HashMap::new()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::{EscapeFormat, TemplateProcessor};

    fn processor(key: &str, value: &str) -> TemplateProcessor {
        let mut vars = HashMap::new();
        vars.insert(key.to_string(), value.to_string());
        TemplateProcessor::new(vars)
    }

    #[test]
    fn toml_value_with_quote_stays_parseable() {
        // A description containing a double quote previously broke the TOML.
        let proc = processor("description", r#"my "great" app"#);
        let rendered = proc.process_string(
            r#"description = "{{description}}""#,
            EscapeFormat::for_path(Path::new("app-config.toml")),
        );
        let parsed: toml::Value = toml::from_str(&rendered).expect("escaped TOML must parse");
        assert_eq!(parsed["description"].as_str().unwrap(), r#"my "great" app"#);
    }

    #[test]
    fn toml_value_with_newline_and_backslash_stays_parseable() {
        let proc = processor("description", "line1\nback\\slash");
        let rendered = proc.process_string(
            r#"description = "{{description}}""#,
            EscapeFormat::for_path(Path::new("app-config.toml")),
        );
        let parsed: toml::Value = toml::from_str(&rendered).expect("escaped TOML must parse");
        assert_eq!(parsed["description"].as_str().unwrap(), "line1\nback\\slash");
    }

    #[test]
    fn json_value_with_quote_stays_parseable() {
        let proc = processor("friendly_app_name", r#"Quote " here"#);
        let rendered = proc.process_string(
            r#"{ "home.heading": "{{friendly_app_name}}" }"#,
            EscapeFormat::for_path(Path::new("i18n/en.json")),
        );
        let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("escaped JSON must parse");
        assert_eq!(parsed["home.heading"], r#"Quote " here"#);
    }

    #[test]
    fn rust_and_slint_quotes_are_escaped() {
        let proc = processor("friendly_app_name", r#"a"b\c"#);
        let rendered = proc.process_string(
            r#"title: "{{friendly_app_name}}";"#,
            EscapeFormat::for_path(Path::new("ui/app.slint")),
        );
        assert_eq!(rendered, r#"title: "a\"b\\c";"#);
    }

    #[test]
    fn plain_text_is_not_escaped() {
        let proc = processor("name", r#"a"b"#);
        let rendered = proc.process_string("name: {{name}}", EscapeFormat::for_path(Path::new("README.md")));
        assert_eq!(rendered, r#"name: a"b"#);
    }
}
