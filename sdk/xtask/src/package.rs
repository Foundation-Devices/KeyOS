// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{boxed_err, selected_targets, Config, Result};
use crate::util;

#[derive(Clone, Debug)]
struct EmbeddedPublicKey {
    armored: String,
    fingerprint: String,
}

#[derive(Clone, Debug)]
pub struct PackageArgs {
    pub targets: Vec<String>,
    pub version: Option<String>,
    pub output_dir: PathBuf,
    pub verbose: bool,
}

#[derive(Clone, Debug)]
pub struct FinalizeArgs {
    pub version: Option<String>,
    pub output_dir: PathBuf,
    pub sign_key: Option<String>,
    pub verbose: bool,
}

impl Default for PackageArgs {
    fn default() -> Self {
        Self { targets: Vec::new(), version: None, output_dir: PathBuf::from("dist"), verbose: false }
    }
}

impl Default for FinalizeArgs {
    fn default() -> Self {
        Self { version: None, output_dir: PathBuf::from("dist"), sign_key: None, verbose: false }
    }
}

impl PackageArgs {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut args = Self::default();
        let mut iter = raw.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--target" => args.targets.push(next_value(&mut iter, "--target")?),
                "--version" => args.version = Some(next_value(&mut iter, "--version")?),
                "--output-dir" => args.output_dir = PathBuf::from(next_value(&mut iter, "--output-dir")?),
                "--verbose" => args.verbose = true,
                other => return Err(boxed_err(format!("unsupported package option: {other}"))),
            }
        }

        Ok(args)
    }
}

impl FinalizeArgs {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut args = Self::default();
        let mut iter = raw.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--version" => args.version = Some(next_value(&mut iter, "--version")?),
                "--output-dir" => args.output_dir = PathBuf::from(next_value(&mut iter, "--output-dir")?),
                "--sign-key" => args.sign_key = Some(next_value(&mut iter, "--sign-key")?),
                "--verbose" => args.verbose = true,
                other => return Err(boxed_err(format!("unsupported finalize option: {other}"))),
            }
        }

        Ok(args)
    }
}

pub fn run(
    root: &Path,
    config: &Config,
    args: &PackageArgs,
    sign_key: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let output_dir = util::absolute_path(root, &args.output_dir);
    let targets = selected_targets(config, &args.targets)?;
    let version = args.version.clone().unwrap_or_else(|| config.sdk.version.clone());

    check_package_prerequisites(sign_key)?;

    let tar_program = find_gnu_tar()?.ok_or_else(|| {
        boxed_err("deterministic packaging requires GNU tar (expected 'tar' or 'gtar' with GNU tar)")
    })?;
    let mut archive_paths = Vec::new();

    let common_stage = common_stage_dir(&output_dir);
    if !common_stage.exists() {
        return Err(boxed_err(format!("missing common build staging directory: {}", common_stage.display())));
    }
    let common_archive_path = output_dir.join(common_archive_name(&version));
    remove_if_exists(&common_archive_path)?;
    let mut common_archive_command =
        deterministic_archive_command(&tar_program, &common_archive_path, &common_stage);
    util::run_command(&mut common_archive_command, verbose)?;
    archive_paths.push(common_archive_path);

    for target in &targets {
        let stage_source = target_stage_dir(&output_dir, target);
        if !stage_source.exists() {
            return Err(boxed_err(format!(
                "missing build staging directory for target {target}: {}",
                stage_source.display()
            )));
        }

        let archive_path = output_dir.join(target_archive_name(&version, target));
        remove_if_exists(&archive_path)?;
        let mut archive_command = deterministic_archive_command(&tar_program, &archive_path, &stage_source);
        util::run_command(&mut archive_command, verbose)?;
        archive_paths.push(archive_path);
    }

    write_release_metadata(&output_dir, &version, &targets, &archive_paths, sign_key, verbose)?;
    Ok(())
}

pub fn finalize(root: &Path, config: &Config, args: &FinalizeArgs) -> Result<()> {
    let output_dir = util::absolute_path(root, &args.output_dir);
    let version = args.version.clone().unwrap_or_else(|| config.sdk.version.clone());
    let sign_key =
        args.sign_key.clone().or_else(|| default_sign_key(config)).ok_or_else(|| {
            boxed_err(format!("finalize requires --sign-key or {}", config.signing.key_env))
        })?;
    finalize_release(&output_dir, config, &version, Some(sign_key.as_str()), args.verbose)
}

pub fn stage_root_dir(output_dir: &Path) -> PathBuf { output_dir.join(".stage") }

pub fn common_stage_dir(output_dir: &Path) -> PathBuf { stage_root_dir(output_dir).join("common") }

pub fn target_stage_dir(output_dir: &Path, target: &str) -> PathBuf {
    stage_root_dir(output_dir).join(target)
}

pub fn check_prerequisites(sign_key: Option<&str>) -> Result<()> { check_package_prerequisites(sign_key) }

fn check_package_prerequisites(sign_key: Option<&str>) -> Result<()> {
    if find_gnu_tar()?.is_none() {
        return Err(boxed_err(
            "deterministic packaging requires GNU tar (expected 'tar' or 'gtar' with GNU tar)",
        ));
    }

    check_checksum_prerequisites()?;
    check_signing_prerequisites(sign_key)?;
    Ok(())
}

fn check_finalize_prerequisites(sign_key: Option<&str>) -> Result<()> {
    check_checksum_prerequisites()?;
    check_signing_prerequisites(sign_key)?;
    Ok(())
}

fn finalize_release(
    output_dir: &Path,
    config: &Config,
    version: &str,
    sign_key: Option<&str>,
    verbose: bool,
) -> Result<()> {
    check_finalize_prerequisites(sign_key)?;

    let common_archive_path = output_dir.join(common_archive_name(version));
    if !common_archive_path.exists() {
        return Err(boxed_err(format!(
            "missing packaged common archive for version {version}: {}",
            common_archive_path.display()
        )));
    }

    let targets = discover_packaged_targets(config, output_dir, version)?;
    require_all_configured_targets(config, version, &targets)?;
    let mut archive_paths = vec![common_archive_path];
    archive_paths.extend(targets.iter().map(|target| output_dir.join(target_archive_name(version, target))));

    write_release_metadata(output_dir, version, &targets, &archive_paths, sign_key, verbose)?;
    Ok(())
}

fn check_checksum_prerequisites() -> Result<()> {
    if !(util::command_exists("shasum") || util::command_exists("sha256sum")) {
        return Err(boxed_err("packaging requires either 'shasum' or 'sha256sum' to compute checksums"));
    }

    Ok(())
}

fn check_signing_prerequisites(sign_key: Option<&str>) -> Result<()> {
    if let Some(key) = sign_key {
        let gpg_program = find_gpg()?.ok_or_else(|| boxed_err("signing requires 'gpg' or 'gpg2'"))?;
        if !gpg_secret_key_exists(&gpg_program, key)? {
            return Err(boxed_err(format!("GPG signing identity not found in secret keyring: {key}")));
        }
    }

    Ok(())
}

fn sign_archive(archive: &Path, key: &str, verbose: bool) -> Result<PathBuf> {
    let signature_path = PathBuf::from(format!("{}.sig", archive.display()));
    if signature_path.exists() {
        fs::remove_file(&signature_path)?;
    }

    let gpg_program = find_gpg()?.ok_or_else(|| boxed_err("signing requires 'gpg' or 'gpg2'"))?;
    let mut command = detached_signature_command(&gpg_program, key, archive, &signature_path);
    util::run_command(&mut command, verbose)?;

    Ok(signature_path)
}

fn export_public_key(signer: &str) -> Result<EmbeddedPublicKey> {
    let gpg_program = find_gpg()?.ok_or_else(|| boxed_err("signing requires 'gpg' or 'gpg2'"))?;
    let output =
        Command::new(&gpg_program).arg("--batch").arg("--armor").arg("--export").arg(signer).output()?;

    if !output.status.success() {
        return Err(boxed_err(format!("failed to export GPG public key for signing identity: {signer}")));
    }

    let public_key = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if public_key.is_empty() || !public_key.contains("BEGIN PGP PUBLIC KEY BLOCK") {
        return Err(boxed_err(format!(
            "exported GPG public key for signing identity was empty or invalid: {signer}"
        )));
    }

    let fingerprint = gpg_public_key_fingerprint(&gpg_program, signer)?;
    Ok(EmbeddedPublicKey { armored: public_key, fingerprint })
}

fn gpg_public_key_fingerprint(gpg_program: impl AsRef<OsStr>, signer: &str) -> Result<String> {
    let output = Command::new(gpg_program)
        .arg("--batch")
        .arg("--with-colons")
        .arg("--fingerprint")
        .arg("--list-keys")
        .arg(signer)
        .output()?;

    if !output.status.success() {
        return Err(boxed_err(format!(
            "failed to read GPG public key fingerprint for signing identity: {signer}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter(|line| line.starts_with("fpr:"))
        .find_map(|line| line.split(':').nth(9))
        .map(str::trim)
        .filter(|fingerprint| !fingerprint.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            boxed_err(format!("failed to locate GPG public key fingerprint for signing identity: {signer}"))
        })
}

fn checksum_line(path: &Path) -> Result<String> {
    Ok(format!("{}  {}", util::sha256(path)?, util::display_name(path)))
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    iter.next().ok_or_else(|| boxed_err(format!("missing value for {flag}")))
}

pub fn default_sign_key(config: &Config) -> Option<String> {
    env::var(&config.signing.key_env)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn find_gnu_tar() -> Result<Option<OsString>> {
    for candidate in ["tar", "gtar"] {
        let output = Command::new(candidate).arg("--version").output();
        match output {
            Ok(output) if output.status.success() => {
                if is_gnu_tar_version_output(&output.stdout) || is_gnu_tar_version_output(&output.stderr) {
                    return Ok(Some(OsString::from(candidate)));
                }
            }
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Ok(None)
}

fn find_gpg() -> Result<Option<OsString>> {
    for candidate in ["gpg", "gpg2"] {
        let output = Command::new(candidate).arg("--version").output();
        match output {
            Ok(output) if output.status.success() => return Ok(Some(OsString::from(candidate))),
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Ok(None)
}

fn is_gnu_tar_version_output(output: &[u8]) -> bool { String::from_utf8_lossy(output).contains("GNU tar") }

fn deterministic_archive_command(
    tar_program: impl AsRef<OsStr>,
    archive_path: &Path,
    stage_source: &Path,
) -> Command {
    let mut command = Command::new(tar_program);
    command
        .arg("--sort=name")
        .arg("--format=gnu")
        .arg("--mtime=@0")
        .arg("--owner=0")
        .arg("--group=0")
        .arg("--numeric-owner")
        .arg("--use-compress-program=gzip -n")
        .arg("-cf")
        .arg(archive_path)
        .arg("-C")
        .arg(stage_source)
        .arg(".");
    command
}

fn common_archive_name(version: &str) -> String { format!("foundation-sdk-{version}-common.tar.gz") }

fn target_archive_name(version: &str, target: &str) -> String {
    format!("foundation-sdk-{version}-{target}.tar.gz")
}

fn remove_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn detached_signature_command(
    gpg_program: impl AsRef<OsStr>,
    signer: &str,
    archive: &Path,
    signature_path: &Path,
) -> Command {
    let mut command = Command::new(gpg_program);
    command
        .arg("--yes")
        .arg("--local-user")
        .arg(signer)
        .arg("--output")
        .arg(signature_path)
        .arg("--detach-sign")
        .arg(archive);
    command
}

fn gpg_secret_key_exists(gpg_program: impl AsRef<OsStr>, signer: &str) -> Result<bool> {
    let output = Command::new(gpg_program)
        .arg("--batch")
        .arg("--with-colons")
        .arg("--list-secret-keys")
        .arg(signer)
        .output()?;

    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().any(|line| {
        line.starts_with("sec:")
            || line.starts_with("sec#")
            || line.starts_with("ssb:")
            || line.starts_with("ssb#")
    }))
}

fn write_release_metadata(
    output_dir: &Path,
    version: &str,
    targets: &[String],
    archives: &[PathBuf],
    sign_key: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let embedded_public_key = sign_key.map(export_public_key).transpose()?;
    let mut checksums = Vec::new();

    for archive in archives {
        if !archive.exists() {
            return Err(boxed_err(format!("missing packaged archive: {}", archive.display())));
        }

        checksums.push(checksum_line(archive)?);
    }

    let install_script_path = output_dir.join("install.sh");
    fs::write(&install_script_path, render_install_script(version, targets, embedded_public_key.as_ref()))?;
    set_install_script_permissions(&install_script_path)?;
    checksums.push(checksum_line(&install_script_path)?);

    let checksums_path = output_dir.join("checksums.sha256");
    checksums.sort();
    fs::write(&checksums_path, checksums.join("\n") + "\n")?;

    let mut upload_files = archives.iter().map(|path| util::display_name(path)).collect::<Vec<_>>();
    upload_files.push(util::display_name(&install_script_path));
    upload_files.push(util::display_name(&checksums_path));

    if let Some(key) = sign_key {
        for artifact in archives
            .iter()
            .map(PathBuf::as_path)
            .chain([install_script_path.as_path(), checksums_path.as_path()])
        {
            sign_archive(artifact, key, verbose)?;
            upload_files.push(format!("{}.sig", util::display_name(artifact)));
        }
    }

    upload_files.sort();
    let upload_script_path = output_dir.join("upload.sh");
    fs::write(&upload_script_path, render_upload_script(&upload_files))?;
    set_install_script_permissions(&upload_script_path)?;

    Ok(())
}

fn discover_packaged_targets(config: &Config, output_dir: &Path, version: &str) -> Result<Vec<String>> {
    let common_archive = common_archive_name(version);
    let prefix = format!("foundation-sdk-{version}-");
    let mut discovered = BTreeSet::new();

    for entry in fs::read_dir(output_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name == common_archive {
            continue;
        }

        let Some(target) = file_name
            .strip_prefix(&prefix)
            .and_then(|value| value.strip_suffix(".tar.gz"))
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        discovered.insert(target.to_string());
    }

    if discovered.is_empty() {
        return Err(boxed_err(format!(
            "no packaged target archives found for version {version} in {}",
            output_dir.display()
        )));
    }

    let mut ordered = Vec::new();
    for target in &config.targets.triples {
        if discovered.remove(target) {
            ordered.push(target.clone());
        }
    }

    if !discovered.is_empty() {
        let unexpected = discovered.into_iter().collect::<Vec<_>>().join(", ");
        return Err(boxed_err(format!(
            "found packaged archives for unsupported target triples: {unexpected}"
        )));
    }

    Ok(ordered)
}

fn require_all_configured_targets(config: &Config, version: &str, targets: &[String]) -> Result<()> {
    let found = targets.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let missing = config
        .targets
        .triples
        .iter()
        .filter(|target| !found.contains(target.as_str()))
        .cloned()
        .collect::<Vec<_>>();

    if missing.is_empty() {
        return Ok(());
    }

    Err(boxed_err(format!(
        "missing packaged target archives for version {version}: {}. Build every configured SDK target before finalizing the release.",
        missing.join(", ")
    )))
}

fn set_install_script_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)?;
    }

    Ok(())
}

/// Minimal RFC 4648 base64 encoder. We only use it for embedding the armored
/// GPG release key in the install script (small, infrequent), so it isn't
/// worth pulling in the `base64` crate. The install script decodes this with
/// `base64 -d`, which is available on every POSIX platform we ship for.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let triple = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 { ALPHABET[((triple >> 6) & 0x3F) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[(triple & 0x3F) as usize] as char } else { '=' });
    }
    out
}

fn render_install_script(
    version: &str,
    targets: &[String],
    embedded_public_key: Option<&EmbeddedPublicKey>,
) -> String {
    let supported_targets = targets.join(" ");
    // Base64-encode the armored key so we can embed it as a single-line shell
    // string without the heredoc dance — the heredoc tripped on keys whose
    // armored representation happened to contain the heredoc terminator.
    let embedded_public_key_assignment = match embedded_public_key {
        Some(public_key) => {
            let base64_key = base64_encode(public_key.armored.as_bytes());
            format!(
                r#"EMBEDDED_GPG_PUBLIC_KEY_FINGERPRINT="{fingerprint}"
EMBEDDED_GPG_PUBLIC_KEY_B64="{base64_key}"
"#,
                fingerprint = public_key.fingerprint,
            )
        }
        None => "EMBEDDED_GPG_PUBLIC_KEY_FINGERPRINT=\"\"\nEMBEDDED_GPG_PUBLIC_KEY_B64=\"\"\n".to_string(),
    };

    format!(
        r##"#!/usr/bin/env sh
set -eu

VERSION="{version}"
DEFAULT_BASE_URL="https://sdk.foundation.xyz"
BASE_URL="${{FOUNDATION_SDK_BASE_URL:-$DEFAULT_BASE_URL}}"
INSTALL_ROOT="${{FOUNDATION_SDK_INSTALL_DIR:-$HOME/.foundation/sdk}}"
UPDATE_RC="${{FOUNDATION_SDK_UPDATE_RC:-}}"
if [ -z "$UPDATE_RC" ]; then
  if [ -n "${{FOUNDATION_SDK_INSTALL_DIR:-}}" ]; then
    UPDATE_RC=0
  else
    UPDATE_RC=1
  fi
fi
SUPPORTED_TARGETS="{supported_targets}"
{embedded_public_key_assignment}

# Opt-out of GPG signature verification for signed releases. UNSAFE: see
# setup_gpg_verifier. Settable via FOUNDATION_SDK_NO_VERIFY=1 or --no-verify.
NO_VERIFY="${{FOUNDATION_SDK_NO_VERIFY:-0}}"
for arg in "$@"; do
  case "$arg" in
    --no-verify) NO_VERIFY=1 ;;
    *)
      echo "Unknown argument: $arg" >&2
      echo "Usage: install.sh [--no-verify]" >&2
      exit 1
      ;;
  esac
done

detect_rc_file() {{
  if [ -n "${{FOUNDATION_SDK_RC_FILE:-}}" ]; then
    echo "$FOUNDATION_SDK_RC_FILE"
    return
  fi

  case "${{SHELL:-}}" in
    */zsh|zsh) echo "$HOME/.zshrc" ;;
    */bash|bash) echo "$HOME/.bashrc" ;;
    *) echo "$HOME/.profile" ;;
  esac
}}

detect_os() {{
  case "$(uname -s)" in
    Darwin) echo "apple-darwin" ;;
    Linux) echo "unknown-linux-gnu" ;;
    *) echo "unsupported" ;;
  esac
}}

detect_arch() {{
  case "$(uname -m)" in
    arm64|aarch64) echo "aarch64" ;;
    x86_64|amd64) echo "x86_64" ;;
    *) echo "unsupported" ;;
  esac
}}

make_absolute_path() {{
  path="$1"
  case "$path" in
    /*) printf '%s\n' "$path" ;;
    *) printf '%s/%s\n' "$(pwd -P)" "$path" ;;
  esac
}}

download_to() {{
  url="$1"
  destination="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fL --progress-bar --show-error "$url" -o "$destination"
    return
  fi
  if command -v wget >/dev/null 2>&1; then
    wget -O "$destination" "$url"
    return
  fi
  echo "Either curl or wget is required to install the Foundation SDK." >&2
  exit 1
}}

sha256_file() {{
  file="$1"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{{print $1}}'
    return
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{{print $1}}'
    return
  fi
  echo "Either shasum or sha256sum is required to verify the Foundation SDK archive." >&2
  exit 1
}}

detect_gpg() {{
  if command -v gpg >/dev/null 2>&1; then
    echo "gpg"
    return
  fi
  if command -v gpg2 >/dev/null 2>&1; then
    echo "gpg2"
    return
  fi
  echo ""
}}

detect_gpgv() {{
  if command -v gpgv >/dev/null 2>&1; then
    echo "gpgv"
    return
  fi
  echo ""
}}

if [ -t 1 ]; then
  COLOR_GREEN="$(printf '\033[32m')"
  COLOR_RED="$(printf '\033[31m')"
  COLOR_YELLOW="$(printf '\033[33m')"
  COLOR_RESET="$(printf '\033[0m')"
else
  COLOR_GREEN=""
  COLOR_RED=""
  COLOR_YELLOW=""
  COLOR_RESET=""
fi

print_ok() {{
  message="$1"
  printf '%s✓%s %s\n' "$COLOR_GREEN" "$COLOR_RESET" "$message"
}}

print_warn() {{
  message="$1"
  printf '%s!%s %s\n' "$COLOR_YELLOW" "$COLOR_RESET" "$message" >&2
}}

print_fail() {{
  message="$1"
  printf '%s✗%s %s\n' "$COLOR_RED" "$COLOR_RESET" "$message" >&2
  exit 1
}}

directory_owner_id() {{
  ls -ldn "$1" | awk '{{ print $3 }}'
}}

directory_mode() {{
  ls -ld "$1" | awk '{{ print $1 }}'
}}

is_group_or_world_writable() {{
  mode="$(directory_mode "$1")"
  group_write="$(printf '%s' "$mode" | cut -c6)"
  world_write="$(printf '%s' "$mode" | cut -c9)"
  [ "$group_write" = "w" ] || [ "$world_write" = "w" ]
}}

ensure_secure_directory() {{
  path="$1"
  label="$2"
  if [ ! -d "$path" ]; then
    print_fail "$label does not exist: $path"
  fi
  if [ -L "$path" ]; then
    print_fail "$label must be a real directory, not a symlink: $path"
  fi
  owner_id="$(directory_owner_id "$path")"
  if [ "$owner_id" != "$(id -u)" ]; then
    print_fail "$label is not owned by the current user: $path"
  fi
  chmod go-w "$path" 2>/dev/null || true
  if is_group_or_world_writable "$path"; then
    print_fail "$label is writable by group or others: $path"
  fi
}}

ensure_secure_launcher_dir() {{
  install_root="$1"
  bin_dir="$2"
  ensure_secure_directory "$install_root" "Foundation SDK install root"
  ensure_secure_directory "$bin_dir" "Foundation SDK launcher directory"
  launcher="$bin_dir/foundation"
  if [ ! -f "$launcher" ] || [ ! -x "$launcher" ]; then
    print_fail "Foundation SDK launcher was not installed correctly: $launcher"
  fi
}}

update_rc_enabled() {{
  case "$UPDATE_RC" in
    1|true|TRUE|yes|YES) return 0 ;;
    0|false|FALSE|no|NO) return 1 ;;
    *) print_fail "FOUNDATION_SDK_UPDATE_RC must be 1/true/yes or 0/false/no" ;;
  esac
}}

no_verify_enabled() {{
  case "$NO_VERIFY" in
    1|true|TRUE|yes|YES) return 0 ;;
    0|false|FALSE|no|NO) return 1 ;;
    *) print_fail "FOUNDATION_SDK_NO_VERIFY must be 1/true/yes or 0/false/no" ;;
  esac
}}

refresh_installed_mtimes() {{
  root="$1"
  # Release archives use normalized mtimes for reproducibility. Refresh the
  # installed source tree so Cargo invalidates app-side path dependency caches
  # when a same-version SDK is reinstalled locally.
  find "$root" -type f -exec touch {{}} +
  find "$root" -type d -exec touch {{}} +
}}

setup_gpg_verifier() {{
  if [ -z "$EMBEDDED_GPG_PUBLIC_KEY_B64" ]; then
    return
  fi
  if [ -z "$EMBEDDED_GPG_PUBLIC_KEY_FINGERPRINT" ]; then
    print_fail "Embedded Foundation release key is missing a pinned fingerprint"
  fi

  # This is a signed release. Require a working gpg by default so the signature
  # is actually checked: checksums.sha256 is downloaded from the same BASE_URL
  # as the archives, so checksum-only verification gives NO protection against a
  # compromised download source (an attacker who can replace the bucket can ship
  # a malicious archive plus a matching checksums file). --no-verify explicitly
  # opts out of signature verification (unsafe).
  if no_verify_enabled; then
    print_warn "--no-verify: skipping Foundation SDK signature verification (UNSAFE). Continuing with checksum-only verification, which does NOT protect against a compromised download source."
    return
  fi

  GPG="$(detect_gpg)"
  if [ -z "$GPG" ]; then
    print_fail "This Foundation SDK release is signed, but gpg is not installed so the signature cannot be verified. Install gpg and re-run, or pass --no-verify to skip signature verification (UNSAFE: continues with checksum-only verification, which does NOT protect against a compromised download source)."
  fi

  GNUPGHOME="$TMPDIR/gnupg"
  mkdir -p "$GNUPGHOME"
  chmod 700 "$GNUPGHOME"
  key_file="$GNUPGHOME/foundation-sdk-release.asc"
  # Base64 was used to embed the armored key safely; decode on disk before import.
  if ! printf '%s' "$EMBEDDED_GPG_PUBLIC_KEY_B64" | base64 -d > "$key_file" 2>/dev/null; then
    print_fail "Could not decode embedded Foundation release key (base64)"
  fi

  if "$GPG" --batch --homedir "$GNUPGHOME" --import "$key_file" >/dev/null 2>&1; then
    actual_fingerprint="$("$GPG" --batch --homedir "$GNUPGHOME" --with-colons --fingerprint | awk -F: '$1 == "fpr" {{ print $10; exit }}')"
    if [ "$actual_fingerprint" != "$EMBEDDED_GPG_PUBLIC_KEY_FINGERPRINT" ]; then
      print_fail "Embedded Foundation release key fingerprint mismatch"
    fi
    GPG_KEYRING="$GNUPGHOME/foundation-sdk-release.gpg"
    "$GPG" --batch --homedir "$GNUPGHOME" --export "$EMBEDDED_GPG_PUBLIC_KEY_FINGERPRINT" > "$GPG_KEYRING"
    GPGV="$(detect_gpgv)"
    SIGNATURE_VERIFICATION_ENABLED=1
    print_ok "Foundation release key imported ($actual_fingerprint)"
  else
    print_fail "Could not import embedded Foundation release key"
  fi
}}

verify_signature() {{
  file="$1"
  signature="$2"
  label="$3"

  if [ -n "$GPGV" ] && "$GPGV" --keyring "$GPG_KEYRING" "$signature" "$file" >/dev/null 2>&1; then
    print_ok "$label signature verified"
  elif "$GPG" --batch --homedir "$GNUPGHOME" --trust-model always --verify "$signature" "$file" >/dev/null 2>&1; then
    print_ok "$label signature verified"
  else
    print_fail "$label signature verification failed"
  fi
}}

verify_checksum() {{
  file="$1"
  checksums_file="$2"
  label="$3"
  expected="$(awk -v name="$label" '$2 == name {{ print $1 }}' "$checksums_file")"
  if [ -z "$expected" ]; then
    print_fail "Could not find a checksum for $label in $(basename "$checksums_file")"
  fi

  actual="$(sha256_file "$file")"
  if [ "$expected" = "$actual" ]; then
    print_ok "$label checksum verified"
  else
    print_fail "$label checksum mismatch"
  fi
}}

rewrite_rc_path_block() {{
  rc_file="$1"
  bin_dir="$2"
  marker_begin="# >>> foundation-sdk >>>"
  marker_end="# <<< foundation-sdk <<<"
  tmp_rc="$TMPDIR/rc.$$.tmp"

  mkdir -p "$(dirname "$rc_file")" || return 1

  if [ -f "$rc_file" ]; then
    awk -v begin="$marker_begin" -v end="$marker_end" '
      $0 == begin {{ skip = 1; next }}
      $0 == end {{ skip = 0; next }}
      skip != 1 {{ print }}
    ' "$rc_file" > "$tmp_rc" || return 1
  else
    : > "$tmp_rc" || return 1
  fi

  {{
    cat "$tmp_rc"
    if [ -s "$tmp_rc" ]; then
      printf '\n'
    fi
    printf '%s\n' "$marker_begin"
    printf '%s\n' "export PATH=\"$bin_dir:\$PATH\""
    printf '%s\n' "$marker_end"
  }} > "$rc_file" || return 1
}}

install_launchers() {{
  install_root="$1"
  sdk_root="$2"
  current_root="$3"
  bin_dir="$install_root/bin"

  rm -rf "$bin_dir"
  mkdir -p "$bin_dir"

  cat > "$bin_dir/foundation" <<EOF
#!/usr/bin/env sh
set -eu
export FOUNDATION_SDK_ROOT="$current_root"
exec "$current_root/bin/foundation" "\$@"
EOF
  chmod 755 "$bin_dir/foundation"
  chmod 755 "$bin_dir"

  if [ -d "$sdk_root/bin" ]; then
    for tool_path in "$sdk_root"/bin/*; do
      if [ ! -f "$tool_path" ]; then
        continue
      fi
      tool_name="$(basename "$tool_path")"
      if [ "$tool_name" = "foundation" ]; then
        continue
      fi
      ln -sfn "$tool_path" "$bin_dir/$tool_name"
    done
  fi
}}

ARCH="$(detect_arch)"
OS="$(detect_os)"
if [ "$ARCH" = "unsupported" ] || [ "$OS" = "unsupported" ]; then
  echo "Unsupported host platform: $(uname -m)-$(uname -s)" >&2
  exit 1
fi

TARGET="$ARCH-$OS"
case " $SUPPORTED_TARGETS " in
  *" $TARGET "*) ;;
  *)
    echo "No packaged Foundation SDK archive is available for $TARGET." >&2
    echo "Supported targets: $SUPPORTED_TARGETS" >&2
    exit 1
    ;;
esac

COMMON_ARCHIVE="foundation-sdk-$VERSION-common.tar.gz"
TARGET_ARCHIVE="foundation-sdk-$VERSION-$TARGET.tar.gz"
CHECKSUMS="checksums.sha256"
COMMON_ARCHIVE_SIG="$COMMON_ARCHIVE.sig"
TARGET_ARCHIVE_SIG="$TARGET_ARCHIVE.sig"
CHECKSUMS_SIG="$CHECKSUMS.sig"
TMPDIR="$(mktemp -d "${{TMPDIR:-/tmp}}/foundation-sdk.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT INT TERM
SIGNATURE_VERIFICATION_ENABLED=0
GPG=""
GPGV=""
GNUPGHOME=""
GPG_KEYRING=""

setup_gpg_verifier

download_to "$BASE_URL/$COMMON_ARCHIVE" "$TMPDIR/$COMMON_ARCHIVE"
download_to "$BASE_URL/$TARGET_ARCHIVE" "$TMPDIR/$TARGET_ARCHIVE"
download_to "$BASE_URL/$CHECKSUMS" "$TMPDIR/$CHECKSUMS"

if [ "$SIGNATURE_VERIFICATION_ENABLED" -eq 1 ]; then
  download_to "$BASE_URL/$CHECKSUMS_SIG" "$TMPDIR/$CHECKSUMS_SIG"
  download_to "$BASE_URL/$COMMON_ARCHIVE_SIG" "$TMPDIR/$COMMON_ARCHIVE_SIG"
  download_to "$BASE_URL/$TARGET_ARCHIVE_SIG" "$TMPDIR/$TARGET_ARCHIVE_SIG"

  verify_signature "$TMPDIR/$CHECKSUMS" "$TMPDIR/$CHECKSUMS_SIG" "$CHECKSUMS"
  verify_signature "$TMPDIR/$COMMON_ARCHIVE" "$TMPDIR/$COMMON_ARCHIVE_SIG" "$COMMON_ARCHIVE"
  verify_signature "$TMPDIR/$TARGET_ARCHIVE" "$TMPDIR/$TARGET_ARCHIVE_SIG" "$TARGET_ARCHIVE"
fi

verify_checksum "$TMPDIR/$COMMON_ARCHIVE" "$TMPDIR/$CHECKSUMS" "$COMMON_ARCHIVE"
verify_checksum "$TMPDIR/$TARGET_ARCHIVE" "$TMPDIR/$CHECKSUMS" "$TARGET_ARCHIVE"

INSTALL_ROOT="$(make_absolute_path "$INSTALL_ROOT")"
mkdir -p "$INSTALL_ROOT"
INSTALL_ROOT="$(CDPATH= cd -- "$INSTALL_ROOT" && pwd -P)"
DESTINATION="$INSTALL_ROOT/foundation-sdk-$VERSION-$TARGET"
CURRENT_LINK="$INSTALL_ROOT/current"
rm -rf "$DESTINATION"
mkdir -p "$DESTINATION"
tar -xzf "$TMPDIR/$COMMON_ARCHIVE" -C "$DESTINATION"
tar -xzf "$TMPDIR/$TARGET_ARCHIVE" -C "$DESTINATION"
refresh_installed_mtimes "$DESTINATION"
rm -f "$CURRENT_LINK"
ln -s "$DESTINATION" "$CURRENT_LINK"

install_launchers "$INSTALL_ROOT" "$DESTINATION" "$CURRENT_LINK"
ensure_secure_launcher_dir "$INSTALL_ROOT" "$INSTALL_ROOT/bin"

echo "Foundation SDK installed to: $DESTINATION"
echo
echo "Next steps:"
if update_rc_enabled; then
  RC_FILE="$(detect_rc_file)"
  if rewrite_rc_path_block "$RC_FILE" "$INSTALL_ROOT/bin"; then
    echo "  source \"$RC_FILE\""
  else
    print_warn "Could not update shell rc file: $RC_FILE"
    echo "  export PATH=\"$INSTALL_ROOT/bin:\$PATH\""
  fi
else
  echo "  export PATH=\"$INSTALL_ROOT/bin:\$PATH\""
fi
echo "  foundation develop"
"##
    )
}

fn render_upload_script(files: &[String]) -> String {
    let commands = files
        .iter()
        .map(|file| format!("gcloud storage cp \"$SCRIPT_DIR/{file}\" \"$BUCKET\""))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r##"#!/usr/bin/env sh
set -eu

BUCKET="${{FOUNDATION_SDK_GCS_BUCKET:-gs://foundation-sdk/}}"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

if ! command -v gcloud >/dev/null 2>&1; then
  echo "gcloud is required to upload Foundation SDK release artifacts." >&2
  exit 1
fi

{commands}
"##
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        common_archive_name, detached_signature_command, deterministic_archive_command,
        discover_packaged_targets, finalize, finalize_release, is_gnu_tar_version_output, next_value,
        render_install_script, render_upload_script, target_archive_name, EmbeddedPublicKey, FinalizeArgs,
        PackageArgs,
    };
    use crate::config::{Config, TargetsConfig};

    const TEST_PUBLIC_KEY: &str =
        "-----BEGIN PGP PUBLIC KEY BLOCK-----\nTESTKEY\n-----END PGP PUBLIC KEY BLOCK-----";
    const TEST_PUBLIC_KEY_FINGERPRINT: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

    #[test]
    fn package_args_parse_flags_and_targets() {
        let parsed = PackageArgs::parse(vec![
            "--target".into(),
            "x86_64-unknown-linux-gnu".into(),
            "--target".into(),
            "aarch64-apple-darwin".into(),
            "--version".into(),
            "1.2.3".into(),
            "--output-dir".into(),
            "out".into(),
            "--verbose".into(),
        ])
        .unwrap();

        assert_eq!(
            parsed.targets,
            vec!["x86_64-unknown-linux-gnu".to_string(), "aarch64-apple-darwin".to_string()]
        );
        assert_eq!(parsed.version.as_deref(), Some("1.2.3"));
        assert_eq!(parsed.output_dir, PathBuf::from("out"));
        assert!(parsed.verbose);
    }

    #[test]
    fn finalize_args_parse_sign_key_and_output_dir() {
        let parsed = FinalizeArgs::parse(vec![
            "--version".into(),
            "1.2.3".into(),
            "--output-dir".into(),
            "release".into(),
            "--sign-key".into(),
            "release@example.com".into(),
            "--verbose".into(),
        ])
        .unwrap();

        assert_eq!(parsed.version.as_deref(), Some("1.2.3"));
        assert_eq!(parsed.output_dir, PathBuf::from("release"));
        assert_eq!(parsed.sign_key.as_deref(), Some("release@example.com"));
        assert!(parsed.verbose);
    }

    #[test]
    fn next_value_requires_following_argument() {
        let mut iter = std::iter::empty::<String>();
        let error = next_value(&mut iter, "--target").unwrap_err();
        assert!(error.to_string().contains("missing value for --target"));
    }

    #[test]
    fn gnu_tar_output_detector_matches_expected_banner() {
        assert!(is_gnu_tar_version_output(b"tar (GNU tar) 1.35\n"));
        assert!(!is_gnu_tar_version_output(b"bsdtar 3.7.0 - libarchive 3.7.0\n"));
    }

    #[test]
    fn deterministic_archive_command_uses_normalized_flags() {
        let command = deterministic_archive_command(
            OsString::from("tar"),
            PathBuf::from("bundle.tar.gz").as_path(),
            PathBuf::from("stage").as_path(),
        );

        assert_eq!(command.get_program(), OsStr::new("tar"));
        let args = command.get_args().map(|arg| arg.to_string_lossy().to_string()).collect::<Vec<_>>();

        assert_eq!(
            args,
            vec![
                "--sort=name",
                "--format=gnu",
                "--mtime=@0",
                "--owner=0",
                "--group=0",
                "--numeric-owner",
                "--use-compress-program=gzip -n",
                "-cf",
                "bundle.tar.gz",
                "-C",
                "stage",
                ".",
            ]
        );
    }

    #[test]
    fn detached_signature_command_uses_gpg_local_user() {
        let command = detached_signature_command(
            OsString::from("gpg"),
            "release@example.com",
            PathBuf::from("bundle.tar.gz").as_path(),
            PathBuf::from("bundle.tar.gz.sig").as_path(),
        );

        assert_eq!(command.get_program(), OsStr::new("gpg"));
        let args = command.get_args().map(|arg| arg.to_string_lossy().to_string()).collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "--yes",
                "--local-user",
                "release@example.com",
                "--output",
                "bundle.tar.gz.sig",
                "--detach-sign",
                "bundle.tar.gz",
            ]
        );
    }

    #[test]
    fn install_script_mentions_version_and_supported_targets() {
        let public_key = EmbeddedPublicKey {
            armored: TEST_PUBLIC_KEY.to_string(),
            fingerprint: TEST_PUBLIC_KEY_FINGERPRINT.to_string(),
        };
        let script = render_install_script(
            "1.2.3",
            &[
                "aarch64-apple-darwin".to_string(),
                "x86_64-apple-darwin".to_string(),
                "x86_64-unknown-linux-gnu".to_string(),
            ],
            Some(&public_key),
        );

        assert!(script.contains("VERSION=\"1.2.3\""));
        assert!(script.contains("DEFAULT_BASE_URL=\"https://sdk.foundation.xyz\""));
        assert!(script.contains(
            "SUPPORTED_TARGETS=\"aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu\""
        ));
        assert!(script.contains("FOUNDATION_SDK_UPDATE_RC"));
        assert!(script.contains("curl -fL --progress-bar --show-error"));
        assert!(script.contains("wget -O \"$destination\" \"$url\""));
        assert!(script
            .contains(&format!("EMBEDDED_GPG_PUBLIC_KEY_FINGERPRINT=\"{TEST_PUBLIC_KEY_FINGERPRINT}\"")));
        assert!(script.contains("EMBEDDED_GPG_PUBLIC_KEY_B64=\""));
        assert!(script.contains("base64 -d > \"$key_file\""));
        assert!(script.contains("SIGNATURE_VERIFICATION_ENABLED=0"));
        assert!(script.contains("setup_gpg_verifier"));
        // Signed release + missing gpg must hard-fail (not fall back to
        // checksum-only) unless the user explicitly opts out with --no-verify.
        assert!(script.contains("--no-verify) NO_VERIFY=1"));
        assert!(script.contains("no_verify_enabled"));
        assert!(script.contains("FOUNDATION_SDK_NO_VERIFY"));
        assert!(script
            .contains("This Foundation SDK release is signed, but gpg is not installed so the signature cannot be verified."));
        assert!(script.contains("if [ \"$SIGNATURE_VERIFICATION_ENABLED\" -eq 1 ]; then"));
        assert!(script
            .contains("verify_signature \"$TMPDIR/$CHECKSUMS\" \"$TMPDIR/$CHECKSUMS_SIG\" \"$CHECKSUMS\""));
        assert!(script.contains("print_ok \"$label signature verified\""));
        assert!(script.contains("print_ok \"$label checksum verified\""));
        assert!(script.contains("checksums.sha256"));
        assert!(script.contains("CHECKSUMS_SIG=\"$CHECKSUMS.sig\""));
        assert!(script.contains("foundation-sdk-$VERSION-common.tar.gz"));
        assert!(script.contains("foundation-sdk-$VERSION-$TARGET.tar.gz"));
        assert!(script.contains("COMMON_ARCHIVE_SIG=\"$COMMON_ARCHIVE.sig\""));
        assert!(script.contains("TARGET_ARCHIVE_SIG=\"$TARGET_ARCHIVE.sig\""));
        assert!(script.contains("export FOUNDATION_SDK_ROOT=\"$current_root\""));
        assert!(script.contains("exec \"$current_root/bin/foundation\""));
        assert!(script.contains("ensure_secure_launcher_dir \"$INSTALL_ROOT\" \"$INSTALL_ROOT/bin\""));
        assert!(script.contains("refresh_installed_mtimes \"$DESTINATION\""));
        assert!(script.contains("Cargo invalidates app-side path dependency caches"));
        assert!(script.contains("if update_rc_enabled; then"));
        assert!(script.contains("print_warn \"Could not update shell rc file: $RC_FILE\""));
        assert!(script.contains("export PATH="));
        assert!(script.contains("foundation develop"));
        assert!(script.contains("source"));
    }

    #[test]
    fn upload_script_mentions_each_release_file() {
        let script = render_upload_script(&[
            "checksums.sha256".to_string(),
            "checksums.sha256.sig".to_string(),
            "foundation-sdk-1.2.3-common.tar.gz".to_string(),
            "foundation-sdk-1.2.3-common.tar.gz.sig".to_string(),
            "install.sh".to_string(),
            "install.sh.sig".to_string(),
        ]);

        assert!(script.contains("BUCKET=\"${FOUNDATION_SDK_GCS_BUCKET:-gs://foundation-sdk/}\""));
        assert!(script.contains("gcloud storage cp \"$SCRIPT_DIR/checksums.sha256\" \"$BUCKET\""));
        assert!(script.contains("gcloud storage cp \"$SCRIPT_DIR/checksums.sha256.sig\" \"$BUCKET\""));
        assert!(script
            .contains("gcloud storage cp \"$SCRIPT_DIR/foundation-sdk-1.2.3-common.tar.gz\" \"$BUCKET\""));
        assert!(script.contains(
            "gcloud storage cp \"$SCRIPT_DIR/foundation-sdk-1.2.3-common.tar.gz.sig\" \"$BUCKET\""
        ));
        assert!(script.contains("gcloud storage cp \"$SCRIPT_DIR/install.sh\" \"$BUCKET\""));
        assert!(script.contains("gcloud storage cp \"$SCRIPT_DIR/install.sh.sig\" \"$BUCKET\""));
    }

    #[test]
    fn archive_names_match_expected_layout() {
        assert_eq!(common_archive_name("1.2.3"), "foundation-sdk-1.2.3-common.tar.gz");
        assert_eq!(
            target_archive_name("1.2.3", "x86_64-unknown-linux-gnu"),
            "foundation-sdk-1.2.3-x86_64-unknown-linux-gnu.tar.gz"
        );
    }

    #[test]
    fn discover_packaged_targets_uses_config_order() {
        let temp_dir = temp_dir("discover-packaged-targets");
        fs::write(temp_dir.join(common_archive_name("1.2.3")), "common").unwrap();
        fs::write(temp_dir.join(target_archive_name("1.2.3", "x86_64-unknown-linux-gnu")), "linux").unwrap();
        fs::write(temp_dir.join(target_archive_name("1.2.3", "aarch64-apple-darwin")), "mac").unwrap();
        fs::write(temp_dir.join("ignore-me.txt"), "ignore").unwrap();

        let mut config = Config::default();
        config.targets = TargetsConfig {
            triples: vec![
                "aarch64-apple-darwin".to_string(),
                "x86_64-apple-darwin".to_string(),
                "x86_64-unknown-linux-gnu".to_string(),
            ],
            overrides: Default::default(),
        };

        let discovered = discover_packaged_targets(&config, &temp_dir, "1.2.3").unwrap();
        assert_eq!(
            discovered,
            vec!["aarch64-apple-darwin".to_string(), "x86_64-unknown-linux-gnu".to_string()]
        );

        cleanup(&temp_dir);
    }

    #[test]
    fn finalize_requires_signing_key() {
        let root = temp_dir("finalize-release-requires-signing-key");
        let output_dir = root.join("dist");
        fs::create_dir_all(&output_dir).unwrap();
        fs::write(output_dir.join(common_archive_name("1.2.3")), "common").unwrap();
        fs::write(output_dir.join(target_archive_name("1.2.3", "x86_64-unknown-linux-gnu")), "linux")
            .unwrap();

        let mut config = Config::default();
        config.sdk.version = "1.2.3".to_string();
        config.targets = TargetsConfig {
            triples: vec!["x86_64-unknown-linux-gnu".to_string()],
            overrides: Default::default(),
        };
        config.signing.key_env = "FOUNDATION_SIGN_KEY".to_string();

        let error = finalize(
            &root,
            &config,
            &FinalizeArgs { output_dir: PathBuf::from("dist"), ..FinalizeArgs::default() },
        )
        .unwrap_err();
        assert!(error.to_string().contains("finalize requires --sign-key or FOUNDATION_SIGN_KEY"));

        cleanup(&root);
    }

    #[test]
    fn finalize_release_regenerates_install_script_and_checksums_from_existing_archives() {
        let root = temp_dir("finalize-release");
        let output_dir = root.join("dist");
        fs::create_dir_all(&output_dir).unwrap();
        fs::write(output_dir.join(common_archive_name("1.2.3")), "common").unwrap();
        fs::write(output_dir.join(target_archive_name("1.2.3", "aarch64-apple-darwin")), "mac-arm").unwrap();
        fs::write(output_dir.join(target_archive_name("1.2.3", "x86_64-apple-darwin")), "mac-x86").unwrap();
        fs::write(output_dir.join(target_archive_name("1.2.3", "x86_64-unknown-linux-gnu")), "linux")
            .unwrap();

        let mut config = Config::default();
        config.sdk.version = "1.2.3".to_string();
        config.targets = TargetsConfig {
            triples: vec![
                "aarch64-apple-darwin".to_string(),
                "x86_64-apple-darwin".to_string(),
                "x86_64-unknown-linux-gnu".to_string(),
            ],
            overrides: Default::default(),
        };
        config.signing.key_env = "FOUNDATION_SIGN_KEY".to_string();

        finalize_release(&output_dir, &config, "1.2.3", None, false).unwrap();

        let install_script = fs::read_to_string(output_dir.join("install.sh")).unwrap();
        assert!(install_script.contains(
            "SUPPORTED_TARGETS=\"aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu\""
        ));

        let checksums = fs::read_to_string(output_dir.join("checksums.sha256")).unwrap();
        assert!(checksums.contains("foundation-sdk-1.2.3-common.tar.gz"));
        assert!(checksums.contains("foundation-sdk-1.2.3-aarch64-apple-darwin.tar.gz"));
        assert!(checksums.contains("foundation-sdk-1.2.3-x86_64-apple-darwin.tar.gz"));
        assert!(checksums.contains("foundation-sdk-1.2.3-x86_64-unknown-linux-gnu.tar.gz"));
        assert!(checksums.contains("install.sh"));
        assert!(!checksums.contains(".sig"));

        let upload_script = fs::read_to_string(output_dir.join("upload.sh")).unwrap();
        assert!(upload_script.contains("gcloud storage cp \"$SCRIPT_DIR/checksums.sha256\" \"$BUCKET\""));
        assert!(upload_script
            .contains("gcloud storage cp \"$SCRIPT_DIR/foundation-sdk-1.2.3-common.tar.gz\" \"$BUCKET\""));
        assert!(upload_script.contains(
            "gcloud storage cp \"$SCRIPT_DIR/foundation-sdk-1.2.3-aarch64-apple-darwin.tar.gz\" \"$BUCKET\""
        ));
        assert!(upload_script.contains(
            "gcloud storage cp \"$SCRIPT_DIR/foundation-sdk-1.2.3-x86_64-apple-darwin.tar.gz\" \"$BUCKET\""
        ));
        assert!(
            upload_script.contains(
                "gcloud storage cp \"$SCRIPT_DIR/foundation-sdk-1.2.3-x86_64-unknown-linux-gnu.tar.gz\" \"$BUCKET\""
            )
        );
        assert!(upload_script.contains("gcloud storage cp \"$SCRIPT_DIR/install.sh\" \"$BUCKET\""));

        cleanup(&root);
    }

    #[test]
    fn finalize_release_requires_all_configured_target_archives() {
        let root = temp_dir("finalize-release-missing-target");
        let output_dir = root.join("dist");
        fs::create_dir_all(&output_dir).unwrap();
        fs::write(output_dir.join(common_archive_name("1.2.3")), "common").unwrap();
        fs::write(output_dir.join(target_archive_name("1.2.3", "aarch64-apple-darwin")), "mac-arm").unwrap();
        fs::write(output_dir.join(target_archive_name("1.2.3", "x86_64-unknown-linux-gnu")), "linux")
            .unwrap();

        let mut config = Config::default();
        config.sdk.version = "1.2.3".to_string();
        config.targets = TargetsConfig {
            triples: vec![
                "aarch64-apple-darwin".to_string(),
                "x86_64-apple-darwin".to_string(),
                "x86_64-unknown-linux-gnu".to_string(),
            ],
            overrides: Default::default(),
        };

        let error = finalize_release(&output_dir, &config, "1.2.3", None, false).unwrap_err();
        assert!(error.to_string().contains("missing packaged target archives for version 1.2.3"));
        assert!(error.to_string().contains("x86_64-apple-darwin"));

        cleanup(&root);
    }

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("foundation-package-{label}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn cleanup(path: &Path) { let _ = fs::remove_dir_all(path); }
}
