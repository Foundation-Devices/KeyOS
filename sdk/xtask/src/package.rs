// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

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

impl Default for PackageArgs {
    fn default() -> Self {
        Self { targets: Vec::new(), version: None, output_dir: PathBuf::from("dist"), verbose: false }
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

pub fn run(
    root: &Path,
    config: &Config,
    args: &PackageArgs,
    sign_key: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let output_dir = util::absolute_path(root, &args.output_dir);
    let targets = selected_targets(config, &args.targets)?;
    let version = crate::release::validated_sdk_version(root, config, args.version.as_deref())?;

    check_package_prerequisites(sign_key)?;

    let tar_program = find_gnu_tar()?.ok_or_else(|| {
        boxed_err("deterministic packaging requires GNU tar (expected 'tar' or 'gtar' with GNU tar)")
    })?;
    let mut archive_paths = vec![write_common_archive(&tar_program, &output_dir, &version, verbose)?];

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

    write_release_metadata(
        &output_dir,
        &version,
        &targets,
        &archive_paths,
        sign_key,
        crate::release::PUBLIC_DOWNLOAD_ROOT,
        verbose,
    )?;
    Ok(())
}

pub fn package_common(
    root: &Path,
    config: &Config,
    requested_output_dir: &Path,
    verbose: bool,
) -> Result<PathBuf> {
    let output_dir = util::absolute_path(root, requested_output_dir);
    let version = crate::release::validated_sdk_version(root, config, None)?;
    check_package_prerequisites(None)?;
    let tar_program = find_gnu_tar()?.ok_or_else(|| {
        boxed_err("deterministic packaging requires GNU tar (expected 'tar' or 'gtar' with GNU tar)")
    })?;
    let archive = write_common_archive(&tar_program, &output_dir, &version, verbose)?;
    println!("packaged common SDK content at {}", archive.display());
    Ok(archive)
}

fn write_common_archive(
    tar_program: &OsString,
    output_dir: &Path,
    version: &str,
    verbose: bool,
) -> Result<PathBuf> {
    let common_stage = common_stage_dir(output_dir);
    if !common_stage.exists() {
        return Err(boxed_err(format!("missing common build staging directory: {}", common_stage.display())));
    }
    let archive = output_dir.join(common_archive_name(version));
    remove_if_exists(&archive)?;
    let mut command = deterministic_archive_command(tar_program, &archive, &common_stage);
    util::run_command(&mut command, verbose)?;
    Ok(archive)
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

pub(crate) fn check_finalize_prerequisites(sign_key: Option<&str>) -> Result<()> {
    if find_gnu_tar()?.is_none() {
        return Err(boxed_err("release validation requires GNU tar (expected 'tar' or 'gtar' with GNU tar)"));
    }
    check_checksum_prerequisites()?;
    check_signing_prerequisites(sign_key)?;
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

pub(crate) fn find_gnu_tar() -> Result<Option<OsString>> {
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

pub(crate) fn find_gpg() -> Result<Option<OsString>> {
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

pub(crate) fn common_archive_name(version: &str) -> String {
    format!("foundation-sdk-{version}-common.tar.gz")
}

pub(crate) fn target_archive_name(version: &str, target: &str) -> String {
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

pub(crate) struct WrittenReleaseMetadata {
    pub files: Vec<PathBuf>,
    pub signing_fingerprint: Option<String>,
}

pub(crate) fn write_release_metadata(
    output_dir: &Path,
    version: &str,
    targets: &[String],
    archives: &[PathBuf],
    sign_key: Option<&str>,
    default_base_url: &str,
    verbose: bool,
) -> Result<WrittenReleaseMetadata> {
    let legacy_upload_script = output_dir.join("upload.sh");
    if legacy_upload_script.exists() {
        fs::remove_file(&legacy_upload_script)?;
    }
    let embedded_public_key = sign_key.map(export_public_key).transpose()?;
    let signing_fingerprint = embedded_public_key.as_ref().map(|key| key.fingerprint.clone());
    let mut checksums = Vec::new();

    for archive in archives {
        if !archive.exists() {
            return Err(boxed_err(format!("missing packaged archive: {}", archive.display())));
        }

        checksums.push(checksum_line(archive)?);
    }

    let install_script_path = output_dir.join("install.sh");
    fs::write(
        &install_script_path,
        render_install_script(version, targets, embedded_public_key.as_ref(), default_base_url),
    )?;
    set_install_script_permissions(&install_script_path)?;
    checksums.push(checksum_line(&install_script_path)?);

    let checksums_path = output_dir.join("checksums.sha256");
    checksums.sort();
    fs::write(&checksums_path, checksums.join("\n") + "\n")?;

    let mut files = archives.to_vec();
    files.push(install_script_path.clone());
    files.push(checksums_path.clone());

    if let Some(key) = sign_key {
        for artifact in archives
            .iter()
            .map(PathBuf::as_path)
            .chain([install_script_path.as_path(), checksums_path.as_path()])
        {
            files.push(sign_archive(artifact, key, verbose)?);
        }
    }

    files.sort();
    Ok(WrittenReleaseMetadata { files, signing_fingerprint })
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
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
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
    default_base_url: &str,
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
DEFAULT_BASE_URL="{default_base_url}"
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
  RELEASE_KEY_FILE="$GNUPGHOME/foundation-sdk-release.asc"
  # Base64 was used to embed the armored key safely; decode on disk before import.
  if ! printf '%s' "$EMBEDDED_GPG_PUBLIC_KEY_B64" | base64 -d > "$RELEASE_KEY_FILE" 2>/dev/null; then
    print_fail "Could not decode embedded Foundation release key (base64)"
  fi

  if "$GPG" --batch --homedir "$GNUPGHOME" --import "$RELEASE_KEY_FILE" >/dev/null 2>&1; then
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
      # Link through `current`, as the foundation launcher does. A link into the
      # versioned directory keeps pointing at the SDK that ran the installer, so
      # switching `current` splits the toolchain and removing an old bundle
      # dangles every tool but `foundation`.
      ln -sfn "$current_root/bin/$tool_name" "$bin_dir/$tool_name"
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
RELEASE_KEY_FILE=""

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
CACHED_BASE_THEME="$HOME/.foundation/themes/json/base_theme.json"
CURRENT_BASE_THEME="$CURRENT_LINK/lib/keyos/sdk/crates/foundation-themes/themes/base_theme.json"
REFRESH_CACHED_BASE_THEME=0
if [ -f "$CACHED_BASE_THEME" ] && [ ! -L "$CACHED_BASE_THEME" ] && \
   [ -f "$CURRENT_BASE_THEME" ] && cmp -s "$CACHED_BASE_THEME" "$CURRENT_BASE_THEME"; then
  # The cache is still the exact Base Theme shipped by the installed SDK, so
  # it is safe to advance it with the SDK. A designer-edited cache is left
  # untouched and remains subject to the compiler's completeness check.
  REFRESH_CACHED_BASE_THEME=1
fi
rm -rf "$DESTINATION"
mkdir -p "$DESTINATION"
tar -xzf "$TMPDIR/$COMMON_ARCHIVE" -C "$DESTINATION"
tar -xzf "$TMPDIR/$TARGET_ARCHIVE" -C "$DESTINATION"
# The anchor 'foundation update' checks the next installer against. It comes
# from this script rather than from the archive, so it is exactly the key that
# authenticated this install, and an install that skipped verification leaves
# the bundle without one.
if [ -n "$RELEASE_KEY_FILE" ]; then
  cp "$RELEASE_KEY_FILE" "$DESTINATION/share/foundation-sdk-release.asc"
fi
refresh_installed_mtimes "$DESTINATION"
rm -f "$CURRENT_LINK"
ln -s "$DESTINATION" "$CURRENT_LINK"

if [ "$REFRESH_CACHED_BASE_THEME" -eq 1 ]; then
  NEW_BASE_THEME="$DESTINATION/lib/keyos/sdk/crates/foundation-themes/themes/base_theme.json"
  if [ -f "$NEW_BASE_THEME" ]; then
    cp "$NEW_BASE_THEME" "$CACHED_BASE_THEME"
    print_ok "Updated unmodified Base Theme cache"
  fi
fi

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

if [ "$OS" = "unknown-linux-gnu" ] && [ ! -f /etc/udev/rules.d/99-passport.rules ]; then
  echo
  echo "To access Passport Prime over USB (sideload, logs, passport-drive), install the udev rules:"
  echo "  sudo cp \"$CURRENT_LINK/share/99-passport.rules\" /etc/udev/rules.d/"
  echo "  sudo udevadm control --reload && sudo udevadm trigger"
fi
"##
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    use super::{
        common_archive_name, detached_signature_command, deterministic_archive_command,
        is_gnu_tar_version_output, next_value, render_install_script, target_archive_name,
        write_release_metadata, EmbeddedPublicKey, PackageArgs,
    };

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
            "https://sdk.foundation.xyz/v1.2",
        );

        assert!(script.contains("VERSION=\"1.2.3\""));
        assert!(script.contains("DEFAULT_BASE_URL=\"https://sdk.foundation.xyz/v1.2\""));
        assert!(script.contains(
            "SUPPORTED_TARGETS=\"aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu\""
        ));
        assert!(script.contains("FOUNDATION_SDK_UPDATE_RC"));
        assert!(script.contains("curl -fL --progress-bar --show-error"));
        assert!(script.contains("wget -O \"$destination\" \"$url\""));
        assert!(script
            .contains(&format!("EMBEDDED_GPG_PUBLIC_KEY_FINGERPRINT=\"{TEST_PUBLIC_KEY_FINGERPRINT}\"")));
        assert!(script.contains("EMBEDDED_GPG_PUBLIC_KEY_B64=\""));
        assert!(script.contains("base64 -d > \"$RELEASE_KEY_FILE\""));
        assert!(script.contains("cp \"$RELEASE_KEY_FILE\" \"$DESTINATION/share/foundation-sdk-release.asc\""));
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
        assert!(script.contains("cmp -s \"$CACHED_BASE_THEME\" \"$CURRENT_BASE_THEME\""));
        assert!(script.contains("REFRESH_CACHED_BASE_THEME=1"));
        assert!(script.contains("print_ok \"Updated unmodified Base Theme cache\""));
        assert!(script.contains("if update_rc_enabled; then"));
        assert!(script.contains("print_warn \"Could not update shell rc file: $RC_FILE\""));
        assert!(script.contains("export PATH="));
        assert!(script.contains("foundation develop"));
        assert!(script.contains("source"));

        // The template is a format string, so an unbalanced edit reaches users as
        // a release that cannot be installed at all.
        let mut shell = Command::new("sh")
            .arg("-n")
            .stdin(Stdio::piped())
            .spawn()
            .expect("sh is required to check the rendered installer");
        shell.stdin.take().expect("piped stdin").write_all(script.as_bytes()).unwrap();
        assert!(shell.wait().unwrap().success(), "rendered install.sh is not valid shell");
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
    fn release_metadata_removes_legacy_upload_script() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join(common_archive_name("1.2.3"));
        fs::write(&archive, "common").unwrap();
        fs::write(temp.path().join("upload.sh"), "legacy").unwrap();

        let written = write_release_metadata(
            temp.path(),
            "1.2.3",
            &["aarch64-apple-darwin".to_string()],
            &[archive],
            None,
            "https://sdk.foundation.xyz/v1.2.3",
            false,
        )
        .unwrap();

        assert!(!temp.path().join("upload.sh").exists());
        assert!(written.files.iter().any(|path| path.ends_with("install.sh")));
        assert!(written.files.iter().any(|path| path.ends_with("checksums.sha256")));
    }
}
