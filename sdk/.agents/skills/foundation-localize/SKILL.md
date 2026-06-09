---
name: foundation-localize
description: Translate a Foundation SDK app i18n JSON file from one locale to another. Invoke as /foundation-localize <source-locale> <target-locale>, for example /foundation-localize en es, when updating i18n/<target-locale>.json from i18n/<source-locale>.json.
argument-hint: <source-locale> <target-locale>
arguments: [source_locale, target_locale]
disable-model-invocation: true
---

<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation Localize

Localize the current Foundation SDK app from `$source_locale` to `$target_locale`.

Workflow:

1. Require both arguments. If either is missing, ask for the missing locale code.
2. Find the app root by walking up to `app-config.toml`.
3. Read `i18n/$source_locale.json`. If it does not exist, stop and report the missing source file.
4. Read `i18n/$target_locale.json` if it exists; otherwise create it.
5. Produce valid JSON whose keys and nesting match the source file.
6. Translate human-facing string values into `$target_locale`.
7. Preserve placeholders and tokens exactly:
   - `{{name}}` template placeholders
   - `%{name}` interpolation placeholders
   - command names, file paths, app IDs, URLs, and translation IDs
   - product names such as Foundation, KeyOS, Passport, Passport Prime, Slint, and Nix
8. Do not modify the source locale file.
9. Format the target JSON with two-space indentation and a trailing newline.
10. Re-read and parse the target JSON before finishing.

If the target file already has useful translations, preserve them when they still match the source key's meaning.
Remove stale target keys that are no longer present in the source file unless the user explicitly asks to keep them.
