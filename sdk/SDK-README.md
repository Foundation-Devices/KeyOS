<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation SDK Maintainer README

This document is for Foundation engineers working on the SDK inside the `KeyOS-dev` monorepo.

It explains:

- what lives in `sdk/`
- how the SDK is wired into the parent monorepo
- how the build and packaging flow works
- how releases are installed
- how third-party developers experience the SDK
- what is generated, what is source, and what not to edit by hand

For deeper reference material, also see:

- `docs/sdk-architecture.md`
- `docs/sdk-build-system.md`
- `crates/cli/README.md`
- `crates/cli/SPEC.md`
- `ui/README.md`
- `resources/README.md`

## What The SDK Is

The Foundation SDK is the developer distribution for building third-party apps that target Passport Prime and related KeyOS devices.

It ships:

- the `foundation` CLI
- the customized `foundation-slint-viewer`
- a prebuilt hosted simulator runtime plus `foundation-simulator`
- a curated source subtree under `lib/keyos/` for app builds
- the public `@ui/...` Slint surface
- templates, examples, docs, and install metadata

Inside this monorepo, `sdk/` is a nested standalone workspace. It is intentionally not a member of the root `KeyOS-dev` Cargo workspace.

## Big Picture

The SDK is built from a Foundation-maintained source checkout, but third-party developers consume a packaged bundle.

There are two distinct environments:

- Maintainer environment: used by Foundation engineers to build, verify, package, and sign SDK releases
- User environment: shipped inside the packaged SDK so app developers can run `foundation develop` and build apps

Those two environments are both Nix-based, but they are not the same flake:

- `sdk/flake.nix` points to the maintainer flake in `sdk/nix/maintainer-flake.nix`
- packaged SDK bundles receive a separate user flake copied from `sdk/nix/sdk-user-flake.nix`

This separation is deliberate. Maintainers need packaging and release tooling. SDK users need a stable runtime environment around the packaged bundle.

## How `sdk/` Fits Into `KeyOS-dev`

The SDK no longer depends on an `external/keyos` submodule. In repo mode it treats the parent directory, `sdk/..`, as the KeyOS source root.

That means:

- the SDK builds some binaries from `sdk/` itself
- it stages source and runtime pieces out of the parent `KeyOS-dev` tree
- it packages those staged outputs into a self-contained SDK bundle

At a high level:

- `KeyOS-dev/ui/` remains the legacy monorepo UI used by Foundation-authored apps
- `KeyOS-dev/ui2/` is the internal shared UI source tree used to generate the SDK surface
- packaged SDK artifacts expose the public SDK `@ui` API backed by generated files copied from `../ui2`

Foundation-authored apps in the monorepo still use the legacy `ui/ui` tree. Third-party SDK apps use the generated SDK-facing surface under `ui/ui` in the packaged SDK artifact.

## Directory Structure

The most important directories in `sdk/` are:

- `crates/`
  The SDK Rust workspace, including the `foundation` CLI and shared crates.
- `xtask/`
  The Rust build orchestrator for staging, packaging, layout validation, and release checks.
- `docs/`
  Architecture and build-system reference material.
- `nix/`
  Maintainer and SDK-user flake definitions.
- external Slint checkout
  The maintainer build resolves the Foundation Slint workspace through `SLINT_DIR` or the pinned Nix-fetched source, then stages a pruned copy into the SDK bundle.
- `examples/`
  Example app trees that ship in the packaged SDK.
- `dist/`
  Staging directories, archives, checksums, and the generated install script.
- `sdk-build.toml`
  The build source of truth for compile targets, copied source trees, targets, and signing settings.
- `Justfile`
  Convenience maintainer commands. Right now this includes `just reinstall` for a local rebuild and reinstall flow.

## Source Of Truth Vs Generated Content

These paths are source:

- `sdk/crates/**`
- `sdk/xtask/**`
- `sdk/docs/**`
- `sdk/nix/**`
- `sdk/examples/**`
- `sdk/sdk-build.toml`

These paths are generated from `../ui2` and must not be edited by hand:

- packaged SDK `ui/ui/**`
- packaged SDK `resources/**`

They are produced while staging an SDK artifact. The generation step does three things:

1. Copies `../ui2/components/ui/*.slint` into the staged `ui/ui` trees
2. Copies `../ui2/resources/**/*` into staged `resources`
3. Rewrites any legacy nested shared-asset paths inside copied Slint files to `../../resources/...`

The SDK still exposes `@ui/...` through the packaged `ui/ui` tree, but the content is generated from the internal `../ui2` source tree.

## Maintainer Nix Workflow

The SDK is Nix-first. That is not optional tooling or a convenience layer. It is part of how the SDK is designed.

Why Nix matters here:

- it provides the correct Rust toolchains
- it wires Apple and Linux linker settings consistently
- it provides packaging tools such as GNU `tar`
- it gives maintainers and SDK users separate, reproducible environments

Common maintainer entrypoint:

```bash
cd /Users/kenc/dev/KeyOS-dev/sdk
nix develop
```

From there, typical commands are:

```bash
cargo xtask check-layout
cargo xtask smoke-check
cargo xtask build --target aarch64-apple-darwin --release
cargo xtask build --target aarch64-apple-darwin --release --package
```

When the SDK directory is not yet tracked by Git in the parent repo, plain flake resolution may hide untracked files. In that case use a path flake explicitly:

```bash
nix develop "path:$PWD"
```

Once `sdk/` is tracked normally in Git, the plain `nix develop` flow works again.

## Build And Packaging Flow

The build orchestration lives in `xtask/` and is invoked through `cargo xtask`.

The most important commands are:

- `cargo xtask check-layout`
- `cargo xtask smoke-check`
- `cargo xtask build`
- `cargo xtask package`
- `cargo xtask finalize`

In normal maintainer usage, `cargo xtask build --package` is the main release flow.

### What `cargo xtask build` Does

For each requested target triple, `cargo xtask build`:

1. Stages generated `ui/ui` and `resources` trees from the internal `../ui2` source tree
2. Validates the expected repo layout
3. Compiles the SDK binaries declared in `sdk-build.toml`
4. Stages the hosted KeyOS simulator runtime
5. Copies the curated source and support trees into `dist/<target>/`
6. Builds or copies documentation unless `--skip-docs` is used
7. Writes bundle metadata such as `manifest.toml`
8. Verifies the staged bundle has the required files
9. Optionally packages the result into `.tar.gz` archives when `--package` is set

### What Gets Compiled

Today the build compiles and stages at least:

- `foundation`
- `foundation-asset-tool`
- `foundation-simulator`
- `foundation-slint-viewer`
- `foundation-passport-drive`
- `cosign2`

The simulator flow is special. `xtask` stages a hosted KeyOS runtime under `lib/keyos/simulator/` and writes the `foundation-simulator` launcher script around it.

### What Gets Copied

The staged bundle also receives:

- selected KeyOS API and runtime source trees under `lib/keyos/`
- the generated public SDK UI under `ui/ui/`
- the generated shared UI assets under `resources/`
- templates under `lib/templates/`
- docs and examples
- the SDK-user `flake.nix`, `flake.lock`, and `setup.sh`

The copy list and compile list are configured in `sdk-build.toml`.

## Local Maintainer Commands

Useful commands from `sdk/`:

```bash
nix develop --command cargo xtask check-layout
nix develop --command cargo xtask smoke-check
nix develop --command cargo xtask build --target aarch64-apple-darwin --release --package
nix develop --command cargo xtask finalize
```

There is also a convenience reinstall flow for Apple Silicon by default:

```bash
just reinstall
```

That command:

1. ensures GNU `tar` is available on `PATH`
2. enters the maintainer flake
3. builds and packages the SDK for `aarch64-apple-darwin`
4. pipes the generated local `install.sh` into `bash`

You can also pass a target explicitly:

```bash
just reinstall x86_64-apple-darwin
```

For macOS release artifacts, use `just build all` on a macOS host so both
`aarch64-apple-darwin` and `x86_64-apple-darwin` archives are present before
running `just finalize`.

## Release Artifact Layout

The build stages outputs in:

```text
sdk/dist/<target>/
```

When packaged, the release outputs include:

- `dist/foundation-sdk-<version>-<target>.tar.gz`
- `dist/checksums.sha256`
- `dist/install.sh`
- `dist/upload.sh`

Inside each archive, the important layout is:

- `bin/`
  Prebuilt SDK tools and launchers.
- `lib/keyos/`
  Curated KeyOS source/runtime subtree used by app builds.
- `ui/ui/`
  Public SDK `@ui` surface.
- `resources/`
  Shared assets used by the public SDK UI.
- `docs/`
  Maintainer and user-facing reference material included in the bundle.
- `examples/`
  Sample app trees.
- `flake.nix` and `flake.lock`
  The SDK-user flake entrypoint.

## Deployment And Installation

Deployment is conceptually simple:

1. build and package one archive per target
2. if the archives were built on multiple machines, copy them into one `dist/`
3. run `cargo xtask finalize` to emit detached GPG signatures for the assembled archives and regenerate `checksums.sha256` plus the release-level `install.sh`; finalization fails if any configured target archive is missing
4. use `--sign-key` only if you want to override the default GPG signing identity source
5. publish the archives plus installer

The generated `install.sh`:

- detects the host OS and CPU architecture
- downloads the matching `foundation-sdk-<version>-<target>.tar.gz`, its detached `.sig`, and the signed `checksums.sha256` with visible curl progress
- imports the embedded Foundation release public key into a temporary GPG home, checks its pinned fingerprint, and verifies the detached signatures when `gpg` or `gpg2` is available
- verifies the archive checksum
- installs it under `~/.foundation/sdk` by default
- refreshes `~/.foundation/sdk/current`
- rebuilds the launcher directory under `~/.foundation/sdk/bin`
- validates the launcher directory before adding it to `PATH`
- tries to update the user's shell rc file to add that launcher directory to `PATH` unless `FOUNDATION_SDK_INSTALL_DIR` is set, or `FOUNDATION_SDK_UPDATE_RC=0` is provided; if the rc file is managed or read-only, installation still succeeds and prints a manual `PATH` export

If `gpg` or `gpg2` is not available on the host, the installer prints a warning and falls back to checksum verification only.

By default the installed SDK lives at:

```text
~/.foundation/sdk/foundation-sdk-<version>-<target>
```

The stable launcher path is:

```text
~/.foundation/sdk/bin/foundation
```

If the same version and target are reinstalled, the install script replaces that existing directory in place and repoints `current`.
If the cached Base Theme still exactly matches the previously installed SDK copy, the installer advances it to the new SDK copy too. A modified Base Theme is never overwritten; it remains editable and must satisfy the compiler's completeness check before an app can build.

For local testing against freshly built artifacts, maintainers can install from `dist/` directly:

```bash
cat dist/install.sh | FOUNDATION_SDK_BASE_URL="file://$PWD/dist" bash
```

## How Third-Party Developers Use The SDK

Third-party developers should not need to understand the SDK maintainer layout.

Their expected workflow is:

1. install the SDK with the release installer
2. run `foundation develop`
3. scaffold a project with `foundation new`
4. build, preview, simulate, and sideload through the CLI

Common commands they use:

```bash
foundation doctor
foundation develop
foundation new my-app
foundation build
foundation sim
foundation preview ui/app.slint
```

The SDK user shell exported by the packaged flake sets:

- `FOUNDATION_SDK_ROOT`
- `FOUNDATION_SDK_BIN`

and adds the packaged `bin/` directory to `PATH`.

App templates and CLI commands treat the following SDK paths as the stable public surface:

- `<sdk-root>/lib/keyos/api/*`
- `<sdk-root>/lib/keyos/server`
- `<sdk-root>/lib/keyos/slint-keyos-platform/*`
- `<sdk-root>/lib/keyos/xous/*`
- `<sdk-root>/ui/ui`
- `<sdk-root>/resources`
- `<sdk-root>/lib/templates`

That is the contract third-party projects are expected to build against.

## SDK Root Discovery

The CLI discovers the SDK root by:

1. `FOUNDATION_SDK_ROOT`
2. walking upward from the current working directory
3. walking upward from the current executable location

It supports both:

- repo layout, where the SDK root is the `sdk/` checkout in `KeyOS-dev`
- bundle layout, where the SDK root is the unpacked installed SDK

That is why the CLI can work both inside this repo and inside an installed SDK bundle.

## The Public UI Surface

For SDK apps, the stable app-facing Slint import surface is `@ui/...`, backed by:

- packaged SDK `ui/ui`
- packaged SDK `resources`

Important points:

- update the internal `../ui2` source tree instead
- run a build flow to generate these directories in `dist/`
- SDK templates and examples should only rely on the generated public `@ui` API

This separation lets Foundation evolve the internal shared UI source tree while still shipping a stable SDK-facing tree.

## Why Slint Is Still Packaged

The SDK still builds and ships a customized Foundation Slint viewer plus a curated `lib/slint` workspace snapshot inside the common SDK archive. Raw image conversion is handled by the prebuilt `foundation-asset-tool` helper, so the main `foundation` CLI does not link the Slint platform build crate for runtime asset staging.

The important distinction is that the maintainer build no longer requires a vendored `sdk/external/slint` checkout. Instead it resolves Slint from `SLINT_DIR` or from the pinned Nix-fetched Foundation source, and `xtask` verifies that the SDK input matches the Slint revision locked by the parent KeyOS workspace.

## Relationship To Foundation-Owned Apps

This is easy to mix up, so it is worth calling out clearly:

- Foundation-owned monorepo apps continue using the legacy `KeyOS-dev/ui/ui` tree
- third-party SDK apps use the generated SDK surface under packaged `ui/ui`

The SDK is designed to expose a clean, stable app-facing surface without forcing the whole monorepo to migrate at once.

## What To Edit When

If you are changing the CLI, edit:

- `sdk/crates/cli/**`
- `sdk/crates/cli/crates/**`

If you are changing build/package behavior, edit:

- `sdk/xtask/**`
- `sdk/sdk-build.toml`
- `sdk/nix/**`

If you are changing the public SDK UI library, edit:

- `KeyOS-dev/ui2/components/ui/**`
- `KeyOS-dev/ui2/resources/**`

Then run a build flow so the SDK-facing surface is generated into `dist/`.

If you are changing Foundation-owned monorepo app UI only, edit:

- `KeyOS-dev/ui/ui/**`

Do not make those changes in generated artifact directories, because those trees are only for the SDK-facing surface.

## Common Maintainer Checklist

Before landing SDK changes:

- run `cargo xtask check-layout`
- run `cargo xtask smoke-check`
- run `cargo test --manifest-path Cargo.toml -p xtask`
- run at least one real `cargo xtask build --target <target> --release --package`
- if UI changed, confirm the staged `ui/ui` and `resources` trees were generated from `../ui2`
- if templates changed, scaffold a fresh app with `foundation new` and test `foundation build`, `foundation sim`, and `foundation preview`

## Troubleshooting Notes

- If `nix develop` complains that `sdk/flake.nix` is not tracked by Git, use `nix develop "path:$PWD"` from `sdk/`, or stage the SDK files in Git.
- If packaging fails on macOS because GNU `tar` is missing, install `gnu-tar` and make sure its `gnubin` directory is on `PATH`.
- If app UI assets look missing in preview or the simulator, verify the build staged `resources` from `../ui2/resources`.
- If a change to `../ui2` is not visible to SDK apps, remember that SDK apps consume the generated packaged `ui/ui` tree, not the source tree directly.

## In One Sentence

The SDK is a Nix-first, packaged developer distribution built from the `sdk/` workspace plus selected KeyOS inputs and generated shared UI inputs from the parent monorepo, with a generated public `@ui` surface intended for third-party app developers.
