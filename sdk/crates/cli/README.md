<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation CLI

`foundation` is the developer CLI for the Foundation SDK and KeyOS application workflows in this repository.

Today it covers:

- app scaffolding from shipped templates
- entering and cleaning up the SDK development shell
- hardware builds and signing
- simulator staging and launch
- Slint preview with generated router and translation preflight
- `plugin search`, `plugin install`, `plugin uninstall`, completions, and external plugin dispatch

## Source Of Truth

- Current behavior: [SPEC.md](SPEC.md)
- Historical design notes: [REQUIREMENTS.md](REQUIREMENTS.md)

## Workspace Crates

- `crates/foundation-core`: shared config, project discovery, SDK discovery, manifest types
- `crates/foundation-i18n`: localization and translated command names
- `crates/foundation-plugins`: plugin install, cache, discovery, and dispatch
- `crates/foundation-plugin-sdk`: support crate for plugin authors
- `crates/foundation-ui`: terminal UI helpers
- `crates/foundation-mcp`: MCP client helpers used by hardware workflows

## Development

Build the CLI workspace:

```bash
cargo build --manifest-path crates/cli/Cargo.toml
```

Run the CLI from source:

```bash
cargo run --manifest-path crates/cli/Cargo.toml -- new my-app
```

Run the CLI workspace tests, including command smoke coverage:

```bash
cargo test --manifest-path crates/cli/Cargo.toml --workspace
```
