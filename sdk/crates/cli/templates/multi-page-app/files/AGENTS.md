<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# {{friendly_app_name}} Agent Guide

This is a Foundation SDK app. The SDK ships its own guide, and it is the authority on both the CLI and on
what each SDK release changed. Read it before running `foundation` commands or porting this app to a newer
SDK. `foundation doctor` prints the SDK root used as `<sdk-root>` below.

- CLI reference: `<sdk-root>/docs/guide/src/foundation-cli.md`, or `sdk/docs/foundation-cli.md` in a KeyOS
  source checkout.
- Migration guide: `<sdk-root>/docs/guide/src/MIGRATIONS.md`, or `sdk/docs/MIGRATIONS.md` in a KeyOS source
  checkout.

Prefer `foundation preview` for UI checks, `foundation sim` for hosted runtime checks, and `foundation build`
only when signed hardware artifacts are needed. Do not run `foundation sideload`, `foundation logs`, or
`foundation cert gen` unless the user explicitly asks for hardware or signing work.

## Skills

`.claude/skills/` and `.agents/skills/` hold the skills the SDK ships: `/foundation-cli`,
`/foundation-localize`, `/foundation-new-page`, and `/foundation-sdk-migrate-newest`. They are copies of the
SDK's own files, so run `foundation skills` to refresh them after an SDK upgrade.

## Driving a real device (passport-drive MCP)

This app ships a `.mcp.json` that registers the `passport-drive` MCP server. With a Passport Prime connected
over USB, its tools let you act on the real device: capture screenshots, inject taps and swipes, and read its
logs. The device only exposes this debug channel when Developer Mode is enabled (in the device's Settings, under
Apps > Developer Mode); with it off, the connection is unavailable. On Linux, USB access also requires the
udev rules shipped at `<sdk-root>/share/99-passport.rules`; a permission error opening the device means they
are not installed (copy the file to `/etc/udev/rules.d/` and reload udev). Only drive hardware when the user
asks for on-device work.
