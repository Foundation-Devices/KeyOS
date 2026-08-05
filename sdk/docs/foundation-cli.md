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
| `foundation new <name> [--template TEMPLATE] [per-field flags] [--no-git]` | Scaffold a new SDK app. `name` is required; every prompted field has a flag, so with no TTY it runs unattended from defaults. The app inherits the built-in Base Theme, with any app-specific theme overrides supplied by the template. | Parent directory where the new project should be created. | SDK root must be discoverable; templates must exist in the SDK. | Creates a project directory, app config, template files, resources, a project-local `.foundation-sdk/current` SDK mapping, and a Git repo unless `--no-git` is set. | Missing `name`, destination already exists, no templates found, invalid field values. |
| `foundation develop` | Enter the SDK Nix development shell. | SDK checkout, unpacked SDK bundle, or any place where `FOUNDATION_SDK_ROOT` is set. | `nix` installed; SDK root discoverable. | Starts a Nix shell and writes isolated shell config under `~/.foundation`. | Nix missing, SDK root not found, unsupported shell. |
| `foundation exit` | Clean Nix cache state after SDK development. | Anywhere. | `nix-collect-garbage` is useful but cleanup is best-effort. | Runs Nix garbage collection and removes `~/.cache/nix`; leaves SDK bundles and signing identities untouched. | Cleanup warnings if files are missing or commands fail. |
| `foundation doctor` | Inspect local SDK/app build readiness. | Anywhere, but SDK discovery is clearer from an SDK or app directory. | None beyond the CLI itself. | Prints tool, SDK, and target checks with suggested fixes. | Reports missing Nix, Cargo, Rust target, strip tool, cosign2, viewer, or Git. |
| `foundation build [--release]` | Build, stage, and sign a hardware-targeted app bundle. | Inside an app project containing `app-config.toml`. | Cargo, `arm-none-eabi-strip`, `cosign2`, signing config, SDK root, and suitable Nix shell. | Writes `target/keyos/<app-name>/app.elf`, `manifest.json`, `icon.bin` (plus `icon-dark.bin` when the app ships a dark icon), and `resources`; converts app icons/images to `.raw`; copies app fonts. | Not in SDK shell, missing signing identity/config, missing icon, unsupported or invalid asset, missing permission template, Slint codegen/preflight failure. |
| `foundation clean` | Remove generated app build and theme artifacts. | Inside an app project containing `app-config.toml`. | None beyond the CLI itself. | Removes `target/` (cargo output, generated `target/foundation/**` UI/resources and theme `json`/`rust`/`slint`, and `target/keyos/**` bundles), the generated `manifest.toml`, the `ui/ui` mapping, and `.foundation-sdk/`; leaves authored source and `~/.foundation/themes` untouched. | Not in an app project, file removal failure. |
| `foundation theme [filename]` | Open the visual theme editor for an explicit theme JSON, or for the current app when no filename is given. | Anywhere with a filename; otherwise inside an app project containing `app-config.toml`. | SDK root discoverable; bundled `foundation-theme-editor` or source checkout fallback available. | Opens the supplied file directly without rewriting it. Without a filename, ensures `theme` in `app-config.toml` points at an editable JSON file, creates `resources/theme.json` when needed, and passes that file to the editor so Save updates it directly. | Editor missing, invalid theme JSON, or (without a filename) not in an app project/configured base theme not found. |
| `foundation preview [file] [options]` | Open a Slint UI in `foundation-slint-viewer` without a hardware build. | App project or path near the target `.slint` file. | Viewer available; SDK root preferred for `@ui` imports. | May run app build-script preflight to materialize `ui/gen`; launches viewer with SDK UI include paths. | Missing viewer, unresolved `@ui` imports, generated Slint files missing, Cargo preflight failure. |
| `foundation sim` | Build the app for hosted execution and launch the simulator. | Inside an app project containing `app-config.toml`. | SDK root discoverable; Cargo available; bundled or PATH simulator available. | Builds debug app, stages it under the SDK simulator app directory, writes manifest, copies app resources into that bundle, and launches simulator with `target/foundation/sim-resources`. | SDK root not found, simulator not found, hosted build failure, asset staging failure, launch failure after staging. |
| `foundation pack [--release] [--out PATH]` | Build/sign the app and wrap the bundle into one `.app` archive a user can install from Settings, with no USB debug or Developer Mode. | Inside an app project containing `app-config.toml`. | Everything needed by `build`. | Writes `target/keyos/<app-name>.app`: a gzip-compressed tar of `manifest.json` (first), then every file it hashes, sorted by name (`app.elf`, `icon-dark.bin` and `icon.bin` when present, `resources`). | Build/signing failure, `--out` pointing inside the bundle or at a symlink, bundle over the 64 MiB an install accepts, archive write failure. |
| `foundation sideload [--release] [--no-run]` | Build/sign/upload an app to connected Passport Prime hardware and optionally launch it. | Inside an app project containing `app-config.toml`. | Everything needed by `build`; Passport unlocked and connected by USB; Developer Mode enabled. | Uploads signed `app.elf`, `manifest.json`, `icon.bin` (plus `icon-dark.bin` when present), and generated `resources` over usb-debug to the sideloaded bundle directory named by the app ID; may send launch command over USB. | USB debug unavailable, Developer Mode disabled, upload failure, MCP launch failure, build/signing failure. |
| `foundation logs [--timeout SECONDS]` | Open the Passport USB log viewer for connected hardware. | Anywhere. | `foundation-keyos-log-viewer` bundled or on `PATH`; Passport connected over USB. | Launches the log viewer and attempts USB discovery/reconnect. | Viewer missing, no device found, USB permission or reconnect timeout issues. |
| `foundation cert gen [name] [--publisher-name NAME] [--contact-email EMAIL] [--support-url URL]` | Create a publisher signing identity for app builds. | App project when using app publisher defaults, otherwise anywhere. | OpenSSL available; explicit user intent to create signing material. | Writes private key, public key, certificate, and `cosign2.toml` under `~/.foundation/signing/<name>`. | Missing required publisher metadata, invalid identity name, OpenSSL failure, accidental duplicate identity. |
| `foundation cert print [name]` | Inspect a stored publisher certificate. | App project for default identity lookup, otherwise anywhere. | OpenSSL available; certificate exists. | Prints decoded certificate details. | Identity not found, certificate missing, OpenSSL failure. |
| `foundation cert fingerprint <cert>` | Print the stable identity users should compare before allowing a publisher. | Anywhere. | OpenSSL available; PEM or DER X.509 certificate exists and contains a secp256k1 public key. | Prints the full and short canonical publisher fingerprint; does not modify the certificate. | Certificate missing or malformed, unsupported public key or curve, OpenSSL failure. |
| `foundation cert install [name]` | Review a stored publisher certificate and allow it on connected hardware. | App project for default identity lookup, otherwise anywhere. | OpenSSL available; certificate exists; Passport unlocked and connected by USB with Developer Mode enabled; interactive user confirmation. | Shows the unverified-identity warning and full/short fingerprint before the prompt, then installs the certificate only after the user chooses Allow; declining makes no changes. | Non-interactive session, identity not found, fingerprint extraction failure, USB debug unavailable, Developer Mode disabled. |
| `foundation plugin search <query>` | Search the configured Foundation plugin index. | Anywhere. | `FOUNDATION_PLUGIN_INDEX` set or `~/.foundation/plugin-index.toml` present. | Prints matching plugin entries. | Plugin index missing or no matches. |
| `foundation plugin install <plugin>` | Install a Foundation CLI plugin from the index or `owner/repo`. | Anywhere. | Network access; matching GitHub release asset for the current platform. | Downloads executable to `~/.foundation/plugins` and updates plugin cache. | Invalid plugin spec, release/asset not found, download failure. |
| `foundation plugin uninstall <plugin>` | Remove an installed plugin. | Anywhere. | Plugin installed under `~/.foundation/plugins`. | Deletes `foundation-<plugin>` and removes its cache entry. | Plugin not installed or file removal failure. |
| `foundation completions <bash\|zsh\|fish\|powershell> [--install]` | Generate or install shell completions. | Anywhere. | Shell name must be supported. | Prints completions to stdout or writes them to the user shell completion directory. | Unsupported shell, install path creation/write failure. |

## Project Files Agents Should Know

- `app-config.toml`: app metadata, app ID, icon path, theme path, version, permissions, optional QR match rules, and optional signing identity. The `icon` file is converted into the bundled `icon.bin` placed next to `app.elf`. A `<stem>-dark.(svg|png)` sibling of the icon (e.g. `resources/icon-dark.svg`) is converted into `icon-dark.bin` beside it and used in dark theme; without one, the light icon serves both themes. The optional `theme` entry names an app-local theme JSON path, typically `resources/theme.json`.
- `permission_templates.toml`: named permission bundles expanded by `app-config.toml`.
- `.foundation-sdk/current`: generated SDK dependency mapping used by template `Cargo.toml` path dependencies. It is ignored and refreshed by `foundation new`, `build`, `sim`, and `preview`.
- `ui/app.slint`: default UI entrypoint for `foundation preview`.
- `ui/gen/*`: generated Slint router, translation, and export files. Do not edit by hand.
- `i18n/*.json`: app translations.
- `resources/`: app icon, app theme JSON, and app-owned resources. Put app images in `resources/images` and app fonts in `resources/fonts`; these app-owned files are searched before SDK shared resources.
- `images/` and `fonts/`: legacy app-owned asset directories. Hardware and simulator builds still stage them for compatibility, but new apps should prefer `resources/images` and `resources/fonts`.
- `target/keyos/<app-name>/`: hardware build output from `foundation build`, including `app.elf`, `manifest.json`, `icon.bin` (and `icon-dark.bin` when the app ships a dark icon), and app-local `resources`.
- `target/keyos/<app-name>.app`: the same bundle packed by `foundation pack` into one file, for installing on a device from a USB drive or the airlock.

## QR Match Rules

Apps can register QR match rules for the Launcher QR scanner tile in `app-config.toml`.
When the Launcher scan result matches one app, KeyOS opens that app directly. When several apps match,
KeyOS asks the user which app should handle the QR data.

Apps launched from a QR match also need permission to read the handoff payload. Declaring any rule
grants the app the `os/gui-server` `GetPendingNavRequest` permission, so the app config does not have
to request it.

Use `[[qr-match-rules]]` entries. Like every other key in `app-config.toml`, the rule fields are
kebab-case; the CLI converts them into the camelCase manifest form when it generates `manifest.json`.

```toml
[[qr-match-rules]]
id = "otpauth"
priority = 5
id-localizations = { en = "OTP Auth" }
sub-rules = { qr = { QR = { regex-pattern = "^otpauth://" } } }

[[qr-match-rules]]
id = "crypto-psbt"
id-localizations = { en = "PSBT" }
sub-rules = { ur = { UR = { ur-type = "psbt" } } }
```

`id` is the stable rule identifier reported to your app with the scan result. `id-localizations` is the
user-facing label shown when the Launcher needs a choice. `priority` is optional and defaults to `3`;
valid values are `1` through `5`, with higher values sorted first.

Each entry in `sub-rules` is keyed by a name you choose and tagged `QR` or `UR`. QR sub-rules match raw
QR payloads: use `min-len`, `max-len`, and `regex-pattern` to narrow the match. UR sub-rules match UR 2.0
payloads by `ur-type`. Unknown fields are rejected, so a misspelling fails the build instead of shipping
a rule that never matches.

## App Assets

Use app-owned asset folders under `resources/` when adding new user assets:

- `resources/images/`: `.svg`, `.png`, `.jpg`, `.jpeg`, `.webp`, and `.bmp`
- `resources/fonts/`: `.ttf`, `.otf`, and `.ttc`

Reference app images from Slint with `Images.image("<relative-path-without-extension>")`. For example,
`resources/images/logo.svg` becomes `Images.image("logo")`.

Fonts in `resources/fonts/` are registered before the Slint app starts. Use the font family's real name in Slint
`font-family` properties; the file name only controls how the file is copied.

SDK shared fonts, icons, and images are searched automatically after local app resources. App projects should not create
symlinks from `resources/fonts`, `resources/icons`, or `resources/images` back to the SDK.

## Allowed Publishers and Fingerprints

In KeyOS v1, the user **allows** a publisher; Foundation does not verify that publisher's identity. Allowing a
publisher means that apps signed by its key may run on that user's Passport. The certificate's publisher name,
organization, email address, and support URL are self-asserted claims, not proof of who controls the key.

Before `foundation cert install` asks the user to allow a publisher, it displays:

> Foundation has NOT verified this publisher's identity

This warning means that neither Foundation nor the device has confirmed the claimed identity. Compare the displayed
fingerprint with a value the publisher provides through a separate, official channel before choosing Allow. The
fingerprint is the publisher's key identity; a matching claimed name is not sufficient.

The canonical publisher fingerprint is the SHA-256 digest of the compressed 33-byte secp256k1 public key, rendered
as 64 lowercase hexadecimal characters. The short display form is the first four bytes and last four bytes of that
digest, rendered as `xxxxxxxx…xxxxxxxx`. The short form is for recognition only; compare the full fingerprint when
making the allow decision.

`foundation cert gen` prints both forms after creating the key and certificate.
`foundation cert fingerprint <cert>` prints them again later. Publishers should place the full fingerprint on their
official website and/or official GitHub repository so users can verify it out-of-band.

After an allow decision, the CLI passes the reviewed full fingerprint to passport-drive.
Passport-drive re-parses the bytes it will send, and firmware previews the same certificate again;
both layers require the fingerprint to match before import.

Publishers are also encouraged to publish this DNS TXT record:

```text
_keyos-publisher.<domain> TXT "v=1; k=secp256k1; fp=<hex>"
```

Replace `<domain>` with the publisher's official domain and `<hex>` with the full 64-character lowercase
fingerprint. This is a publication convention only in v1: KeyOS and Foundation do not retrieve or verify the record.
It is intended as a stable home for a future attestation service.

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

`foundation cert install` is an identity-allow operation and requires an interactive confirmation. Do not bypass or
automate the warning. The user must compare the full fingerprint against the publisher's official website or GitHub
before allowing it.

`foundation build` signs app artifacts and may prompt for an identity. In non-interactive agent runs, prefer setting
`signing-identity` or `cosign2-config` in `app-config.toml` only when the user has identified the intended publisher.

## Recommended Troubleshooting Flow

1. Environment issue: run `foundation doctor`.
2. Slint/UI issue: run `foundation preview ui/app.slint --auto-reload` or a targeted preview command.
3. Hosted runtime issue: run `foundation sim`.
4. Hardware install issue: confirm user intent, then run `foundation sideload --no-run` before launching.
5. Signing issue: inspect `app-config.toml`, then explain the needed `foundation cert gen` or config change.
