// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Builds the public API rustdoc site and annotates Rust items through a parsed HTML DOM.
//! Regular expressions are limited to Rust type text and rustdoc's search-index data; they are
//! never used to locate or rewrite HTML elements.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

use anyhow::{bail, ensure, Context, Result};
use app_manifest::{ApiManifest, ApprovalBehavior, Message, RequiredSignature};
use clap::Args;
use dom_query::{Document, Selection};
use regex::Regex;
use serde::Serialize;

use crate::builder::{cargo, project_root};

#[derive(Args, Debug)]
pub struct DocsApiArgs {
    /// Cargo packages to include. All public API packages are included by default.
    #[arg(value_name = "PACKAGE")]
    packages: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
struct CrateDoc {
    package: &'static str,
    crate_name: &'static str,
    description: &'static str,
    manifest: Option<&'static str>,
}

impl CrateDoc {
    fn href(self) -> String { format!("{}/index.html", self.crate_name) }
}

const CRATES: &[CrateDoc] = &[
    CrateDoc {
        package: "server",
        crate_name: "server",
        description: "Type-safe server traits, checked connections, and IPC helpers.",
        manifest: None,
    },
    CrateDoc {
        package: "app-manager",
        crate_name: "app_manager",
        description: "App launch, app metadata, QR acceptance criteria, and third-party certificates.",
        manifest: Some("api/app-manager/manifest.toml"),
    },
    CrateDoc {
        package: "backup",
        crate_name: "backup",
        description: "Backup and restore operations.",
        manifest: Some("api/backup/manifest.toml"),
    },
    CrateDoc {
        package: "camera",
        crate_name: "camera",
        description: "Camera frame subscription and camera configuration.",
        manifest: Some("api/camera/manifest.toml"),
    },
    CrateDoc {
        package: "crypto",
        crate_name: "crypto",
        description: "Cryptographic server APIs.",
        manifest: Some("api/crypto/manifest.toml"),
    },
    CrateDoc {
        package: "fs",
        crate_name: "fs",
        description: "Filesystem handles, directories, metadata, and mapped files.",
        manifest: Some("api/fs/manifest.toml"),
    },
    CrateDoc {
        package: "gui-server-api",
        crate_name: "gui_server_api",
        description: "Rust API crate for the os/gui-server service.",
        manifest: Some("api/gui-server/manifest.toml"),
    },
    CrateDoc {
        package: "haptics",
        crate_name: "haptics",
        description: "Haptic patterns and playback control.",
        manifest: Some("api/haptics/manifest.toml"),
    },
    CrateDoc {
        package: "keycard",
        crate_name: "keycard",
        description: "Keycard operations and backup shard management.",
        manifest: Some("api/keycard/manifest.toml"),
    },
    CrateDoc {
        package: "nfc",
        crate_name: "nfc",
        description: "NFC operations.",
        manifest: Some("api/nfc/manifest.toml"),
    },
    CrateDoc {
        package: "power-manager",
        crate_name: "power_manager",
        description: "Reboot, shutdown, charging, battery, and OTG status.",
        manifest: Some("api/power-manager/manifest.toml"),
    },
    CrateDoc {
        package: "quantum-link",
        crate_name: "quantum_link",
        description: "QuantumLink state, firmware fetch, pairing, PSBT, and account-sync helpers.",
        manifest: Some("api/quantum-link/manifest.toml"),
    },
    CrateDoc {
        package: "rgb-led",
        crate_name: "rgb_led",
        description: "RGB LED color and animation control.",
        manifest: Some("api/rgb-led/manifest.toml"),
    },
    CrateDoc {
        package: "security",
        crate_name: "security",
        description: "Authentication, seeds, keys, and device security state.",
        manifest: Some("api/security/manifest.toml"),
    },
    CrateDoc {
        package: "settings",
        crate_name: "settings",
        description: "Typed settings get, set, and subscribe APIs.",
        manifest: Some("api/settings/manifest.toml"),
    },
    CrateDoc {
        package: "update",
        crate_name: "update",
        description: "Firmware update status and progress APIs.",
        manifest: Some("api/update/manifest.toml"),
    },
    CrateDoc {
        package: "usb",
        crate_name: "usb",
        description: "USB host and device-mode APIs.",
        manifest: Some("api/usb/manifest.toml"),
    },
    CrateDoc {
        package: "fido",
        crate_name: "fido",
        description: "FIDO2/U2F security-key service APIs.",
        manifest: Some("os/fido/manifest.toml"),
    },
];

const FORBIDDEN_CRATES: &[&str] = &["bt", "gpio", "i2c", "spi", "dma", "keyos_api_docs"];
const TEMPLATE_DIR: &str = "xtask/assets/docs-api";
const GENERATED_ASSET_DIR: &str = "foundation-assets";
const FOUNDATION_CSS: &str = include_str!("../assets/docs-api/foundation.css");
const FOUNDATION_HEADER: &str = include_str!("../assets/docs-api/header.html");
const TEMPLATE_ASSETS: &[(&str, &str)] = &[
    ("ui/ui/fonts/Montserrat-Light.ttf", "Montserrat-Light.ttf"),
    ("ui/ui/fonts/Montserrat-Regular.ttf", "Montserrat-Regular.ttf"),
    ("ui/ui/fonts/Montserrat-Medium.ttf", "Montserrat-Medium.ttf"),
    ("utils/font-gen/fonts/SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    ("utils/font-gen/fonts/SourceCodePro-SemiBold.ttf", "SourceCodePro-SemiBold.ttf"),
    ("xtask/assets/docs-api/top-logo.webp", "top-logo.webp"),
    ("xtask/assets/docs-api/top-logo-dark.webp", "top-logo-dark.webp"),
];

static MESSAGE_ALLOWED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"MessageAllowed\s*<\s*(?:(?:[A-Za-z_][A-Za-z0-9_]*)::)*([A-Za-z_][A-Za-z0-9_]*)\s*>")
        .expect("MessageAllowed regex is valid")
});

#[derive(Clone, Debug)]
struct MessageDefinition {
    server: String,
    message: Message,
}

type MessageMap = BTreeMap<String, BTreeMap<String, MessageDefinition>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionInfo {
    message: String,
    server: Option<String>,
    permission_group: Option<String>,
    required_signature: Option<String>,
    approval: Option<String>,
    status: PermissionStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum PermissionStatus {
    Known,
    UnknownServer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct PermissionRecord {
    #[serde(rename = "crate")]
    crate_name: String,
    html_path: String,
    kind: String,
    item: String,
    item_id: String,
    permissions: Vec<PermissionInfo>,
}

pub fn run(args: DocsApiArgs) -> Result<()> {
    let root = project_root();
    let doc_dir = root.join("target/doc");
    let crates = select_crates(&args.packages)?;

    run_rustdoc(&root, &doc_dir, &crates)?;
    copy_template_assets(&root, &doc_dir)?;
    verify_crate_outputs(&doc_dir, &crates)?;
    write_index(&doc_dir, &crates)?;
    rewrite_template_asset_paths(&doc_dir)?;

    let messages = build_message_map(&root, &crates)?;
    let records = annotate_html(&doc_dir, &crates, &messages)?;
    scrub_forbidden_search_paths(&doc_dir)?;
    assert_no_forbidden_artifacts(&doc_dir)?;
    assert_template_artifacts(&doc_dir, &crates)?;

    println!("Built KeyOS API docs at {}", doc_dir.join("index.html").display());
    println!("Annotated {} rustdoc function/method entries", records.len());
    Ok(())
}

fn select_crates(packages: &[String]) -> Result<Vec<CrateDoc>> {
    if packages.is_empty() {
        return Ok(CRATES.to_vec());
    }

    let by_package: BTreeMap<_, _> = CRATES.iter().map(|entry| (entry.package, *entry)).collect();
    let mut unknown = packages
        .iter()
        .filter(|package| !by_package.contains_key(package.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    unknown.sort();
    unknown.dedup();
    ensure!(unknown.is_empty(), "unknown API doc package(s): {}", unknown.join(", "));

    let mut seen = BTreeSet::new();
    Ok(packages
        .iter()
        .filter(|package| seen.insert(package.as_str()))
        .map(|package| by_package[package.as_str()])
        .collect())
}

fn run_rustdoc(root: &Path, doc_dir: &Path, crates: &[CrateDoc]) -> Result<()> {
    ensure!(!root.join("api/docs").exists(), "api/docs still exists; keyos-api-docs must not be restored");
    ensure!(
        root.join("permission_templates.toml").exists(),
        "permission_templates.toml is required by the API proc macros"
    );
    let template_dir = root.join(TEMPLATE_DIR);
    let template_css = template_dir.join("foundation.css");
    let template_header = template_dir.join("header.html");
    ensure!(template_css.exists(), "missing API docs template: {}", template_css.display());
    ensure!(template_header.exists(), "missing API docs template: {}", template_header.display());

    match fs::remove_dir_all(doc_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("removing {}", doc_dir.display())),
    }

    let mut command = Command::new(cargo());
    command.args(["doc", "--no-deps"]);
    for entry in crates {
        command.args(["-p", entry.package]);
    }

    let existing_rustdoc_flags = env::var("RUSTDOCFLAGS").unwrap_or_default();
    let rustdoc_flags = compose_rustdoc_flags(&existing_rustdoc_flags, &template_css, &template_header);
    command.current_dir(root).env("RUSTDOCFLAGS", rustdoc_flags);

    let status = command.status().context("running cargo doc")?;
    ensure!(status.success(), "cargo doc failed with {status}");
    Ok(())
}

fn compose_rustdoc_flags(existing: &str, template_css: &Path, template_header: &Path) -> String {
    let template_flags = format!(
        "--default-theme light --extend-css {} --html-before-content {}",
        template_css.display(),
        template_header.display()
    );
    match existing.trim() {
        "" => template_flags,
        flags => format!("{flags} {template_flags}"),
    }
}

fn copy_template_assets(root: &Path, doc_dir: &Path) -> Result<()> {
    let destination_dir = doc_dir.join(GENERATED_ASSET_DIR);
    fs::create_dir_all(&destination_dir)
        .with_context(|| format!("creating {}", destination_dir.display()))?;

    for (source, destination) in TEMPLATE_ASSETS {
        let source = root.join(source);
        let destination = destination_dir.join(destination);
        fs::copy(&source, &destination)
            .with_context(|| format!("copying {} to {}", source.display(), destination.display()))?;
    }
    Ok(())
}

fn template_asset_root(doc_dir: &Path, html_path: &Path) -> String {
    let relative_path = html_path.strip_prefix(doc_dir).unwrap_or(html_path);
    let depth = relative_path.parent().map_or(0, |parent| parent.components().count());
    if depth == 0 {
        "./".to_owned()
    } else {
        "../".repeat(depth)
    }
}

fn rewrite_template_asset_paths(doc_dir: &Path) -> Result<()> {
    let mut paths = collect_files(doc_dir)?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "html"))
        .collect::<Vec<_>>();
    paths.sort();

    for path in paths {
        let root_path = template_asset_root(doc_dir, &path);
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let updated = text
            .replace(
                "src=\"./top-logo.webp\"",
                &format!("src=\"{root_path}foundation-assets/top-logo.webp\""),
            )
            .replace(
                "srcset=\"./top-logo-dark.webp\"",
                &format!("srcset=\"{root_path}foundation-assets/top-logo-dark.webp\""),
            );
        if updated != text {
            fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(())
}

fn find_static(doc_dir: &Path, prefix: &str, suffix: &str) -> Result<String> {
    let static_dir = doc_dir.join("static.files");
    let mut matches = fs::read_dir(&static_dir)
        .with_context(|| format!("reading {}", static_dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(prefix) && name.ends_with(suffix))
        .collect::<Vec<_>>();
    matches.sort();
    matches.into_iter().next().with_context(|| format!("missing rustdoc asset matching {prefix}*{suffix}"))
}

fn write_index(doc_dir: &Path, crates: &[CrateDoc]) -> Result<()> {
    let normalize_css = find_static(doc_dir, "normalize-", ".css")?;
    let rustdoc_css = find_static(doc_dir, "rustdoc-", ".css")?;
    let noscript_css = find_static(doc_dir, "noscript-", ".css")?;
    let storage_js = find_static(doc_dir, "storage-", ".js")?;
    let main_js = find_static(doc_dir, "main-", ".js")?;
    let search_js = find_static(doc_dir, "search-", ".js")?;
    let stringdex_js = find_static(doc_dir, "stringdex-", ".js")?;
    let settings_js = find_static(doc_dir, "settings-", ".js")?;
    let favicon_png = find_static(doc_dir, "favicon-32x32-", ".png")?;
    let favicon_svg = find_static(doc_dir, "favicon-", ".svg")?;

    let sidebar = crates
        .iter()
        .map(|entry| {
            format!(
                "<li><a href=\"{}\">{}</a></li>",
                escape_html(&entry.href()),
                escape_html(entry.crate_name)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let table_rows = crates
        .iter()
        .map(|entry| {
            format!(
                "<dt><a class=\"mod\" href=\"{}\" title=\"crate {}\">{}</a></dt><dd>{}</dd>",
                escape_html(&entry.href()),
                escape_html(entry.crate_name),
                escape_html(entry.crate_name),
                escape_html(entry.description)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let index = format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<meta name="generator" content="rustdoc">
<meta name="description" content="KeyOS API documentation">
<title>KeyOS API Docs</title>
<link rel="stylesheet" href="./static.files/{normalize_css}">
<link rel="stylesheet" href="./static.files/{rustdoc_css}">
<link rel="stylesheet" href="./theme.css">
<script id="default-settings" data-use_system_theme="true" data-theme="light"></script>
<meta name="rustdoc-vars" data-root-path="./" data-static-root-path="./static.files/" data-current-crate="" data-themes="" data-resource-suffix="" data-rustdoc-version="generated" data-channel="" data-search-js="{search_js}" data-stringdex-js="{stringdex_js}" data-settings-js="{settings_js}">
<script src="./static.files/{storage_js}"></script>
<script defer src="./crates.js"></script>
<script defer src="./static.files/{main_js}"></script>
<noscript><link rel="stylesheet" href="./static.files/{noscript_css}"></noscript>
<link rel="alternate icon" type="image/png" href="./static.files/{favicon_png}">
<link rel="icon" type="image/svg+xml" href="./static.files/{favicon_svg}">
</head>
<body class="rustdoc mod crate">
{FOUNDATION_HEADER}
<rustdoc-topbar><h2><a href="#">KeyOS API Docs</a></h2></rustdoc-topbar>
<nav class="sidebar">
<div class="sidebar-crate"><h2><a href="./index.html">KeyOS API Docs</a></h2></div>
<div class="sidebar-elems"><section id="rustdoc-toc"><h3><a href="#crates">Crates</a></h3>
<ul class="block">{sidebar}</ul></section></div>
</nav>
<div class="sidebar-resizer" title="Drag to resize sidebar"></div>
<main><div class="width-limiter"><section id="main-content" class="content">
<div class="main-heading"><h1>KeyOS API Docs</h1><rustdoc-toolbar></rustdoc-toolbar></div>
<details class="toggle top-doc" open><summary class="hideme"><span>Expand description</span></summary>
<div class="docblock"><p>Generated rustdoc entry point for the public KeyOS API crates.</p></div></details>
<h2 id="crates" class="section-header">Crates<a href="#crates" class="anchor">&sect;</a></h2>
<dl class="item-table">{table_rows}</dl>
</section></div></main>
</body>
</html>
"##
    );
    fs::write(doc_dir.join("index.html"), index).context("writing rustdoc index")?;
    Ok(())
}

fn build_message_map(root: &Path, crates: &[CrateDoc]) -> Result<MessageMap> {
    let mut by_crate = BTreeMap::new();
    for entry in crates {
        let mut messages = BTreeMap::new();
        if let Some(relative_manifest) = entry.manifest {
            let manifest_path = root.join(relative_manifest);
            let manifest_dir = manifest_path
                .parent()
                .with_context(|| format!("manifest has no parent: {}", manifest_path.display()))?;
            let manifest = ApiManifest::load_with_tracking(manifest_dir, |_| {});
            for (server, server_messages) in manifest.servers {
                for (name, message) in server_messages {
                    ensure!(
                        messages
                            .insert(name.clone(), MessageDefinition { server: server.clone(), message })
                            .is_none(),
                        "message {name} appears in multiple servers for {}",
                        entry.crate_name
                    );
                }
            }
        }
        by_crate.insert(entry.crate_name.to_owned(), messages);
    }
    Ok(by_crate)
}

fn extract_message_allowed(text: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    MESSAGE_ALLOWED_RE
        .captures_iter(text)
        .filter_map(|capture| capture.get(1).map(|value| value.as_str().to_owned()))
        .filter(|message| seen.insert(message.clone()))
        .collect()
}

fn resolve_permissions(crate_name: &str, names: &[String], messages: &MessageMap) -> Vec<PermissionInfo> {
    names
        .iter()
        .map(|name| {
            let Some(definition) =
                messages.get(crate_name).and_then(|crate_messages| crate_messages.get(name))
            else {
                return PermissionInfo {
                    message: name.clone(),
                    server: None,
                    permission_group: None,
                    required_signature: None,
                    approval: None,
                    status: PermissionStatus::UnknownServer,
                };
            };

            PermissionInfo {
                message: name.clone(),
                server: Some(definition.server.clone()),
                permission_group: definition.message.permission_group.clone(),
                required_signature: Some(
                    required_signature_name(definition.message.required_signature()).to_owned(),
                ),
                approval: Some(approval_name(definition.message.approval).to_owned()),
                status: PermissionStatus::Known,
            }
        })
        .collect()
}

fn required_signature_name(signature: RequiredSignature) -> &'static str {
    match signature {
        RequiredSignature::ThirdParty => "thirdParty",
        RequiredSignature::Foundation => "foundation",
    }
}

fn approval_name(approval: ApprovalBehavior) -> &'static str {
    match approval {
        ApprovalBehavior::AutoAllow => "autoAllow",
        ApprovalBehavior::GrantOnFirstUse => "grantOnFirstUse",
        ApprovalBehavior::NotUserGrantable => "notUserGrantable",
    }
}

fn render_permission_block(permissions: &[PermissionInfo]) -> String {
    if permissions.is_empty() {
        return "<div class=\"docblock keyos-permissions\"><p><strong>Permission:</strong> none</p></div>"
            .to_owned();
    }

    if permissions.len() == 1 {
        return format_permission(&permissions[0]);
    }

    let items = permissions
        .iter()
        .map(|permission| format!("<li>{}</li>", format_permission_content(permission)))
        .collect::<String>();
    format!("<div class=\"docblock keyos-permissions\"><ul>{items}</ul></div>")
}

fn format_permission(permission: &PermissionInfo) -> String {
    format!("<div class=\"docblock keyos-permissions\">{}</div>", format_permission_content(permission))
}

fn format_permission_content(permission: &PermissionInfo) -> String {
    let permission_name = if let Some(server) = &permission.server {
        format!("<code>{} / {}</code>", escape_html(server), escape_html(&permission.message))
    } else {
        format!("TBD (<code>MessageAllowed&lt;{}&gt;</code>)", escape_html(&permission.message))
    };
    let group = permission
        .permission_group
        .as_deref()
        .map(|value| format!("<code>{}</code>", escape_html(value)))
        .unwrap_or_else(|| if permission.server.is_some() { "none".to_owned() } else { "TBD".to_owned() });
    let signature = code_or_tbd(permission.required_signature.as_deref());
    let approval = code_or_tbd(permission.approval.as_deref());

    format!(
        "<p><strong>Permission:</strong> {permission_name}</p>\
         <p><strong>Permission group:</strong> {group}</p>\
         <p><strong>Required signature:</strong> {signature}</p>\
         <p><strong>Approval:</strong> {approval}</p>"
    )
}

fn code_or_tbd(value: Option<&str>) -> String {
    value.map(|value| format!("<code>{}</code>", escape_html(value))).unwrap_or_else(|| "TBD".to_owned())
}

fn annotate_html(
    doc_dir: &Path,
    crates: &[CrateDoc],
    messages: &MessageMap,
) -> Result<Vec<PermissionRecord>> {
    let mut records = Vec::new();
    for entry in crates {
        let crate_dir = doc_dir.join(entry.crate_name);
        if !crate_dir.exists() {
            continue;
        }

        let mut paths = collect_files(&crate_dir)?
            .into_iter()
            .filter(|path| path.extension().is_some_and(|extension| extension == "html"))
            .collect::<Vec<_>>();
        paths.sort();

        for path in paths {
            let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let relative_path = path.strip_prefix(doc_dir).unwrap_or(&path);
            if let Some(updated) =
                annotate_document(&text, entry.crate_name, relative_path, messages, &mut records)
            {
                fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
            }
        }
    }

    fs::write(
        doc_dir.join("keyos-permissions.json"),
        serde_json::to_string_pretty(&records).context("serializing permission records")? + "\n",
    )
    .context("writing keyos-permissions.json")?;
    Ok(records)
}

fn annotate_document(
    text: &str,
    crate_name: &str,
    html_path: &Path,
    messages: &MessageMap,
    records: &mut Vec<PermissionRecord>,
) -> Option<String> {
    let document = Document::from(text);
    let initial_record_count = records.len();

    for impl_block in document.select("details.implementors-toggle").iter() {
        annotate_impl_block(&impl_block, crate_name, html_path, messages, records);
    }
    for deref_block in document.select("details.big-toggle").iter() {
        annotate_deref_block(&deref_block, crate_name, html_path, messages, records);
    }
    annotate_function_page(&document, crate_name, html_path, messages, records);

    (records.len() != initial_record_count).then(|| document.root().inner_html().to_string())
}

fn annotate_impl_block(
    impl_block: &Selection<'_>,
    crate_name: &str,
    html_path: &Path,
    messages: &MessageMap,
    records: &mut Vec<PermissionRecord>,
) {
    let header = impl_block.select("h3.code-header").first();
    if !header.exists() {
        return;
    }

    let header_text = header.text().to_string();
    let is_trait_impl = header_text.contains(" for ");
    if is_trait_impl && is_external_trait(&header) {
        return;
    }
    let impl_messages = extract_message_allowed(&header_text);

    annotate_method_container(
        impl_block,
        &impl_messages,
        !is_trait_impl,
        crate_name,
        html_path,
        messages,
        records,
    );
}

fn annotate_deref_block(
    deref_block: &Selection<'_>,
    crate_name: &str,
    html_path: &Path,
    messages: &MessageMap,
    records: &mut Vec<PermissionRecord>,
) {
    if !deref_block.select(r#"h2[id^="deref-methods-"]"#).exists() {
        return;
    }

    annotate_method_container(deref_block, &[], true, crate_name, html_path, messages, records);
}

fn annotate_method_container(
    container: &Selection<'_>,
    inherited_messages: &[String],
    include_without_permissions: bool,
    crate_name: &str,
    html_path: &Path,
    messages: &MessageMap,
    records: &mut Vec<PermissionRecord>,
) {
    for method in container.select(r#"section[id^="method."]"#).iter() {
        if !has_local_source(&method, crate_name) {
            continue;
        }

        let Some(item_id) = method.attr("id").map(|value| value.to_string()) else {
            continue;
        };
        let Some(item) = item_id.strip_prefix("method.").map(str::to_owned) else {
            continue;
        };

        let mut names = inherited_messages.to_vec();
        for name in extract_message_allowed(&method.text()) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        if names.is_empty() && !include_without_permissions {
            continue;
        }
        let permissions = resolve_permissions(crate_name, &names, messages);
        let block = render_permission_block(&permissions);

        let method_node = *method.nodes().first().expect("method selection is non-empty");
        if let Some(parent) = method_node.parent().filter(|parent| parent.is("summary")) {
            parent.after_html(block);
        } else {
            method_node.after_html(block);
        }

        records.push(permission_record(crate_name, html_path, "method", &item, &item_id, permissions));
    }
}

fn annotate_function_page(
    document: &Document,
    crate_name: &str,
    html_path: &Path,
    messages: &MessageMap,
    records: &mut Vec<PermissionRecord>,
) {
    let Some(file_name) = html_path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let Some(item) = file_name.strip_prefix("fn.").and_then(|name| name.strip_suffix(".html")) else {
        return;
    };
    if !has_local_source(&document.select("html"), crate_name) {
        return;
    }

    let declaration = document.select("pre.rust.item-decl").first();
    if !declaration.exists() {
        return;
    }
    let permissions =
        resolve_permissions(crate_name, &extract_message_allowed(&declaration.text()), messages);
    declaration.after_html(render_permission_block(&permissions));
    records.push(permission_record(
        crate_name,
        html_path,
        "function",
        item,
        &format!("fn.{item}"),
        permissions,
    ));
}

fn has_local_source(selection: &Selection<'_>, crate_name: &str) -> bool {
    let path_fragment = format!("src/{crate_name}/");
    selection
        .select("a.src[href]")
        .iter()
        .filter_map(|link| link.attr("href"))
        .any(|href| href.contains(&path_fragment))
}

fn is_external_trait(header: &Selection<'_>) -> bool {
    let Some(header_node) = header.nodes().first() else {
        return false;
    };
    let mut generic_depth = 0usize;
    let mut implemented_trait_href = None;

    // Generic bounds can contain trait links before the implemented trait. Only inspect links at
    // the top level, stopping at `for`, to identify the trait this impl actually implements.
    for node in header_node.descendants_it() {
        if node.is_text() {
            if text_reaches_for_at_top_level(&node.text(), &mut generic_depth) {
                break;
            }
        } else if generic_depth == 0 && node.is("a.trait[href]") {
            implemented_trait_href = node.attr("href");
        }
    }

    implemented_trait_href.is_some_and(|href| href.starts_with("http://") || href.starts_with("https://"))
}

fn text_reaches_for_at_top_level(text: &str, generic_depth: &mut usize) -> bool {
    let mut offset = 0;
    while offset < text.len() {
        if *generic_depth == 0 && text[offset..].starts_with(" for ") {
            return true;
        }

        let character = text[offset..].chars().next().expect("offset is within the string");
        match character {
            '<' => *generic_depth += 1,
            '>' => *generic_depth = generic_depth.saturating_sub(1),
            _ => {}
        }
        offset += character.len_utf8();
    }
    false
}

fn permission_record(
    crate_name: &str,
    html_path: &Path,
    kind: &str,
    item: &str,
    item_id: &str,
    permissions: Vec<PermissionInfo>,
) -> PermissionRecord {
    PermissionRecord {
        crate_name: crate_name.to_owned(),
        html_path: html_path.to_string_lossy().into_owned(),
        kind: kind.to_owned(),
        item: item.to_owned(),
        item_id: item_id.to_owned(),
        permissions,
    }
}

fn scrub_forbidden_search_paths(doc_dir: &Path) -> Result<()> {
    let search_index = doc_dir.join("search.index");
    if !search_index.exists() {
        return Ok(());
    }

    for file in collect_files(&search_index)? {
        let bytes = fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
        let content = String::from_utf8_lossy(&bytes);
        let mut updated = content.to_string();
        for crate_name in FORBIDDEN_CRATES.iter().copied().filter(|name| *name != "keyos_api_docs") {
            let pattern = Regex::new(&format!(r#""{}(?:::[^"]*)?""#, regex::escape(crate_name)))
                .expect("forbidden crate regex is valid");
            updated = pattern.replace_all(&updated, "\"internal\"").into_owned();
        }
        if updated != content {
            fs::write(&file, updated).with_context(|| format!("writing {}", file.display()))?;
        }
    }
    Ok(())
}

fn assert_no_forbidden_artifacts(doc_dir: &Path) -> Result<()> {
    let forbidden = FORBIDDEN_CRATES.iter().copied().collect::<BTreeSet<_>>();
    let mut failures = BTreeSet::new();

    for path in collect_paths(doc_dir)? {
        let relative = path.strip_prefix(doc_dir).unwrap_or(&path);
        let first = relative.components().next().and_then(|part| part.as_os_str().to_str());
        if first.is_some_and(|part| forbidden.contains(part)) {
            failures.insert(relative.display().to_string());
        }
        let relative_text = relative.to_string_lossy();
        if relative_text.contains("keyos_api_docs") || relative_text.contains("keyos-api-docs") {
            failures.insert(relative_text.into_owned());
        }
    }

    let crates_js = doc_dir.join("crates.js");
    if crates_js.exists() {
        let content = fs::read_to_string(&crates_js).context("reading crates.js")?;
        for crate_name in FORBIDDEN_CRATES {
            if content.contains(&format!("\"{crate_name}\"")) {
                failures.insert(format!("crates.js references {crate_name}"));
            }
        }
    }

    let search_index = doc_dir.join("search.index");
    if search_index.exists() {
        for file in collect_files(&search_index)? {
            let content = String::from_utf8_lossy(&fs::read(&file)?).into_owned();
            for crate_name in FORBIDDEN_CRATES {
                let pattern = Regex::new(&format!(r#""{}(?:::[^"]*)?""#, regex::escape(crate_name)))
                    .expect("forbidden crate regex is valid");
                if pattern.is_match(&content) {
                    failures.insert(format!(
                        "{} references {crate_name}",
                        file.strip_prefix(doc_dir).unwrap_or(&file).display()
                    ));
                }
            }
        }
    }

    if !failures.is_empty() {
        bail!("forbidden rustdoc artifacts found:\n{}", failures.into_iter().collect::<Vec<_>>().join("\n"));
    }
    Ok(())
}

fn assert_template_artifacts(doc_dir: &Path, crates: &[CrateDoc]) -> Result<()> {
    ensure!(
        FOUNDATION_CSS.contains(".foundation-docs-header"),
        "Foundation API docs template is missing its header styles"
    );
    let missing_assets = TEMPLATE_ASSETS
        .iter()
        .map(|(_, destination)| doc_dir.join(GENERATED_ASSET_DIR).join(destination))
        .filter(|path| !path.exists())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    ensure!(missing_assets.is_empty(), "missing API docs template assets: {}", missing_assets.join(", "));

    let theme_css = fs::read_to_string(doc_dir.join("theme.css")).context("reading generated theme CSS")?;
    ensure!(
        theme_css.contains(".foundation-docs-header"),
        "generated theme CSS does not include the Foundation template"
    );

    let index = fs::read_to_string(doc_dir.join("index.html")).context("reading generated docs index")?;
    ensure!(
        index.contains("class=\"foundation-docs-header\""),
        "generated docs index does not include the Foundation header"
    );
    ensure!(
        index.contains("src=\"./foundation-assets/top-logo.webp\""),
        "generated docs index does not link the Foundation logo"
    );

    if let Some(entry) = crates.first() {
        let crate_index = fs::read_to_string(doc_dir.join(entry.crate_name).join("index.html"))
            .with_context(|| format!("reading generated {} crate index", entry.crate_name))?;
        ensure!(
            crate_index.contains("class=\"foundation-docs-header\""),
            "generated crate pages do not include the Foundation header"
        );
        ensure!(
            crate_index.contains("src=\"../foundation-assets/top-logo.webp\""),
            "generated crate pages do not link the Foundation logo"
        );
    }
    Ok(())
}

fn verify_crate_outputs(doc_dir: &Path, crates: &[CrateDoc]) -> Result<()> {
    let missing = crates
        .iter()
        .filter(|entry| !doc_dir.join(entry.crate_name).join("index.html").exists())
        .map(|entry| entry.crate_name)
        .collect::<Vec<_>>();
    ensure!(missing.is_empty(), "missing rustdoc crate output(s): {}", missing.join(", "));
    Ok(())
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(collect_paths(root)?.into_iter().filter(|path| path.is_file()).collect())
}

fn collect_paths(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }

    let mut paths = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).with_context(|| format!("reading {}", directory.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use app_manifest::MessageType;

    use super::*;

    fn message(
        permission_group: Option<&str>,
        required_signature: Option<RequiredSignature>,
        approval: ApprovalBehavior,
    ) -> Message {
        Message {
            id: 1,
            r#type: MessageType::BlockingScalar,
            description: None,
            cfg: None,
            permission_group: permission_group.map(str::to_owned),
            required_signature,
            required_type: None,
            approval,
        }
    }

    fn message_map(name: &str, message: Message) -> MessageMap {
        BTreeMap::from([(
            "settings".to_owned(),
            BTreeMap::from([(
                name.to_owned(),
                MessageDefinition { server: "os/settings".to_owned(), message },
            )]),
        )])
    }

    fn build_rustdoc_fixture() -> PathBuf {
        let output_dir = project_root().join("target/xtask-docs-api-rustdoc-test");
        match fs::remove_dir_all(&output_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("removing {}: {error}", output_dir.display()),
        }
        fs::create_dir_all(&output_dir).unwrap();

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/docs_api.rs");
        let rustdoc = env::var_os("RUSTDOC").unwrap_or_else(|| "rustdoc".into());
        let output = Command::new(rustdoc)
            .args(["--crate-name", "docs_api_fixture", "--edition", "2021", "--out-dir"])
            .arg(&output_dir)
            .arg(fixture)
            .output()
            .expect("running rustdoc fixture");
        assert!(
            output.status.success(),
            "rustdoc fixture failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output_dir
    }

    #[test]
    fn current_rustdoc_output_is_annotated() {
        let output_dir = build_rustdoc_fixture();
        let crate_dir = output_dir.join("docs_api_fixture");
        let messages = BTreeMap::from([(
            "docs_api_fixture".to_owned(),
            BTreeMap::from([(
                "GetThing".to_owned(),
                MessageDefinition {
                    server: "os/settings".to_owned(),
                    message: message(Some("settings.read"), None, ApprovalBehavior::AutoAllow),
                },
            )]),
        )]);
        let mut records = Vec::new();

        for file_name in ["struct.Api.html", "fn.read_free.html"] {
            let path = crate_dir.join(file_name);
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            let relative_path = path.strip_prefix(&output_dir).unwrap();
            let annotated =
                annotate_document(&text, "docs_api_fixture", relative_path, &messages, &mut records)
                    .unwrap_or_else(|| panic!("{} was not annotated", relative_path.display()));
            assert!(annotated.contains("keyos-permissions"));
            assert!(annotated.contains("settings.read"));
            assert!(annotated.contains("thirdParty"));
            assert!(annotated.contains("autoAllow"));
        }

        let items = records.iter().map(|record| record.item.as_str()).collect::<BTreeSet<_>>();
        assert!(items.contains("read"));
        assert!(items.contains("local"));
        assert!(items.contains("inherited"));
        assert!(items.contains("read_free"));
        assert!(!items.contains("plain"));
        assert!(!items.contains("drop"));
        assert!(records
            .iter()
            .filter(|record| ["read", "local", "read_free"].contains(&record.item.as_str()))
            .all(|record| record.permissions.first().is_some_and(|permission| {
                permission.message == "GetThing" && permission.status == PermissionStatus::Known
            })));

        let external_path = crate_dir.join("struct.ExternalDeref.html");
        let external_text = fs::read_to_string(&external_path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", external_path.display()));
        let external_relative = external_path.strip_prefix(&output_dir).unwrap();
        let initial_record_count = records.len();
        assert!(annotate_document(
            &external_text,
            "docs_api_fixture",
            external_relative,
            &messages,
            &mut records,
        )
        .is_none());
        assert_eq!(records.len(), initial_record_count);

        fs::remove_dir_all(output_dir).unwrap();
    }

    #[test]
    fn selects_all_crates_by_default() {
        assert_eq!(select_crates(&[]).unwrap().len(), CRATES.len());
    }

    #[test]
    fn rejects_unknown_packages() {
        let error = select_crates(&["nope".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("unknown API doc package(s): nope"));
    }

    #[test]
    fn ignores_duplicate_packages() {
        let crates = select_crates(&["settings".to_owned(), "settings".to_owned()]).unwrap();
        assert_eq!(crates.len(), 1);
        assert_eq!(crates[0].package, "settings");
    }

    #[test]
    fn rustdoc_flags_include_foundation_template() {
        let flags = compose_rustdoc_flags(
            "--cfg docsrs",
            Path::new("/repo/xtask/assets/docs-api/foundation.css"),
            Path::new("/repo/xtask/assets/docs-api/header.html"),
        );

        assert!(flags.starts_with("--cfg docsrs "));
        assert!(flags.contains("--default-theme light"));
        assert!(flags.contains("--extend-css /repo/xtask/assets/docs-api/foundation.css"));
        assert!(flags.contains("--html-before-content /repo/xtask/assets/docs-api/header.html"));
    }

    #[test]
    fn foundation_template_assets_are_local_and_packaged() {
        assert!(!FOUNDATION_CSS.contains("@import"));
        assert!(!FOUNDATION_CSS.contains("http://"));
        assert!(!FOUNDATION_CSS.contains("https://"));
        assert!(!FOUNDATION_CSS.contains("../foundation-assets/"));

        let root = project_root();
        let mut destinations = BTreeSet::new();
        for (source, destination) in TEMPLATE_ASSETS {
            assert!(root.join(source).exists(), "missing template source asset: {source}");
            assert!(
                destinations.insert((*destination).to_owned()),
                "duplicate template destination: {destination}"
            );
        }

        let mut referenced = BTreeSet::new();
        let css_url_re = Regex::new(r#"url\(\s*[\"']?([^\"')\s]+)[\"']?\s*\)"#).unwrap();
        for capture in css_url_re.captures_iter(FOUNDATION_CSS) {
            let url = capture.get(1).unwrap().as_str();
            let destination = url
                .strip_prefix("foundation-assets/")
                .unwrap_or_else(|| panic!("CSS asset URL is not local to foundation-assets: {url}"));
            assert!(destinations.contains(destination), "CSS asset is not packaged: {destination}");
            referenced.insert(destination.to_owned());
        }

        let header = Document::from(FOUNDATION_HEADER);
        for (selector, attribute) in [("img[src]", "src"), ("source[srcset]", "srcset")] {
            for element in header.select(selector).iter() {
                let value = element.attr(attribute).unwrap();
                for candidate in value.split(',') {
                    let url = candidate.split_whitespace().next().unwrap();
                    let destination = url
                        .strip_prefix("./")
                        .unwrap_or_else(|| panic!("header asset URL is not local: {url}"));
                    assert!(
                        destinations.contains(destination),
                        "header asset is not packaged: {destination}"
                    );
                    referenced.insert(destination.to_owned());
                }
            }
        }

        assert_eq!(referenced, destinations, "template assets and references differ");
    }

    #[test]
    fn template_asset_paths_follow_html_depth() {
        let root = Path::new("target/doc");
        assert_eq!(template_asset_root(root, &root.join("index.html")), "./");
        assert_eq!(template_asset_root(root, &root.join("settings/index.html")), "../");
        assert_eq!(template_asset_root(root, &root.join("settings/global/enum.SystemTheme.html")), "../../");
    }

    #[test]
    fn ungrouped_messages_show_effective_foundation_policy() {
        let messages = message_map("GetThing", message(None, None, ApprovalBehavior::NotUserGrantable));
        let permissions = resolve_permissions("settings", &["GetThing".to_owned()], &messages);

        assert_eq!(permissions[0].permission_group, None);
        assert_eq!(permissions[0].required_signature.as_deref(), Some("foundation"));
        assert_eq!(permissions[0].approval.as_deref(), Some("notUserGrantable"));
        assert!(render_permission_block(&permissions).contains("Permission group:</strong> none"));
    }

    #[test]
    fn unknown_messages_are_explicitly_marked() {
        let permissions = resolve_permissions("settings", &["Missing".to_owned()], &MessageMap::new());
        assert_eq!(permissions[0].status, PermissionStatus::UnknownServer);
        assert!(render_permission_block(&permissions).contains("MessageAllowed&lt;Missing&gt;"));
        assert!(serde_json::to_string(&permissions).unwrap().contains("unknown-server"));
    }

    #[test]
    fn permission_record_preserves_json_contract() {
        let record = permission_record(
            "settings",
            Path::new("settings/fn.read.html"),
            "function",
            "read",
            "fn.read",
            Vec::new(),
        );
        let value = serde_json::to_value(record).unwrap();

        assert_eq!(value["crate"], "settings");
        assert!(value.get("crate_name").is_none());
        assert_eq!(value["html_path"], "settings/fn.read.html");
    }

    #[test]
    fn html_values_are_escaped() {
        assert_eq!(escape_html("<&\"'>"), "&lt;&amp;&quot;&#39;&gt;");
    }
}
