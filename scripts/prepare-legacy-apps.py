#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

"""Build a standalone unsigned Legacy release from KeyOS without rebuilding firmware."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib


SOURCE_ROOT = Path(__file__).resolve().parents[1]
RELEASE_CRATES = (
    "gui-app-emu-flux-server",
    "app-flux-cosmos",
    "app-flux-dogecoin",
    "app-flux-ethereum",
    "app-flux-monero",
    "app-flux-polkadot",
    "app-flux-solana",
    "app-flux-tezos",
    "app-flux-xrp",
    "app-flux-zcash",
)
ARTIFACT_DIR = Path("target/armv7a-unknown-xous-elf/release/sideload-bundles-unsigned")
REVIEWED_APP_SOURCE = "ee0f697c17bc3227e99f06a4dc1243c6c14a3aa4"
# The published v1.4.1 tag resolves to this commit. Pin the reviewed firmware
# baseline because older local clones can retain a stale v1.4.1 tag.
RELEASED_KEYOS_SOURCE = "bb344470feb7e00da1422e878118fecfbbe30b2e"
RELEASE_REPO_URLS = {
    "git@github.com:Foundation-Devices/KeyOS-Releases-private.git",
    "https://github.com/Foundation-Devices/KeyOS-Releases-private.git",
    "ssh://git@github.com/Foundation-Devices/KeyOS-Releases-private.git",
}


class PreparationError(Exception):
    pass


def run(args: list[str], cwd: Path, *, capture: bool = False) -> str:
    result = subprocess.run(args, cwd=cwd, check=False, text=True, capture_output=capture)
    if result.returncode:
        detail = (result.stderr or result.stdout or "").strip() if capture else ""
        raise PreparationError(f"{' '.join(args)} failed (exit {result.returncode}){': ' + detail if detail else ''}")
    return result.stdout.strip() if capture else ""


def git(cwd: Path, *args: str) -> str:
    return run(["git", *args], cwd, capture=True)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise PreparationError(message)


def artifact_name(crate: str) -> str:
    return "gui-app-emu-flux" if crate == "gui-app-emu-flux-server" else crate


def crate_dir(crate: str) -> Path:
    return SOURCE_ROOT / "apps" / artifact_name(crate)


def source_manifest(crate: str) -> dict:
    with (crate_dir(crate) / "manifest.toml").open("rb") as file:
        return tomllib.load(file)


def crate_version(crate: str) -> str:
    with (crate_dir(crate) / "Cargo.toml").open("rb") as file:
        return tomllib.load(file)["package"]["version"]


def version_tuple(version: str) -> tuple[int, int, int]:
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", version)
    require(match is not None, f"Expected a stable three-part version, got {version!r}")
    return tuple(map(int, match.groups()))


def released_keyos_commit(keyos_version: str) -> str:
    # Resolve the published annotated tag, not a local tag that may predate the
    # final release. Keep the reviewed firmware baseline pinned as well.
    ref = f"refs/tags/v{keyos_version}^{{}}"
    published = git(SOURCE_ROOT, "ls-remote", "--tags", "origin", ref)
    require(published == f"{RELEASED_KEYOS_SOURCE}\t{ref}",
            f"Published KeyOS v{keyos_version} tag does not match the reviewed 1.4.1 source")
    return RELEASED_KEYOS_SOURCE


def check_source(release_name: str, keyos_version: str) -> tuple[str, str]:
    require(git(SOURCE_ROOT, "status", "--porcelain") == "", "KeyOS source checkout must be clean")
    source_commit = git(SOURCE_ROOT, "rev-parse", "HEAD")
    release_source_commit = released_keyos_commit(keyos_version)
    run(["git", "merge-base", "--is-ancestor", release_source_commit, source_commit], SOURCE_ROOT)
    run(["git", "merge-base", "--is-ancestor", REVIEWED_APP_SOURCE, source_commit], SOURCE_ROOT)

    # The 1.1.1 app changes at this exact commit were reviewed against KeyOS 1.4.1.
    # Allow release tooling to evolve, but require another review for any subsequent app or OS edit.
    changed_after_review = git(SOURCE_ROOT, "diff", "--name-only", REVIEWED_APP_SOURCE, source_commit).splitlines()
    tooling_files = {"Justfile", "xtask/src/main.rs", "imports/prepare-release/prepare-release-bin/src/build.rs",
                     "scripts/prepare-legacy-apps.py", "scripts/test_prepare_legacy_apps.py"}
    require(all(path in tooling_files for path in changed_after_review),
            f"Source changed after the reviewed 1.1.1 app commit: {', '.join(set(changed_after_review) - tooling_files)}")

    # A sideload-only release cannot silently depend on unreleased firmware services or UI crates.
    changed = git(SOURCE_ROOT, "diff", "--name-only", release_source_commit, source_commit).splitlines()
    core_changes = [
        path for path in changed
        if path.startswith(("boot/", "os/", "xous/", "server/", "ui/", "ui2/", "slint-keyos-platform/",
                            "kernel/", "loader/"))
        or (path.startswith("api/") and not path.startswith("api/gui-app-emu-flux/"))
        or (path.startswith("imports/") and not path.startswith("imports/prepare-release/"))
    ]
    require(not core_changes, f"Source changes require a firmware compatibility review: {', '.join(core_changes)}")
    legacy_version = release_name.removeprefix("legacy-mode-v")
    require(crate_version("gui-app-emu-flux-server") == legacy_version,
            "Release name must match the Legacy emulator crate version")
    app_ids = set()
    for crate in RELEASE_CRATES:
        manifest = source_manifest(crate)
        app_id = manifest["appId"]
        require(app_id not in app_ids, f"Duplicate app ID {app_id}")
        app_ids.add(app_id)
        require(manifest["publisher"] == "Foundation", f"{crate} must be Foundation-published")
        require(version_tuple(manifest["minKeyosVersion"]) <= version_tuple(keyos_version),
                f"{crate} requires KeyOS {manifest['minKeyosVersion']}, newer than {keyos_version}")
        if crate != "gui-app-emu-flux-server":
            require(manifest["permissions"]["template"] == ["flux-app"],
                    f"{crate} must use the Flux app permission template")
        if crate != "gui-app-emu-flux-server":
            require(crate_version(crate).startswith(f"{legacy_version}+"),
                    f"{crate} version must start with {legacy_version}+")
    return source_commit, release_source_commit


def read_manifest(data: bytes) -> dict:
    start = data.find(b'{"appName"')
    require(start >= 0, "App manifest JSON payload not found")
    try:
        return json.loads(data[start:])
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise PreparationError(f"Invalid app manifest JSON: {error}") from error


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def check_release_repo(releases_repo: Path) -> str:
    require(releases_repo.is_dir(), f"Release repository does not exist: {releases_repo}")
    require(Path(git(releases_repo, "rev-parse", "--show-toplevel")) == releases_repo,
            "--releases-repo must name the top-level KeyOS-Releases-private checkout")
    remote = git(releases_repo, "remote", "get-url", "origin")
    require(remote in RELEASE_REPO_URLS, f"Unexpected release repository origin: {remote}")
    run(["git", "fetch", "origin", "main"], releases_repo)
    return git(releases_repo, "rev-parse", "refs/remotes/origin/main")


def build_unsigned(keyos_version: str) -> None:
    args = ["cargo", "xtask", "build-sideload-apps", "--dont-sign", "--production-firmware",
            "--keyos-version", keyos_version, *RELEASE_CRATES]
    target = subprocess.run(["rustc", "--target", "armv7a-unknown-xous-elf", "-Zunstable-options", "--print", "cfg"],
                            cwd=SOURCE_ROOT, capture_output=True, check=False)
    if target.returncode:
        require(shutil.which("nix") is not None, "ARM target unavailable; Nix is required for this build")
        args = ["nix", "develop", ".#build", "--command", *args]
    print("Building ten unsigned Legacy bundles...")
    run(args, SOURCE_ROOT)


def check_built_bundles() -> dict[str, dict]:
    receipt = {}
    for crate in RELEASE_CRATES:
        bundle = artifact_name(crate)
        directory = SOURCE_ROOT / ARTIFACT_DIR / bundle
        require(directory.is_dir(), f"Unsigned bundle missing: {directory}")
        files = {item.name: item for item in directory.iterdir() if item.is_file()}
        require({"app.elf", "manifest.json", "icon.bin"} <= files.keys(), f"Incomplete bundle: {bundle}")
        require(not any(item.is_dir() or item.is_symlink() for item in directory.iterdir()),
                f"Unexpected nested or linked file in {bundle}")
        manifest_data = files["manifest.json"].read_bytes()
        require(manifest_data.startswith(b"{"), f"{bundle} was signed; expected an unsigned manifest")
        manifest = read_manifest(manifest_data)
        require(manifest["version"] == crate_version(crate), f"Version mismatch in {bundle}")
        require(manifest["appId"] == source_manifest(crate)["appId"], f"App ID mismatch in {bundle}")
        require(manifest["publisher"] == "Foundation", f"Publisher mismatch in {bundle}")
        hashes = manifest["fileHashes"]
        require(set(hashes) == set(files) - {"manifest.json"}, f"Incomplete fileHashes in {bundle}")
        for name, expected in hashes.items():
            require(sha256(files[name].read_bytes()) == expected, f"Hash mismatch for {bundle}/{name}")
        receipt[bundle] = {"crate": crate, "version": manifest["version"], "appId": manifest["appId"],
                           "fileHashes": hashes, "manifestSha256": sha256(manifest_data)}
    return receipt


def create_release_pr(worktree: Path, release_name: str, source_commit: str) -> None:
    compare_url = f"https://github.com/Foundation-Devices/KeyOS-Releases-private/compare/main...{release_name}"
    if shutil.which("gh"):
        try:
            run(["gh", "pr", "create", "--repo", "Foundation-Devices/KeyOS-Releases-private",
                 "--base", "main", "--head", release_name, "--title", f"Prepare {release_name} apps",
                 "--body", f"Ten unsigned Legacy 1.1.1 bundles from KeyOS {source_commit}, built for KeyOS 1.4.1 without rebuilding firmware."], worktree)
            return
        except PreparationError as error:
            # The push already succeeded; leave the branch available for a manual PR.
            print(f"Branch pushed, but PR creation failed: {error}")
    print(f"Open a PR into main: {compare_url}")


def stage(releases_repo: Path, release_name: str, keyos_version: str, source_commit: str,
          release_source_commit: str, base: str, built: dict[str, dict], push: bool) -> Path:
    expected = {artifact_name(crate) for crate in RELEASE_CRATES}
    require(set(built) == expected, "Exactly ten reviewed Legacy bundles are required for staging")
    worktree = releases_repo.parent / release_name
    require(not worktree.exists(), f"Release worktree already exists: {worktree}")
    require(subprocess.run(["git", "show-ref", "--verify", "--quiet", f"refs/heads/{release_name}"],
                           cwd=releases_repo, check=False).returncode != 0,
            f"Local release branch already exists: {release_name}")
    require(subprocess.run(["git", "ls-remote", "--exit-code", "--heads", "origin", release_name],
                           cwd=releases_repo, check=False, capture_output=True).returncode == 2,
            f"Remote release branch already exists or could not be checked: {release_name}")
    run(["git", "worktree", "add", "-b", release_name, str(worktree), base], releases_repo)
    release_dir = worktree / release_name
    bundles_dir = release_dir / "sideload-apps"
    bundles_dir.mkdir(parents=True)
    for bundle in built:
        source = SOURCE_ROOT / ARTIFACT_DIR / bundle
        destination = bundles_dir / bundle
        shutil.copytree(source, destination)
    receipt = {
        "schemaVersion": 2,
        "releaseVersion": release_name,
        "keyosVersion": keyos_version,
        "sourceCommit": source_commit,
        "releaseSourceCommit": release_source_commit,
        "bundles": built,
    }
    (release_dir / "legacy-apps-source.json").write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    shutil.copyfile(SOURCE_ROOT / "Cargo.lock", release_dir / "Cargo.lock")
    run(["git", "add", "--", release_name], worktree)
    staged = git(worktree, "diff", "--cached", "--name-only").splitlines()
    require(staged and all(path.startswith(f"{release_name}/") for path in staged),
            "Staged changes include files outside the standalone Legacy release")
    if push:
        run(["git", "commit", "-m", f"Add unsigned Legacy release {release_name} from KeyOS {source_commit[:12]}"], worktree)
        run(["git", "push", "-u", "origin", f"HEAD:refs/heads/{release_name}"], worktree)
        create_release_pr(worktree, release_name, source_commit)
    return worktree


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("release_name", help="Standalone release branch, e.g. legacy-mode-v1.1.1")
    parser.add_argument("--keyos-version", required=True, help="Already released firmware version, e.g. 1.4.1")
    parser.add_argument("--releases-repo", type=Path, required=True, help="KeyOS-Releases-private checkout")
    parser.add_argument("--check-only", action="store_true", help="Validate source and release baseline without building")
    parser.add_argument("--push", action="store_true", help="On the initial run, commit, push the new branch, and open its PR")
    args = parser.parse_args()
    try:
        require(re.fullmatch(r"legacy-mode-v\d+\.\d+\.\d+", args.release_name) is not None,
                "Release name must look like legacy-mode-v1.1.1")
        require(args.keyos_version == "1.4.1", "This app-only release targets the released KeyOS 1.4.1 firmware")
        require(not (args.check_only and args.push), "--check-only cannot be combined with --push")
        releases_repo = args.releases_repo.expanduser().resolve()
        source_commit, release_source_commit = check_source(args.release_name, args.keyos_version)
        if args.push:
            published_source = git(SOURCE_ROOT, "branch", "-r", "--contains", source_commit).splitlines()
            require(any(branch.strip().startswith("origin/") for branch in published_source),
                    "Push the KeyOS source branch first so the release receipt names a published commit")
        base = check_release_repo(releases_repo)
        print(f"KeyOS source: {source_commit}; firmware baseline: {release_source_commit}")
        print(f"Private base: {base}")
        if args.check_only:
            print("Preflight passed; no files changed")
            return
        build_unsigned(args.keyos_version)
        built = check_built_bundles()
        worktree = stage(releases_repo, args.release_name, args.keyos_version, source_commit,
                         release_source_commit, base, built, args.push)
        print(f"Prepared {len(built)} unsigned bundles in {worktree}")
        if not args.push:
            print("Review, commit, and push this branch in GitKraken, then open a PR into main:")
            print(f"https://github.com/Foundation-Devices/KeyOS-Releases-private/compare/main...{args.release_name}")
            print("--push applies only to the initial run, before the worktree exists")
    except (PreparationError, OSError, KeyError, ValueError) as error:
        parser.exit(1, f"error: {error}\n")


if __name__ == "__main__":
    main()
