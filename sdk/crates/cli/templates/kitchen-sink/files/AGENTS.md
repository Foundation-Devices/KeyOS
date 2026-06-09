<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# {{friendly_app_name}} Agent Guide

This is a Foundation SDK app. Before running `foundation` commands, consult the SDK CLI guide:

- source checkout: `sdk/docs/foundation-cli.md`
- packaged SDK: `<sdk-root>/docs/guide/src/foundation-cli.md`

Use `foundation doctor` to inspect the local SDK environment. Prefer `foundation preview` for UI checks,
`foundation sim` for hosted runtime checks, and `foundation build` only when signed hardware artifacts are needed.
Do not run `foundation sideload`, `foundation logs`, or `foundation cert gen` unless the user explicitly asks for
hardware or signing work.
