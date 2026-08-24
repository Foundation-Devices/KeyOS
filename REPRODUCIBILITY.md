<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Reproducibility

KeyOS uses a pinned Nix environment and deterministic production build settings so published firmware can be compared with a local build.

## What Can Be Verified

| Artifact                         | Verifiable | Method                                                |
| -------------------------------- | ---------- | ----------------------------------------------------- |
| `boot.bin` (plaintext)           | Yes        | Compare its modeled SRAM hash with System Information |
| `boot.cip`                       | No         | Encrypted and authenticated with release-only keys    |
| `app.bin`                        | Yes        | Hash without the cosign2 signature header             |
| `recovery.bin`                   | Yes        | Hash without the cosign2 signature header             |
| Built-in application `app.elf`s | Yes        | Hash without the cosign2 signature header             |

The local build produces the plaintext bootloader as `boot.bin`. The published bootloader is `boot.cip`, which is encrypted and authenticated for the Passport Prime MCU's Secure Boot mechanism, so third parties cannot reproduce it byte-for-byte. The plaintext `boot.bin` can still be transformed into the plaintext SRAM image hashed by System Information on a production Secure Boot device.

## Prerequisites

1. Install Nix and enable flakes as described in the [Nix install](DEVELOPMENT.md#nix-install) section of `DEVELOPMENT.md`.

2. Use an AArch64 Linux build machine for byte-for-byte reproducibility. The pinned dependencies are the same on AArch64 macOS, but the host-specific build tools do not produce an identical bootloader binary.

3. To verify a release with the current build commands, check out its exact tag. To use the historical wrapper below, remain on a current checkout and give the wrapper the exact tag; it creates the historical checkout itself. Only tagged releases have corresponding binaries in the [KeyOS-Releases](https://github.com/Foundation-Devices/KeyOS-Releases) repository.

4. Enter the pinned build environment:

   ```console
   nix develop
   ```

## Verifying the Bootloader

Build only the production bootloader and print its hashes:

```console
just build-repro-bootloader
```

The command prints two hashes:

- `Raw bootloader SHA256` hashes the local plaintext `boot.bin`. This is useful for diagnosing a local build, but it is **not** the value displayed by the device.
- `On-device bootloader SHA256` models the plaintext SRAM image after Secure Boot and bootloader cleanup. Compare this value with **System Information → Bootloader → SHA256 Hash** on a production Passport Prime with MCU Secure Boot enabled. On a development unit booting plaintext `boot.bin`, System Information instead displays the raw `boot.bin` hash because Secure Boot did not rewrite the size or add the padding and CMAC area.

The comparison build does not require the release encryption key, signing material, or secret `EXTRA_ENTROPY` value.

The bootloader Cargo profile disables incremental compilation. Production release builds use the same setting, preventing cached compiler state from changing the binary compared with a clean verification build.

### Verifying a Historical Release

Historical tags do not contain the current SRAM hash model, and changing their source would invalidate the comparison. Run the historical wrapper from a current KeyOS checkout instead:

```console
nix develop .#build --command just reproduce-bootloader v1.2.0
```

If already inside the pinned Nix environment, run `just reproduce-bootloader v1.2.0`. The Rust `cargo xtask reproduce-bootloader` command resolves the tag to an exact commit, creates two independent temporary Git worktrees, and builds each worktree with that tag's own `flake.lock`, `Cargo.lock`, build scripts, and commit-derived `SOURCE_DATE_EPOCH`. It deliberately leaves the original entropy marker in each historical `boot.bin`; the current Rust hash model then finds that unique slot, replaces it with the public runtime value in a copied artifact, and calculates both the normalized raw hash and the on-device hash. The historical worktrees are removed after each build.

Artifacts and `report.json` are retained under `target/bootloader-reproductions/`. The command fails if the two independent builds differ. Use `--builds 1` for a single diagnostic build, but that does not test reproducibility.

Run the final comparison on AArch64 Linux. Other hosts are allowed so host-dependent behavior can be diagnosed, but the wrapper warns that their result may not match a release device.

### Canonical Build Timestamp

The bootloader includes a build date, so its `SOURCE_DATE_EPOCH` is a build input. KeyOS stores the canonical value for the current bootloader in `boot/keyos-boot/SOURCE_DATE_EPOCH`. Production and verification builds both read that file. This keeps the output stable when private KeyOS-dev changes are copied into squashed commits in the public KeyOS repository.

When a bootloader change produces a different normalized or canonical hash, CI requires both the bootloader version and this timestamp to increase. Changing only the timestamp changes the canonical binary and therefore also requires a version increase. Unrelated commits and public-repository commit timestamps do not affect the bootloader.

### How `EXTRA_ENTROPY` Is Handled

Production releases build the bootloader with a secret 32-byte `EXTRA_ENTROPY` value. Before starting recovery, the bootloader replaces those bytes in SRAM with this fixed public 32-byte value:

```text
000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f
```

This is the Bitcoin genesis-block hash. The reproducible build uses the same public value; it must not use zeroes or leave the build marker in place.

Secure SAM-BA also pads the plaintext to a 16-byte AES block, adds a 16-byte CMAC, and rewrites the sixth Arm vector with the resulting size. The bootloader cleanup clears the padding and CMAC area before recovery reads SRAM. The hash command models both transformations: it updates the size vector and extends the local image with the zeroes recovery will observe.

No bytes from `boot.bin` are skipped. The Rust command makes a temporary, in-memory copy of the local file look like the plaintext image Recovery sees in SRAM, then hashes the complete copy. It does not modify `boot.bin` on disk.

```text
LOCAL VERIFICATION

boot.bin with fixed EXTRA_ENTROPY
        |
        | Copy in memory
        | Set size word at 0x14 to the Secure Boot size
        | Append zeroes for the cleared padding and 16-byte CMAC area
        v
Expected plaintext SRAM image  ---------->  SHA256  ---------->  On-device bootloader SHA256


PRODUCTION DEVICE

boot.bin with secret EXTRA_ENTROPY
        |
        | Secure SAM-BA pads, authenticates, and encrypts it
        | MCU Secure Boot decrypts it into SRAM
        | Bootloader replaces EXTRA_ENTROPY with the fixed value
        | Bootloader clears the padding and CMAC area
        v
Actual plaintext SRAM image    ---------->  SHA256  ---------->  System Information
```

The expected and actual plaintext SRAM images should be byte-for-byte identical when they were built from the same source tree, canonical bootloader timestamp, and reproducible build environment.

## Building and Verifying All Firmware

Build all production components and print their hashes:

```console
just build-repro
```

For `app.bin`, `recovery.bin`, and built-in application `app.elf` files, the printed hash excludes the first `0x800` bytes containing the non-deterministic cosign2 signature header. To calculate the same hash from a published file:

```console
tail -c +2049 <file> | sha256sum
```

Compare those values with the corresponding files from the matching release in KeyOS-Releases. For the bootloader, the summary prints two hashes: `bootloader (raw plaintext)` hashes the local `boot.bin` and is not device-comparable, while `bootloader (on-device)` is the value to compare with System Information. The build records the latter alongside `boot.bin` while the exact entropy slot is known, and the summary verifies that record belongs to the current file before printing it.

## Troubleshooting

- Confirm the source is at the exact release tag.
- Confirm the build is running inside `nix develop`.
- Confirm the build machine is AArch64.
- Do not override `KEYOS_SOURCE_DATE_EPOCH`; normal builds use the value tracked in `boot/keyos-boot/SOURCE_DATE_EPOCH`.
- Compare the device against an `on-device` hash; hashing `boot.bin` directly produces the raw hash rather than the on-device hash.
- Do not set `EXTRA_ENTROPY` for the comparison build. The recipe deliberately uses the public runtime replacement value.
