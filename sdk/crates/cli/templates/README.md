<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation CLI Templates

This directory contains project templates for the `foundation new` command.

## Template Structure

Each template is a directory containing:

```
template-name/
├── template.toml          # Template metadata and variables
├── README.md              # Template documentation
└── files/                 # Template file hierarchy
    ├── app-config.toml    # Foundation app configuration
    ├── Cargo.toml         # Rust project config
    ├── Cargo.lock         # Pinned dependency graph validated by the SDK
    ├── build.rs           # Build script
    ├── src/               # Source files
    ├── ui/                # UI files (for GUI apps)
    ├── i18n/              # Internationalization
    └── resources/         # App icon, app theme, and app-owned resources
```

Scaffolded GUI apps also receive a generated private SDK UI/resource search tree at build/preview time. That tree supplies the SDK-owned public `@ui` surface and shared component assets, while app-owned files in `resources/` take precedence. Put user images in `resources/images/` and user fonts in `resources/fonts/`.

## Template Variables

Templates use `{{variable}}` syntax for placeholders that get replaced during project creation:

- `{{app_name}}` - Internal app name (kebab-case)
- `{{friendly_app_name}}` - User-facing display name
- `{{launcher_app_name}}` - Name shown in launcher
- `{{description}}` - App description
- `{{icon}}` - Icon file path
- `{{app_id}}` - Unique 16-byte hex app ID
- `{{version}}` - App version (semver)
- `{{min_keyos_version}}` - Minimum required KeyOS version
- `{{sdk_keyos_root}}` - Preferred project-relative KeyOS source root inside the SDK mapping
- `{{sdk_root}}` - Compatibility/advanced variable for the SDK mapping root (`.foundation-sdk/current`)
- `{{sdk_ui_root}}` - Compatibility/advanced variable for the shared Slint UI library inside the SDK mapping
- `{{sdk_path}}` - Compatibility alias for `{{sdk_keyos_root}}`

Bundled templates should prefer `{{sdk_keyos_root}}` for KeyOS crate path dependencies. The other SDK path variables remain available for SDK-provided and user templates that already consume them.

## File Processing

The template processor handles files differently based on type:

### Processed Files (template variables replaced)
- `.rs` - Rust source files
- `.toml` - Configuration files
- `.json` - JSON files (i18n, etc.)
- `.slint` - UI files
- `.txt` - Text files
- `.md` - Markdown files
- `.lock` - Lockfiles such as `Cargo.lock`

### Copied Files (binary copy, no processing)
- `.svg` - SVG images
- `.png` - PNG images
- `.jpg`, `.jpeg` - JPEG images
- All other file types

## Available Templates

### default-app (default)

The default KeyOS GUI application with:
- A generated shared SDK `@ui` component surface
- A card-based hero panel and primary action button
- English and Spanish localizations
- English and Spanish localizations
- SVG placeholder icon
- App theme JSON under `resources/theme.json`
- Sample app-owned image under `resources/images/`
- Single-page layout (no router)

Perfect for simple apps that don't need navigation.

### multi-page-app

A multi-page KeyOS GUI application with:
- Router-based navigation
- shared SDK cards and primary action buttons
- Generated private SDK UI/resource search tree supplied by the SDK
- English and Spanish localizations
- SVG placeholder icon
- App theme JSON under `resources/theme.json`
- Sample app-owned image under `resources/images/`
- Example of page-based architecture

Use this when you need multiple screens or sections in your app.

### kitchen-sink

A ui2 component gallery with:
- A top component dropdown
- One page per shared component
- Size, style, tone, and state variants
- App-local `resources/theme.json` for `foundation theme`
- Sample app-owned image under `resources/images/`

Use this when designers need to edit a theme and inspect the result through `foundation sim` or `foundation sideload`.

## Creating a New Template

1. Create a directory: `templates/my-template/`
2. Add `template.toml` with metadata:
   ```toml
   name = "my-template"
   description = "Description of template"
   version = "1.0.0"

   [variables]
   # List expected variables with defaults
   app_name = ""
   # ...
   ```
3. Create `files/` subdirectory with your project structure
4. Use `{{variable}}` placeholders in text files
5. Add a README.md documenting the template

## Using Templates

```bash
# Use default template (default-app)
foundation new my-app

# Specify a template
foundation new my-app --template my-template

# Or use the localized flag name
FOUNDATION_LANG=es foundation nuevo mi-app --plantilla my-template
```

## Template Locations

Templates are searched in the following order:

1. `./templates/` (current directory)
2. `<exe_dir>/templates/`
3. `<exe_dir>/../templates/`
4. `<exe_dir>/../../templates/`
5. `<exe_dir>/../../../templates/`

This allows templates to work both in development and when installed.
