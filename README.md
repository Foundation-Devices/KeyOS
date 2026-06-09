<!--
SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# KeyOS

KeyOS is Foundation's operating system and app platform for Passport Prime. This
repo contains the source needed to reproduce KeyOS release builds, plus the SDK
source used to publish the app developer tools.

Most people arrive here for one of two reasons:

- They want to rebuild KeyOS and compare hashes against an official release.
- They want to build a KeyOS app with the Foundation SDK.

<img src="media/passport-prime-device.png" width="800" alt="Passport Prime running KeyOS"/>

## Reproduce A KeyOS Release

See [`REPRODUCIBILITY.md`](REPRODUCIBILITY.md) for the full release verification
procedure, including prerequisites, build commands, exact release-artifact
layout, and caveats.

## Build KeyOS Apps

Use this path if you want to create an app for Passport Prime. App developers
should install the Foundation SDK rather than building the whole OS tree.

### Install The SDK

The SDK installer downloads the current SDK bundle, verifies checksums, installs
it under `~/.foundation/sdk`, and tries to add the `foundation` launcher to your
shell path.

```bash
curl -fsSL https://sdk.foundation.xyz/install.sh | sh
source ~/.zshrc # or ~/.bashrc
foundation doctor
```

On NixOS or systems where shell rc files are managed or read-only, disable the
rc-file update and add the launcher to your shell configuration yourself:

```bash
curl -fsSL https://sdk.foundation.xyz/install.sh | FOUNDATION_SDK_UPDATE_RC=0 sh
export PATH="$HOME/.foundation/sdk/bin:$PATH"
foundation doctor
```

The SDK uses Nix for the app build environment. If `foundation doctor` reports
that Nix is missing, install Nix first:

```bash
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install
```

### Enter The Development Shell

Start the SDK shell before creating or building apps:

```bash
foundation develop
```

This starts the Nix environment that provides the Rust toolchain, KeyOS target,
Slint viewer, simulator, signing tools, and SDK paths needed by app projects.
Type `exit` or press `Ctrl+D` to leave the shell.

### Create An App

Scaffold a new app from the SDK templates:

```bash
foundation new my-app
cd my-app
```

The generated project includes:

- `app-config.toml` - App name, app ID, publisher info, icon, permissions, and
  signing configuration.
- `Cargo.toml` - Rust package configuration with SDK path dependencies.
- `build.rs` - Slint and KeyOS code generation.
- `src/main.rs` - App entrypoint and callback wiring.
- `ui/` - Slint UI files.
- `i18n/` - Translation files.
- `resources/` - App icon and local assets.

### Run In The Simulator

From inside the app project:

```bash
foundation sim
```

This builds the app for hosted execution, stages it into the SDK simulator app
directory, and starts the KeyOS simulator.

You can also preview only the UI:

```bash
foundation preview ui/app.slint
```

### Sideload To Passport Prime

Connect Passport Prime over USB-C with USB enabled, and enable Developer Mode in
Settings > Apps. Then run:

```bash
foundation sideload
```

This builds and signs the app, copies `app.elf` and `manifest.json` to the
mounted `PRIME` volume, and launches the app over the USB control channel. Use
`foundation sideload --no-run` if you only want to copy the app bundle.

For deeper SDK details, see:

- [`sdk/SDK-README.md`](sdk/SDK-README.md) - SDK maintainer and release notes.
- [`sdk/crates/cli/SPEC.md`](sdk/crates/cli/SPEC.md) - Current `foundation` CLI
  behavior.
- [`sdk/crates/cli/templates`](sdk/crates/cli/templates) - Shipped app
  templates.

## Repository Structure

Most app developers should not need this source tree day to day, but these are
the important directories:

- [`sdk`](sdk) - Source for the Foundation SDK, `foundation` CLI, simulator
  packaging, templates, examples, and release installer.
- [`apps`](apps) - Foundation-authored in-tree KeyOS apps.
- [`api`](api) - Client crates used by apps to talk to KeyOS services.
- [`server`](server) - Typed IPC helpers for KeyOS servers and clients.
- [`slint-keyos-platform`](slint-keyos-platform) - KeyOS Slint runtime and build
  integration, including routing and translation codegen.
- [`ui`](ui) - Shared UI assets used by Foundation-authored in-tree apps.
- [`ui2`](ui2) - Internal source for the SDK-facing shared UI surface.
- [`os`](os), [`xous`](xous), [`boot`](boot), [`loader`](loader), and
  [`xtask`](xtask) - OS internals, kernel, boot flow, loader, image builder, and
  release tooling.

Foundation engineers working on KeyOS internals should use
[`DEVELOPMENT.md`](DEVELOPMENT.md).

## Security Vulnerability Disclosure

Please report suspected security vulnerabilities in private to
security@foundationdevices.com. Please do not create publicly viewable issues for
suspected security vulnerabilities.

## Licensing

KeyOS uses [REUSE](https://reuse.software/) metadata. License information is
declared in file headers where possible and in [`.reuse/dep5`](.reuse/dep5) for
generated files, assets, imported code, and other exceptions. See the
[`LICENSES`](LICENSES) folder for the license texts used in this repository.

Because KeyOS includes GPL-licensed components, KeyOS firmware should be treated
in a copyleft manner.
