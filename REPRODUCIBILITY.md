<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Reproducibility

This document provides instructions for building KeyOS in a reproducible manner using Nix, allowing verification against official releases.

## What Can Be Verified

Not all binaries can be fully verified by external developers:

| Binary         | Verifiable | Reason                                 |
| -------------- | ---------- | -------------------------------------- |
| `app.bin`      | Yes        | Hash computed without signature header |
| `recovery.bin` | Yes        | Hash computed without signature header |
| `apps/*.elf`   | Yes        | Hash computed without signature header |
| `boot.bin`     | No         | Is encrypted for secure boot           |

The bootloader (boot.bin) is encrypted with a secret key to support the Passport Prime MCU’s secure boot mechanism and therefore cannot be reproduced or verified byte-for-byte by third parties. All other firmware images and application binaries are fully reproducible and can be verified using normal hashing techniques such as SHA256.

We are investigating ways to make the bootloader verifiable in the future.

## Prerequisites

1. Install Nix and enable flakes as described in the [Nix install](DEVELOPMENT.md#nix-install) section of DEVELOPMENT.md.

2. Ensure you're building on an aarch64 architecture for perfect reproducibility.

3. Get the source code as described in the [Get the Source Code](DEVELOPMENT.md#get-the-source-code) section of DEVELOPMENT.md.

## Building Locally

1. Checkout the release tag you want to verify (e.g., `v1.1.0`):

   ```
   git checkout v1.1.0
   ```

   Only tagged releases have corresponding official binaries in the [KeyOS-Releases](https://github.com/Foundation-Devices/KeyOS-Releases) repository.

2. Enter the Nix development environment:

   ```
   nix develop
   ```

   This sets up the reproducible build environment with all required dependencies.

3. Build the production firmware and print the hashes:

   ```
   just build-repro
   ```

   This builds all production components (bootloader, recovery, and main firmware) in a deterministic way, then outputs the SHA256 hashes of the built binaries. For signed binaries (`app.bin`, `recovery.bin`, app ELF files), the hash is computed **without** the cosign2 signature header (first 0x800 bytes), since signatures are non-deterministic.

## Verifying Reproducibility

To verify that your local build produces the same binaries as the official release:

1. Find the official release binaries:
   - Go to the [KeyOS-Releases](https://github.com/Foundation-Devices/KeyOS-Releases) repository.
   - Navigate to the version directory matching your tag (e.g., `1.1.0/` for tag `v1.1.0`).
   - The release contains:
     - `boot.bin` - bootloader binary (cannot be verified, see above)
     - `app.bin` - main firmware binary
     - `recovery.bin` - recovery firmware binary
     - `apps/` - individual application binaries (`app.elf` files)

2. Compute hashes of the official release binaries:
   For signed binaries, you need to skip the cosign2 header (first 2048 bytes) when computing the hash:

   ```bash
   # For app.bin, recovery.bin, or app ELF files:
   tail -c +2049 <file> | sha256sum
   ```

3. Compare hashes:
   - Compare the hash output from your local `just build-repro` run with the hashes computed from the release binaries.
   - The hashes for `app.bin`, `recovery.bin`, and all app ELF files should match.
   - The `boot.bin` hash will differ due to the secret `EXTRA_ENTROPY` value.

4. If hashes differ (for verifiable binaries):
   - Ensure you're on the exact tag/commit corresponding to the release.
   - Verify your Nix installation and configuration.
   - Check that you're on an aarch64 architecture.
   - Make sure you used `just build-repro`, which passes the production build flags.

## Notes

- Reproducibility guarantees that identical inputs produce identical outputs on the same CPU architecture.
- The `just build-repro` recipe passes `--production-firmware`, which implies `--reproducible` and disables incremental compilation for deterministic builds.
- You do not need to use `--dont-sign` for verification, as the hash comparison ignores the signature header anyway.
