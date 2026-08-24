---
name: foundation-sdk-migrate-newest
description: Port an existing KeyOS app to the newest Foundation SDK by following the SDK migration guide. Use when asked to migrate, port, or upgrade an app to a newer SDK ("port this 0.4.0 app to the new SDK", "migrate my app to SDK 1.0.0"), or when an app fails to build because it targets an older SDK than the one installed. Not for ordinary build errors in an app already on the current SDK. Invoke as /foundation-sdk-migrate-newest [path-to-existing-app].
argument-hint: [path-to-existing-app]
arguments: [app_path]
---

<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: MIT
-->

# Foundation SDK Migrate Newest

Port the app at `$app_path` (default: the current app root) to the newest Foundation SDK.

**The migration guide is the authority; this skill is only the procedure.** Do not migrate from memory of
what an SDK release changed. Read the guide, then follow it.

## 1. Read the guide

Find `MIGRATIONS.md` in the first location that exists:

- SDK source checkout: `<sdk-root>/docs/MIGRATIONS.md` (the SDK root is the `sdk/` directory, so this is
  `sdk/docs/MIGRATIONS.md` from a KeyOS repo root)
- Packaged SDK bundle: `<sdk-root>/docs/guide/src/MIGRATIONS.md`
- App project: resolve the SDK root from `FOUNDATION_SDK_ROOT` or the root path `foundation doctor` reports,
  then use a path above.

If no copy is reachable, stop and say so rather than porting blind.

The guide holds one chapter per SDK release worth migrating to. Read the chapter for the release you are
migrating **to** in full before editing anything; it is long, and the ranked gotchas at the end are where the
expensive mistakes live.

## 2. Establish the target and the starting state

1. Read the installed SDK version from the SDK root: `[sdk] version` in `<sdk-root>/manifest.toml` for a
   packaged bundle, or in `<sdk-root>/sdk-build.toml` for a source checkout. **Do not use `foundation
   --version`**, which reports the CLI crate version (`0.1.0`), not the SDK version; `foundation doctor` prints
   the SDK root path but no version. Pick the guide chapter that matches. If the newest chapter is older than
   the installed SDK, say so: the guide is behind and the gap is unmapped.
2. Classify the app against the chapter's "Which migration path applies" fingerprint table (entry macro, how SDK
   deps are referenced, where the theme lives, how it is built). State which row you matched and why.
3. Follow the sections that row points at. Some starting states need extra sections, for example an in-tree
   `app!` app needs the compat-shim section, and an app depending on an OS surface the SDK does not ship needs
   the feature-gate and mock section.

## 3. Port into a fresh scaffold

The guide's recommended path is to generate a new app and port into it, not to patch the old one in place.

- Generate into a **new directory**, passing the old app's identity explicitly:
  `foundation new <name> --app-id <existing-app-id> --app-version <bumped> …`. Leave the original app untouched
  so it stays a reference and a fallback.
- **Never omit `--app-id`.** It is optional, and `foundation new` generates a random one when it is missing,
  which is silent in a non-interactive run. The app-id is the device's install identity *and* the
  domain-separation input to the app seed, so letting it default rotates every app-seed-derived key
  unrecoverably.
- **Bump the Cargo package version** above the last release of the original so a sideload upgrades the
  installed app cleanly. `Cargo.toml` is the only canonical app version source.
- Ask the user for any metadata you cannot read out of the old `app-config.toml` (publisher, contact, support
  URL). Do not invent it.
- Keep the generated `Cargo.toml`, `build.rs`, `theme.rs`, `.gitignore` and `permission_templates.toml`, then
  re-apply the app's own additions on top. Copying the old ones back is the most common way a port fails to
  compile.

## 4. Work the checklist

Turn the chapter's migration checklist into your task list and work it in order, one item at a time. Do not
skip an item silently: if something does not apply, say which item and why.

## 5. Never guess an API

When a call site or a Slint component stops compiling, read the real source instead of guessing at the new
shape:

- Rust service APIs: the crates under `.foundation-sdk/current/lib/keyos/api/<service>`.
- Message names, ids and permission metadata: that service's `manifest.toml`.
- `@ui` components: the `ui/ui` symlink, which is the property surface's authority.

The guide lists the breaking changes it knows about; the source settles anything it does not.

## 6. Audit permissions before running on hardware

For every message the app grants, resolve its effective signature requirement from its `manifest.toml` entry.
`permissionGroup` alone does not decide it:

1. An explicit `requiredSignature` wins. `foundation` is Foundation-signed-only even when the message carries a
   `permissionGroup`; `thirdParty` is grantable even when it carries none.
2. With no explicit `requiredSignature`, a grouped message defaults to `thirdParty` and an ungrouped one to
   `foundation`.
3. Finally, `approval` must be `autoAllow` or `grantOnFirstUse`. It is `notUserGrantable` when absent, and that
   is unavailable no matter what the two steps above resolved to.

`approval` does not decide grantability in the other direction: an `autoAllow` message is not third-party-usable
unless steps 1 and 2 already said so. Checking for a `permissionGroup` alone passes messages that are grouped
*and* `requiredSignature = "foundation"`, which is a live combination (several `os/settings` and
`os/quantum-link` entries), so the app ships and the call is refused the first time it runs. That refusal aborts
the app wherever the API wrapper unwraps it, which is most of them, and comes back as an error where the wrapper
uses a `try_` send. Do this audit before sideloading, and report anything the app cannot hold to the user
instead of working around it.

## 7. Verify

Always run the chapter's icon verification (exact size plus the transparency sample check) once the icon is in
place, whatever else the user asked for. The build validates the icon's size and never its transparency, so
nothing downstream catches a background slab; the guide's checklist makes this step mandatory. The transparency
half needs ImageMagick, which the SDK dev shell does not provide: if `magick` is not on `PATH`, say the icon is
**unverified** and name the check the user still owes. Do not substitute an improvised check and do not pass
over it in silence.

Then work up the ladder, and only as far as the user asked:

1. `cargo check` (after one `foundation build`, with `FOUNDATION_THEMES_RUST_DIR` set as the chapter describes).
2. `foundation preview` for UI, `foundation sim` for the hosted runtime.
3. `foundation build` for a signed artifact.

Do not run `foundation sideload`, `foundation logs`, or drive a device over MCP unless the user asked for
on-device work. Do not run `foundation cert gen` or touch signing identities unless the user asked for signing
setup.

## 8. Report honestly

Finish with what moved, what did not, and what needs a decision:

- Anything left stubbed or feature-gated because the SDK does not expose the surface, and what it would take to
  finish it.
- Any behavior change the port forces on users, for example a key derivation re-rooted in the app seed.
- Any checklist item you skipped.

A port that compiles but quietly dropped a feature is worse than one that reports the gap.
