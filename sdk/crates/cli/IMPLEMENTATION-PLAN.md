<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation CLI Implementation Plan

This plan has now largely been executed. It remains in the repo as a compact completion record plus a short deferred backlog.

## Completed

### Docs and source of truth

- [x] Keep `SPEC.md` as the canonical current-state CLI contract
- [x] Reduce `REQUIREMENTS.md` to an archival pointer plus roadmap themes
- [x] Update `README.md` to point at the canonical spec

### Shared config and manifest contract

- [x] Rename project config to `app-config.toml`
- [x] Make `foundation-core::AppConfig` the shared config model
- [x] Make `ProjectContext` discover and parse `app-config.toml`
- [x] Emit `manifest.json` from a shared manifest model
- [x] Include resolved KeyOS permissions in emitted manifests
- [x] Cover app ID lowercasing and launcher-name fallback with tests

### Templates and scaffolding

- [x] Switch scaffold defaults to `resources/icon.svg`
- [x] Add explicit template variables:
  - `sdk_root`
  - `sdk_keyos_root`
  - `sdk_ui_root`
- [x] Keep `sdk_path` as a compatibility alias
- [x] Update shipped templates to use the preferred `sdk_keyos_root` variable
- [x] Keep the other SDK path variables available for SDK/user template compatibility
- [x] Add deterministic scaffold smoke coverage

### Command behavior

- [x] Fix `preview` for default relative `ui/app.slint` paths
- [x] Keep `build`, `sim`, and `preview` on the shared config/codegen path
- [x] Add `sideload` to build, sign, upload over usb-debug, and optionally launch apps on hardware
- [x] Make `sim` build, stage, and launch the simulator
- [x] Make `new` initialize Git by default
- [x] Make `--no-git` skip repository initialization
- [x] Consolidate `view` into `preview`
- [x] Rename `undevelop` to `exit`
- [x] Group certificate operations under `cert` and generate a publisher certificate
- [x] Move plugin management under `foundation plugin ...`
- [x] Prefer Foundation tool names in help text and command messaging
- [x] Keep English commands stable even when localized aliases exist
- [x] Remove unreleased compatibility aliases for renamed commands

### Plugins

- [x] Dispatch unknown commands to installed external plugins
- [x] Normalize direct `owner/repo` plugin installs
- [x] Keep installs in `~/.foundation/plugins`
- [x] Include installed plugins in generated completions
- [x] Refactor plugin install logic so release fetching is mockable
- [x] Add a deterministic success-path install test

### Test coverage

- [x] Add deterministic workspace tests for:
  - `new`
  - `develop`
  - `exit`
  - `build`
  - `sideload`
  - `sim`
  - `cert`
  - `doctor`
  - `preview`
  - `plugin search`
  - `plugin install`
  - `plugin uninstall`
  - `completions`
  - external plugin dispatch

## Verification

Current verification command:

```bash
cargo test --manifest-path crates/cli/Cargo.toml --workspace
```

This includes unit coverage plus process-level CLI smoke tests for the built-in subcommands and plugin flows.

## Deferred Backlog

These items are intentionally deferred and are not part of the current completion target:

- possible future built-ins:
  - `sign`
  - `package`
  - `test`
  - `clean`
  - `add`
  - `screenshot`
- gate USB sideload/launch on explicit device settings once those settings are exposed through KeyOS APIs
- add end-to-end device or emulator coverage for the `!foundation ping` / `!foundation launch` USB control protocol
- ~~rename internal legacy module paths like `commands::genkey` and `commands::undevelop` once the external command transition is fully settled~~ (done — now `commands::cert` and `commands::exit`)
- a public command surface for `foundation-mcp`
- richer plugin metadata / `--describe`
- broader adoption of `foundation-ui` abstractions across the CLI
