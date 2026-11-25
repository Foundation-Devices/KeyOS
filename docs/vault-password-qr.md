# Plain-text password QR

In the existing Vault, open a generated or imported password's Details. Save any
edits first. Tap **Password** with the QR icon to explicitly reveal a static QR;
opening an entry alone never reveals it. The existing masked password input and
its reveal control are unchanged. Archived passwords support the same QR action.

The payload is only the selected stored password's exact UTF-8 bytes: no account,
label, seed, URL wrapper, trimming, or Unicode normalization. Password text that
itself resembles a URL is still encoded unchanged. Empty passwords and passwords
over 2331 UTF-8 bytes produce no QR (the shared renderer's medium-error-correction
byte capacity); large codes may be impractical to scan. Nothing is truncated.

Use a trusted **offline** webcam QR scanner that displays plain text. Copy its
decoded text, select the intended password field in Firefox, and paste manually.
Do not open decoded text as a link or submit it to an online QR service. Test the
scanner's handling of punctuation, spaces and Unicode with dummy passwords first;
scanner/clipboard software can alter text, retain history, sync it, or expose it
to extensions. Verify the destination before submitting. Clear clipboard/history
afterwards. Cameras, screenshots, nearby people and the scanner computer can all
capture the password. The QR is not encrypted.

**Done** or the 30-second timeout dismisses the QR and releases its image. Vault
also clears it on native hide/show and navigation notifications. The supported
GUI lock flow hides the app, so unlocking does not redisplay it. Internal page
navigation is blocked by the QR overlay. This drops active UI references, not a
claim of scrubbing every live-process allocation or camera/clipboard copy.
Storage, derivation, backup, and the master key are unchanged.

## Verification and installation limits

Dummy-only unit tests cover exact payload bytes for both password types,
punctuation/Unicode, byte-capacity boundaries, shared rendering, non-password
type rejection, and which native lifecycle events invalidate a reveal. They do
not prove physical-camera decoding, Firefox behavior, device lock timing, or the
rendered UI lifecycle. ARM/host compilation and tests
require the repository's supported build environment; no device or live simulator
validation is implied by source inspection or unit tests.

The existing manifest requests `os/security.GetSeed`, which is Foundation-only.
This change does not alter permissions, app identity, firmware policy or signing.
A separately signed app cannot be assumed to replace Foundation's Vault, receive
that permission, or inherit existing Vault storage. Installation, signing,
flashing, device access and data migration require separate authorization and
supported tooling; this implementation performs none of them.
