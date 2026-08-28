#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Check for a newer upstream release of a Flux app and repin its build.rs to it.
#
#   scripts/update-flux.sh <ethereum|solana|monero|zcash> [--check]
#
# Release tags look like flex_<flexver>_<appver>_sdk_<sdkref>; the app version is
# the third underscore-separated field. A pin carries both the tag and the full
# commit OID it points at, and the build rejects a tag that has moved off its
# pinned OID, so each new tag is resolved here. `--check` reports without writing
# changes.
set -euo pipefail

app="${1:-}"
case "$app" in
    ethereum | solana | monero | zcash) ;;
    *)
        echo "usage: $0 <ethereum|solana|monero|zcash> [--check]" >&2
        exit 2
        ;;
esac

build_rs="apps/app-flux-$app/build.rs"
manifest="apps/app-flux-$app/Cargo.toml"
[ -f "$build_rs" ] || {
    echo "not found: $build_rs (run from the repo root)" >&2
    exit 1
}

read_pin() { sed -n "s/.*$1\"\([^\"]*\)\".*/\1/p" "$build_rs" | head -1; }
app_name="$(sed -n '/const APP_SOURCE/,/};/s/.*name: "\([^"]*\)".*/\1/p' "$build_rs")"
cur_tag="$(read_pin 'pin: GitPin { tag: ')"
cur_sdk="$(read_pin 'SDK_PIN: GitPin = GitPin { tag: ')"
[ -n "$app_name" ] && [ -n "$cur_tag" ] && [ -n "$cur_sdk" ] ||
    {
        echo "couldn't read the APP_SOURCE and SDK_PIN pins from $build_rs" >&2
        exit 1
    }
cur_ver="$(printf '%s' "$cur_tag" | cut -d_ -f3)"

# Upstream repositories. The org in these URLs is an external identifier that
# can't be renamed; they are the only such references here.
repo="https://github.com/LedgerHQ/$app_name.git"
sdk_repo="https://github.com/LedgerHQ/ledger-secure-sdk.git"
echo "app-flux-$app: pinned to $cur_tag (app $cur_ver, sdk $cur_sdk)"
echo "checking upstream for a newer $app_name release ..."

# Upstream release tags, split into final releases and prereleases: a prerelease
# carries a suffix on the app version (1.17.0-rc1), a release does not.
all_tags="$(git ls-remote --tags "$repo" |
    grep -oE 'flex_[0-9.]+_[0-9][0-9A-Za-z.-]*_sdk_[A-Za-z0-9._]+' |
    sort -u)"
final_tags="$(printf '%s\n' "$all_tags" | awk -F_ '$3 !~ /-/')"
newest_tag="$(printf '%s\n' "$final_tags" | awk -F_ '{ print $3 "\t" $0 }' | sort -V | tail -1 | cut -f2)"
[ -n "$newest_tag" ] || {
    echo "no flex_* release tags found upstream" >&2
    exit 1
}
new_ver="$(printf '%s' "$newest_tag" | cut -d_ -f3)"

# Upstream version numbers are not monotonic in time: app-solana kept tagging
# 1.15.x after 1.16.0, so the highest version can be an older build off their
# development line. Date the release tags to catch that. Fetching them with
# tree:0 and depth 1 pulls only the commits they point at, a couple of hundred
# KB into a throwaway bare repo.
dates_dir="$(mktemp -d)"
trap 'rm -rf "$dates_dir"' EXIT
git init -q --bare "$dates_dir"
refspecs=()
while read -r t; do
    [ -n "$t" ] && refspecs+=("refs/tags/$t:refs/tags/$t")
done <<<"$final_tags"
dated_tags=""
if [ ${#refspecs[@]} -gt 0 ] &&
    git -C "$dates_dir" fetch -q --filter=tree:0 --depth=1 --no-tags "$repo" "${refspecs[@]}" 2>/dev/null; then
    dated_tags="$(git -C "$dates_dir" for-each-ref --sort=-creatordate \
        --format='%(creatordate:short) %(refname:strip=2)' refs/tags)"
fi
tag_date() { printf '%s\n' "$dated_tags" | awk -v t="$1" '$2 == t { print $1; exit }'; }

newest_dated="$(printf '%s\n' "$dated_tags" | head -1)"
if [ -n "$dated_tags" ] && [ "${newest_dated#* }" != "$newest_tag" ]; then
    echo
    echo "note: the most recently tagged release is not the highest version"
    echo "  newest  : ${newest_dated#* } (${newest_dated%% *})"
    echo "  highest : $newest_tag"
    echo "  upstream may still ship the older line; check its master branch before pinning"
fi

newest_pre="$(printf '%s\n' "$all_tags" | awk -F_ '$3 ~ /-/ { print $3 "\t" $0 }' | sort -V | tail -1 | cut -f2)"
pre_ver="$(printf '%s' "$newest_pre" | cut -d_ -f3)"
if [ -n "$newest_pre" ] && [ "$pre_ver" != "$cur_ver" ] &&
    [ "$(printf '%s\n%s\n' "$cur_ver" "$pre_ver" | sort -V | tail -1)" = "$pre_ver" ]; then
    echo
    echo "note: newer prerelease upstream, not pinned (final releases only): $newest_pre"
fi

# A deliberate pin to a maintained older line must not be undone by the version sort.
cur_date="$(tag_date "$cur_tag")"
new_date="$(tag_date "$newest_tag")"
if [ -n "$cur_date" ] && [ -n "$new_date" ] && [ "$new_date" \< "$cur_date" ]; then
    echo
    echo "staying on $cur_ver: $new_ver is the higher version but its tag is older"
    echo "  pinned  : $cur_tag ($cur_date)"
    echo "  highest : $newest_tag ($new_date)"
    exit 0
fi

# Strictly-newer test: the version-sorted maximum must be the upstream one.
if [ "$cur_ver" = "$new_ver" ] ||
    [ "$(printf '%s\n%s\n' "$cur_ver" "$new_ver" | sort -V | tail -1)" != "$new_ver" ]; then
    echo "already on the newest release (upstream newest is $new_ver)"
    exit 0
fi

new_sdk="${newest_tag#*_sdk_}"
echo
echo "newer release available:"
echo "  tag : $cur_tag"
echo "     -> $newest_tag"
echo "  app : $cur_ver -> $new_ver"
echo "  sdk : $cur_sdk -> $new_sdk"

if [ "${2:-}" = "--check" ]; then
    echo
    echo "(--check: no changes written)"
    exit 0
fi

# The commit a tag names. An annotated tag carries it on its peeled ref, which is
# also what the build's `rev-parse <tag>^{commit}` check compares the pin against.
resolve_tag() {
    local refs oid
    refs="$(git ls-remote "$1" "refs/tags/$2" "refs/tags/$2^{}")"
    oid="$(printf '%s\n' "$refs" | sed -n 's/^\([0-9a-f]\{40\}\).*\^{}$/\1/p')"
    if [ -z "$oid" ]; then
        oid="$(printf '%s\n' "$refs" | sed -n 's/^\([0-9a-f]\{40\}\).*/\1/p')"
    fi
    printf '%s' "$oid"
}

# Both pins are written as a single-line `GitPin { tag, commit }` literal; a
# wrapped one means the file was reformatted and needs repinning by hand.
repin() {
    local literal=' { tag: "[^"]*", commit: "[^"]*" }'
    grep -q "$1$literal" "$build_rs" || {
        echo "couldn't find a one-line \`$1 { tag, commit }\` pin in $build_rs" >&2
        exit 1
    }
    sed -i "s#$1$literal#$1 { tag: \"$2\", commit: \"$3\" }#" "$build_rs"
}

app_oid="$(resolve_tag "$repo" "$newest_tag")"
sdk_oid="$(resolve_tag "$sdk_repo" "$new_sdk")"
[ -n "$app_oid" ] && [ -n "$sdk_oid" ] ||
    {
        echo "couldn't resolve $newest_tag and $new_sdk to commit OIDs" >&2
        exit 1
    }

repin 'pin: GitPin' "$newest_tag" "$app_oid"
repin 'SDK_PIN: GitPin = GitPin' "$new_sdk" "$sdk_oid"

IFS=. read -r m n p <<EOF
$new_ver
EOF
# Hosted-build version defines, so the simulator reports the same version.
sed -i "s/(\"APPVERSION_M\", \"[0-9]*\")/(\"APPVERSION_M\", \"$m\")/" "$build_rs"
sed -i "s/(\"APPVERSION_N\", \"[0-9]*\")/(\"APPVERSION_N\", \"$n\")/" "$build_rs"
sed -i "s/(\"APPVERSION_P\", \"[0-9]*\")/(\"APPVERSION_P\", \"$p\")/" "$build_rs"
sed -i "s/(\"MAJOR_VERSION\", \"[0-9]*\")/(\"MAJOR_VERSION\", \"$m\")/" "$build_rs"
sed -i "s/(\"MINOR_VERSION\", \"[0-9]*\")/(\"MINOR_VERSION\", \"$n\")/" "$build_rs"
sed -i "s/(\"PATCH_VERSION\", \"[0-9]*\")/(\"PATCH_VERSION\", \"$p\")/" "$build_rs"
sed -i 's/"\\"[0-9][0-9.]*\\""/"\\"'"$new_ver"'\\""/' "$build_rs"
# The crate version tracks the upstream app version.
sed -i "0,/^version = \"[^\"]*\"/s//version = \"$new_ver\"/" "$manifest"

echo
echo "updated $build_rs and $manifest to $new_ver."
echo "  app : $newest_tag @ $app_oid"
echo "  sdk : $new_sdk @ $sdk_oid"
echo "next steps:"
echo "  - review : git diff apps/app-flux-$app"
echo "  - format : just fmt   (a longer tag can push a pin past the line limit)"
echo "  - build  : just build   (clones the new source and updates Cargo.lock; the"
echo "             source patches in patch_app and the skipped-file lists may need"
echo "             updating for the new release)"
echo "  - test the app on device and in the simulator, then commit"
