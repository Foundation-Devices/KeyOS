<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation SDK Build Plan

This checklist tracks the remaining work to turn the current bootstrap into a usable SDK build and packaging flow. Items are checked off as they land in the repo.

## Current Status

- [x] Bootstrap the workspace, `xtask`, required flake, and release packaging skeleton.
- [x] Pin the public KeyOS submodule to `Foundation-Devices/KeyOS` tag `v1.2.0`.
- [x] Import the real `foundation` CLI crate into `crates/cli/` from its separate repo/folder.
- [x] Retire the CLI's standalone embedded Nix flake and point `foundation develop` at the SDK repo/bundle flake instead.
- [x] Rewire the CLI to resolve SDK paths from its new repo or bundle layout instead of `KEYOS_SDK_PATH`.

## Immediate Build-System Tasks

- [x] Align the active `sdk-build.toml` entries with the real `KeyOS v1.2.0` layout and stop relying on placeholder compile/copy paths.
- [x] Extend `xtask` compile metadata so a build entry can distinguish:
  - Cargo manifest path
  - Cargo package name
  - Built artifact name
  - Packaged SDK binary name
- [x] Add a layout validation command/check so missing compile/copy/doc sources fail before a long build starts.
- [x] Make the staged SDK manifest richer by recording build metadata, source origins, and resolved KeyOS commit information.
- [x] Tighten packaging so archive creation, checksums, and signing outputs are deterministic and easier to verify.

## Nix Tasks

- [x] Expand the flake beyond a single dev shell so the repo exposes clearer maintainer/user entrypoints.
- [x] Add a `nix flake check` path that exercises the Rust workspace and build configuration.
- [ ] Verify the flake exports the environment needed for Linux cross-compilation to `aarch64-unknown-linux-gnu`.

## SDK Layout Tasks

- [ ] Decide the shipped KeyOS source surface for app developers based on the actual `v1.2.0` tree.
- [ ] Lock the shipped API docs surface for the copied SDK crates; current builds only generate workspace `foundation-manifest` docs by default.
- [x] Replace the placeholder `message-iface` assumptions with the real staged KeyOS subtree layout under `lib/keyos/...`.
- [x] Stage the actual shared Slint UI source tree from KeyOS in the packaged SDK layout.
- [x] Vendor KeyOS `ui/ui` into the SDK root and keep the viewer plus `foundation build` aligned on that shared `@ui` import path.
- [x] Replace the legacy vendored `ui/ui` snapshot with a generated shared SDK public surface staged only in SDK artifacts.
- [x] Stage generated shared SDK assets into SDK artifacts and materialize them into app projects alongside `ui/ui`.
- [x] Run app `build.rs` Slint codegen before `foundation preview` and `foundation view` so generated `ui/gen/*` files exist for routed and localized apps.
- [x] Keep templates, examples, docs, and `AGENTS.md` wired into the packaged SDK bundle.
- [ ] Consolidate the template source of truth (`crates/cli/templates/` vs repo `templates/`) and make sure the intended template set is what ships in `lib/templates/`.
- [ ] Curate the shipped `examples/` tree so it contains complete SDK-usable examples, or narrow the docs to the minimal examples that actually ship today.

## Verification Tasks

- [x] Add automated Rust tests for the local config parser and core build/package helpers.
- [x] Add a smoke test path that validates `cargo xtask check-submodules`, source layout checks, and packaging preconditions.
- [ ] Verify a full release-style bundle build (without `--skip-docs` / `--skip-simulator`) contains all expected binaries and assets: `foundation`, `foundation-asset-tool`, `foundation-simulator`, `foundation-slint-viewer`, `foundation-passport-drive`, `cosign2`, docs, `AGENTS.md`, templates, examples, `flake.nix`, `flake.lock`, `setup.sh`, and release-level `install.sh`.
- [x] Validate a host-native non-CLI smoke build path (`cargo xtask build --target aarch64-apple-darwin --skip-docs --skip-simulator`).
- [ ] Run a Linux-host smoke build once the CLI crate is available.
- [x] Run a macOS-host smoke build once the CLI crate is available.

## Release Flow Tasks

- [ ] Decide whether `docs/guide/` in release bundles must be rendered mdBook output or copied markdown source, and add `docs/book.toml` if rendered output is required.
- [ ] Document the exact manual release checklist for Linux and macOS hosts.
- [ ] Verify that a release bundle can be installed and used via `foundation develop` without a repo checkout.
- [ ] Verify the curl-based installer flow (`install.sh`) against a real release-style asset host and checksum file.
- [ ] Verify signing-key handling for manual release builds.

## Current Blockers

- [x] Pin the Foundation Slint fork separately and build the customized `foundation-slint-viewer` from the resolved Slint workspace for every SDK target triple.
