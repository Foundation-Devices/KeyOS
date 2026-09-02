# prepare-release

A Rust implementation of the KeyOS release preparation tool, converted from the original `scripts/prepare-release.sh` bash script.

## Features

- **Type-safe version validation**: Uses `semver` crate for proper semantic version parsing
- **Single release-version input**: Passes the CLI version through to the production firmware build
  and rejects RecoveryOS-incompatible prereleases such as `beta.3` (use `beta3`)
- **Better error handling**: Structured error types with detailed error messages
- **Environment validation**: Validates `EXTRA_ENTROPY` format using regex
- **Interactive prompts**: Asks for user confirmation when overwriting existing branches/directories
- **Comprehensive git operations**: Handles branch creation, file copying, commits, and GitHub PR creation
- **Progress feedback**: Clear status messages throughout the process

## Usage

```bash
cd /path/to/keyOS
cargo run --bin prepare-release -- <new-version>
```

Examples:

```bash
# Using semantic versions
cargo run --bin prepare-release -- 1.2.3
```

## Prerequisites

1. **EXTRA_ENTROPY environment variable**: Must be set to a 64-character hexadecimal string (32 bytes)

   ```bash
   export EXTRA_ENTROPY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
   ```

2. **KeyOS-Releases-private repository**: Must exist as a sibling directory to the keyOS repository

   ```
   parent-directory/
   ├── keyOS/                 # Main keyOS repository
   └── KeyOS-Releases-private/ # Private releases repository
   ```

3. **Build tools**:

   - Requires `cargo xtask` commands for building firmware
   - ARM cross-compiler toolchain may be required for bootloader builds

4. **Git and GitHub CLI** (optional): For automatic PR creation, install `gh` CLI tool

## What it does

1. **Validates inputs**:

   - Parses and validates semantic version
   - Validates EXTRA_ENTROPY format

2. **Builds firmware** (all unsigned for release preparation):

   - For factory releases: builds bootloader, recovery, and main firmware
   - For update releases: builds main firmware only
   - Verifies all firmware files exist

3. **Manages git operations**:
   - Switches to KeyOS-Releases-private repository
   - Creates new branch for the release
   - Copies firmware files, apps, and bootloader assets (blassets - .raw files only)
   - Commits and pushes changes
   - Creates GitHub PR (if gh CLI available)

## Error Handling

The Rust version provides much better error handling than the original bash script:

- **Structured error types**: Each module has its own error enum with specific error variants
- **Detailed error messages**: Clear descriptions of what went wrong and how to fix it
- **Early validation**: Catches errors before starting expensive build operations
- **Graceful cleanup**: Properly handles git stash operations and directory changes

## Dependencies

The tool uses carefully selected crates modeled after the `cosign2` implementation:

- `clap`: Command-line argument parsing with derive macros
- `colored`: Colored terminal output for better UX
- `semver`: Semantic version parsing and comparison
- `regex`: EXTRA_ENTROPY format validation
- `serde` + `toml`: Configuration file generation
- `tempfile`: Safe temporary file handling (if needed for future features)

## Building

```bash
cd imports/prepare-release
cargo build --release
```

The binary will be available at `target/release/prepare-release`.
