// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Generate signing material and a publisher certificate for app signing.

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use foundation_core::{
    list_signing_identities, signing_identity_paths, ProjectContext, PublisherConfig, SigningIdentityPaths,
};
use foundation_mcp::PassportDriveMcpClient;
use foundation_ui::Prompts;
use serde::Serialize;

/// Default certificate name when no project context is available.
const DEFAULT_CERT_NAME: &str = "developer";
const CERT_VALIDITY_DAYS: &str = "3650";

// Status indicators for the `cert gen` progress lines. Centralized so the
// escape sequences live in one place and TTY-detection (if we add it later)
// only changes here.
const STATUS_OK: &str = "\x1b[32m✓\x1b[0m";
const STATUS_WARN: &str = "\x1b[33m⚠\x1b[0m";
const STATUS_ERR: &str = "\x1b[31m✗\x1b[0m";

#[derive(Debug, Clone)]
struct CertificateIdentity {
    publisher_name: String,
    contact_email: String,
    support_url: String,
}

/// Execute `foundation cert gen`.
pub fn execute_gen(
    cert_name: Option<&str>,
    publisher_name: Option<&str>,
    contact_email: Option<&str>,
    support_url: Option<&str>,
) -> Result<()> {
    println!("Generating signing certificate...");
    println!();

    let project = ProjectContext::discover_optional();
    let default_cert_name = project
        .as_ref()
        .and_then(|project| project.config.publisher.name_value())
        .unwrap_or(DEFAULT_CERT_NAME);

    let cert_name = match cert_name {
        Some(name) => name.trim().to_string(),
        None => prompt_value("Publisher identity name", default_cert_name, false)?,
    };

    if !is_valid_identity_name(&cert_name) {
        anyhow::bail!(
            "Invalid publisher identity name '{}'. It cannot be empty or contain path separators.",
            cert_name
        );
    }

    let identity = resolve_identity(
        project.as_ref().map(|project| &project.config.publisher),
        publisher_name,
        contact_email,
        support_url,
    )?;

    let identity_paths = signing_identity_paths(&cert_name)?;
    ensure_directory(&identity_paths.root)?;

    if [
        identity_paths.private_key.as_path(),
        identity_paths.public_key.as_path(),
        identity_paths.certificate.as_path(),
        identity_paths.cosign2_config.as_path(),
    ]
    .into_iter()
    .any(Path::exists)
    {
        println!(
            "  {STATUS_WARN} {}",
            format!("Signing material with this name already exists at: {}", identity_paths.root.display())
        );
        if !prompt_overwrite()? {
            return Ok(());
        }
    }

    if !is_openssl_available() {
        println!("  {STATUS_ERR} {}", "OpenSSL not found. Please install OpenSSL to generate keys.");
        anyhow::bail!("OpenSSL not found. Please install OpenSSL to generate keys.");
    }
    println!("  {STATUS_OK} {}", "OpenSSL found");

    let private_key = generate_private_key()?;
    write_private_key(&identity_paths.private_key, &private_key)
        .with_context(|| format!("Failed to write key file: {}", identity_paths.private_key.display()))?;
    println!("  {STATUS_OK} {}", "Private key generated");

    let public_key_hex = extract_public_key(&private_key)?;
    fs::write(&identity_paths.public_key, &public_key_hex)
        .with_context(|| format!("Failed to write key file: {}", identity_paths.public_key.display()))?;
    println!("  {STATUS_OK} {}", "Public key extracted");

    generate_certificate(&identity_paths.private_key, &identity_paths.certificate, &identity)?;
    println!("  {STATUS_OK} {}", "X.509 certificate generated");

    create_cosign2_config(&identity_paths.cosign2_config, &identity_paths.private_key, &public_key_hex)?;
    println!("  {STATUS_OK} {}", "Configuration created");

    println!();
    println!("  {STATUS_OK} {}", "Signing certificate generated successfully!");
    println!();
    println!("    {} {}", "Private key:", identity_paths.private_key.display());
    println!("    {} {}", "Public key:", identity_paths.public_key.display());
    println!("    {} {}", "Certificate:", identity_paths.certificate.display());
    println!("    {} {}", "Public key (hex):", public_key_hex);
    println!("    {} {}", "Config file:", identity_paths.cosign2_config.display());
    println!();
    println!("You can now run 'foundation build' to build and sign your app.");

    Ok(())
}

/// Execute `foundation cert print`.
pub fn execute_print(cert_name: Option<&str>) -> Result<()> {
    println!("Printing signing certificate...");
    println!();

    if !is_openssl_available() {
        anyhow::bail!("OpenSSL not found. Please install OpenSSL to generate keys.");
    }

    let identity = resolve_identity_for_print(cert_name)?;
    println!("  {STATUS_OK} {}", format!("Using publisher identity {}", identity.identity_name));
    println!("  {STATUS_OK} {}", format!("Using certificate {}", identity.certificate.display()));
    println!();

    let status = Command::new("openssl")
        .args(["x509", "-in"])
        .arg(&identity.certificate)
        .args(["-text", "-noout"])
        .status()
        .context("Failed to print X.509 certificate")?;

    if !status.success() {
        anyhow::bail!("Failed to print X.509 certificate");
    }

    Ok(())
}

/// Execute `foundation cert install`.
pub fn execute_install(cert_name: Option<&str>) -> Result<()> {
    println!("Installing signing certificate...");
    println!();

    let identity = resolve_identity_for_print(cert_name)?;
    let certificate_len = certificate_len_for_install(&identity.certificate)?;
    println!("  {STATUS_OK} {}", format!("Using publisher identity {}", identity.identity_name));
    println!(
        "  {STATUS_OK} {}",
        format!("Using certificate {} ({certificate_len} bytes)", identity.certificate.display())
    );
    println!();

    println!("Checking passport-drive MCP control...");
    let mut mcp = PassportDriveMcpClient::connect().map_err(|error| {
        anyhow::anyhow!("Could not start passport-drive MCP control. Make sure Passport Prime is unlocked, connected by USB, and Developer Mode is enabled: {error}")
    })?;
    println!("passport-drive MCP control connected.");

    println!("Installing certificate via usb-debug...");
    let install_response = mcp.install_certificate(&identity.certificate).map_err(|error| {
        anyhow::anyhow!(
            "Could not install {} over usb-debug. Make sure Developer Mode is enabled and no other process is using the Passport USB debug interface. Reason: {}",
            identity.certificate.display(),
            error
        )
    })?;
    println!("{install_response}");
    println!("Certificate installed successfully!");

    Ok(())
}

fn resolve_identity(
    defaults: Option<&PublisherConfig>,
    publisher_name: Option<&str>,
    contact_email: Option<&str>,
    support_url: Option<&str>,
) -> Result<CertificateIdentity> {
    let publisher_name = resolve_required_value(
        publisher_name,
        defaults.and_then(PublisherConfig::name_value),
        "Publisher name",
        validate_non_empty,
        "Publisher name cannot be empty.",
    )?;
    let contact_email = resolve_required_value(
        contact_email,
        defaults.and_then(PublisherConfig::contact_email_value),
        "Contact email address",
        validate_email,
        "Contact email must be a valid email address.",
    )?;
    let support_url = resolve_required_value(
        support_url,
        defaults.and_then(PublisherConfig::support_url_value),
        "Support website URL",
        validate_support_url,
        "Support website URL must start with http:// or https://.",
    )?;

    Ok(CertificateIdentity { publisher_name, contact_email, support_url })
}

fn resolve_required_value(
    explicit: Option<&str>,
    default: Option<&str>,
    prompt: &str,
    validator: fn(&str) -> bool,
    error_message: &str,
) -> Result<String> {
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        if validator(value) {
            return Ok(value.to_string());
        }
        anyhow::bail!("{}", error_message);
    }

    if let Some(value) = default.map(str::trim).filter(|value| !value.is_empty()) {
        if validator(value) {
            return Ok(value.to_string());
        }
    }

    loop {
        let value = prompt_value(prompt, "", false)?;
        if validator(&value) {
            return Ok(value);
        }
        eprintln!("Error: {}", error_message);
    }
}

fn resolve_identity_for_print(explicit_name: Option<&str>) -> Result<SigningIdentityPaths> {
    if let Some(name) = explicit_name.map(str::trim).filter(|value| !value.is_empty()) {
        if !is_valid_identity_name(name) {
            anyhow::bail!(
                "Invalid publisher identity name '{name}'. It cannot be empty or contain path separators."
            );
        }
        let mut identity = signing_identity_paths(name)?;
        select_existing_certificate_path(&mut identity);
        if identity.certificate.exists() {
            return Ok(identity);
        }
        anyhow::bail!("Certificate not found: {}", identity.certificate.display());
    }

    let mut identities: Vec<SigningIdentityPaths> =
        list_signing_identities()?.into_iter().filter_map(existing_certificate_identity).collect();

    if let Some(project_identity) = ProjectContext::discover_optional()
        .and_then(|project| project.config.publisher.name_value().map(str::to_string))
        .and_then(|publisher_name| {
            identities.iter().find(|identity| identity.identity_name == publisher_name).cloned()
        })
    {
        return Ok(project_identity);
    }

    match identities.as_slice() {
        [] => anyhow::bail!("No publisher certificates were found. Run 'foundation cert gen' first."),
        [identity] => Ok(identity.clone()),
        _ if !std::io::stderr().is_terminal() => anyhow::bail!(
            "Multiple publisher certificates are available ({}), but this session is not interactive. Pass the identity name explicitly.",
            identities
                .iter()
                .map(|identity| identity.identity_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => {
            let options: Vec<&str> =
                identities.iter().map(|identity| identity.identity_name.as_str()).collect();
            let selection = Prompts::new()
                .select("Select a publisher certificate", &options)
                .context("Failed to choose a publisher certificate")?;
            Ok(identities.remove(selection))
        }
    }
}

fn existing_certificate_identity(mut identity: SigningIdentityPaths) -> Option<SigningIdentityPaths> {
    select_existing_certificate_path(&mut identity);
    identity.certificate.exists().then_some(identity)
}

fn select_existing_certificate_path(identity: &mut SigningIdentityPaths) {
    if !identity.certificate.exists() {
        let legacy_certificate = identity.legacy_certificate();
        if legacy_certificate.exists() {
            identity.certificate = legacy_certificate;
        }
    }
}

fn certificate_len_for_install(certificate_path: &Path) -> Result<u64> {
    let metadata = fs::metadata(certificate_path)
        .with_context(|| format!("Failed to read certificate metadata: {}", certificate_path.display()))?;
    if metadata.len() == 0 {
        anyhow::bail!("Certificate is empty: {}", certificate_path.display());
    }
    Ok(metadata.len())
}

fn ensure_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)
            .with_context(|| format!("Failed to create directory: {}", path.display()))?;
        println!("  {STATUS_OK} {}", format!("Created {}", path.display()));
    } else {
        println!("  {STATUS_OK} {}", "Signing directory exists");
    }
    Ok(())
}

fn prompt_value(prompt: &str, default: &str, allow_empty: bool) -> Result<String> {
    if default.is_empty() {
        print!("{}: ", prompt);
    } else {
        print!("{} [{}]: ", prompt, default);
    }
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    if input.is_empty() {
        if allow_empty {
            Ok(String::new())
        } else {
            Ok(default.to_string())
        }
    } else {
        Ok(input.to_string())
    }
}

fn prompt_overwrite() -> Result<bool> {
    print!("Overwrite existing signing material? [y/N]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    Ok(input == "y" || input == "yes")
}

fn validate_non_empty(value: &str) -> bool { !value.trim().is_empty() }

fn validate_email(value: &str) -> bool {
    let trimmed = value.trim();
    let mut parts = trimmed.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();
    if !parts.next().is_none() || local.is_empty() {
        return false;
    }
    // Domain must look like at least `label.tld`: no leading/trailing dot, no empty labels,
    // every label non-empty. This rejects `user@localhost`, `user@.com`, `user@a.`, and `user@a..b`.
    if domain.starts_with('.') || domain.ends_with('.') {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    labels.len() >= 2 && labels.iter().all(|label| !label.is_empty())
}

fn validate_support_url(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("https://") || trimmed.starts_with("http://")
}

fn is_valid_identity_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed != "."
        && trimmed != ".."
        && !trimmed.chars().any(|c| c == '/' || c == '\\' || c.is_control())
}

fn is_openssl_available() -> bool {
    Command::new("openssl").arg("version").output().map(|output| output.status.success()).unwrap_or(false)
}

/// Write the app-signing private key with restrictive permissions so other
/// local users can't read it. The previous implementation used `fs::write`,
/// which under a typical Unix `umask 022` creates the file as mode 0644 — i.e.
/// world-readable. On a shared/multi-user dev machine that leaks the signing
/// key. Create with mode 0o600 on Unix; on other platforms fall back to the
/// default behaviour (Windows ACLs already restrict to the creating user).
/// When replacing an existing key, reset the permissions after opening because
/// `OpenOptionsExt::mode` only affects newly-created files.
fn write_private_key(path: &Path, key: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file =
            fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(key)?;
        file.sync_all()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, key)
    }
}

fn generate_private_key() -> Result<Vec<u8>> {
    let output = Command::new("openssl")
        .args(["ecparam", "-noout", "-genkey", "-name", "secp256k1"])
        .output()
        .context("Failed to generate private key")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{}: {}", "Failed to generate private key", stderr);
    }

    Ok(output.stdout)
}

fn extract_public_key(private_key_pem: &[u8]) -> Result<String> {
    let mut openssl_ec = Command::new("openssl")
        .args(["ec", "-pubout", "-conv_form", "compressed", "-outform", "DER"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to extract public key")?;

    if let Some(mut stdin) = openssl_ec.stdin.take() {
        // openssl can exit before reading the whole key (e.g. a bad key),
        // surfacing here as a broken pipe; ignore it so the status check below
        // reports openssl's real error instead of an opaque EPIPE.
        if let Err(err) = stdin.write_all(private_key_pem) {
            if err.kind() != io::ErrorKind::BrokenPipe {
                return Err(err).context("Failed to write private key to openssl");
            }
        }
    }

    let output = openssl_ec.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{}: {}", "Failed to extract public key", stderr);
    }

    let der = output.stdout;
    if der.len() < 33 {
        anyhow::bail!("{}: DER output too short", "Failed to extract public key");
    }

    Ok(hex::encode(&der[der.len() - 33..]))
}

fn generate_certificate(
    private_key_path: &Path,
    certificate_path: &Path,
    identity: &CertificateIdentity,
) -> Result<()> {
    let config_contents = openssl_config(identity);
    // The config carries publisher identity info, so it must not outlive this
    // call; NamedTempFile removes it on drop even if openssl fails or we return early.
    let mut config_file = tempfile::Builder::new()
        .suffix(".cnf")
        .tempfile()
        .context("Failed to create temporary openssl config")?;
    config_file
        .write_all(config_contents.as_bytes())
        .with_context(|| format!("Failed to write openssl config: {}", config_file.path().display()))?;

    let output = Command::new("openssl")
        .args([
            "req",
            "-new",
            "-x509",
            "-key",
            &private_key_path.display().to_string(),
            "-out",
            &certificate_path.display().to_string(),
            "-days",
            CERT_VALIDITY_DAYS,
            "-sha256",
            "-config",
            &config_file.path().display().to_string(),
        ])
        .output()
        .context("Failed to generate X.509 certificate")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{}: {}", "Failed to generate X.509 certificate", stderr);
    }

    Ok(())
}

fn openssl_config(identity: &CertificateIdentity) -> String {
    let publisher_name = escape_openssl_value(&identity.publisher_name);
    let contact_email = escape_openssl_value(&identity.contact_email);
    let support_url = escape_openssl_value(&identity.support_url);

    format!(
        "[req]\n\
prompt = no\n\
distinguished_name = dn\n\
x509_extensions = v3_req\n\
\n\
[dn]\n\
CN = {publisher_name}\n\
O = {publisher_name}\n\
emailAddress = {contact_email}\n\
\n\
[v3_req]\n\
basicConstraints = CA:FALSE\n\
keyUsage = digitalSignature\n\
extendedKeyUsage = codeSigning\n\
subjectAltName = @san\n\
\n\
[san]\n\
email.1 = {contact_email}\n\
URI.1 = {support_url}\n"
    )
}

fn escape_openssl_value(value: &str) -> String { value.replace('\\', "\\\\").replace('\n', " ") }

#[derive(Serialize)]
struct Cosign2Config {
    pubkey: String,
    secret: String,
    target: String,
}

fn create_cosign2_config(config_path: &Path, private_key_path: &Path, public_key_hex: &str) -> Result<()> {
    let config = Cosign2Config {
        pubkey: public_key_hex.to_string(),
        secret: private_key_path.display().to_string(),
        target: "atsama5d27-keyos".to_string(),
    };
    let mut config_content = String::from(
        r#"# cosign2 configuration for Foundation app signing
# Generated by: foundation cert gen

"#,
    );
    config_content.push_str(&toml::to_string(&config)?);

    fs::write(config_path, config_content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::test_support::make_temp_dir;

    #[test]
    fn invalid_print_identity_error_includes_name() {
        let error = super::resolve_identity_for_print(Some("invalid/name")).unwrap_err();

        assert_eq!(
            error.to_string(),
            "Invalid publisher identity name 'invalid/name'. It cannot be empty or contain path separators."
        );
    }

    #[cfg(unix)]
    #[test]
    fn overwriting_private_key_repairs_existing_permissions() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let root_dir = make_temp_dir("cert-perms");
        let root = root_dir.path();
        let key_path = root.join("private.pem");

        fs::write(&key_path, b"old key").unwrap();
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();

        super::write_private_key(&key_path, b"new key").unwrap();

        let mode = fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(fs::read(&key_path).unwrap(), b"new key");
    }

    #[test]
    fn cosign2_config_escapes_toml_string_values() {
        use std::fs;

        let root_dir = make_temp_dir("cert-config");
        let root = root_dir.path();

        let config_path = root.join("cosign2.toml");
        let private_key_path = root.join(r#"identity "demo"\private.pem"#);
        let public_key_hex = "02abcdef";

        super::create_cosign2_config(&config_path, &private_key_path, public_key_hex).unwrap();

        let config_contents = fs::read_to_string(&config_path).unwrap();
        let parsed: toml::Value = toml::from_str(&config_contents).unwrap();
        assert_eq!(parsed["pubkey"].as_str(), Some(public_key_hex));
        assert_eq!(parsed["secret"].as_str(), Some(private_key_path.display().to_string().as_str()));
        assert_eq!(parsed["target"].as_str(), Some("atsama5d27-keyos"));
    }

    #[test]
    fn existing_certificate_identity_accepts_legacy_certificate_filename() {
        use std::fs;

        let root_dir = make_temp_dir("cert-legacy");
        let root = root_dir.path();
        let identity_root = root.join("demo");
        fs::create_dir_all(&identity_root).unwrap();

        let identity = foundation_core::SigningIdentityPaths::new("demo", identity_root);
        let legacy_certificate = identity.legacy_certificate();
        fs::write(&legacy_certificate, b"CERTIFICATE").unwrap();

        let resolved = super::existing_certificate_identity(identity).unwrap();
        assert_eq!(resolved.certificate, legacy_certificate);
    }
}
