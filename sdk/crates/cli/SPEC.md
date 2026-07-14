<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation CLI Specification

This is the canonical current-state specification for the `foundation` CLI in this repository.

- `SPEC.md` is the maintained source of truth.
- [REQUIREMENTS.md](REQUIREMENTS.md) is archived historical context plus roadmap notes.

## Scope

The currently implemented built-in commands are:

- `new`
- `develop`
- `exit`
- `build`
- `clean`
- `sideload`
- `sim`
- `cert`
- `theme`
- `themes`
- `doctor`
- `preview`
- `plugin`
- `completions`

The CLI also dispatches unknown subcommands to external `foundation-*` plugins discovered in `~/.foundation/plugins` or on `PATH`.

## Architecture Overview

The CLI workspace is organized around a few small responsibilities:

- `foundation-core`: shared project config, SDK discovery, project discovery, emitted manifest model
- `foundation-plugins`: plugin discovery, install/uninstall, cache, and external command dispatch
- `foundation-plugin-sdk`: support crate for plugin authors
- `foundation-ui`: terminal UI helpers
- `foundation-mcp`: MCP client helpers for host/device control flows

## Global Behavior

### Error handling conventions

All command modules use `anyhow` for error propagation:

- Bail with a clear message: `anyhow::bail!("Descriptive error message")`
- Add context to propagated errors: `result.context("Operation failed")?`
- Use `with_context(|| ...)` when the context message needs the failing value

Avoid mixing `anyhow!`-then-`map_err` with `bail!` in the same function — pick
one and stay consistent. Avoid `unwrap()` on any user-reachable path, even
when clap or the type system seemingly guarantees the value is present.

### User-scoped directories

The CLI uses `~/.foundation` for user-managed state:

| Path | Purpose |
|------|---------|
| `~/.foundation/.zshrc` (or `.bashrc`) | Shell rc for `foundation develop` (Foundation-managed, but free for user customization) |
| `~/.foundation/plugins/` | Installed external plugin binaries (`foundation-<name>`) |
| `~/.foundation/plugin-index.toml` | Plugin registry index (overridable via `FOUNDATION_PLUGIN_INDEX`) |
| `~/.foundation/signing/<identity>/private.pem` | ECDSA secp256k1 private key for app signing |
| `~/.foundation/signing/<identity>/public.pub` | Public key, hex-encoded |
| `~/.foundation/signing/<identity>/<identity>.crt` | X.509 publisher certificate |
| `~/.foundation/signing/<identity>/cosign2.toml` | cosign2 config pointing at the above |

The plugin cache lives in the user cache directory (resolved via the `dirs`
crate) as `foundation/plugin-cache.json`. On a stale or unreadable cache the
CLI warns to stderr and rebuilds; on a version mismatch it warns and rebuilds.

### SDK root discovery

Commands that need the SDK use `SdkRoot` discovery. The SDK root may be:

- explicitly set with `FOUNDATION_SDK_ROOT`
- discovered by walking upward from the current directory
- discovered relative to the current executable

Both repo checkouts and unpacked SDK bundles are supported.

### Stable app-facing SDK surface

App projects and templates should treat the following SDK paths as intentionally public:

- `<sdk-root>/lib/keyos/api/*`
- `<sdk-root>/lib/keyos/server`
- `<sdk-root>/lib/keyos/slint-keyos-platform/*`
- `<sdk-root>/lib/keyos/xous/*`
- `<sdk-root>/ui/ui`
- `<sdk-root>/resources`
- `<sdk-root>/lib/templates`

Commands and templates should not rely on broader repo-internal source layout beyond those paths.

### Project discovery

Project commands discover the project root by walking upward until they find `app-config.toml`.

This shared discovery is used by:

- `build`
- `sim`

`preview` accepts an explicit `.slint` path, but it still walks upward from the target file to find the nearest Cargo project when it needs build-script-driven Slint code generation.

### Command stability

The CLI does not preserve unreleased compatibility aliases for renamed commands.

- Only the canonical current command names are supported.
- English command names remain stable and always work.

## Project Contract

### `app-config.toml`

The canonical application config shape mirrors `foundation_core::AppConfig`.

Example:

```toml
app-name = "my-app"
friendly-app-name = "My App"
launcher-app-name = "My App"
description = "Example application"
icon = "resources/icon.svg"
theme = "resources/theme.json"
app-id = "0x00112233445566778899aabbccddeeff"
version = "0.1.0"
min-keyos-version = "1.0.0"
signing-identity = "Example Company"
cosign2-config = "~/.foundation/signing/my-app/cosign2.toml"

[publisher]
name = "Example Company"
contact-email = "support@example.com"
support-url = "https://example.com/support"

[permissions]
template = ["gui-app"]
"os/settings" = ["GetDeviceName"]
```

Observed behavior:

- `app-name` is the Cargo package name and build output directory name
- `launcher-app-name` falls back to `friendly-app-name`
- `friendly-app-name` and `launcher-app-name` may contain only ASCII letters, numbers, spaces, and hyphens
- `app-id` must be a `0x`-prefixed even-length hex string
- `icon` is validated relative to the project root and must be exactly `96x96px`
- `theme`, when present, names either an app-local editable JSON file or a base theme id
- `permissions.template` expands through `permission_templates.toml`
- explicit permission entries merge with expanded templates
- emitted `manifest.json` uses resolved concrete permissions, not template aliases
- `publisher` metadata is used by `foundation cert gen` as defaults for certificate generation
- `signing-identity` optionally selects a publisher identity under `~/.foundation/signing/<identity>`

### `permission_templates.toml`

If `permissions.template` is non-empty, the project must include `permission_templates.toml`.

Example:

```toml
[gui-app]
"os/gui-server" = ["RegisterAppMessage", "RequestRedraw"]
"os/settings" = ["GetDeviceName"]
```

### Emitted `manifest.json`

`build` and `sim` emit the same manifest shape, serialized through the shared
`app-manifest` crate used by KeyOS runtime manifest readers.

Example:

```json
{
  "appName": {
    "en": "My App"
  },
  "appId": "0x00112233445566778899aabbccddeeff",
  "permissions": {
    "os/gui-server": ["RegisterAppMessage", "RequestRedraw"],
    "os/settings": ["GetDeviceName"]
  }
}
```

Observed behavior:

- `appId` is lowercased in emitted JSON
- `appName.en` comes from `launcher-app-name`, falling back to `friendly-app-name`
- `permissions` contains resolved concrete KeyOS permission entries

### App assets

App-owned images and fonts should live under the app's `resources` directory:

- `resources/images/` contains app images referenced from Slint with `Images.image("<relative-name>")`
- `resources/fonts/` contains app fonts registered before the Slint app starts
- `resources/icon.svg` remains the default app icon source referenced by `app-config.toml`
- `resources/theme.json` is the default app-local theme source referenced by `app-config.toml`

`foundation build` converts supported app images to KeyOS raw image files and copies supported fonts into the generated
app bundle resources. `foundation sim` stages original app images and fonts for hosted runtime loading.

Supported app image source extensions are `.svg`, `.png`, `.jpg`, `.jpeg`, `.webp`, and `.bmp`. Supported app font
extensions are `.ttf`, `.otf`, and `.ttc`.

Legacy top-level `images/` and `fonts/` directories are still staged for compatibility with older app projects. SDK shared
fonts, icons, and images are searched automatically after app-local `resources/`; app projects should not symlink
`resources/fonts`, `resources/icons`, or `resources/images` back to the SDK.

## Built-in Templates

The shipped templates are:

- `default-app`
- `multi-page-app`
- `kitchen-sink`

Template variables currently provided by `foundation new`:

- `app_name`
- `friendly_app_name`
- `launcher_app_name`
- `description`
- `publisher_name`
- `contact_email`
- `support_url`
- `icon`
- `selected_theme_id`
- `app_id`
- `version`
- `min_keyos_version`
- `sdk_keyos_root` - preferred for Cargo path dependencies on KeyOS crates
- `sdk_root` - compatibility/advanced variable for the project-local SDK mapping root
- `sdk_ui_root` - compatibility/advanced variable for the shared Slint UI library
- `sdk_path` - compatibility alias for `sdk_keyos_root`

The SDK path variables expand to project-relative paths under `.foundation-sdk/current`, which `foundation new`, `build`, `sim`, and `preview` create or refresh from the active SDK. For installed SDKs this targets the installer-managed `current` symlink instead of a concrete `foundation-sdk-<version>-<target>` directory. Generated Cargo workspaces exclude `.foundation-sdk` so SDK path dependencies keep their own workspace metadata instead of inheriting from the app workspace.
Shipped templates should use `sdk_keyos_root` for KeyOS source dependencies; the other SDK path variables remain available for SDK-provided and user templates that already consume them.

## Command Specifications

### `new`

Signature:

```text
foundation new <name> [--template TEMPLATE] [--theme THEME_ID]
                      [--friendly-name NAME] [--launcher-name NAME]
                      [--description TEXT] [--publisher-name NAME]
                      [--contact-email EMAIL] [--support-url URL]
                      [--app-id ID] [--app-version VERSION]
                      [--min-keyos-version VERSION] [--no-git]
```

Behavior:

- `name` is required; it becomes the project directory and Cargo package name, so it must contain only ASCII letters, digits, hyphens, and underscores and begin with a letter or underscore
- Every configurable field has a flag; a supplied flag is used as-is and its prompt is skipped
- For a field that was not supplied:
  - when stdin is a terminal, prompts for it (pre-filled with the default) and re-asks on invalid input
  - when stdin is not a terminal, uses the default without prompting, so `new` runs unattended
- Defaults: `--template` is `default-app`, `--theme` is `default_theme`; per-field defaults come from the selected template's `[variables]` in `template.toml`, falling back to built-in values
- Prompt-backed fields: friendly app name, launcher app name, description, publisher name, contact email, support website URL, app ID, version, minimum KeyOS version
- The icon path is not configurable; it is always the template default (`resources/icon.svg`)
- `--app-id` generates a random 16-byte hex ID with `0x` prefix only when omitted; an explicitly empty value stays empty
- Description, version, and minimum KeyOS version reject empty values; publisher name, contact email, and support URL may be empty
- `--theme` must name an installed theme id
- Rejects friendly and launcher app names containing characters outside ASCII letters, numbers, spaces, and hyphens
- Creates the project directory from the selected template
- Writes `app-config.toml`, `permission_templates.toml`, template source files, and resources
- Initializes a Git repository by default with initial branch `main`
- Does not create an initial commit
- `--no-git` skips repository initialization
- If Git is unavailable, scaffolding still succeeds and the command prints a warning instead of failing

### `develop`

Signature:

```text
foundation develop
```

Behavior:

- Requires `nix`
- Resolves the SDK root
- Ensures a Foundation shell config exists under `~/.foundation/`
- Prefers the user's supported login shell (`zsh` or `bash`), falling back to `zsh`/`bash` from `PATH`
- Launches:

```text
nix develop <sdk-root> -c <shell> <shell-specific-interactive-args>
```

- Uses `ZDOTDIR=~/.foundation` for `zsh`, or `--rcfile ~/.foundation/.bashrc` for `bash`, so the Foundation shell config is isolated

### `exit`

Signature:

```text
foundation exit
```

Behavior:

- Runs `nix-collect-garbage -d`
- Removes `~/.cache/nix` if present
- Treats cleanup as best-effort and reports step-by-step status
- Does not remove installed SDK bundles or signing identities

### `build`

Signature:

```text
foundation build [--release]
```

Purpose:

- Build, stage, and sign a hardware-targeted KeyOS app bundle

Behavior:

- Requires a discoverable project with `app-config.toml`
- Requires `cargo`, `arm-none-eabi-strip`, `cosign2`, and a usable `cosign2.toml`
- Uses `cosign2-config` from `app-config.toml` when provided
- Otherwise resolves a signing identity from `~/.foundation/signing`:
  - use `signing-identity` from `app-config.toml` when provided
  - otherwise prefer an identity whose name matches `[publisher].name`
  - otherwise use the only configured identity
  - otherwise prompt the user to choose a publisher identity interactively
  - if the build is non-interactive and multiple identities exist, fail and ask the user to set `signing-identity` or `cosign2-config`
- If not already inside a suitable Nix shell, prompts the user to run `foundation develop`
- If the user accepts, launches `foundation develop`, exits after the shell closes, and asks the user to rerun `foundation build`
- Ensures the app-local `ui/ui` mapping and generated private SDK resource search tree exist, generates the configured app theme Rust module, and runs build-script Slint code generation before the hardware build
- Runs:

```text
cargo build --target armv7a-unknown-xous-elf --package <app-name> [--release]
```

- Uses:

```text
RUSTFLAGS="--cfg keyos -C relocation-model=pic -C link-arg=-pie"
```

- Strips the binary into:

```text
target/keyos/<app-name>/app.elf
```

- Writes:

```text
target/keyos/<app-name>/manifest.json
```

- Converts the app icon into bundle-local raw image data for app-manager UI surfaces:

```text
target/keyos/<app-name>/icon.bin
```

- Refuses to build when the source icon is not exactly `96x96px`

- Converts and stages app assets into:

```text
target/keyos/<app-name>/resources/
```

  - `app-config.toml` `icon` -> `target/keyos/<app-name>/icon.bin`
  - `resources/images/**/*.{svg,png,jpg,jpeg,webp,bmp}` -> `resources/images/**/*.raw`
  - `resources/fonts/**/*.{ttf,otf,ttc}` -> `resources/fonts/**/*`
  - legacy `images/` and `fonts/` sources are staged into the same app-local `resources/` tree

- Signs `app.elf` in place with `cosign2`

### `clean`

Signature:

```text
foundation clean
```

Purpose:

- Remove the generated build and theme artifacts for the current app, leaving authored source untouched

Behavior:

- Requires a discoverable project with `app-config.toml`
- Removes these generated paths when present:
  - `target/` - cargo output plus the generated `target/foundation/**` UI, resources, and sim-resources trees, the generated theme `target/foundation/themes/{json,rust,slint}` files, and the `target/keyos/<app-name>/**` hardware bundles
  - `manifest.toml` - the compatibility manifest written before each cargo build
  - `ui/ui` - the SDK UI library mapping (symlink or copied snapshot)
  - `.foundation-sdk/` - the project-local SDK dependency mapping
- Never touches authored content under `resources/` (app icon, theme JSON, images, fonts)
- Treats a missing path as a no-op and reports step-by-step status
- Does not touch the shared theme cache under `~/.foundation/themes`

### `sim`

Signature:

```text
foundation sim
```

Purpose:

- Build the app for hosted execution, stage it into the SDK simulator app directory, and start the simulator

Behavior:

- Requires a discoverable SDK root and project root
- Ensures shared `@ui` sources plus generated Slint router/translation files are ready before the hosted build
- Runs:

```text
cargo build --package <app-name>
```

with:

```text
RUSTFLAGS="--cfg keyos"
```

- Always stages debug output
- Copies the built binary and manifest to:

```text
<sdk-root>/target/apps/<app-name>/
```

- Writes:
  - `<sdk-root>/target/apps/<app-name>/app.elf`
  - `<sdk-root>/target/apps/<app-name>/manifest.json`
- Stages hosted app resources into:
  - `<project-root>/target/foundation/sim-resources/`
- Copies those hosted resources to:
  - `<sdk-root>/target/apps/<app-name>/resources/`
- Sets `FOUNDATION_APP_RESOURCES_DIR` for the simulator so hosted image and font loading can resolve app resources.
- Launch resolution order:
  1. bundled `foundation-simulator`
  2. `foundation-simulator` on `PATH`
  3. repo-layout fallback to `just sim` in the SDK KeyOS root
- If launch fails after staging, the error includes the staged app path

### `sideload`

Signature:

```text
foundation sideload [--release] [--no-run]
```

Purpose:

- Build and sign a hardware-targeted KeyOS app bundle, upload it to a connected Passport Prime over usb-debug, and launch it over the same usb-debug channel by default

Behavior:

- Reuses the `build` flow and its hardware artifacts:
  - `target/keyos/<app-name>/app.elf`
  - `target/keyos/<app-name>/manifest.json`
  - `target/keyos/<app-name>/icon.bin`
- Starts passport-drive MCP and checks that Developer Mode is enabled before uploading the app bundle.
- Uploads the signed app bundle to:

```text
keyos/sideloaded-apps/<app-id>/
```

- Writes:
  - `keyos/sideloaded-apps/<app-id>/app.elf`
  - `keyos/sideloaded-apps/<app-id>/manifest.json`
  - `keyos/sideloaded-apps/<app-id>/icon.bin`
- The `<app-id>` directory is the normalized 32-character lowercase hex app ID without the `0x` prefix.
- Uploads generated app resources from `target/keyos/<app-name>/resources` into:

```text
keyos/sideloaded-apps/<app-id>/resources/
```

- Launches the app through passport-drive MCP unless `--no-run` is set.
- Upload failures tell the user to check that the device is unlocked, connected by USB, Developer Mode is enabled, and no other process is using the USB debug interface.
- MCP launch failures tell the user to check that Developer Mode is enabled.
- If upload succeeds but launch fails, the error reports that the app bundle was uploaded and includes the failing response.
- `--no-run` skips launch after upload, but the command still probes passport-drive MCP and Developer Mode before uploading.

### `cert`

Signature:

```text
foundation cert gen [name] [--publisher-name NAME] [--contact-email EMAIL] [--support-url URL]

foundation cert print [name]
```

Behavior:

- Uses the provided name or prompts for one
- Defaults to `[publisher].name` when `app-config.toml` is available, otherwise `developer`
- Accepts any non-empty publisher identity name that does not contain path separators
- Reads publisher metadata from `app-config.toml` when available and prompts for any missing required fields
- Requires:
  - publisher name
  - contact email address
  - support website URL
- Writes:
  - `~/.foundation/signing/<name>/private.pem`
  - `~/.foundation/signing/<name>/public.pub`
  - `~/.foundation/signing/<name>/<name>.crt`
  - `~/.foundation/signing/<name>/cosign2.toml`
- Uses OpenSSL to generate:
  - a secp256k1 private key
  - a compressed public key hex file
  - a self-signed X.509 code-signing certificate
  - matching `cosign2` configuration

### `doctor`

Signature:

```text
foundation doctor
```

Behavior:

- Checks:
  - `nix`
  - active Nix shell markers
  - SDK root discovery
  - `cargo`
  - `armv7a-unknown-xous-elf` target support
  - `arm-none-eabi-strip`
  - `cosign2`
  - `foundation-asset-tool`
  - `foundation-slint-viewer` or `slint-viewer`
  - `git`
  - app-config names and app icon size when run inside an app project
- Prints pass/fail plus suggested fixes
- Exits non-zero when any check fails, so scripts and CI can gate on it

### `preview`

Signature:

```text
foundation preview [file] [options]
```

Behavior:

- Defaults to `ui/app.slint`
- Uses `foundation-slint-viewer` as the primary viewer and accepts `slint-viewer` as a fallback
- If the target file belongs to a Cargo project with `build.rs`, runs a preflight `cargo check --quiet --package <package>` to materialize generated Slint files such as `ui/gen/router.slint`, `ui/gen/navigate.slint`, `ui/gen/tr.slint`, and `ui/gen/exports.slint`
- If plain-shell `cargo check` cannot run and the SDK root is known, retries the preflight through:

```text
nix develop <sdk-root> --command cargo check --quiet --package <package>
```

- Launches the viewer with `-L ui=<project>/target/foundation/ui/ui` when the generated project UI
  mapping exists, falling back to `-L ui=<sdk-root>/ui/ui`
- Forwards these viewer options:
  - `-I <include-path>` repeatable
  - `--style <style>`
  - `--component <component>`
  - `--backend <backend>`
  - `--auto-reload`
  - `--load-data <path>`
  - `--save-data <path>`
  - `--on <callback> <handler>` repeatable
  - `--i18n-dir <path>`
  - `--locale <locale>`

### `logs`

Signature:

```text
foundation logs [--timeout SECONDS]
```

Behavior:

- Resolves `foundation-keyos-log-viewer` from the SDK bundle first, then from `PATH`
- Launches the Passport USB log viewer and forwards the reconnect timeout
- Uses a default reconnect timeout of 3 seconds
- Requires connected Passport hardware for useful output

### `plugin`

Signature:

```text
foundation plugin <install|uninstall|search> ...
```

Behavior:

- Provides the built-in plugin-management subcommands

#### `plugin search`

Signature:

```text
foundation plugin search <query>
```

Behavior:

- Searches the local plugin index
- Uses:
  - `FOUNDATION_PLUGIN_INDEX`, if set
  - otherwise `~/.foundation/plugin-index.toml`
- Matches against plugin name, description, and tags

#### `plugin install`

Signature:

```text
foundation plugin install <plugin>
```

Accepted inputs:

- a plugin name from the local index
- a direct GitHub repository reference `owner/repo`

Behavior:

- Resolves index entries when the input is not already `owner/repo`
- Fetches the latest GitHub release for the repository
- Chooses the asset whose filename ends with the current platform target triple
- Downloads the binary to `~/.foundation/plugins`
- Marks it executable on Unix
- Updates the plugin cache
- Installs binaries as `foundation-<plugin-name>`
- Direct `owner/repo` installs normalize to the repository name, stripping a leading `foundation-` prefix when present

#### `plugin uninstall`

Signature:

```text
foundation plugin uninstall <plugin>
```

Behavior:

- Removes `~/.foundation/plugins/foundation-<plugin>`
- Removes the cache entry for that plugin

### `completions`

Signature:

```text
foundation completions <bash|zsh|fish|powershell> [--install]
```

Behavior:

- Without `--install`, writes completions to stdout
- With `--install`, writes to the standard user-scoped location for the target shell
- Generated completions include installed plugin subcommands from `~/.foundation/plugins`

### External plugin dispatch

Before argument parsing, `foundation` resolves its first argument against external `foundation-<name>` plugins. Resolution is unconditional, so an installed plugin can shadow a built-in command or a global flag (`-h`, `--help`, `-V`, `--version`).

Resolution sources:

- installed plugin cache
- `~/.foundation/plugins`
- `PATH`

When resolved, `foundation <name> ...` execs the external plugin binary directly; otherwise argument parsing proceeds normally. Resolution runs on every invocation, so a built-in command also triggers a plugin-cache lookup (and a `PATH` rescan on a cache miss).

## Deferred / Planned Features

These are intentionally not part of the current behavioral contract:

- extra built-ins mentioned in older design docs:
  - `sign`
  - `package`
  - `test`
  - `clean`
  - `add`
  - `screenshot`
- a public MCP server command surface from `foundation-mcp`
- richer plugin metadata / `--describe`
- broad adoption of the `foundation-ui` terminal abstractions across all commands
