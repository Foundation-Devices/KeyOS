---
name: foundation-new-page
description: Add a routed page to the current Foundation SDK Slint app. Invoke as /foundation-new-page <name>, for example /foundation-new-page settings, when adding ui/pages/<name>/page.slint and props.slint to a router-enabled app.
argument-hint: <page-name>
arguments: [page_name]
disable-model-invocation: true
---

<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation New Page

Add a new routed Slint page named `$page_name` to the current Foundation SDK app.

Workflow:

1. Require `$page_name`. If it is missing, ask for the page name.
2. Find the app root by walking up to `app-config.toml`.
3. Confirm the app is router-enabled before editing:
   - `build.rs` should set `include_router: true`
   - `ui/app.slint` should import and instantiate `Router`
4. If the app is not router-enabled, stop and explain that the command only adds pages to routed apps.
5. Normalize `$page_name`:
   - directory and route slug: lower-case kebab-case, for example `settings` or `seed-backup`
   - component prefix: PascalCase without separators, for example `Settings` or `SeedBackup`
6. Create `ui/pages/<slug>/props.slint` unless it already exists:

```slint
// Route props for the <display name> page
@rust-attr(route(path = "/<slug>"))
export struct <Prefix>PageProps {}
```

7. Create `ui/pages/<slug>/page.slint` unless it already exists. Follow the existing app's page style when clear; otherwise use the current multi-page template pattern with `Theme`, `UISize`, `Button`, `Card`, and `Navigate.backward()`.
8. If the app has an obvious default page navigation list or button section, add a navigation action to `Navigate.<slug-with-page-suffix>({ });`. If the existing page structure is not obvious, leave navigation unwired and report the generated route.
9. Do not overwrite existing page files.
10. Do not add Foundation copyright or SPDX headers to generated app files. This is SDK-user code. Only copy a copyright/license header when the app's neighboring user-owned page files already use one.
11. Run the lightest useful validation after editing:
    - prefer `foundation preview ui/app.slint` when the user asked for visual verification
    - otherwise run `cargo check` or explain why validation was skipped

Keep edits scoped to routing files, page UI, and only the translation keys needed for the new page label if the app already localizes page labels.
