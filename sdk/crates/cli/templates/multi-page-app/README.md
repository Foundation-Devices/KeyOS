<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Multi-Page App Template

A KeyOS GUI application template with router and navigation between multiple pages.

## Structure

This template creates a multi-page GUI app with:

- **Main Page**: the default route at `ui/pages/page.slint`
- **Second Page**: a subpage at `ui/pages/second/page.slint`
- **Router**: handles navigation between pages with animations

## What's Included

- `app-config.toml` - Foundation app configuration with KeyOS permissions
- `AGENTS.md` - AI-agent guidance that points to the SDK CLI guide
- `Cargo.toml` - Rust project configuration
- `Cargo.lock` - Pinned dependency graph validated by the SDK
- `build.rs` - Build script for compiling Slint UI with router enabled
- `src/main.rs` - Minimal application entry point
- `ui/` - Slint UI files
  - `app.slint` - Main app window with router
  - `pages/page.slint` - Default route page
  - `pages/props.slint` - Default route props
  - `pages/second/` - Second page component
- `i18n/` - Internationalization files (English and Spanish)
- `resources/icon.svg` - 110x110px app icon image converted into the hardware app icon
- `resources/icon-dark.svg` - optional 110x110px dark-theme variant; without it the light icon serves both themes
- `resources/theme.json` - App-local theme opened by `foundation theme`
- `resources/images/sample-scene.svg` - Sample app-owned image, loaded with `Images.image("sample-scene")`

At build, preview, and sim time the SDK also materializes a generated private SDK UI/resource search tree so the public `@ui` component imports and shared assets resolve consistently after app-local `resources/`.

## Navigation

The router automatically generates the `Navigate` global from the page structure:

- `Navigate.second-page({ })` pushes the second page
- `Navigate.backward()` returns to the previous page

## Template Variables

- `{{app_name}}` - Internal app name (used in Cargo.toml)
- `{{friendly_app_name}}` - User-facing app name (shown in heading)
- `{{launcher_app_name}}` - Name shown in launcher
- `{{description}}` - App description
- `{{icon}}` - Icon path
- `{{app_id}}` - Unique 16-byte hex app ID
- `{{version}}` - App version
- `{{min_keyos_version}}` - Minimum required KeyOS version

## Adding More Pages

1. Create a new directory in `ui/pages/` (e.g., `ui/pages/third/`)
2. Add `page.slint` with your page component
3. Add a neighboring `props.slint` that exports a `struct` annotated with `@rust-attr(route(...))`
4. Mark exactly one page as the default route with `@rust-attr(route(default, path = "..."))`
5. Add translations to `i18n/en.json` and `i18n/es.json` if needed

## Customization

After creating a project from this template, you can:

1. Modify page layouts in `ui/pages/*/page.slint`
2. Edit `i18n/en.json` and `i18n/es.json` to update text
3. Replace `resources/icon.svg` with your own 110x110px icon
4. Run `foundation theme` to edit `resources/theme.json`
5. Add app images under `resources/images/` and load them from Slint with `Images.image("<name>")`
6. Add app fonts under `resources/fonts/` and use their family names in Slint `font-family` properties
7. Add more pages following the pattern above
8. Add page-specific route state in each page's `props.slint`

## Building

```bash
foundation build
```

## Running on Simulator

```bash
foundation sim
```
