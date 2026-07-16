# KeyOS

## Localization

Translation string IDs come from Figma in dot-notation form, e.g. `"camera.qrModalUnknown.title"`.

**How to resolve an ID to a Slint enum variant:**

1. Look up the ID root (first segment) in `localizer.json` → `apps[].name` to find which app owns it.
   - Example: `"camera"` → `apps/gui-app-qr-scanner`
2. Drop the root segment; convert the remaining segments to PascalCase with periods removed.
   - Example: `"qrModalUnknown.title"` → `QrModalUnknownTitle`
3. The generated Slint enum is at `<app-path>/ui/gen/tr.slint` as `TrId.QrModalUnknownTitle`.
4. IDs whose root is `"common"` are included in multiple apps via the `"include"` fields in `localizer.json`. Their enum variant keeps `"Common"` as the first word — `"common.button.done"` → `TrId.CommonButtonDone`.
5. Non-common IDs listed in an app's `"include"` array are also available in that app's `TrId` enum.

**At runtime in Slint:** `TR2.lookup(TrId.QrModalUnknownTitle)`

**At runtime in Rust:** `tr::lookup_id(TrId::QrModalUnknownTitle)` (or via the generated `tr` module).

## Review guidelines

You are reviewing a PR in KeyOS, the Rust firmware that runs on Foundation Devices' Passport hardware wallet. It is a Xous-based microkernel system with Slint UIs, a secure element accessed via cryptoauthlib, signed bootloader/loader stages, OTA updates, and on-device crypto including Bitcoin wallet, authenticator, and security-keys apps. Builds are intended to be reproducible. License is GPL-3.0; every new file needs SPDX headers.

### Related repositories

Other Foundation Devices repos KeyOS implements, consumes, or talks to. Consult them when a diff touches the integration surface; assume the contract on the other side is fixed unless the PR description says otherwise.

- [`foundation-api`](https://github.com/Foundation-Devices/foundation-api) — Rust monorepo defining the device-to-device API on Blockchain Commons' GSTP. Defines Quantum Link (QL) messages, the Beefcake Transfer Protocol (BTP) for MTU-sized chunking, and BLE/SE abstractions. KeyOS implements the device side of this protocol; the `api/quantum-link` crate and BLE servers in this repo are the on-device counterpart to Envoy's wrapper.
- [`ngwallet`](https://github.com/Foundation-Devices/ngwallet) — Foundation's next-gen Bitcoin wallet core, built on a Foundation-forked BDK. Owns wallet logic: account/key derivation, PSBT construction and signing, fee handling, RBF, UTXO selection, `sign_message`. The KeyOS Bitcoin app (`apps/gui-app-bitcoin`) depends on it directly.
- [`envoy-server`](https://github.com/Foundation-Devices/envoy-server) — Private Rust + Axum backend. KeyOS firmware releases are published through it (GitHub webhook → release metadata → device update flow). Anything in `api/update` ultimately resolves against this server.
- [`backup-server`](https://github.com/Foundation-Devices/backup-server) — Private Rust + Axum service for encrypted backup storage using post-quantum signatures (`libcrux-ml-dsa` / ML-DSA). The endpoint the on-device backup/restore flows talk to.

### Review scope

First, check whether you have reviewed this PR before — look for earlier reviews or review comments you authored on it.

- If this is your first review: review the entire diff and raise every issue you find. Be thorough; this is the moment to surface everything about the existing code, because later reviews will not revisit it.
- If you have reviewed this PR before: comment only on what changed in the commits pushed since your last review. Do not raise new issues about code that was already present at your previous review, even if you only noticed it now. Before reviewing the new changes, revisit each finding you raised earlier on this PR and check whether the new commits address it: if one is now fixed, reply on its thread citing the commit that fixed it (for example, `Resolved by <sha>.`) and resolve that thread; leave findings that still stand open.

### How to comment

Give every finding a priority — the reviewer triages from it, and any finding promoted to a Linear ticket inherits it:

- **Urgent** — must fix before merge: a correctness, security, or data-loss bug.
- **High** — should fix before merge: likely to bite, but not catastrophic.
- **Medium** — worth fixing; can be deferred to a follow-up ticket.
- **Low** — minor; nice-to-have.

Lead every inline comment with the priority in brackets, then a prefix that signals the action expected:

- *(no prefix)* — change this, or justify why not.
- `Optional:` — an improvement; can be dismissed without justification.
- `Note:` — FYI only, no action required.

For example: `[Urgent] <problem>. <fix>.` or `[Low] Optional: <suggestion>.` or `[Medium] Note: <observation>.`

Resolve only your own threads, and only when the code genuinely addresses them — never resolve a comment authored by a human.

### What to look for

Urgent:

- Anything weakening key custody: seed generation/derivation, BIP32 paths, PSBT signing, key export, descriptor handling, key comparison that isn't constant-time.
- Bootloader, loader, or update-path changes that could weaken signature verification, anti-rollback, or image integrity. Changes under `boot/`, `loader/`, `api/update`, or anything touching `cosign2` outputs warrant extra scrutiny.
- Secure element (`cryptoauthlib`, `api/security`) misuse: command framing, session handling, slot configuration, leaking values that should stay inside the SE.
- `unsafe` blocks inside `apps/gui-app-*` (GUI apps should not need `unsafe`), or any `unsafe` block anywhere without a comment justifying why the safe alternative is infeasible.
- Logging, panics, or `Debug` impls that could print seeds, keys, signatures, PSBTs, or other secrets — including via `log::*`, `defmt`, or `systemview-keyos`.
- Permission template changes (`permission_templates.toml`) that grant a user app OS/system-level capabilities, or remove existing scoping constraints.
- Xous IPC handling that trusts message contents without validation: archive sizes, scalar bounds, lend-mut buffers that may alias. The permission system already enforces *who* can talk to a server; only flag missing sender validation when a server has multiple legitimate callers that must be differentiated by capability.
- Side-channel leaks: data-dependent branches or memory accesses in crypto paths; non-constant-time comparison of secrets.

High:

- Missing or incorrect error handling on peripheral APIs (SPI, I2C, DMA, USB, NFC, BT, camera, GPIO) that could wedge a service.
- Resource leaks: archives not freed, mappings not unmapped, servers not cleanly shut down on error paths.
- Subtle correctness issues in `unsafe` blocks outside GUI apps: ownership, aliasing, lifetimes, DMA buffers crossing peripheral boundaries, MMIO ordering, volatile reads/writes, alignment.
- Other permission template changes that broaden an app's capabilities short of OS/system level.
- Slint UI strings hardcoded in Rust or `.slint` files instead of going through the localisation pipeline (`TrId` enums resolved from `i18n/` per the rules in the Localization section above).

Medium:

- Changes that hurt build reproducibility: embedded timestamps, absolute paths leaking in, non-deterministic ordering.
- Latent bugs that only trigger under uncommon conditions, or error paths that leave a service wedged with no recovery.
- New TODOs or technical debt added without a tracking ticket.

Low:

- Typos in user-facing strings, rustdoc, or code comments.

### Do not comment on

- Formatting / style — `rustfmt` and `taplo` cover it.
- Renames or comment rewording.
- Speculative refactors ("you could extract this...") unless the code as written is wrong.
- Things the PR author explicitly called out in the description.

Skip preamble. Skip "great work!". Skip emoji.

Post each finding as its own inline comment, anchored to the exact line it concerns — one finding per comment, never batched into a single review. Use the `[Priority] Prefix: ...` format above: state the problem, then the fix, in one short paragraph.

Post exactly one top-level summary comment, and keep it to a single short paragraph: the overall verdict, optionally with a count of findings by priority. Do not restate the individual findings there — they live in the inline comments. If you keep a working checklist while reviewing, edit it out when you finish: the final summary comment must be just that one paragraph, not the checklist.

If you find nothing to flag, post the summary comment anyway with a short verdict (for example, "Reviewed the diff — no issues found.") rather than only a reaction or emoji.
