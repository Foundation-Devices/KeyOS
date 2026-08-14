<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation SDK Architecture

## Overview

The Foundation SDK is a set of tools for developers to create custom apps that run on Foundation Passport Prime and future devices (e.g., Passport Pulse). Developers use a single CLI executable (`foundation`) to create, build, sign, preview, and simulate apps regardless of target platform.

All supported workflows are Nix-first. Foundation engineers build the SDK from a maintainer flake in the repo, while SDK users install the SDK and build apps from a separately maintained SDK-user flake that ships in the release bundle. Both entrypoints use `nix develop`, but they intentionally expose different toolsets. The SDK is not documented as a standalone non-Nix install.

## Repository Structure

Monorepo using a Cargo workspace plus a vendored CLI subtree. Separate maintainer and SDK-user flakes are required for SDK builds, SDK installation, app development, and cross-compilation.

```
KeyOS-dev/
├── Cargo.toml                  # monorepo workspace root (KeyOS source tree)
├── ui/                         # legacy monorepo UI used by Foundation-authored apps
├── ui2/                        # internal shared UI source tree
│   ├── Cargo.toml
│   ├── components/
│   ├── resources/
│   └── theme-editor/
└── sdk/                        # imported Foundation SDK workspace snapshot
    ├── Cargo.toml
    ├── flake.nix
    ├── sdk-build.toml
    ├── crates/
    ├── crates/cli/
    ├── docs/
    ├── examples/
    └── xtask/
```

### External Dependencies

- **KeyOS monorepo source** — in repo mode the SDK treats `sdk/..` as the KeyOS source root. `sdk-build.toml`, `xtask`, and `SdkRoot::keyos_root()` all resolve compile/copy/runtime inputs from the parent `KeyOS-dev` checkout instead of an `external/keyos` submodule.
- **Foundation Slint fork** — resolved by the maintainer build from `SLINT_DIR` or from the pinned Nix-fetched Foundation Slint source. The customized `foundation-slint-viewer` binary is built from `tools/viewer` in that checkout, and `xtask` verifies that the resolved Slint source matches the revision locked by the parent KeyOS workspace. See [Build System Specification](./sdk-build-system.md) for pinning and override details.
- **Shared `@ui` library** — the SDK's public `@ui` surface is generated into packaged `ui/ui` from `../ui2/components/ui` during artifact staging. Shared assets are generated into packaged `resources` from `../ui2/resources`. These generated paths are the supported SDK surface for app developers and must not be edited by hand.
- **Generated app-local Slint artifacts** — routed or localized apps still generate `ui/gen/` files such as `router.slint`, `navigate.slint`, `tr.slint`, and `exports.slint` from their own `build.rs`. The SDK's `foundation build`, `foundation sim`, `foundation preview`, and `foundation view` commands are responsible for running that existing codegen before compiling or opening the viewer.

## Why Monorepo

- The CLI orchestrates across the simulator, builder, signing, manifest handling, Slint components, and QuantumLink transport. A single commit can update all of these atomically.
- Submodules are fragile (stale refs, automation complexity) and only make sense for external dependencies you don't control.
- Subtrees add merge overhead and make atomic cross-component changes painful.
- Nix + Cargo workspace gives SDK maintainers and SDK users reproducible, role-specific environments across local development, verification builds, and release bundles.

## Required Nix Workflow

- Nix is required for building the SDK, installing the SDK, and building apps with the SDK.
- The repo maintainer flake is the supported entrypoint for SDK build and release workflows (`cargo xtask`, docs generation, packaging).
- Release bundles include a separately maintained SDK-user `flake.nix` and `flake.lock` so unpacked SDKs can be entered through `foundation develop` without carrying maintainer-only tooling.
- `setup.sh` is only a convenience script for verification and onboarding; the supported user-facing runtime entrypoint is `foundation develop`.

## Build Targets

Four platform targets, built from two host environments. Windows WSL runs Linux userspace, so WSL targets are identical to their Linux counterparts — no separate Windows binaries needed.

| Platform          | Rust Triple                 | Build Host | Method                    |
| ----------------- | --------------------------- | ---------- | ------------------------- |
| macOS ARM64       | `aarch64-apple-darwin`      | macOS      | Nix shell (native)        |
| macOS AMD64       | `x86_64-apple-darwin`       | macOS      | Nix shell (native/cross)  |
| Linux AMD64 / WSL | `x86_64-unknown-linux-gnu`  | Linux      | Nix shell (native)        |
| Linux ARM64 / WSL | `aarch64-unknown-linux-gnu` | Linux      | Nix shell (cross)         |

### Notes

- All compilation is driven from flake-provided toolchains, linkers, sysroots, and docs tooling.
- The simulator uses Slint's **software renderer**, which eliminates platform windowing/GPU dependencies and makes cross-compilation viable from Linux.
- macOS targets must be built on a macOS host because the Apple SDK/Xcode toolchain is only available through Apple's licensed macOS environment.
- Build orchestration is handled by a Rust `xtask` binary. See [Build System Specification](./sdk-build-system.md) for full details.

## SDK Distribution Package

The build pipeline produces a self-contained, downloadable package per target triple. Each package also carries the flake required to use the SDK after unpacking.

```
foundation-sdk-v1.0.0-macos-arm64/
├── flake.nix                   # required developer entrypoint after unpacking
├── flake.lock
├── bin/
│   ├── foundation              # CLI binary
│   ├── foundation-asset-tool   # internal raw asset conversion helper
│   ├── foundation-simulator    # simulator launcher script
│   ├── foundation-theme-editor # visual app theme editor
│   ├── foundation-slint-viewer # customized Slint viewer with Foundation support
│   ├── foundation-passport-drive # USB/MCP helper
│   └── cosign2                 # signing/packaging helper
├── lib/
│   ├── keyos/                  # preserved KeyOS source subtree for app builds
│   │   ├── api/                  # selected server interfaces from sdk-build.toml
│   │   ├── i18n/
│   │   ├── server/
│   │   ├── slint-keyos-platform/
│   │   ├── simulator/            # hosted simulator runtime (kernel, services, staged apps dir)
│   │   ├── ui/
│   │   ├── worker/
│   │   └── xous/
│   └── templates/              # `foundation new` scaffolding
├── ui/
│   └── ui/                     # generated public @ui surface
├── resources/                  # generated shared SDK assets referenced by the public @ui surface
├── docs/
│   ├── guide/                  # rendered mdBook output when configured, otherwise copied guide source
│   ├── api/                    # generated API docs for configured crates (currently workspace docs first)
│   └── AGENTS.md               # AI-assisted workflow guide
├── examples/
│   └── hello-world/            # current example source tree
├── manifest.toml               # SDK metadata (version, API compat, checksums)
└── setup.sh                    # verifies Nix is installed and prints `foundation develop` onboarding
```

### Design Rationale

- **Core binaries are pre-compiled** per platform (`foundation`, `foundation-asset-tool`, `foundation-theme-editor`, `foundation-slint-viewer`, `foundation-passport-drive`, `cosign2`), and the simulator ships as a launcher script plus a prebuilt hosted KeyOS runtime under `lib/keyos/simulator/`.
- **Source crates ship as source** via a curated `lib/keyos/` subtree so KeyOS path dependencies continue to resolve when developers compile apps against the SDK without exposing the full upstream tree.
- **Shared Slint UI sources ship at the package root** via `ui/ui/`, and their shared assets ship at `resources/`, so both the bundled viewer and app builds can resolve the generated shared SDK `@ui/...` imports without any extra per-project configuration.
- **Preview and view stay project-aware** by running the app's existing Slint codegen before opening the viewer, so apps that import `gen/router.slint` or `gen/exports.slint` work without a manual pre-build step.
- **Release bundles ship a dedicated SDK-user flake** (`flake.nix` + `flake.lock`) so app developers get the runtime toolchain they need without inheriting the SDK maintainer shell.
- Similar model to the Android NDK: prebuilt toolchain + source libraries.

### How Developers Use the SDK

SDK users install or unpack a release bundle, add the stable SDK launcher directory to `PATH`, and enter the SDK shell with `foundation develop`. Maintainers use the repo's separate flake when building or packaging the SDK itself.

```bash
curl -fsSL https://.../install.sh | sh
source ~/.zshrc  # or ~/.bashrc
foundation develop
foundation new hello-world
```

### How Developers Consume Source Crates

`foundation new` scaffolds a project with path dependencies pointing to the SDK install:

```toml
[dependencies]
gui-server-api = { path = "${FOUNDATION_SDK}/lib/keyos/api/gui-server" }
slint-keyos-platform = { path = "${FOUNDATION_SDK}/lib/keyos/slint-keyos-platform/runtime" }
```

The CLI resolves the SDK root relative to its installed location via `std::env::current_exe()`, so it can work against both the repo layout and the packaged SDK bundle layout.

## Release Build Pipeline

```
1. Enter the native or cross Nix shell for the target triple
2. Run `cargo xtask build --target <TRIPLE> --release --package`, which stages `ui/ui` and `resources` from the internal `../ui2` source tree
3. Compile `foundation`, `foundation-asset-tool`, `foundation-theme-editor`, `foundation-slint-viewer`, `foundation-passport-drive`, and `cosign2`, then stage the hosted KeyOS simulator runtime and generate the `foundation-simulator` launcher
4. Copy the curated KeyOS source subtree, the generated `ui/ui/` public SDK UI surface, the generated `resources/` asset tree, templates, examples, and static developer docs such as `AGENTS.md`
5. Build docs (`cargo doc`, plus `mdbook` when a guide book is configured; otherwise copy the guide source tree)
6. Copy the SDK-user `flake.nix`, SDK-user `flake.lock`, and `setup.sh` into the package root
7. Generate `manifest.toml` with version, checksums, and API compat info
8. Package as `.tar.gz` for all target triples (WSL users consume the matching Linux archive)
9. Generate a release-level `install.sh` that installs the right `.tar.gz` asset, updates the user's shell `PATH`, and points them at `foundation develop`
10. Sign the archive with Foundation's release key when requested
11. Publish to developer portal + GitHub releases
```

## Optional Verification

Release artifacts are built manually. If Foundation wants automated verification, it can run the same `nix develop --command cargo xtask ...` entrypoints on one macOS machine and one Linux machine, but that automation is only a confidence check and not part of the required release process.

## Versioning

- **Workspace-level version** for the SDK release — CLI, simulator, and components ship together.
- **Individual crate semver** for anything published to crates.io or consumed as a library.
- `manifest.toml` in each SDK package declares compatible KeyOS API versions. `foundation build` checks this against the app's target and warns on mismatches.

## App Manifest

Each app includes a manifest defining:

- App name
- App ID
- Permissions
- Icon
- Target API version

## Side-Loading

- Drop the signed app package into the Passport airlock.
- First time: Passport prompts to install. The developer's public key must also be installed on the device.
- Subsequent updates: if the same App ID is already installed, it is replaced automatically with a toast notification.

## Feature Roadmap (Not All Required for v1.0)

### CLI

- `foundation new` — scaffold a new app from template
- `foundation build` — compile, sign, and package the app
- `foundation sign` — sign an existing build
- `foundation sim` — build and run the current app in the simulator
- `foundation preview` — preview UI in the bundled Slint viewer
- `foundation genkey` — generate a signing keypair for a developer key
- `foundation doctor` — verify Nix, SDK install, targets, and device prerequisites
- `foundation completions` — install or generate shell completions

### Simulator

- Cross-platform (macOS, Linux, Windows via WSL)
- NFC simulation (files + USB card reader)
- Bluetooth to Envoy
- Launch to specific view/app
- Set date, time, time zone
- PC keyboard as virtual keyboard
- Scriptable touch/keyboard automation API

### Developer Portal

- Account creation and profile management
- App management (add, edit, remove)
- App submission and approval workflow

### Developer Experience

- `AGENTS.md` reference guide for AI-assisted app development, simulator screenshots, and log capture
- Slint preview in VSCode
- Slint preview CLI
- Localization support in apps and builder tooling
- Unit testing support
- Debugging (on device + simulator)
- Crash reports
- Logging to PC
- Build verification helpers (Nix-based)

### KeyOS Improvements

- Toast mechanism
- Lock screen notifications
- Notification Center in Control Center pulldown
- App update notifications

### Slint Components

- Themeable component library
- Custom fonts, colors, corner radii, outlines, gradients

### QuantumLink

- Transport abstraction
- QL addressing
- TCP transport
- Protocol message definitions
