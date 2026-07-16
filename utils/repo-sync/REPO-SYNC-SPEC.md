<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# repo-sync — Specification

## Overview

`repo-sync` is a Rust CLI tool that publishes release snapshots from a source Git repository to a destination Git repository. It guarantees byte-for-byte identical file contents between the source repo at a given tag and the corresponding commit in the destination repo.

The destination repo contains exactly one commit per release on `main`, ordered chronologically by sync time. Re-syncing a tag destructively replaces the previous commit and rewrites history so no trace of the prior sync remains.

All commits and tags are GPG-signed so they appear as verified on GitHub.

## Requirements

- **Byte-for-byte fidelity**: The destination repo working tree at a given tag must be identical to the source repo working tree at the same tag, minus any explicitly excluded files.
- **Single commit per release**: The destination repo's `main` branch contains one commit per synced release. No merge commits, no intermediate history.
- **Destructive re-sync**: If a tag has already been synced, re-running `sync` for that tag replaces the commit entirely. The old commit is unreachable and garbage-collectible. Force-push updates the remote.
- **No source history leakage**: No commit messages, author info, timestamps, or file contents from the source repo's history appear in the destination repo beyond the snapshot itself.
- **GPG-signed commits and tags**: Every commit and tag in the destination repo must be signed with a GPG key. The tool refuses to create unsigned commits.
- **Configuration layered**: CLI flags take precedence over config file values, which take precedence over built-in defaults.
- **Semver tag format**: Tags must follow the `vMAJOR.MINOR.PATCH` format (e.g. `v1.2.3`). Non-conforming tags trigger a warning and interactive confirmation prompt before proceeding. This validation applies to both the source tag in the source repo and the destination tag written to the destination repo.

## CLI Interface

### Global

```
repo-sync — Publish release snapshots from a source repo to a destination repo.

USAGE:
    repo-sync [OPTIONS] <COMMAND>

COMMANDS:
    init      Initialize a new destination repo for release syncing
    sync      Sync a release tag to the destination repo
    verify    Verify destination repo matches source repo at a given tag
    list      List synced releases in the destination repo
    help      Print help information

OPTIONS:
    -c, --config <FILE>    Config file path [default: .repo-sync.toml]
    -h, --help             Print help
    -V, --version          Print version
```

### `init`

```
repo-sync init — Initialize a new destination repo for release syncing.

USAGE:
    repo-sync init [OPTIONS] --destination <PATH>

OPTIONS:
    --destination <PATH>        Path where the destination repo will be created
    --remote <URL>         Optional remote URL to configure as 'origin'
```

Creates a new Git repository at the specified path with:

- An empty `main` branch (orphan, no initial commit)
- The configured remote, if provided
- A `.gitkeep` or equivalent is **not** created — the repo remains truly empty until the first `sync`

If the directory already exists and contains a git repo, the command exits with an error. If the directory exists but is not a git repo, the command initializes one inside it.

### `sync`

```
repo-sync sync — Sync a release tag to the destination repo as a single commit.
                  If the tag was previously synced, destructively replaces it.

USAGE:
    repo-sync sync [OPTIONS] --tag <TAG>

OPTIONS:
    --source <PATH>             Path to source repo
    --destination <PATH>        Path to destination repo
    --tag <TAG>            Release tag to sync (e.g. v1.2.3)
    --gpg-key <KEY_ID>     GPG key ID to sign commits and tags
    --exclude <FILE>       Optional exclude file path (gitignore-style patterns)
    --message <MSG>        Commit message template
    --push                      Force-push to destination remote after sync
    --dry-run              Show what would change without modifying anything
    -v, --verbose          Verbose output
```

`--tag` is required and has no config file equivalent. `--dry-run` and `--verbose` are CLI-only flags.
When `--push` is set, the tool pushes explicitly to `origin` with `main` and tags; it does not rely on `push.default` or upstream tracking. If `origin` is missing, the command fails before making changes.

### `verify`

```
repo-sync verify — Verify byte-for-byte match between repos at a tag.

USAGE:
    repo-sync verify [OPTIONS] --tag <TAG>

OPTIONS:
    --source <PATH>             Path to source repo
    --destination <PATH>        Path to destination repo
    --tag <TAG>            Release tag to verify
    --exclude <FILE>       Optional exclude file path (same rules as sync)
```

Exits with code 0 if all files match, non-zero otherwise. Prints a diff summary of any mismatches to stderr.

### `list`

```
repo-sync list — List all synced releases in the destination repo.

USAGE:
    repo-sync list [OPTIONS]

OPTIONS:
    --destination <PATH>        Path to destination repo
```

Outputs one line per synced tag with the tag name, commit hash, and commit date.

## Tag Format Validation

Tags are validated against the pattern `v<MAJOR>.<MINOR>.<PATCH>` where MAJOR, MINOR, and PATCH are non-negative integers (e.g. `v1.2.3`, `v0.12.0`, `v10.0.1`).

If a tag does not match this format, the tool:

1. Prints a warning to stderr: `Warning: Tag '<tag>' does not follow the expected vMAJOR.MINOR.PATCH format.`
2. Prompts the user: `Continue with non-standard tag format? [y/N]`
3. Aborts unless the user explicitly confirms.

This check runs on every command that accepts `--tag` (`sync`, `verify`). It validates the tag string itself — it does not check whether the tag exists yet (that happens later in the command's execution).

In non-interactive contexts (e.g. piped stdin), the prompt defaults to No and the command aborts. A future `--force` flag could bypass this.

## GPG Signing

Every commit and annotated tag created in the destination repo must be GPG-signed. This is a hard requirement — the tool will not create unsigned commits under any circumstances.

### Key Resolution

The GPG key is resolved in this order:

1. `--gpg-key <KEY_ID>` CLI flag
2. `signing.key` in the config file
3. `user.signingkey` from the destination repo's git config
4. GPG's default key (based on the configured author email)

If no usable key is found, the tool exits with an error before making any changes.

### Signing Behavior

- **Commits** are created with GPG signatures (equivalent to `git commit -S`).
- **Tags** are created as signed annotated tags (equivalent to `git tag -s`).
- During a **re-sync**, all rewritten commits are re-signed. This means the signing key must be available for the entire history rewrite, not just the new commit.
- The tool invokes `gpg` (or `gpg2`) via the system PATH for signing. It does not implement GPG internally. The `gpg-agent` must be running and the key must be unlocked (or the agent configured with a passphrase preset) for non-interactive use.

### Verification on GitHub

For commits to show as "Verified" on GitHub:

- The GPG key's email must match the `author.email` in the config.
- The public key must be uploaded to the signer's GitHub account.
- These are GitHub-side requirements outside the scope of this tool, but the tool should print a reminder if `--verbose` is set.

## Configuration File

Default path: `.repo-sync.toml` in the current working directory.

```toml
[repo]
source = "../passport-source"
destination = "../passport-destination"

[sync]
message = "Release {tag}"
push = false

[signing]
key = "ABCDEF1234567890"

[author]
name = "Foundation Devices"
email = "releases@foundation.xyz"
```

### Config Fields

| Section   | Key       | Type   | Default           | Description                                                                                 |
| --------- | --------- | ------ | ----------------- | ------------------------------------------------------------------------------------------- |
| `repo`    | `source`  | string | `"."`             | Path to the source repository                                                               |
| `repo`    | `destination` | string | —            | Path to the destination repository                                                          |
| `sync`    | `exclude` | string | — (disabled)      | Optional path to exclude patterns file (relative to source repo root)                      |
| `sync`    | `message` | string | `"Release {tag}"` | Commit message template. `{tag}` is substituted.                                            |
| `sync`    | `push`    | bool   | `false`           | Whether to force-push after sync                                                            |
| `signing` | `key`     | string | —                 | GPG key ID for signing commits and tags. Falls back to git config / GPG default if omitted. |
| `author`  | `name`    | string | —                 | Git author name for destination commits                                                     |
| `author`  | `email`   | string | —                 | Git author email for destination commits                                                    |

If `author` is omitted, the tool falls back to the destination repo's `user.name` / `user.email` git config.

Legacy compatibility: the tool also accepts `--private`/`--public` CLI flags and `[repo].private`/`[repo].public` config keys as aliases for `source`/`destination`.

## Resolution Order

For any given setting: **CLI flag → config file → built-in default**.

Fields with no built-in default (`repo.destination`, `author.*`) must be specified via CLI or config. The tool exits with an error if a required value is missing.

## Exclude File (Optional)

The exclude feature is entirely opt-in. By default, no files are excluded and the destination repo is an exact copy of the source repo at the given tag.

If needed, an exclude file can be specified via `--exclude` on the CLI or `sync.exclude` in the config. The file uses gitignore-style patterns:

```
# Lines starting with # are comments
# Blank lines are ignored

# Exclude specific files
internal-notes.md

# Exclude directories
scripts/internal/
docs/private/

# Glob patterns
*.secret
.env*
```

An example file (`exclude.example`) is included in the repo-sync project for reference.

No files are ever implicitly excluded.

## Sync Algorithm

1. **Validate inputs**: Confirm both repos exist and are valid git repos. Confirm the tag exists in the source repo.
2. **Validate tag format**: Check tag against `vMAJOR.MINOR.PATCH`. Warn and prompt if non-conforming.
3. **Validate GPG key**: Resolve the signing key and confirm it is available and usable. Abort if no key is found.
4. **Export snapshot**: Use `git archive` on the source repo at the given tag to extract a clean snapshot into a temporary directory.
5. **Apply excludes**: If an exclude file is configured, remove matching files from the snapshot.
6. **Prepare destination repo**: Check out `main` in the destination repo.
7. **Determine insertion point**: Scan existing commits on `main` to find where this tag belongs chronologically. If the tag already exists, identify the commit to replace.
8. **Replace working tree**: Delete all tracked files in the destination repo working tree. Copy the snapshot contents in. This ensures byte-for-byte fidelity — no merge, no diff, just a full replacement.
9. **Commit and sign**: Stage all changes and create a GPG-signed commit with the configured author and message.
10. **Tag and sign**: Create a signed annotated tag pointing to the new commit.
11. **Rewrite history (if re-sync)**: If replacing an existing tag's commit, splice the new commit in place of the old one and rebase all subsequent commits to maintain a clean linear history. All rewritten commits are re-signed. The old commit SHA and all subsequent SHAs will change.
12. **Force-push (if `--push`)**: Confirm `origin` exists, then run `git push --force origin refs/heads/main:refs/heads/main` and `git push --force origin --tags`.

### Re-sync Behavior

When a tag already exists in the destination repo:

- The old commit for that tag is replaced with a new commit containing the updated snapshot.
- The old tag is deleted and recreated as a new signed annotated tag pointing to the new commit.
- All subsequent commits (later releases) are rebased on top of the replacement and re-signed.
- The result is a clean linear history with no evidence of the re-sync.
- This is a destructive operation. The old commit SHA will change, as will all subsequent commit SHAs.

## Verify Algorithm

1. Export the source repo snapshot at the given tag (same as sync steps 2–5).
2. Export the destination repo contents at the given tag.
3. Recursively compare all files byte-for-byte using SHA-256 hashes.
4. Report any differences: missing files, extra files, content mismatches.
5. Exit 0 if identical, exit 1 if differences found.

## Dependencies (Recommended Crates)

- `clap` — CLI argument parsing with derive macros
- `serde` + `toml` — Config file deserialization
- `git2` — libgit2 bindings for Git operations
- `sha2` — SHA-256 hashing for verification
- `tempfile` — Temporary directory management
- `glob` or `ignore` — Gitignore-style pattern matching (only needed if exclude is used)
- `anyhow` — Error handling

Note: GPG signing is handled by invoking the system `gpg`/`gpg2` binary rather than through a Rust crate. This ensures compatibility with existing gpg-agent setups and key management.

## Exit Codes

| Code | Meaning                                    |
| ---- | ------------------------------------------ |
| 0    | Success                                    |
| 1    | Verification failed (files differ)         |
| 2    | Invalid arguments or missing configuration |
| 3    | Git operation failed                       |
| 4    | I/O error                                  |
| 5    | GPG signing failed                         |

## Future Considerations

- Additional message template placeholders: `{date}`, `{hash}`, `{short_hash}`
- `--force` flag to bypass interactive prompts (e.g. non-standard tag confirmation)
- Pre-sync and post-sync hook scripts
- Support for syncing to multiple destination repos from a single config
