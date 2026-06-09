<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation SDK Agent Guide

Before running or recommending `foundation` CLI commands, read `docs/foundation-cli.md`. It is the
agent-facing command guide; `crates/cli/SPEC.md` remains the internal implementation spec.

Use the SDK from the Nix flake:

```bash
nix develop
```

From that shell:

- Build SDK artifacts with `cargo xtask build --target all --release`
- Build and package a target in one step with `cargo xtask build --target aarch64-apple-darwin --release --package`
- Package release archives with `cargo xtask package --target all`
- Validate the pinned KeyOS and Slint inputs with `cargo xtask check-submodules`
- Scaffold a new app with `foundation new <name>`
- Inspect the environment with `foundation doctor`

For local developer builds against unpublished dependency checkouts, set `KEYOS_DIR=/path/to/KeyOS` and/or `SLINT_DIR=/path/to/slint` before running `cargo xtask build`. The Slint checkout must match the revision locked by the parent KeyOS workspace.
