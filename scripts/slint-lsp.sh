#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
LOCAL_TARGET_DIR="${REPO_ROOT}/sdk/target/slint-lsp"
LOCAL_BIN="${LOCAL_TARGET_DIR}/debug/slint-lsp"
MIN_VERSION="1.12.0"

slint_source_root() {
    if [[ -n "${SLINT_DIR:-}" ]]; then
        printf '%s\n' "${SLINT_DIR}"
        return 0
    fi

    local fallback="${REPO_ROOT}/sdk/external/slint"
    if [[ -f "${fallback}/Cargo.toml" ]]; then
        printf '%s\n' "${fallback}"
        return 0
    fi

    return 1
}

version_is_compatible() {
    local version="$1"
    [[ -n "${version}" ]] || return 1
    [[ "$(printf '%s\n' "${MIN_VERSION}" "${version}" | sort -V | head -n1)" == "${MIN_VERSION}" ]]
}

if [[ -n "${SLINT_LSP_BIN:-}" ]]; then
    exec "${SLINT_LSP_BIN}" "$@"
fi

if [[ -x "${LOCAL_BIN}" ]]; then
    exec "${LOCAL_BIN}" "$@"
fi

if command -v slint-lsp >/dev/null 2>&1; then
    version="$(slint-lsp --version 2>/dev/null | awk 'NR == 1 { print $2 }')"
    if version_is_compatible "${version}"; then
        exec slint-lsp "$@"
    fi
fi

SOURCE_ROOT="$(slint_source_root)" || {
    echo "Unable to locate Slint sources for slint-lsp. Set SLINT_DIR or run from the Foundation SDK maintainer shell." >&2
    exit 1
}

CARGO_TARGET_DIR="${LOCAL_TARGET_DIR}" cargo build --manifest-path "${SOURCE_ROOT}/Cargo.toml" -p slint-lsp
exec "${LOCAL_BIN}" "$@"
