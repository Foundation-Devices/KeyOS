<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Kitchen Sink Template

The KeyOS ui2 component gallery template for theme work.

## Structure

This template creates a GUI app with:

- **Component selector**: a top dropdown chooses the visible component page
- **Variant matrices**: each page shows the component's size, style, state, and tone variants
- **App-local theme**: `resources/theme.json` is opened directly by `foundation theme`

## What's Included

- `app-config.toml` - Foundation app configuration with KeyOS permissions
- `AGENTS.md` - AI-agent guidance that points to the SDK CLI guide
- `Cargo.toml` - Rust project configuration
- `Cargo.lock` - Pinned dependency graph validated by the SDK
- `build.rs` - Build script for compiling Slint UI
- `src/main.rs` - Minimal application entry point
- `ui/` - Slint UI files
  - `app.slint` - Main component gallery using shared `@ui` imports
- `resources/icon.svg` - App icon image converted into the hardware app icon
- `resources/theme.json` - App-local theme opened by `foundation theme`
- `resources/images/sample.svg` - Sample app-owned image, referenced with `Images.common("images/sample")`

At build, preview, and sim time the SDK also materializes a generated private SDK UI/resource search tree so the public `@ui` surface and shared component assets resolve consistently after app-local `resources/`.

## Template Variables

- `{{app_name}}` - Internal app name (used in Cargo.toml)
- `{{friendly_app_name}}` - User-facing app name (shown in heading)
- `{{launcher_app_name}}` - Name shown in launcher
- `{{description}}` - App description
- `{{icon}}` - Icon path
- `{{app_id}}` - Unique 16-byte hex app ID
- `{{version}}` - App version
- `{{min_keyos_version}}` - Minimum required KeyOS version

## Customization

After creating a project from this template, you can:

1. Run `foundation theme` to edit `resources/theme.json`
2. Run `foundation sim` to inspect the theme in the simulator
3. Replace `resources/icon.svg` with your own icon
4. Add app images under `resources/images/` and load them from Slint with `Images.common("images/<name>")`
5. Add app fonts under `resources/fonts/` and use their family names in Slint `font-family` properties

## Building

```bash
foundation build
```

## Running on Simulator

```bash
foundation sim
```
