<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# `foundation new` Design Notes

This file captures the command-specific design for `foundation new`.

The full CLI contract lives in [SPEC.md](../../../SPEC.md). This note stays focused on scaffolding behavior and template inputs.

## Current Behavior

`foundation new` creates a new Foundation/KeyOS application project from a shipped template.

Current templates:

- `default-app`
- `multi-page-app`
- `kitchen-sink`

The command:

- accepts the project name as an optional positional argument
- prompts for the template when `--template` is omitted
- prompts for:
  - friendly app name
  - launcher app name
  - description
  - icon path
  - app ID
  - version
  - minimum KeyOS version
- generates a random 16-byte `0x`-prefixed app ID when the prompt is left blank
- writes the scaffolded project files
- runs `git init` by default
- skips repository initialization when `--no-git` is passed
- does not create an initial commit

If Git is unavailable, scaffolding still succeeds and the command reports that the repository was not initialized.

## Scaffolded Project Shape

The shipped templates currently create projects shaped like:

```text
my-app/
├── app-config.toml
├── permission_templates.toml
├── Cargo.toml
├── Cargo.lock
├── build.rs
├── src/
├── ui/
├── i18n/
└── resources/
    ├── images/  # app-owned images
    ├── fonts/   # optional app-owned fonts
    └── theme.json
```

The exact `ui/` contents depend on the chosen template. `resources/` contains the app icon, app theme, and app-owned
resources. SDK shared resources are searched automatically after local app resources.

## Template Variables

The command currently expands these variables:

- `app_name`
- `friendly_app_name`
- `launcher_app_name`
- `description`
- `icon`
- `app_id`
- `version`
- `min_keyos_version`
- `sdk_root`
- `sdk_keyos_root`
- `sdk_ui_root`
- `sdk_path`

`sdk_path` is a compatibility alias for `sdk_keyos_root`.

## Config Contract

The generated project is centered on `app-config.toml`, which mirrors `foundation_core::AppConfig`.
It also includes a pinned `Cargo.lock` so new apps reproduce the SDK-validated dependency graph instead of drifting with the latest crates.io resolution.

That means the scaffolded config is expected to carry:

- application naming fields
- icon path
- optional theme path
- app ID
- semantic version
- minimum KeyOS version
- KeyOS permission configuration
- optional `cosign2-config`

Permission template aliases are resolved from the generated `permission_templates.toml`.

## Intentional Non-Goals

The command does not currently:

- create an initial Git commit
- generate CI/CD configuration
- open an editor or IDE automatically
- install dependencies after scaffolding
- support user-defined external templates beyond the current template loading rules
