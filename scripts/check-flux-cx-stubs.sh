#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

# Fail when a built Flux app still links a weak `cx_` symbol. A weak definition is the SDK's
# `cx_stubs.S` trampoline, which branches into memory only the upstream OS maps, so calling one
# faults the app. Run it from the repo root after the Flux apps have been built.

set -euo pipefail

# Weak `cx_` stubs with no KeyOS shim. `cx_aes_siv_reset` has no caller in any app we build;
# `cx_decode_coord` and `cx_x25519` do and are tracked by SFT-7764 follow-ups.
allowed="cx_aes_siv_reset cx_decode_coord cx_x25519"

elf_dir="target/armv7a-unknown-xous-elf/release"
status=0
checked=0

shopt -s nullglob
for elf in "$elf_dir"/app-flux-*; do
    app=$(basename "$elf")
    # The glob also catches the build artefacts beside the ELF (app-flux-x.d, app-flux-x_icon.gif).
    if [[ "$app" == *.* ]]; then
        continue
    fi
    checked=$((checked + 1))
    unexpected=$(arm-none-eabi-readelf -sW "$elf" |
        awk -v allowed=" $allowed " \
            '$5 == "WEAK" && $8 ~ /^cx_/ && index(allowed, " " $8 " ") == 0 { print $8 }' |
        sort -u)
    if [ -n "$unexpected" ]; then
        echo "$app links Flux crypto stubs with no KeyOS implementation:" >&2
        echo "$unexpected" | sed 's/^/    /' >&2
        status=1
    fi
done

if [ "$checked" -eq 0 ]; then
    echo "No Flux app ELF under $elf_dir: build them with 'cargo xtask build-sideload-apps'." >&2
    exit 1
fi

if [ "$status" -ne 0 ]; then
    echo "Add a shim in utils/app-flux-runtime/src/crypto.rs, or list the symbol in $0 once you" >&2
    echo "have confirmed nothing calls it." >&2
fi

exit "$status"
