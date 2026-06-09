// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Shared Foundation signing identity layout and discovery.

use std::fs;
use std::path::{Path, PathBuf};

pub const FOUNDATION_DIR_NAME: &str = ".foundation";
pub const SIGNING_DIR_NAME: &str = "signing";
pub const PRIVATE_KEY_FILE: &str = "private.pem";
pub const PUBLIC_KEY_FILE: &str = "public.pub";
pub const CERTIFICATE_FILE: &str = "certificate.crt";
pub const COSIGN2_CONFIG_FILE: &str = "cosign2.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningIdentityPaths {
    pub identity_name: String,
    pub root: PathBuf,
    pub private_key: PathBuf,
    pub public_key: PathBuf,
    pub certificate: PathBuf,
    pub cosign2_config: PathBuf,
}

impl SigningIdentityPaths {
    pub fn new(identity_name: impl Into<String>, root: PathBuf) -> Self {
        let identity_name = identity_name.into();
        Self {
            identity_name,
            private_key: root.join(PRIVATE_KEY_FILE),
            public_key: root.join(PUBLIC_KEY_FILE),
            certificate: root.join(CERTIFICATE_FILE),
            cosign2_config: root.join(COSIGN2_CONFIG_FILE),
            root,
        }
    }
}

pub fn foundation_dir() -> Result<PathBuf, SigningError> {
    let home = dirs::home_dir().ok_or(SigningError::HomeDirUnavailable)?;
    Ok(home.join(FOUNDATION_DIR_NAME))
}

pub fn signing_root_dir() -> Result<PathBuf, SigningError> { Ok(foundation_dir()?.join(SIGNING_DIR_NAME)) }

pub fn signing_identity_paths(identity_name: &str) -> Result<SigningIdentityPaths, SigningError> {
    Ok(signing_identity_paths_in_root(identity_name, &signing_root_dir()?))
}

pub fn list_signing_identities() -> Result<Vec<SigningIdentityPaths>, SigningError> {
    let root = signing_root_dir()?;
    list_signing_identities_in_root(&root)
}

pub fn configured_signing_identities() -> Result<Vec<SigningIdentityPaths>, SigningError> {
    configured_signing_identities_in_root(&signing_root_dir()?)
}

pub fn resolve_identity_cosign2_config(identity_name: &str) -> Result<PathBuf, SigningError> {
    resolve_identity_cosign2_config_in_root(identity_name, &signing_root_dir()?)
}

fn signing_identity_paths_in_root(identity_name: &str, signing_root: &Path) -> SigningIdentityPaths {
    SigningIdentityPaths::new(identity_name, signing_root.join(identity_name))
}

fn list_signing_identities_in_root(root: &Path) -> Result<Vec<SigningIdentityPaths>, SigningError> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut identities = Vec::new();
    for entry in
        fs::read_dir(root).map_err(|source| SigningError::ListFailed { path: root.to_path_buf(), source })?
    {
        let entry = entry.map_err(|source| SigningError::ListFailed { path: root.to_path_buf(), source })?;
        let file_type =
            entry.file_type().map_err(|source| SigningError::ListFailed { path: root.to_path_buf(), source })?;
        if !file_type.is_dir() {
            continue;
        }

        let identity_name = entry.file_name().to_string_lossy().to_string();
        identities.push(SigningIdentityPaths::new(identity_name, entry.path()));
    }

    identities.sort_by(|left, right| left.identity_name.cmp(&right.identity_name));
    Ok(identities)
}

fn configured_signing_identities_in_root(root: &Path) -> Result<Vec<SigningIdentityPaths>, SigningError> {
    Ok(list_signing_identities_in_root(root)?
        .into_iter()
        .filter(|identity| identity.cosign2_config.exists())
        .collect())
}

fn resolve_identity_cosign2_config_in_root(
    identity_name: &str,
    signing_root: &Path,
) -> Result<PathBuf, SigningError> {
    let identity = signing_identity_paths_in_root(identity_name, signing_root);
    if identity.cosign2_config.exists() {
        Ok(identity.cosign2_config)
    } else {
        Err(SigningError::IdentityConfigMissing {
            identity_name: identity_name.to_string(),
            path: identity.cosign2_config,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    #[error("Could not determine home directory")]
    HomeDirUnavailable,

    #[error("Failed to list signing identities under {path}: {source}")]
    ListFailed { path: PathBuf, source: std::io::Error },

    #[error("No cosign2 configuration was found for signing identity '{identity_name}' at {path}")]
    IdentityConfigMissing { identity_name: String, path: PathBuf },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        configured_signing_identities_in_root, resolve_identity_cosign2_config_in_root,
        signing_identity_paths_in_root, COSIGN2_CONFIG_FILE, SIGNING_DIR_NAME,
    };

    #[test]
    fn resolves_named_identity_config() {
        let root = make_temp_dir("signing-preferred");
        let signing_root = root.join(SIGNING_DIR_NAME);
        fs::create_dir_all(signing_root.join("demo-app")).unwrap();
        fs::write(
            signing_root.join("demo-app").join(COSIGN2_CONFIG_FILE),
            "pubkey = \"abc\"\nsecret = \"/tmp/key\"\n",
        )
        .unwrap();

        let resolved = resolve_identity_cosign2_config_in_root("demo-app", &signing_root).unwrap();
        assert_eq!(resolved, signing_root.join("demo-app").join(COSIGN2_CONFIG_FILE));

        cleanup(&root);
    }

    #[test]
    fn lists_only_configured_identities() {
        let root = make_temp_dir("signing-configured");
        let signing_root = root.join(SIGNING_DIR_NAME);
        fs::create_dir_all(signing_root.join("with-config")).unwrap();
        fs::create_dir_all(signing_root.join("without-config")).unwrap();
        fs::write(
            signing_root.join("with-config").join(COSIGN2_CONFIG_FILE),
            "pubkey = \"abc\"\nsecret = \"/tmp/key\"\n",
        )
        .unwrap();

        let identities = configured_signing_identities_in_root(&signing_root).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].identity_name, "with-config");

        cleanup(&root);
    }

    #[test]
    fn creates_identity_paths_with_stable_filenames() {
        let root = make_temp_dir("signing-paths");
        let signing_root = root.join(SIGNING_DIR_NAME);
        fs::create_dir_all(&signing_root).unwrap();

        let identity = signing_identity_paths_in_root("sample", &signing_root);
        assert_eq!(identity.root, signing_root.join("sample"));
        assert_eq!(identity.cosign2_config, identity.root.join(COSIGN2_CONFIG_FILE));
        assert_eq!(identity.private_key, identity.root.join("private.pem"));
        assert_eq!(identity.public_key, identity.root.join("public.pub"));
        assert_eq!(identity.certificate, identity.root.join("certificate.crt"));

        cleanup(&root);
    }

    fn make_temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("foundation-signing-{label}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn cleanup(path: &PathBuf) { let _ = fs::remove_dir_all(path); }
}
