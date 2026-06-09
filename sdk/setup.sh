#!/usr/bin/env sh
# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: MIT

set -eu

SDK_ROOT=$(CDPATH= cd -- "$(dirname "$0")" && pwd)

if ! command -v nix >/dev/null 2>&1; then
  echo "Nix is required to use the Foundation SDK."
  exit 1
fi

echo "Foundation SDK root: $SDK_ROOT"
echo "Enter the SDK environment with:"
echo
echo "  FOUNDATION_SDK_ROOT=\"$SDK_ROOT\" \"$SDK_ROOT/bin/foundation\" develop"
echo
echo "For a shell-wide install that adds 'foundation' to your PATH, use the curl installer."

if [ ! -f "$SDK_ROOT/flake.lock" ]; then
  echo
  echo "Warning: flake.lock is missing. Generate it with 'nix flake lock' before packaging releases."
fi
