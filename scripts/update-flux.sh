#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Check for a newer upstream release of a Flux app and repin its build.rs to it.
#
#   scripts/update-flux.sh <ethereum|solana|monero|zcash> [--check]
#
# Release tags look like flex_<flexver>_<appver>_sdk_<sdkref>; the app version is
# the third underscore-separated field. `--check` reports without writing changes.
set -euo pipefail

app="${1:-}"
mode="${2:-update}"
case "$app" in
    ethereum | solana | monero | zcash) ;;
    *)
        echo "usage: $0 <ethereum|solana|monero|zcash> [--check]" >&2
        exit 2
        ;;
esac
[ "$mode" = "--check" ] && mode="check"

build_rs="apps/app-flux-$app/build.rs"
[ -f "$build_rs" ] || {
    echo "not found: $build_rs (run from the repo root)" >&2
    exit 1
}

read_const() { sed -n "s/.*$1: &str = \"\\([^\"]*\\)\".*/\\1/p" "$build_rs"; }
app_name="$(read_const APP_NAME)"
cur_tag="$(read_const APP_GIT_TAG)"
[ -n "$app_name" ] && [ -n "$cur_tag" ] ||
    {
        echo "couldn't read APP_NAME/APP_GIT_TAG from $build_rs" >&2
        exit 1
    }
cur_ver="$(printf '%s' "$cur_tag" | cut -d_ -f3)"

# Upstream app repository. The org in this URL is an external identifier that
# can't be renamed; it is the only such reference here.
repo="https://github.com/LedgerHQ/$app_name.git"
echo "app-flux-$app: pinned to $cur_tag (app $cur_ver)"
echo "checking upstream for a newer $app_name release ..."

# Newest release tag by app version (field 3), among the flex_* tags upstream.
newest_tag="$(git ls-remote --tags "$repo" |
    grep -oE 'flex_[0-9.]+_[0-9.]+_sdk_[A-Za-z0-9._]+' |
    sort -u |
    awk -F_ '{ print $3 "\t" $0 }' |
    sort -V |
    tail -1 |
    cut -f2)"
[ -n "$newest_tag" ] || {
    echo "no flex_* release tags found upstream" >&2
    exit 1
}
new_ver="$(printf '%s' "$newest_tag" | cut -d_ -f3)"

# Strictly-newer test: the version-sorted maximum must be the upstream one.
if [ "$cur_ver" = "$new_ver" ] ||
    [ "$(printf '%s\n%s\n' "$cur_ver" "$new_ver" | sort -V | tail -1)" != "$new_ver" ]; then
    echo "already on the newest release (upstream newest is $new_ver)"
    exit 0
fi

new_sdk="${newest_tag#*_sdk_}"
cur_sdk="${cur_tag#*_sdk_}"
echo
echo "newer release available:"
echo "  tag : $cur_tag"
echo "     -> $newest_tag"
echo "  app : $cur_ver -> $new_ver"
echo "  sdk : $cur_sdk -> $new_sdk"

if [ "$mode" = "check" ]; then
    echo
    echo "(--check: no changes written)"
    exit 0
fi

IFS=. read -r m n p <<EOF
$new_ver
EOF
sed -i "s#APP_GIT_TAG: &str = \"[^\"]*\"#APP_GIT_TAG: \&str = \"$newest_tag\"#" "$build_rs"
sed -i "s#SDK_GIT_TAG: &str = \"[^\"]*\"#SDK_GIT_TAG: \&str = \"$new_sdk\"#" "$build_rs"
# Hosted-build version defines, so the simulator reports the same version.
sed -i "s/(\"APPVERSION_M\", \"[0-9]*\")/(\"APPVERSION_M\", \"$m\")/" "$build_rs"
sed -i "s/(\"APPVERSION_N\", \"[0-9]*\")/(\"APPVERSION_N\", \"$n\")/" "$build_rs"
sed -i "s/(\"APPVERSION_P\", \"[0-9]*\")/(\"APPVERSION_P\", \"$p\")/" "$build_rs"
sed -i "s/(\"MAJOR_VERSION\", \"[0-9]*\")/(\"MAJOR_VERSION\", \"$m\")/" "$build_rs"
sed -i "s/(\"MINOR_VERSION\", \"[0-9]*\")/(\"MINOR_VERSION\", \"$n\")/" "$build_rs"
sed -i "s/(\"PATCH_VERSION\", \"[0-9]*\")/(\"PATCH_VERSION\", \"$p\")/" "$build_rs"
sed -i 's/"\\"[0-9][0-9.]*\\""/"\\"'"$new_ver"'\\""/' "$build_rs"

echo
echo "updated $build_rs to $new_ver."
echo "next steps:"
echo "  - review : git diff $build_rs"
echo "  - build  : just build   (clones the new source; the source patches in"
echo "             patch_app and the skipped-file lists may need updating for the"
echo "             new release)"
echo "  - test the app on device and in the simulator, then commit"
