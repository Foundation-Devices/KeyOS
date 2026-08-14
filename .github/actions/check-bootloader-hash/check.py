#!/usr/bin/env python3

# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

import tomllib

EXPECTED_FILE = Path(".github/actions/check-bootloader-hash/expected.toml")
VERSION_FILE = Path("boot/keyos-boot/Cargo.toml")
BOOTLOADER_FILE = Path("target/armv7a-unknown-xous-elf/release/images/boot.bin")
FIXED_SOURCE_DATE_EPOCH = "1"
BOOTSTRAP_EXPECTED = {
    "version": "0.2.1",
    "normalized_sha256": "6d5c7ed481d8a0dcfe6a4ef3f3fe2c207778156f833d9d677a0b49cd4dbb7297",
}
SEMVER_PATTERN = re.compile(
    r"^(0|[1-9][0-9]*)\."
    r"(0|[1-9][0-9]*)\."
    r"(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)


def error(message: str, *, file: Path = EXPECTED_FILE) -> None:
    print(f"::error file={file},title=Bootloader hash check failed::{message}")
    print(message, file=sys.stderr)


def load_expected(data: bytes) -> dict[str, str]:
    record = tomllib.loads(data.decode())
    version = record.get("version")
    normalized_sha256 = record.get("normalized_sha256")
    if not isinstance(version, str) or not isinstance(normalized_sha256, str):
        raise TypeError(
            "the tracked bootloader record must contain string version and normalized_sha256 fields"
        )
    if not re.fullmatch(r"[0-9a-f]{64}", normalized_sha256):
        raise ValueError(
            "the tracked bootloader normalized_sha256 must be 64 lowercase hexadecimal characters"
        )
    return {"version": version, "normalized_sha256": normalized_sha256}


def semver_precedence(
    version: str,
) -> tuple[tuple[int, int, int], tuple[tuple[int, int | str], ...] | None]:
    match = SEMVER_PATTERN.fullmatch(version)
    if match is None:
        raise ValueError(f"invalid SemVer version: {version}")

    core = tuple(int(part) for part in match.group(1, 2, 3))
    prerelease = match.group(4)
    if prerelease is None:
        return core, None

    identifiers: list[tuple[int, int | str]] = []
    for identifier in prerelease.split("."):
        if identifier.isdigit():
            if len(identifier) > 1 and identifier.startswith("0"):
                raise ValueError(f"invalid SemVer version: {version}")
            identifiers.append((0, int(identifier)))
        else:
            identifiers.append((1, identifier))
    return core, tuple(identifiers)


def semver_greater_than(candidate: str, base: str) -> bool:
    candidate_core, candidate_prerelease = semver_precedence(candidate)
    base_core, base_prerelease = semver_precedence(base)
    if candidate_core != base_core:
        return candidate_core > base_core
    if candidate_prerelease is None:
        return base_prerelease is not None
    if base_prerelease is None:
        return False
    return candidate_prerelease > base_prerelease


def package_version() -> str:
    output = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    packages = json.loads(output)["packages"]
    versions = [
        package["version"] for package in packages if package["name"] == "keyos-boot"
    ]
    if len(versions) != 1:
        raise ValueError(f"expected one keyos-boot package, found {len(versions)}")
    return versions[0]


def base_expected(base_ref: str) -> dict[str, str]:
    commit = subprocess.run(
        ["git", "cat-file", "-e", f"{base_ref}^{{commit}}"],
        capture_output=True,
        check=False,
    )
    if commit.returncode != 0:
        raise ValueError(f"the bootloader base ref is not a commit: {base_ref}")

    result = subprocess.run(
        ["git", "show", f"{base_ref}:{EXPECTED_FILE}"],
        capture_output=True,
        check=False,
    )
    if result.returncode == 0:
        return load_expected(result.stdout)

    # This fixed record is the one-time baseline for branches that predate
    # expected.toml. A changed hash still has to increase this version.
    print(f"The base branch has no {EXPECTED_FILE}; using the bootstrap record.")
    return BOOTSTRAP_EXPECTED.copy()


def main() -> int:
    base_ref = os.environ.get("BOOTLOADER_BASE_REF")
    if not base_ref:
        error("BOOTLOADER_BASE_REF is required")
        return 2

    try:
        current = load_expected(EXPECTED_FILE.read_bytes())
        base = base_expected(base_ref)
        actual_version = package_version()
    except (
        OSError,
        TypeError,
        UnicodeError,
        ValueError,
        tomllib.TOMLDecodeError,
    ) as exception:
        error(str(exception))
        return 2

    environment = os.environ.copy()
    environment["KEYOS_SOURCE_DATE_EPOCH"] = FIXED_SOURCE_DATE_EPOCH
    subprocess.run(
        ["cargo", "xtask", "build-bootloader", "--production-bootloader"],
        check=True,
        env=environment,
    )
    actual_hash = hashlib.sha256(BOOTLOADER_FILE.read_bytes()).hexdigest()

    print(
        f"Tracked bootloader: version {current['version']}, normalized SHA-256 "
        f"{current['normalized_sha256']}"
    )
    print(
        f"Built bootloader:   version {actual_version}, normalized SHA-256 {actual_hash}"
    )

    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        with open(summary_path, "a", encoding="utf-8") as summary:
            summary.write("### Bootloader hash check\n\n")
            summary.write(
                "| Source | Version | Normalized SHA-256 |\n| --- | --- | --- |\n"
            )
            summary.write(
                f"| Tracked | `{current['version']}` | `{current['normalized_sha256']}` |\n"
            )
            summary.write(f"| Built | `{actual_version}` | `{actual_hash}` |\n")

    if actual_version != current["version"]:
        error(
            f"The keyos-boot package version ({actual_version}) does not match the tracked version "
            f"({current['version']}). Update {EXPECTED_FILE}.",
            file=VERSION_FILE,
        )
        return 1
    if actual_hash != current["normalized_sha256"]:
        error(
            f"The normalized bootloader SHA-256 does not match its tracked value. "
            f"Increase the package version "
            f"in {VERSION_FILE}, then update {EXPECTED_FILE}."
        )
        return 1

    if current["normalized_sha256"] != base["normalized_sha256"]:
        try:
            increased = semver_greater_than(current["version"], base["version"])
        except ValueError as exception:
            error(str(exception))
            return 2
        if not increased:
            error(
                f"The tracked bootloader hash changed, but its version did not increase "
                f"(base: {base['version']}, merge result: {current['version']})."
            )
            return 1

    print("The built bootloader matches its tracked version and hash.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except subprocess.CalledProcessError as exception:
        error(f"command failed with exit status {exception.returncode}")
        raise SystemExit(exception.returncode) from exception
