# KeyOS

`docs/architecture-faq.md` records properties of the system that the code
around them takes for granted. Read it before concluding that something is
broken.

## Localization

Translation string IDs come from Figma in dot-notation form, e.g. `"camera.qrModalUnknown.title"`.

Do not manually create or invent new key-value pairs in any JSON files under `i18n/` directories. Those files are generated content. `just localize` downloads the latest source strings into `ui/ui/i18n/sources/` from Localazy, then propagates them to each app according to `localizer.json`. Made-up keys or values will be deleted by the next `just localize` run and can cause build failures or missing-string regressions.

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

## Integration tests

For multi-service integration tests, do not use plain `cargo test` to infer pass/fail. Run them through the Just wrapper so the full service harness is exercised and the exit status is explicit:

- `just one-int-test <apps and servers> && echo $?`

Use that form when verifying KeyOS integration tests in this repository.

## Review guidelines

`docs/automatic-code-review.md` is the review policy: scope, priorities, what to look for,
what to leave alone. Read it in full before reviewing a PR, along with
`docs/architecture-faq.md`, which it depends on.

### Posting the review

Post each finding as its own inline comment, anchored to the exact line it concerns — one finding per comment, never batched into a single review. Use the `[Priority] Prefix: ...` format from the policy: state the problem, then the fix, in one short paragraph.

Post exactly one top-level summary comment, and keep it to a single short paragraph: the overall verdict, optionally with a count of findings by priority. Do not restate the individual findings there — they live in the inline comments. If you keep a working checklist while reviewing, edit it out when you finish: the final summary comment must be just that one paragraph, not the checklist.

If you find nothing to flag, post the summary comment anyway with a short verdict (for example, "Reviewed the diff — no issues found.") rather than only a reaction or emoji.
