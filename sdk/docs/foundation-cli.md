<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation CLI Agent Guide

This is the AI-friendly usage guide for the `foundation` CLI. The implementation contract lives in
`sdk/crates/cli/SPEC.md`; use this guide when deciding which command to run for an SDK app workflow.

Use the canonical English command names in generated commands and docs.

## Agent Rules

- Run `foundation doctor` before diagnosing missing SDK tools or environment setup.
- Run app project commands from a directory inside the app project unless the command says otherwise.
- Do not run hardware-affecting commands such as `foundation sideload` or `foundation logs` unless the user asked to use a connected Passport Prime or hardware.
- Do not generate certificates, rotate signing identities, or edit `~/.foundation/signing` unless the user explicitly asked for signing setup.
- Prefer `foundation preview` for UI-only Slint checks before using simulator or hardware.
- Use `foundation sim` for hosted app behavior checks when the app needs runtime callbacks or generated manifests.
- Use `foundation build` for signed hardware artifacts. It may prompt for signing identity and can modify `target/keyos`.

## Command Matrix

| Command | Use When | Run From | Preconditions | Side Effects and Outputs | Common Failures |
| --- | --- | --- | --- | --- | --- |
| `foundation new [name] [--template TEMPLATE] [--no-git]` | Scaffold a new SDK app. | Parent directory where the new project should be created. | SDK root must be discoverable; templates must exist in the SDK. | Creates a project directory, app config, template files, resources, a project-local `.foundation-sdk/current` SDK mapping, and a Git repo unless `--no-git` is set. | Destination already exists, no templates found, invalid prompt values. |
| `foundation develop` | Enter the SDK Nix development shell. | SDK checkout, unpacked SDK bundle, or any place where `FOUNDATION_SDK_ROOT` is set. | `nix` installed; SDK root discoverable. | Starts a Nix shell and writes isolated shell config under `~/.foundation`. | Nix missing, SDK root not found, unsupported shell. |
| `foundation exit` | Clean Nix cache state after SDK development. | Anywhere. | `nix-collect-garbage` is useful but cleanup is best-effort. | Runs Nix garbage collection and removes `~/.cache/nix`; leaves SDK bundles and signing identities untouched. | Cleanup warnings if files are missing or commands fail. |
| `foundation doctor` | Inspect local SDK/app build readiness. | Anywhere, but SDK discovery is clearer from an SDK or app directory. | None beyond the CLI itself. | Prints tool, SDK, and target checks with suggested fixes. | Reports missing Nix, Cargo, Rust target, strip tool, cosign2, viewer, or Git. |
| `foundation build [--release]` | Build, stage, and sign a hardware-targeted app bundle. | Inside an app project containing `app-config.toml`. | Cargo, `arm-none-eabi-strip`, `cosign2`, signing config, SDK root, and suitable Nix shell. | Writes `target/keyos/<app-name>/app.elf`, `manifest.json`, `icon.bin`, and `resources`; converts app icons/images to `.raw`; copies app fonts. | Not in SDK shell, missing signing identity/config, missing icon, unsupported or invalid asset, missing permission template, Slint codegen/preflight failure. |
| `foundation clean` | Remove generated app build and theme artifacts. | Inside an app project containing `app-config.toml`. | None beyond the CLI itself. | Removes `target/` (cargo output, generated `target/foundation/**` UI/resources and theme `json`/`rust`/`slint`, and `target/keyos/**` bundles), the generated `manifest.toml`, the `ui/ui` mapping, and `.foundation-sdk/`; leaves authored source and `~/.foundation/themes` untouched. | Not in an app project, file removal failure. |
| `foundation theme` | Open the visual theme editor for the current app. | Inside an app project containing `app-config.toml`. | SDK root discoverable; bundled `foundation-theme-editor` or source checkout fallback available. | Ensures `theme` in `app-config.toml` points at an editable JSON file, creates `resources/theme.json` when needed, and passes that file to the editor so Save updates it directly. | Not in an app project, editor missing, invalid theme JSON, configured base theme not found. |
| `foundation preview [file] [options]` | Open a Slint UI in `foundation-slint-viewer` without a hardware build. | App project or path near the target `.slint` file. | Viewer available; SDK root preferred for `@ui` imports. | May run app build-script preflight to materialize `ui/gen`; launches viewer with SDK UI include paths. | Missing viewer, unresolved `@ui` imports, generated Slint files missing, Cargo preflight failure. |
| `foundation sim` | Build the app for hosted execution and launch the simulator. | Inside an app project containing `app-config.toml`. | SDK root discoverable; Cargo available; bundled or PATH simulator available. | Builds debug app, stages it under the SDK simulator app directory, writes manifest, copies app resources into that bundle, and launches simulator with `target/foundation/sim-resources`. | SDK root not found, simulator not found, hosted build failure, asset staging failure, launch failure after staging. |
| `foundation sideload [--release] [--no-run]` | Build/sign/upload an app to connected Passport Prime hardware and optionally launch it. | Inside an app project containing `app-config.toml`. | Everything needed by `build`; Passport unlocked and connected by USB; Developer Mode enabled. | Uploads signed `app.elf`, `manifest.json`, `icon.bin`, and generated `resources` over usb-debug to the sideloaded bundle directory named by the app ID; may send launch command over USB. | USB debug unavailable, Developer Mode disabled, upload failure, MCP launch failure, build/signing failure. |
| `foundation logs [--timeout SECONDS]` | Open the Passport USB log viewer for connected hardware. | Anywhere. | `foundation-keyos-log-viewer` bundled or on `PATH`; Passport connected over USB. | Launches the log viewer and attempts USB discovery/reconnect. | Viewer missing, no device found, USB permission or reconnect timeout issues. |
| `foundation cert gen [name] [--publisher-name NAME] [--contact-email EMAIL] [--support-url URL]` | Create a publisher signing identity for app builds. | App project when using app publisher defaults, otherwise anywhere. | OpenSSL available; explicit user intent to create signing material. | Writes private key, public key, certificate, and `cosign2.toml` under `~/.foundation/signing/<name>`. | Missing required publisher metadata, invalid identity name, OpenSSL failure, accidental duplicate identity. |
| `foundation cert print [name]` | Inspect a stored publisher certificate. | App project for default identity lookup, otherwise anywhere. | OpenSSL available; certificate exists. | Prints decoded certificate details. | Identity not found, certificate missing, OpenSSL failure. |
| `foundation plugin search <query>` | Search the configured Foundation plugin index. | Anywhere. | `FOUNDATION_PLUGIN_INDEX` set or `~/.foundation/plugin-index.toml` present. | Prints matching plugin entries. | Plugin index missing or no matches. |
| `foundation plugin install <plugin>` | Install a Foundation CLI plugin from the index or `owner/repo`. | Anywhere. | Network access; matching GitHub release asset for the current platform. | Downloads executable to `~/.foundation/plugins` and updates plugin cache. | Invalid plugin spec, release/asset not found, download failure. |
| `foundation plugin uninstall <plugin>` | Remove an installed plugin. | Anywhere. | Plugin installed under `~/.foundation/plugins`. | Deletes `foundation-<plugin>` and removes its cache entry. | Plugin not installed or file removal failure. |
| `foundation completions <bash\|zsh\|fish\|powershell> [--install]` | Generate or install shell completions. | Anywhere. | Shell name must be supported. | Prints completions to stdout or writes them to the user shell completion directory. | Unsupported shell, install path creation/write failure. |

## Project Files Agents Should Know

- `app-config.toml`: app metadata, app ID, icon path, theme path, version, permissions, and optional signing identity. The `icon` file is converted into the bundled `icon.bin` placed next to `app.elf`. The optional `theme` entry names an app-local theme JSON path, typically `resources/theme.json`.
- `permission_templates.toml`: named permission bundles expanded by `app-config.toml`.
- `.foundation-sdk/current`: generated SDK dependency mapping used by template `Cargo.toml` path dependencies. It is ignored and refreshed by `foundation new`, `build`, `sim`, and `preview`.
- `ui/app.slint`: default UI entrypoint for `foundation preview`.
- `ui/gen/*`: generated Slint router, translation, and export files. Do not edit by hand.
- `i18n/*.json`: app translations.
- `resources/`: app icon, app theme JSON, and app-owned resources. Put app images in `resources/images` and app fonts in `resources/fonts`; these app-owned files are searched before SDK shared resources.
- `images/` and `fonts/`: legacy app-owned asset directories. Hardware and simulator builds still stage them for compatibility, but new apps should prefer `resources/images` and `resources/fonts`.
- `target/keyos/<app-name>/`: hardware build output from `foundation build`, including `app.elf`, `manifest.json`, `icon.bin`, and app-local `resources`.

## App Assets

Use app-owned asset folders under `resources/` when adding new user assets:

- `resources/images/`: `.svg`, `.png`, `.jpg`, `.jpeg`, `.webp`, and `.bmp`
- `resources/fonts/`: `.ttf`, `.otf`, and `.ttc`

Reference app images from Slint with `Images.common("images/<relative-path-without-extension>")`. For example,
`resources/images/logo.svg` becomes `Images.common("images/logo")`. Dark variants keep the existing `-dark` naming
convention, and nine-slice filenames keep their parsed image name.

Fonts in `resources/fonts/` are registered before the Slint app starts. Use the font family's real name in Slint
`font-family` properties; the file name only controls how the file is copied.

SDK shared fonts, icons, and images are searched automatically after local app resources. App projects should not create
symlinks from `resources/fonts`, `resources/icons`, or `resources/images` back to the SDK.

## AI Skills and Slash Workflows

The SDK ships Codex and Claude skill files under `.agents/skills/` and `.claude/skills/` in the SDK bundle.
These are prompt workflows, not `foundation` CLI subcommands:

- `/foundation-localize <source-locale> <target-locale>` updates `i18n/<target-locale>.json` from
  `i18n/<source-locale>.json` while preserving keys, placeholders, product names, and valid JSON formatting.
- `/foundation-new-page <name>` adds a new routed Slint page to a router-enabled app by creating
  `ui/pages/<name>/page.slint` and `ui/pages/<name>/props.slint`, then wiring navigation only when the existing app
  structure makes the insertion point obvious.

## Hardware and Signing Notes

`foundation sideload` uploads files to a Passport Prime over usb-debug and can launch an app on the device.
Use `--no-run` when the user only wants the bundle uploaded; it still verifies passport-drive MCP and Developer Mode
before writing.

`foundation cert gen` creates long-lived signing material in the user's home directory. Treat this as a user
identity operation, not a routine build step. If a build fails because no signing identity exists, explain the
required setup instead of generating one silently.

`foundation build` signs app artifacts and may prompt for an identity. In non-interactive agent runs, prefer setting
`signing-identity` or `cosign2-config` in `app-config.toml` only when the user has identified the intended publisher.

## Recommended Troubleshooting Flow

1. Environment issue: run `foundation doctor`.
2. Slint/UI issue: run `foundation preview ui/app.slint --auto-reload` or a targeted preview command.
3. Hosted runtime issue: run `foundation sim`.
4. Hardware install issue: confirm user intent, then run `foundation sideload --no-run` before launching.
5. Signing issue: inspect `app-config.toml`, then explain the needed `foundation cert gen` or config change.
