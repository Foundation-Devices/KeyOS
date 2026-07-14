// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use app_manifest::{ApiManifest, Manifest};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "keyos-dep-graph")]
#[command(about = "Generate IPC dependency graphs for KeyOS modules")]
struct Args {
    #[command(subcommand)]
    command: Option<CliCommand>,

    /// Comma-separated list of module paths or server names to analyze
    #[arg(long = "servers", short = 's', value_name = "MODULE", value_delimiter = ',')]
    servers: Vec<String>,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Generate DEPENDENCIES.md for a module, or every module with "all"
    Generate {
        /// Module path, server name, or "all"
        module: String,
    },
    /// Render a module's DEPENDENCIES.md to DEPENDENCIES.png
    Draw {
        /// Module path or server name
        module: String,
        /// Render the PNG without opening an image viewer
        #[arg(long)]
        no_open: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModuleKind {
    Api,
    App,
    Os,
}

#[derive(Debug, Clone)]
struct Module {
    path: String,
    kind: ModuleKind,
    servers: BTreeSet<String>,
    permissions: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug)]
struct ModuleIndex {
    modules: BTreeMap<String, Module>,
    server_owners: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Dependency {
    from: String,
    to: String,
    server: String,
    messages: BTreeSet<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure_workspace_root()?;

    let index = ModuleIndex::load()?;
    match args.command {
        Some(CliCommand::Generate { module }) => generate_dependencies(&index, &module),
        Some(CliCommand::Draw { module, no_open }) => draw_dependencies(&index, &module, no_open),
        None => print_dependencies(&index, &args.servers),
    }
}

fn ensure_workspace_root() -> Result<()> {
    if !Path::new("Cargo.toml").exists()
        || !Path::new("api").exists()
        || !Path::new("os").exists()
        || !Path::new("apps").exists()
    {
        bail!("This tool must be run from the KeyOS workspace root directory");
    }

    Ok(())
}

fn print_dependencies(index: &ModuleIndex, modules: &[String]) -> Result<()> {
    if modules.is_empty() {
        bail!("No modules specified. Pass --servers or use the generate/draw subcommands");
    }

    print!("{}", mermaid_graph_for_modules(index, modules)?);
    Ok(())
}

fn mermaid_graph_for_modules(index: &ModuleIndex, modules: &[String]) -> Result<String> {
    let targets =
        modules.iter().map(|module| index.resolve_target(module)).collect::<Result<BTreeSet<_>>>()?;

    let mut dependencies = BTreeSet::new();
    for target in &targets {
        index.collect_direct_dependencies(target, &mut dependencies)?;
    }
    index.collect_reverse_dependencies(&targets, &mut dependencies)?;

    Ok(render_mermaid_graph(&dependencies, index, &targets))
}

fn generate_dependencies(index: &ModuleIndex, module: &str) -> Result<()> {
    if module == "all" {
        return generate_all_dependencies(index);
    }

    let target = index.resolve_target(module)?;
    let output_path = dependencies_markdown_path(&target);
    let graph = mermaid_graph_for_modules(index, std::slice::from_ref(&target))?;
    fs::write(&output_path, graph).with_context(|| format!("Failed to write {}", output_path.display()))?;

    println!("Dependency graph generated successfully: {}", output_path.display());
    Ok(())
}

fn generate_all_dependencies(index: &ModuleIndex) -> Result<()> {
    println!("Generating dependency graphs for all modules...");
    println!("Found {} modules:", index.modules.len());
    for module_path in index.modules.keys() {
        println!("  {module_path}");
    }
    println!();

    for module_path in index.modules.keys() {
        let output_path = dependencies_markdown_path(module_path);
        let graph = mermaid_graph_for_modules(index, std::slice::from_ref(module_path))?;
        fs::write(&output_path, graph)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;
        println!("{module_path}: generated {}", output_path.display());
    }

    println!();
    println!("Generated {} dependency graphs", index.modules.len());
    Ok(())
}

fn draw_dependencies(index: &ModuleIndex, module: &str, no_open: bool) -> Result<()> {
    let target = index.resolve_target(module)?;
    let dependencies_file = dependencies_markdown_path(&target);
    if !dependencies_file.exists() {
        bail!(
            "DEPENDENCIES.md file not found at {}. Run 'just generate-dep-graph {target}' first.",
            dependencies_file.display()
        );
    }

    let output_png = dependencies_png_path(&target);
    run_mmdc(&dependencies_file, &output_png)?;
    println!("Graph saved to: {}", output_png.display());

    if !no_open {
        open_image(&output_png);
    }

    Ok(())
}

fn dependencies_markdown_path(module_path: &str) -> PathBuf { Path::new(module_path).join("DEPENDENCIES.md") }

fn dependencies_png_path(module_path: &str) -> PathBuf { Path::new(module_path).join("DEPENDENCIES.png") }

fn run_mmdc(input: &Path, output: &Path) -> Result<()> {
    let markdown =
        fs::read_to_string(input).with_context(|| format!("Failed to read {}", input.display()))?;
    let mermaid_source = extract_mermaid_source(&markdown)
        .with_context(|| format!("Failed to extract Mermaid diagram from {}", input.display()))?;
    let temp_input = temp_mermaid_path();
    fs::write(&temp_input, mermaid_source)
        .with_context(|| format!("Failed to write {}", temp_input.display()))?;

    let status = Command::new("mmdc")
        .arg("-i")
        .arg(&temp_input)
        .arg("-o")
        .arg(output)
        .arg("-t")
        .arg("neutral")
        .arg("-b")
        .arg("transparent")
        .status();

    let _ = fs::remove_file(&temp_input);

    let status = match status {
        Ok(status) => status,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            bail!("mmdc command not found. Please install mermaid-cli.");
        }
        Err(err) => return Err(err).context("Failed to launch mmdc"),
    };

    if !status.success() {
        bail!("Failed to render Mermaid diagram from {}", input.display());
    }

    Ok(())
}

fn extract_mermaid_source(markdown: &str) -> Result<&str> {
    let Some((_, after_start)) = markdown.split_once("```mermaid") else {
        bail!("No Mermaid code fence found");
    };
    let Some((source, _)) = after_start.split_once("```") else {
        bail!("Mermaid code fence is not closed");
    };

    Ok(source.trim())
}

fn temp_mermaid_path() -> PathBuf {
    let timestamp =
        SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or(0);
    env::temp_dir().join(format!("keyos-dep-graph-{}-{timestamp}.mmd", std::process::id()))
}

fn open_image(path: &Path) {
    for command in ["imv", "xdg-open"] {
        match Command::new(command).arg(path).spawn() {
            Ok(_) => return,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                eprintln!("Failed to launch {command}: {err}");
                return;
            }
        }
    }
}

impl ModuleIndex {
    fn load() -> Result<Self> {
        let mut modules = BTreeMap::new();
        for root in ["api", "os", "apps"] {
            let root_path = Path::new(root);
            if root_path.exists() {
                find_modules(root_path, &mut modules)?;
            }
        }

        let mut index = Self { modules, server_owners: BTreeMap::new() };
        index.build_server_owners();
        Ok(index)
    }

    fn build_server_owners(&mut self) {
        for module in self.modules.values() {
            for server in &module.servers {
                match self.server_owners.get(server) {
                    None => {
                        self.server_owners.insert(server.clone(), module.path.clone());
                    }
                    Some(existing_path) => {
                        let existing_kind = self.modules.get(existing_path).map(|module| module.kind);
                        if existing_kind == Some(ModuleKind::Api) && module.kind != ModuleKind::Api {
                            self.server_owners.insert(server.clone(), module.path.clone());
                        }
                    }
                }
            }
        }
    }

    fn resolve_target(&self, requested: &str) -> Result<String> {
        if self.modules.contains_key(requested) {
            return Ok(requested.to_owned());
        }

        if let Some(owner) = self.server_owners.get(requested) {
            return Ok(owner.clone());
        }

        if requested.contains('/') {
            bail!("Module or server '{requested}' not found under api/, os/, or apps/");
        }

        for prefix in ["os", "apps", "api"] {
            let candidate = format!("{prefix}/{requested}");
            if self.modules.contains_key(&candidate) {
                return Ok(candidate);
            }
        }

        let server_candidate = format!("os/{requested}");
        if let Some(owner) = self.server_owners.get(&server_candidate) {
            return Ok(owner.clone());
        }

        let matches: Vec<&Module> = self
            .modules
            .values()
            .filter(|module| module.path.rsplit('/').next() == Some(requested))
            .collect();
        let runtime_matches: Vec<&Module> =
            matches.iter().copied().filter(|module| module.kind != ModuleKind::Api).collect();

        match runtime_matches.as_slice() {
            [module] => return Ok(module.path.clone()),
            [] => {
                if let [module] = matches.as_slice() {
                    return Ok(module.path.clone());
                }
            }
            _ => {}
        }

        if matches.len() > 1 {
            let candidates = matches.iter().map(|module| module.path.as_str()).collect::<Vec<_>>().join(", ");
            bail!("Module name '{requested}' is ambiguous. Use one of: {candidates}");
        }

        bail!("Module or server '{requested}' not found under api/, os/, or apps/");
    }

    fn collect_direct_dependencies(
        &self,
        module_path: &str,
        dependencies: &mut BTreeSet<Dependency>,
    ) -> Result<()> {
        let module =
            self.modules.get(module_path).with_context(|| format!("Module '{module_path}' not found"))?;

        for (server, messages) in &module.permissions {
            if server == "template" {
                continue;
            }

            dependencies.insert(Dependency {
                from: module.path.clone(),
                to: self.dependency_target(server),
                server: server.clone(),
                messages: messages.clone(),
            });
        }

        Ok(())
    }

    fn collect_reverse_dependencies(
        &self,
        targets: &BTreeSet<String>,
        dependencies: &mut BTreeSet<Dependency>,
    ) -> Result<()> {
        for module in self.modules.values() {
            if targets.contains(&module.path) {
                continue;
            }

            for (server, messages) in &module.permissions {
                if server == "template" {
                    continue;
                }

                for target in targets {
                    if self.permission_matches_target(server, target)? {
                        dependencies.insert(Dependency {
                            from: module.path.clone(),
                            to: target.clone(),
                            server: server.clone(),
                            messages: messages.clone(),
                        });
                    }
                }
            }
        }

        Ok(())
    }

    fn dependency_target(&self, server: &str) -> String {
        self.server_owners.get(server).cloned().unwrap_or_else(|| server.to_owned())
    }

    fn permission_matches_target(&self, server: &str, target_path: &str) -> Result<bool> {
        let target = self
            .modules
            .get(target_path)
            .with_context(|| format!("Target module '{target_path}' not found"))?;

        Ok(target.servers.contains(server)
            || self.server_owners.get(server).is_some_and(|owner| owner == target_path))
    }

    fn module_kind(&self, node: &str) -> Option<ModuleKind> {
        self.modules.get(node).map(|module| module.kind)
    }
}

fn find_modules(dir: &Path, modules: &mut BTreeMap<String, Module>) -> Result<()> {
    let cargo_toml = dir.join("Cargo.toml");
    let manifest_toml = dir.join("manifest.toml");
    if cargo_toml.exists() && manifest_toml.exists() {
        let module = load_module(dir)?;
        modules.insert(module.path.clone(), module);
    }

    for entry in fs::read_dir(dir).with_context(|| format!("Failed to read directory {}", dir.display()))? {
        let entry = entry.with_context(|| format!("Failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            find_modules(&path, modules)?;
        }
    }

    Ok(())
}

fn load_module(crate_dir: &Path) -> Result<Module> {
    let path = module_path(crate_dir)?;
    let kind = module_kind_from_path(&path)?;

    let (servers, permissions) = match kind {
        ModuleKind::Api => {
            let manifest = ApiManifest::load_with_tracking(crate_dir, |_| {});
            (manifest.servers.into_keys().collect(), BTreeMap::new())
        }
        ModuleKind::App | ModuleKind::Os => {
            let manifest = Manifest::load_with_tracking(crate_dir, Path::new("."), |_| {});
            (manifest.servers.into_keys().collect(), manifest.permissions)
        }
    };

    Ok(Module { path, kind, servers, permissions })
}

fn module_path(crate_dir: &Path) -> Result<String> {
    let path = crate_dir
        .strip_prefix(".")
        .unwrap_or(crate_dir)
        .to_str()
        .ok_or_else(|| anyhow!("Module path is not valid UTF-8: {}", crate_dir.display()))?;

    Ok(path.replace('\\', "/"))
}

fn module_kind_from_path(path: &str) -> Result<ModuleKind> {
    if path.starts_with("api/") {
        Ok(ModuleKind::Api)
    } else if path.starts_with("apps/") {
        Ok(ModuleKind::App)
    } else if path.starts_with("os/") {
        Ok(ModuleKind::Os)
    } else {
        bail!("Unsupported module path '{path}'");
    }
}

fn render_mermaid_graph(
    dependencies: &BTreeSet<Dependency>,
    index: &ModuleIndex,
    target_modules: &BTreeSet<String>,
) -> String {
    use std::fmt::Write as _;

    let mut graph = String::new();
    // REUSE-IgnoreStart -- SPDX header emitted into the generated markdown, not this file's license
    writeln!(
        &mut graph,
        "<!-- SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz> -->"
    )
    .unwrap();
    writeln!(&mut graph, "<!-- SPDX-License-Identifier: GPL-3.0-or-later -->").unwrap();
    // REUSE-IgnoreEnd
    writeln!(&mut graph).unwrap();
    writeln!(&mut graph, "```mermaid").unwrap();
    writeln!(&mut graph, "stateDiagram-v2").unwrap();

    let edges = collect_edges(dependencies);
    let nodes = collect_nodes(&edges, target_modules);
    let node_ids = build_node_ids(&nodes);

    for node in &nodes {
        writeln!(&mut graph, "  state \"{}\" as {}", escape_mermaid_label(node), node_ids[node]).unwrap();
    }

    for ((from, to), server_messages) in &edges {
        let label = edge_label(to, server_messages, index);
        writeln!(&mut graph, "  {} --> {}: {}", node_ids[from], node_ids[to], label).unwrap();
    }

    writeln!(
        &mut graph,
        "  classDef Api stroke-width:1px,stroke-dasharray:none,stroke:#7B61FF,fill:#F1EEFF,color:#34215F;"
    )
    .unwrap();
    writeln!(
        &mut graph,
        "  classDef App stroke-width:1px,stroke-dasharray:none,stroke:#FBB35A,fill:#FFEFDB,color:#8F632D;"
    )
    .unwrap();
    writeln!(
        &mut graph,
        "  classDef Os stroke-width:1px,stroke-dasharray:none,stroke:#4A90E2,fill:#E6F3FF,color:#2C5282;"
    )
    .unwrap();
    writeln!(
        &mut graph,
        "  classDef External stroke-width:1px,stroke-dasharray:3 3,stroke:#8A8F98,fill:#F4F5F7,color:#3F444D;"
    )
    .unwrap();

    write_class(&mut graph, "Api", ModuleKind::Api, &nodes, &node_ids, index);
    write_class(&mut graph, "App", ModuleKind::App, &nodes, &node_ids, index);
    write_class(&mut graph, "Os", ModuleKind::Os, &nodes, &node_ids, index);

    let external_nodes: Vec<&str> =
        nodes.iter().map(String::as_str).filter(|node| index.module_kind(node).is_none()).collect();
    if !external_nodes.is_empty() {
        let ids = external_nodes.iter().map(|node| node_ids[*node].as_str()).collect::<Vec<_>>().join(",");
        writeln!(&mut graph, "  class {ids} External").unwrap();
    }

    writeln!(&mut graph, "```").unwrap();
    graph
}

type EdgeMap = BTreeMap<(String, String), BTreeMap<String, BTreeSet<String>>>;

fn collect_edges(dependencies: &BTreeSet<Dependency>) -> EdgeMap {
    let mut edges: EdgeMap = BTreeMap::new();
    for dependency in dependencies {
        edges
            .entry((dependency.from.clone(), dependency.to.clone()))
            .or_default()
            .entry(dependency.server.clone())
            .or_default()
            .extend(dependency.messages.iter().cloned());
    }
    edges
}

fn collect_nodes(edges: &EdgeMap, target_modules: &BTreeSet<String>) -> BTreeSet<String> {
    let mut nodes = target_modules.clone();
    for (from, to) in edges.keys() {
        nodes.insert(from.clone());
        nodes.insert(to.clone());
    }
    nodes
}

fn build_node_ids(nodes: &BTreeSet<String>) -> BTreeMap<String, String> {
    let mut ids = BTreeMap::new();
    let mut used = BTreeMap::<String, usize>::new();

    for node in nodes {
        let base = sanitize_node_id(node);
        let counter = used.entry(base.clone()).or_default();
        *counter += 1;

        let id = if *counter == 1 { base } else { format!("{base}_{}", counter) };
        ids.insert(node.clone(), id);
    }

    ids
}

fn sanitize_node_id(node: &str) -> String {
    let mut id = String::new();
    for ch in node.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch);
        } else {
            id.push('_');
        }
    }

    if id.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        id.insert(0, '_');
    }

    id
}

fn edge_label(
    target: &str,
    server_messages: &BTreeMap<String, BTreeSet<String>>,
    index: &ModuleIndex,
) -> String {
    let include_server_names = should_include_server_names(target, server_messages, index);
    let mut lines = Vec::new();

    for (server, messages) in server_messages {
        if include_server_names {
            lines.push(server.clone());
        }

        if messages.is_empty() {
            lines.push("(all)".to_owned());
        } else {
            lines.extend(messages.iter().cloned());
        }
    }

    lines.join("<br>")
}

fn should_include_server_names(
    target: &str,
    server_messages: &BTreeMap<String, BTreeSet<String>>,
    index: &ModuleIndex,
) -> bool {
    if server_messages.len() > 1 {
        return true;
    }

    let Some((server, _)) = server_messages.first_key_value() else {
        return false;
    };

    match index.modules.get(target) {
        Some(module) => module.servers.len() != 1 || module.path != *server,
        None => true,
    }
}

fn write_class(
    graph: &mut String,
    class_name: &str,
    kind: ModuleKind,
    nodes: &BTreeSet<String>,
    node_ids: &BTreeMap<String, String>,
    index: &ModuleIndex,
) {
    let ids: Vec<&str> = nodes
        .iter()
        .filter(|node| index.module_kind(node) == Some(kind))
        .map(|node| node_ids[node].as_str())
        .collect();

    if !ids.is_empty() {
        use std::fmt::Write as _;

        writeln!(graph, "  class {} {class_name}", ids.join(",")).unwrap();
    }
}

fn escape_mermaid_label(label: &str) -> String { label.replace('"', "\\\"") }
