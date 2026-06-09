---
name: foundation-cli
description: Use when choosing, explaining, or running Foundation SDK `foundation` CLI commands for KeyOS app development, especially build, preview, simulator, sideload, logs, plugin, completions, and signing workflows.
---

<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation CLI

Use the Foundation CLI guide as the source of truth before running or recommending `foundation` commands.

Find the guide in the first location that exists:

- SDK source checkout: `docs/foundation-cli.md`
- Packaged SDK bundle: `docs/guide/src/foundation-cli.md`
- App project: locate the SDK root with `FOUNDATION_SDK_ROOT`, `foundation doctor`, or project context, then use one of the paths above.

Follow these rules:

- Prefer canonical English command names.
- Do not create slash commands that merely duplicate `foundation` commands.
- Do not run `foundation sideload` or `foundation logs` unless the user intends to interact with connected hardware.
- Do not run `foundation cert gen` or edit signing identities unless the user explicitly asked for signing setup.
- Prefer `foundation preview` for UI checks, `foundation sim` for hosted runtime checks, and `foundation build` for signed hardware artifacts.

Command quick reference:

- `foundation new`: scaffold an SDK app.
- `foundation develop`: enter the SDK Nix shell.
- `foundation exit`: clean SDK/Nix cache state without deleting signing identities.
- `foundation doctor`: inspect SDK environment readiness.
- `foundation build`: build and sign hardware artifacts.
- `foundation clean`: remove generated app build and theme files (`target/`, `manifest.toml`, `ui/ui`); leaves authored source and `~/.foundation/themes` untouched.
- `foundation preview`: open Slint UI in the SDK viewer.
- `foundation sim`: build and run in the hosted simulator.
- `foundation sideload`: copy and optionally launch on connected Passport hardware.
- `foundation logs`: open the Passport USB log viewer.
- `foundation cert gen` / `foundation cert print`: create or inspect publisher signing certificates.
- `foundation plugin search` / `foundation plugin install` / `foundation plugin uninstall`: manage CLI plugins.
- `foundation completions`: generate or install shell completions.
