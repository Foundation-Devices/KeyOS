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
- shell completion generation and installation

The CLI plugin implementation is retained for development behind the
default-off `experimental-plugins` Cargo feature. Supported SDK builds do not
include plugin commands, discovery, installation, or external dispatch while
the plugin trust model is being redesigned.

## Source Of Truth

- Current behavior: [SPEC.md](SPEC.md)
- Historical design notes: [REQUIREMENTS.md](REQUIREMENTS.md)

## Workspace Crates

- `crates/foundation-core`: shared config, project discovery, SDK discovery, manifest types
- `crates/foundation-i18n`: localization and translated command names
- `crates/foundation-plugins`: quarantined plugin install, cache, discovery, and dispatch implementation
- `crates/foundation-plugin-sdk`: quarantined support crate for plugin authors
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

Maintainers can also verify the quarantined implementation without enabling it
in supported SDK builds:

```bash
cargo test --manifest-path crates/cli/Cargo.toml --package foundation --features experimental-plugins
```
