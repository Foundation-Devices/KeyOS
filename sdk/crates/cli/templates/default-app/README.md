<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Default App Template

The default KeyOS GUI application template with a clean, simple interface.

## Structure

This template creates a basic GUI app with:

- **shared SDK public `@ui` imports**: the template uses the generated SDK component surface directly
- **Hero card**: displays the localized heading and message
- **Primary action button**: demonstrates a concrete shared SDK button import without relying on the legacy `@ui/widgets.slint` facade

## What's Included

- `app-config.toml` - Foundation app configuration with KeyOS permissions
- `AGENTS.md` - AI-agent guidance that points to the SDK CLI guide
- `Cargo.toml` - Rust project configuration
- `Cargo.lock` - Pinned dependency graph validated by the SDK
- `build.rs` - Build script for compiling Slint UI
- `src/main.rs` - Application entry point with button callback handler
- `ui/` - Slint UI files
  - `app.slint` - Main app window using `@ui/card.slint` and `@ui/button.slint`
  - `callbacks.slint` - Callback definitions
- `i18n/` - Internationalization files (English and Spanish)
- `resources/icon.svg` - 96x96px app icon image converted into the hardware app icon
- `resources/theme.json` - App-local theme opened by `foundation theme`
- `resources/images/checkmark.svg` - Sample app-owned image, loaded with `Images.image("checkmark")`

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

1. Modify `ui/app.slint` to change the layout or add more UI elements
2. Edit `i18n/en.json` and `i18n/es.json` to update text
3. Replace `resources/icon.svg` with your own 96x96px icon
4. Run `foundation theme` to edit `resources/theme.json`
5. Add app images under `resources/images/` and load them from Slint with `Images.image("<name>")`
6. Add app fonts under `resources/fonts/` and use their family names in Slint `font-family` properties
7. Add more callbacks in `ui/callbacks.slint` and wire them up in `src/main.rs`
8. Add additional UI components or create a multi-page app by enabling the router

## Building

```bash
foundation build
```

## Running on Simulator

```bash
foundation sim
```
