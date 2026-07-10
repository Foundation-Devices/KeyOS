<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation SDK — Build System Specification

## Overview

The Foundation SDK build system compiles the SDK distribution package (CLI, simulator launcher + hosted runtime, source crates, docs) for all supported platforms. It is implemented as a Rust binary (`xtask`) in the workspace, invoked via `cargo xtask`. This replaces ad-hoc shell scripts with a typed, cross-platform build orchestrator.

All supported workflows are Nix-first. The repo uses a maintainer flake for building and packaging the SDK itself, and each release bundle includes a separately maintained SDK-user flake so app developers install the SDK and build apps from `nix develop` without inheriting maintainer-only tools.

## Nix Flake Responsibilities

The SDK ships two distinct flake entrypoints, each maintained in its own source file. Together they are a required part of the SDK surface area, not optional tooling:

- The repo maintainer flake must provide:
  - a maintainer environment for `cargo xtask`, docs generation, signing, and release packaging
  - host-native and cross-compilation toolchains, linkers, sysroots, and target-specific environment variables
  - access to source-only inputs such as submodules and build orchestration config
- The SDK-user flake must provide:
  - an app-developer environment for `foundation`, `foundation-simulator`, `cosign2`, and related tools
  - access to the packaged SDK layout (`bin/`, `lib/`, `docs/`, `examples/`, `manifest.toml`) so app developers can build against the installed SDK
  - `PATH` wiring for the prebuilt binaries shipped in the bundle
- Both entrypoints use `nix develop`, but they are intentionally defined in separate files so the maintainer and SDK-user environments can evolve independently.

Because `foundation` discovers the SDK root from its installed layout, the bundle flake and the archive layout must stay structurally aligned even though the repo checkout has a different maintainer-oriented source tree.

## Supported Platforms

### macOS

| Architecture | Rust Triple            | Notes                    |
| ------------ | ---------------------- | ------------------------ |
| ARM64        | `aarch64-apple-darwin` | Apple Silicon (M-series) |
| AMD64        | `x86_64-apple-darwin`  | Intel Macs               |

### Linux

| Architecture | Rust Triple                 | Notes                     |
| ------------ | --------------------------- | ------------------------- |
| AMD64        | `x86_64-unknown-linux-gnu`  | Standard desktop/server   |
| ARM64        | `aarch64-unknown-linux-gnu` | Raspberry Pi, ARM servers |

### Windows (WSL)

| Architecture | Rust Triple                 | Notes                                         |
| ------------ | --------------------------- | --------------------------------------------- |
| AMD64        | `x86_64-unknown-linux-gnu`  | WSL runs Linux binaries — same as Linux AMD64 |
| ARM64        | `aarch64-unknown-linux-gnu` | WSL on ARM — same as Linux ARM64              |

**Note:** Since WSL runs Linux userspace, Windows targets are identical to their Linux counterparts. No MinGW or MSVC targets are needed. This reduces the build matrix from six to four distinct targets.

### Final Build Matrix

| Target                      | Build Host | Method                    |
| --------------------------- | ---------- | ------------------------- |
| `aarch64-apple-darwin`      | macOS      | Nix shell (native)        |
| `x86_64-apple-darwin`       | macOS      | Nix shell (native/cross)  |
| `x86_64-unknown-linux-gnu`  | Linux      | Nix shell (native)        |
| `aarch64-unknown-linux-gnu` | Linux      | Nix shell (cross)         |

Two host environments, four artifacts.

macOS targets must be built on a macOS host because the Apple SDK/Xcode toolchain is only available through Apple's licensed macOS environment. This is a host requirement, not a requirement to use GitHub Actions.

## Build Tool: `cargo xtask`

The build orchestrator lives in the workspace as `xtask/`, following the standard Rust [xtask pattern](https://github.com/matklad/cargo-xtask).

### Workspace Layout

```
foundation-sdk/
├── Cargo.toml              # workspace members include "xtask"
├── xtask/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs         # CLI entry point
│       ├── build.rs        # SDK build orchestration
│       ├── package.rs      # archive packaging + signing
│       ├── submodules.rs   # submodule resolution + validation
│       └── config.rs       # parses sdk-build.toml
├── sdk-build.toml          # build configuration
└── ...
```

### CLI Interface

All commands below are run from the maintainer flake, either from an interactive `nix develop` shell or with `nix develop --command ...`.

```
cargo xtask build [OPTIONS]

Options:
    --target <TRIPLE>       Target triple (repeatable, or "all")
    --release               Release mode (default: debug)
    --package               Package built staging output into dist/*.tar.gz after the build
    --skip-simulator        Skip simulator compilation
    --skip-docs             Skip documentation generation
    --keyos-dir <PATH>      Override KeyOS source directory (instead of submodule)
    --slint-dir <PATH>      Override Foundation Slint source directory (instead of submodule)
    --sign                  Sign the output archive with Foundation release key
    --sign-key <KEYID>      GPG key ID, fingerprint, or email (default: from env FOUNDATION_SIGN_KEY)
    --output-dir <PATH>     Output directory (default: ./dist)
    --jobs <N>              Parallel compilation jobs
    --verbose               Verbose output

cargo xtask check-layout [OPTIONS]

Options:
    --keyos-dir <PATH>      Override KeyOS source directory (instead of submodule)
    --slint-dir <PATH>      Override Foundation Slint source directory (instead of submodule)
    --verbose               Verbose output

cargo xtask package [OPTIONS]

Options:
    --target <TRIPLE>       Target triple (repeatable, or "all")
    --version <SEMVER>      SDK version string (default: from workspace Cargo.toml)
    --output-dir <PATH>     Output directory (default: ./dist)
    --verbose               Verbose output

cargo xtask finalize [OPTIONS]

Options:
    --version <SEMVER>      SDK version string (default: from workspace Cargo.toml)
    --output-dir <PATH>     Output directory (default: ./dist)
    --sign-key <KEYID>      GPG key ID, fingerprint, or email (default: from env FOUNDATION_SIGN_KEY)
    --verbose               Verbose output

cargo xtask clean
    Remove all build artifacts and dist/ contents.

cargo xtask check-submodules
    Validate all submodules are at their pinned refs.

cargo xtask smoke-check [OPTIONS]

Options:
    --keyos-dir <PATH>      Override KeyOS source directory (instead of submodule)
    --slint-dir <PATH>      Override Foundation Slint source directory (instead of submodule)
    --sign                  Also validate signing prerequisites
    --sign-key <KEYID>      GPG key ID, fingerprint, or email (default: from env FOUNDATION_SIGN_KEY)
    --verbose               Verbose output
```

### Examples

```bash
# Build and package for a single macOS target
nix develop --command cargo xtask build --target aarch64-apple-darwin --release --package

# Validate the resolved source layout before building
nix develop --command cargo xtask check-layout

# Validate submodules, staged source layout, and packaging prerequisites
nix develop --command cargo xtask smoke-check

# Build for all targets
nix develop --command cargo xtask build --target all --release

# Build with local KeyOS checkout
nix develop --command cargo xtask build --target x86_64-unknown-linux-gnu --keyos-dir ../keyos

# Build with local KeyOS + Foundation Slint checkouts
nix develop --command cargo xtask build --target aarch64-apple-darwin --keyos-dir ../keyos --slint-dir ../slint

# Build and package
nix develop --command cargo xtask build --target all --release --package

# Build, package, and sign
nix develop --command cargo xtask build --target all --release --package --sign
```

## GitHub Actions Packaging

The SDK can also be built in GitHub Actions without publishing anything publicly.

- A macOS job stages the Darwin targets into `dist/aarch64-apple-darwin/` and `dist/x86_64-apple-darwin/`.
- A Linux job stages the Linux targets into `dist/x86_64-unknown-linux-gnu/` and `dist/aarch64-unknown-linux-gnu/`.
- A final packaging job downloads both staging artifacts, merges the `dist/` tree, runs `cargo xtask package --target all`, and uploads the resulting archives as a workflow artifact.

This flow is intended for internal release testing:

- download the workflow artifact from the Actions run
- test the generated archives and installer
- publish the same artifacts elsewhere after validation

The workflow artifact is not itself a public release channel. It is a private build output tied to a workflow run and retained according to the repository or organization Actions artifact retention policy.

## Build Configuration: `sdk-build.toml`

Single source of truth for what gets compiled, copied, and how submodules are pinned.

```toml
[sdk]
version = "1.0.0"                          # overridden by --version flag or workspace Cargo.toml
api_version = "1"                           # KeyOS API version this SDK targets
keyos_api_interfaces = [                    # selected KeyOS server interface crates to ship
    "app-manager",
    "crypto",
    "fs",
    "gui-server",
    "haptics",
    "quantum-link",
    "rgb-led",
    "settings",
]

# ---------------------------------------------------------------------------
# Submodule Definitions
# ---------------------------------------------------------------------------
[submodules.slint]
path = "external/slint"
repo = "git@github.com:Foundation-Devices/slint.git"
ref = "v1.12.1-foundation10"
env_override = "SLINT_DIR"

# `external/slint` is a logical source root that xtask resolves through
# `SLINT_DIR` or the maintainer shell's pinned Nix-fetched Slint checkout.
# This tag must stay aligned with the Slint tag referenced by the parent
# KeyOS workspace, and xtask verifies the resolved source against Cargo.lock.

# ---------------------------------------------------------------------------
# Targets
# ---------------------------------------------------------------------------
[targets]
triples = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
]

# Per-target overrides (optional)
[targets.overrides."aarch64-unknown-linux-gnu"]
cross = true                                # use the required Nix cross shell
linker = "aarch64-linux-gnu-gcc"

# ---------------------------------------------------------------------------
# Compile: binaries to build from submodules
# ---------------------------------------------------------------------------
[[compile]]
name = "foundation"
manifest = "crates/cli"                     # relative to SDK repo root (workspace member)
artifact = "foundation"
binary = "foundation"

[[compile]]
name = "simulator"
manifest = "../apps/simulator"              # handled specially by xtask
binary = "foundation-simulator"

# xtask treats this entry as "bundle the hosted simulator runtime".
# It builds the hosted KeyOS kernel and service set, stages them under
# lib/keyos/simulator/, and writes bin/foundation-simulator as a launcher script.

[[compile]]
name = "cosign2"
manifest = "../imports/cosign2/cosign2-bin"
package = "cosign2-bin"
artifact = "cosign2"
binary = "cosign2"

[[compile]]
name = "slint-viewer"
manifest = "external/slint/tools/viewer"
package = "slint-viewer"
artifact = "slint-viewer"
binary = "foundation-slint-viewer"
cargo_flags = [
    "--no-default-features",
    "--features",
    "backend-winit,renderer-femtovg,renderer-software,custom-translations",
]

# ---------------------------------------------------------------------------
# Copy: source code to include in the SDK distribution
# ---------------------------------------------------------------------------
[[copy]]
source = "../i18n"
dest = "lib/keyos/i18n"

[[copy]]
source = "../keyos"
dest = "lib/keyos/keyos"

[[copy]]
source = "../server"
dest = "lib/keyos/server"

[[copy]]
source = "../slint-keyos-platform"
dest = "lib/keyos/slint-keyos-platform"

[[copy]]
source = "ui"
dest = "lib/keyos/ui"

[[copy]]
source = "ui"
dest = "ui"

[[copy]]
source = "resources"
dest = "resources"

[[copy]]
source = "../utils/file-backed"
dest = "lib/keyos/utils/file-backed"

[[copy]]
source = "../utils/fiat-symbols"
dest = "lib/keyos/utils/fiat-symbols"

[[copy]]
source = "../worker"
dest = "lib/keyos/worker"

[[copy]]
source = "../xous/api"
dest = "lib/keyos/xous/api"

[[copy]]
source = "../xous/xous-rs"
dest = "lib/keyos/xous/xous-rs"

[[copy]]
source = "crates/cli/templates"
dest = "lib/templates"

[[copy]]
source = "examples"
dest = "examples"

# ---------------------------------------------------------------------------
# Docs
# ---------------------------------------------------------------------------
[docs]
guide_source = "docs"                       # mdbook source
api_crates = []                             # staged KeyOS source docs are deferred until the final shipped source surface is locked
api_crates_workspace = [                    # workspace members to include in API docs
    "foundation-manifest",
]

# ---------------------------------------------------------------------------
# Signing
# ---------------------------------------------------------------------------
[signing]
key_env = "FOUNDATION_SIGN_KEY"             # env var for the GPG signing identity
algorithm = "openpgp-detached"              # detached GPG signatures
```

`optional = true` is only used for intentionally pending SDK pieces. The Foundation CLI and Foundation Slint viewer are now required SDK outputs.

## External Source Pinning

### Where Pins Are Defined

The SDK's pinned Slint ref is defined in the SDK build config and must stay aligned with the parent KeyOS workspace:

1. **`sdk-build.toml`** — the `ref` field under `[submodules.<name>]`. This is what the build tool reads and validates.
2. **KeyOS root `Cargo.toml` + `Cargo.lock`** — the Slint tag and exact locked commit used by the parent monorepo release.

The `cargo xtask check-submodules` command validates that these match and errors if they diverge.

### How to Update the Remaining Pin

```bash
# 1. Update your Foundation Slint checkout
cd /path/to/slint
git fetch
git checkout v1.12.1-foundation10

# 2. Update sdk-build.toml to match if the ref changed

# 3. Update nix/maintainer-flake.nix if the fetched-source hash changed

# 4. Verify from sdk/
SLINT_DIR=/path/to/slint cargo xtask check-submodules
```

The maintainer shell sets `SLINT_DIR` automatically, so a normal `nix develop`
or `just reinstall` flow does not need a repo-local Slint checkout.

The KeyOS source tree is no longer pinned as an SDK submodule in repo mode. It is resolved from the parent `KeyOS-dev` checkout (`sdk/..`) and packaged from there.

### Pinning Rules

- **Always pin to a tag or commit SHA.** Never a branch name. Branches move; tags and SHAs don't.
- **Tags are preferred** for releases (`v1.2.0`). Commit SHAs are acceptable for pre-release work.
- **Build verification enforces the pin.** Manual release checklists and any optional automation should run `cargo xtask check-submodules` before building. If the resolved Slint source does not match the KeyOS-locked revision, the build must fail.
- **Release builds pin Slint and use the checked-out monorepo parent for KeyOS.**
- **Local dev can still use `SLINT_DIR`** to point at a local working copy of the Foundation Slint fork.

### Auto-Initialization

The xtask build command resolves Slint automatically:

```
1. For each logical source root in sdk-build.toml:
   a. Check if env override is set → use that path
   b. Otherwise use the configured repo-relative source path if it exists
   c. In the maintainer shell, fall back to the pinned Nix-fetched Slint source
   d. Verify the resolved Slint source matches the version locked by KeyOS
```

## Build Pipeline Detail

What `cargo xtask build --target <TRIPLE> --release` does:

```
 1. Parse sdk-build.toml
 2. Resolve retained external source roots (currently Slint only)
 3. Validate the resolved Slint source matches the KeyOS-locked revision
 4. Stage the generated shared SDK UI and resources from ../ui2 into dist/
 5. For the target triple:
    a. Enter the flake-provided native or cross shell for the target
       - Nix provides linker, sysroot, CC/CXX env vars, docs tooling, and host SDK dependencies
    b. Compile each [[compile]] entry:
       - cargo build --release --target <TRIPLE> --manifest-path <source>/Cargo.toml [cargo_flags]
       - Copy resulting binary to dist/<TRIPLE>/bin/<binary>
    c. Copy each [[copy]] entry to its configured destination under dist/<TRIPLE>/
       - Most staged source crates land under lib/keyos/... or lib/templates/
       - The generated public `@ui` surface is staged at ui/ui/ in the package root
       - Generated shared SDK assets are staged at resources/
    d. Build docs (unless --skip-docs) — runs AFTER copy so copied source is available:
       - mdbook build for the guide when docs/book.toml exists; otherwise copy the guide source tree
       - cargo doc on configured copied source crates (currently deferred until the final staged KeyOS SDK surface is locked)
       - cargo doc on workspace members (foundation-manifest)
       - Copy all docs output to dist/<TRIPLE>/docs/
       - Copy static docs such as AGENTS.md into dist/<TRIPLE>/docs/
    e. Copy nix/sdk-user-flake.nix as flake.nix, nix/sdk-user-flake.lock as flake.lock, and setup.sh into dist/<TRIPLE>/
    f. Generate manifest.toml:
       - SDK version, API version, target triple
       - Build profile, host triple, workspace commit, and dirty state
       - SHA-256 checksums of all binaries
       - Staged source mappings and submodule refs used in this build
 6. If --package: package dist/<TRIPLE>/ into foundation-sdk-<VERSION>-<TRIPLE>.tar.gz and refresh release metadata for the targets built in that run
 7. If --sign: sign the packaged archive with the configured GPG identity
 8. If release artifacts are assembled from multiple machines, run `cargo xtask finalize` after copying every configured per-target archive into one `dist/` to emit detached GPG `.sig` files for the assembled archives and regenerate `install.sh` plus `checksums.sha256`. Finalization fails when a configured target, such as `x86_64-apple-darwin`, is missing.
```

## Distribution Output

After `cargo xtask build --target all --release --package`:

```
dist/
├── upload.sh
├── install.sh
├── foundation-sdk-1.0.0-aarch64-apple-darwin.tar.gz
├── foundation-sdk-1.0.0-x86_64-apple-darwin.tar.gz
├── foundation-sdk-1.0.0-x86_64-unknown-linux-gnu.tar.gz
├── foundation-sdk-1.0.0-aarch64-unknown-linux-gnu.tar.gz
└── checksums.sha256
```

If `--sign` is also passed, matching detached GPG `.sig` files are emitted beside each archive, `install.sh`, and `checksums.sha256`. `upload.sh` is a maintainer convenience helper that uploads the generated release artifacts to `gs://foundation-sdk/` by default.

### Local Install From `dist/`

You can test the packaged SDK locally without uploading it anywhere by pointing the generated
installer at your local `dist/` directory:

```bash
cat dist/install.sh | FOUNDATION_SDK_BASE_URL="file://$PWD/dist" bash
```

Each archive contains:

```
foundation-sdk-1.0.0-<target>/
├── flake.nix
├── flake.lock
├── bin/
│   ├── foundation
│   ├── foundation-asset-tool
│   ├── foundation-simulator   # launcher script
│   ├── foundation-slint-viewer
│   ├── foundation-passport-drive
│   └── cosign2
├── lib/
│   ├── keyos/
│   │   ├── api/                # selected interfaces from sdk.keyos_api_interfaces
│   │   ├── i18n/
│   │   ├── server/
│   │   ├── slint-keyos-platform/
│   │   ├── simulator/          # hosted simulator runtime (kernel, services, staged apps dir)
│   │   ├── ui/
│   │   ├── worker/
│   │   └── xous/
│   └── templates/          # currently sourced from crates/cli/templates
├── ui/
│   └── ui/
├── resources/
├── docs/
│   ├── guide/              # rendered mdBook output when configured, otherwise copied guide source
│   ├── api/                # generated docs for configured crates (currently foundation-manifest first)
│   └── AGENTS.md
├── examples/
├── manifest.toml
└── setup.sh
```

`setup.sh` is a helper for verification and onboarding only. The supported user-facing way to use the installed SDK is `foundation develop`.

At the release level, `dist/install.sh` is generated as a curl-friendly installer. It detects the host target, downloads the matching `.tar.gz` archive, detached `.sig` sidecars, and `checksums.sha256` plus its detached signature from `https://sdk.foundation.xyz` by default with visible curl progress, imports the embedded Foundation release public key into a temporary GPG home, checks its pinned fingerprint, verifies the signatures when `gpg` or `gpg2` is available, verifies the archive checksum, extracts the SDK under `~/.foundation/sdk/` by default, creates and validates a stable launcher directory at `~/.foundation/sdk/bin`, tries to update the user's shell rc file to put that launcher directory on `PATH`, and then points the user at `foundation develop`. If the rc file is managed or read-only, such as on NixOS/Home Manager, the install still succeeds and prints a manual `PATH` export. If `gpg` is unavailable, the installer prints a warning and falls back to checksum verification only. Callers can still override the archive base URL with `FOUNDATION_SDK_BASE_URL`; callers that set `FOUNDATION_SDK_INSTALL_DIR` get a non-mutating install by default and can force shell rc updates with `FOUNDATION_SDK_UPDATE_RC=1`.

## Downstream SDK Usage

Release bundles are still Nix-backed, but the supported entrypoint after installation is the Foundation CLI launcher. After installing a target archive, SDK users should:

```bash
source ~/.zshrc # or ~/.bashrc
foundation develop
foundation new hello-world
```

This is why every archive must include the SDK-user `flake.nix` and `flake.lock` alongside the packaged binaries, source crates, docs, and examples.

When an app is built from that shell, `foundation build` and `foundation sim` materialize `project/ui/ui` plus a generated private SDK UI/resource search tree under `project/target/foundation` so the existing `slint-keyos-platform-build` helper can continue resolving `@ui/<component>.slint` imports plus their shared asset references. App-owned images and fonts live in `resources/images` and `resources/fonts` and take precedence over SDK shared resources; hardware raw image conversion is delegated to the packaged `foundation-asset-tool` helper, and `foundation sim` stages original resources under `target/foundation/sim-resources` before launching the hosted runtime with `FOUNDATION_APP_RESOURCES_DIR`.

The same rule now applies to viewer workflows: `foundation preview` and `foundation view` discover the nearest Cargo project for the target `.slint` file, rerun that app's existing `build.rs` code generation before launching the viewer, and then launch `foundation-slint-viewer` with the generated project `-L ui=<project>/target/foundation/ui/ui` mapping when available, falling back to the SDK's `ui/ui` mapping. This is what allows apps that depend on generated `ui/gen/router.slint`, `ui/gen/navigate.slint`, `ui/gen/tr.slint`, or `ui/gen/exports.slint` to preview correctly without a separate manual build step.

## Optional Verification Builds

Releases are built manually. If Foundation wants a confidence check before release, it can optionally run the same maintainer-flake-backed commands on one macOS machine and one Linux machine:

```bash
# macOS host
nix develop --command cargo xtask build --target aarch64-apple-darwin --release
nix develop --command cargo xtask build --target x86_64-apple-darwin --release

# Linux host
nix develop --command cargo xtask build --target x86_64-unknown-linux-gnu --release
nix develop --command cargo xtask build --target aarch64-unknown-linux-gnu --release
```

That verification path is optional. The required release process is still a manual build and packaging flow driven by `cargo xtask` from the repo flake.
