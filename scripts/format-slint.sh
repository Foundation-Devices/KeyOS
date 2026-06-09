#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
SLINT_LSP="${SCRIPT_DIR}/slint-lsp.sh"

CHECK=false
if [[ "${1:-}" == "--check" ]]; then
    CHECK=true
    shift
fi

# Set the base directory, or use the repo root if none is specified
BASE_DIR="${1:-.}"

mktemp_slint() {
    mktemp --suffix=.slint
}

normalize_trailing_newlines() {
    local file="$1"
    local content

    [[ -s "${file}" ]] || return 0

    if [[ "$(tail -c 1 "${file}" | wc -l)" -eq 1 ]]; then
        content="$(<"${file}")"
        printf '%s\n' "${content}" > "${file}"
    fi

    return 0
}

format_into() {
    local source="$1"
    local destination="$2"

    "${SLINT_LSP}" format "${source}" > "${destination}" 2>/dev/null || return $?
    normalize_trailing_newlines "${destination}"
}

# Find all tracked .slint files and format or check them
git -C "${REPO_ROOT}" ls-files "$BASE_DIR" | grep '\.slint$' | while read -r file; do
    formatted_file="$(mktemp_slint)"
    stability_file="$(mktemp_slint)"

    if ! format_into "$file" "${formatted_file}"; then
        rm -f "${formatted_file}" "${stability_file}"
        echo "Failed to format $file." >&2
        exit 1
    fi

    if [[ "$(<"$file")" != "$(<"$formatted_file")" ]]; then
        if format_into "${formatted_file}" "${stability_file}" && [[ "$(<"$formatted_file")" != "$(<"$stability_file")" ]]; then
            rm -f "${formatted_file}" "${stability_file}"
            continue
        fi

        if [[ "${CHECK}" == true ]]; then
            rm -f "${formatted_file}" "${stability_file}"
            echo "$file is not properly formatted."
            exit 1
        fi

        cp "${formatted_file}" "${file}"
    fi

    rm -f "${formatted_file}" "${stability_file}"
done
