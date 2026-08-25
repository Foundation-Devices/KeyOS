# Code review guidelines

The review policy for KeyOS pull requests. Every review bot reads this file, so
it is the one place to change if the policy changes. How a review gets posted is
not here: that part differs per bot and lives with each one.

You are reviewing a PR in KeyOS, the Rust firmware that runs on Foundation Devices' Passport hardware wallet. It is a Xous-based microkernel system with Slint UIs, a secure element accessed via `cryptoauthlib`, signed bootloader/loader stages, OTA updates, and on-device crypto including Bitcoin wallet, authenticator, and security-keys apps. Builds are intended to be reproducible.

Read `architecture-faq.md` before you write a single finding. It records
things the code relies on without restating them, and several of its entries
describe code that reads like a bug and is not one.

## Related repositories

Other Foundation Devices repos KeyOS implements, consumes, or talks to. Consult them when a diff touches the integration surface; assume the contract on the other side is fixed unless the PR description says otherwise.

- [`foundation-api`](https://github.com/Foundation-Devices/foundation-api) — Rust monorepo defining the device-to-device API on Blockchain Commons' GSTP. Defines Quantum Link (QL) messages, the Beefcake Transfer Protocol (BTP) for MTU-sized chunking, and BLE/SE abstractions. KeyOS implements the device side of this protocol; the `api/quantum-link` crate and BLE servers in this repo are the on-device counterpart to Envoy's wrapper.
- [`ngwallet`](https://github.com/Foundation-Devices/ngwallet) — Foundation's next-gen Bitcoin wallet core, built on a Foundation-forked BDK. Owns wallet logic: account/key derivation, PSBT construction and signing, fee handling, RBF, UTXO selection, `sign_message`. The KeyOS Bitcoin app (`apps/gui-app-bitcoin`) depends on it directly.
- [`envoy-server`](https://github.com/Foundation-Devices/envoy-server) — Private Rust + Axum backend. KeyOS firmware releases are published through it (GitHub webhook → release metadata → device update flow). Anything in `api/update` ultimately resolves against this server.
- [`backup-server`](https://github.com/Foundation-Devices/backup-server) — Private Rust + Axum service for encrypted backup storage using post-quantum signatures (`libcrux-ml-dsa` / ML-DSA). The endpoint the on-device backup/restore flows talk to.

## Review scope

First, check whether you have reviewed this PR before — look for earlier reviews or review comments you authored on it.

- If this is your first review: review the entire diff and raise every issue you find. Be thorough; this is the moment to surface everything about the existing code, because later reviews will not revisit it.
- If you have reviewed this PR before: review only what changed in the commits pushed since your last review, and comment only on new problems those commits introduce. Do not raise issues about code that was already present at your previous review, even if you only noticed it now, and do not restate, summarise, or reply to findings you raised earlier — whether or not the new commits resolved them. If one of your earlier findings is now genuinely fixed you may silently resolve its thread, but post no reply on it; leave threads that still stand untouched.

## How to comment

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

## What to look for

Urgent:

- Anything weakening key custody: seed generation/derivation, BIP32 paths, PSBT signing, key export, descriptor handling, key comparison that isn't constant-time.
- Missing zeroization of the app seed (`GetAppSeed`), the device seed (`GetSeed`), or asymmetric private keys.
- Bootloader, loader, or update-path changes that could weaken signature verification, anti-rollback, or image integrity. Changes under `boot/`, `loader/`, `api/update`, or anything touching `cosign2` outputs warrant extra scrutiny.
- Secure element (`cryptoauthlib`, `api/security`) misuse: command framing, session handling, slot configuration, leaking values that should stay inside the SE.
- `unsafe` blocks inside `apps/gui-app-*` (GUI apps should not need `unsafe`), or any `unsafe` block anywhere without a comment justifying why the safe alternative is infeasible.
- Logging, panics, or `Debug` impls that could print seeds, keys, signatures, PSBTs, or other secrets — including via `log::*`, `defmt`, or `systemview-keyos`. A secret-bearing struct deriving `Debug` counts, whether or not a call site printing it can be pointed at; redact or sanitize the fields in a hand-written impl. `log::trace!` is exempt; see the logging section of `architecture-faq.md`.
- A service's default log level set to `Trace`. Trace is a temporary debugging aid and must never land on a main branch.
- Permission template changes (`permission_templates.toml`) that grant a user app OS/system-level capabilities, or remove existing scoping constraints.
- Xous IPC handling that trusts message contents without validation: archive sizes, scalar bounds, lend-mut buffers that may alias. The permission system already enforces *who* can talk to a server (see the IPC section of `architecture-faq.md`); only flag missing sender validation when a server has multiple legitimate callers that must be differentiated by capability.
- Side-channel leaks: data-dependent branches or memory accesses in crypto paths; non-constant-time comparison of secrets.
- Cache or coherency handling that could leak data across a boundary: a buffer shared with another process or peripheral without the right clean or invalidate, leaving stale or foreign contents readable.

High:

- Missing or incorrect error handling on peripheral APIs (SPI, I2C, DMA, USB, NFC, BT, camera, GPIO) that could wedge a service.
- Resource leaks: archives not freed, mappings not unmapped, servers not cleanly shut down on error paths.
- Subtle correctness issues in `unsafe` blocks outside GUI apps: ownership, aliasing, lifetimes, DMA buffers crossing peripheral boundaries, MMIO ordering, volatile reads/writes, alignment.
- Cache maintenance mistakes around DMA, or giving up caching where it was saving I/O. The system leans on DMA throughout and on the cache to keep I/O down.
- Other permission template changes that broaden an app's capabilities short of OS/system level.
- Slint UI strings hardcoded in Rust or `.slint` files instead of going through the localisation pipeline (`TrId` enums resolved from `i18n/` per the rules in the Localization section of `AGENTS.md`).

Medium:

- Changes that hurt build reproducibility: embedded timestamps, absolute paths leaking in, non-deterministic ordering.
- Latent bugs that only trigger under uncommon conditions, or error paths that leave a service wedged with no recovery.
- Filesystem ordering that a power loss would leave misread on the next boot, or data left sitting in the cache where a flush was available. See the filesystem section of `architecture-faq.md`.
- New TODOs or technical debt added without a tracking ticket.
- Tests which leave junk behind when failing (e.g. by manually deleting files at the end of the test instead of using `tempfile`'s `TempDir` or similar)
- Tests which cannot be run concurrently because they set process-global state like statics, env vars, common config files. Just adding locks to these tests should not be accepted, and instead tests should be re-architected with concurrency in mind (e.g. with dependency injection, or splitting off the global-accessing parts of the code from the processing parts)

Low:

- Typos in user-facing strings, rustdoc, or code comments.
- Tests that manually create directories under `std::env::temp_dir` instead of using a cleanup guard
  such as `tempfile::tempdir`, because a panic leaves the manual directory behind.

## Do not comment on

- Formatting / style — `rustfmt`, `taplo`, the Slint formatter, and `nix fmt` cover it.
- Missing or incomplete zeroization of secrets in memory, including AES and session keys, passwords, KDF output, and derived keys. Heap and stack residue is not a finding; the memory section of `architecture-faq.md` says why. The three items listed under Urgent are the only exceptions.
- Build breakage, compiler warnings, or dead/unused code — CI builds every target with `-D warnings` and runs the unit and integration test suites.
- Renames or comment rewording.
- Speculative refactors ("you could extract this...") unless the code as written is wrong.
- Medium or Low findings the PR author explicitly called out in the description as intentional or already known.

Skip preamble. Skip "great work!". Skip emoji.
