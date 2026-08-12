<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation CLI Requirements Archive

This document is historical context only.

Plugin support described here is not part of supported SDK builds. Its
implementation is retained only behind the default-off `experimental-plugins`
feature while the trust model is redesigned.

The maintained current-state contract for the CLI is now [SPEC.md](SPEC.md).

`REQUIREMENTS.md` originally mixed:

- implemented behavior
- aspirational architecture
- future commands
- older file names such as `app.toml`

That made it too easy for the docs to drift away from the code. The repository now treats `SPEC.md` as the single source of truth for what the CLI actually does today.

## Retained Roadmap Themes

The main ideas worth preserving from the older requirements work are:

- keep the plugin system extensible, including richer metadata and possible `--describe` support
- keep MCP-related workflows explicit, with client integration for current hardware flows and any future server command surface added as a deliberate feature
- consider broader adoption of the `foundation-ui` terminal abstractions where they add real value
- evaluate future built-in commands like `sign`, `package`, `test`, `clean`, `add`, or `screenshot` as explicit roadmap work instead of implying they already exist

If any of those themes are revived, their behavior should be specified in [SPEC.md](SPEC.md) when implementation starts.
