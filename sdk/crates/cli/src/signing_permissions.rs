// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Filesystem permission hardening for CLI-managed signing identities.

use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SigningDirectoryStatus {
    Created,
    Repaired,
    Unchanged,
}

/// Create a managed signing directory with mode `0700`, or remove group/world
/// permissions from an existing directory while preserving its owner mode.
pub(crate) fn ensure_signing_directory(path: &Path) -> std::io::Result<SigningDirectoryStatus> {
    let existing = metadata_without_symlink(path)?;
    if existing.as_ref().is_some_and(|metadata| !metadata.is_dir()) {
        return Err(invalid_path_error(path, "is not a directory"));
    }
    let existed = existing.is_some();

    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(path)?;

        let metadata = metadata_without_symlink(path)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("signing directory disappeared: {}", path.display()),
            )
        })?;
        if !existed {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            return Ok(SigningDirectoryStatus::Created);
        }
        if remove_group_world_permissions(path, &metadata)? {
            return Ok(SigningDirectoryStatus::Repaired);
        }
    }

    #[cfg(not(unix))]
    fs::create_dir_all(path)?;

    Ok(if existed { SigningDirectoryStatus::Unchanged } else { SigningDirectoryStatus::Created })
}

/// Remove group/world permissions from an existing managed private key while
/// preserving its owner mode. Returns `true` when the key needed repair.
pub(crate) fn repair_private_key_permissions(path: &Path) -> std::io::Result<bool> {
    let Some(metadata) = metadata_without_symlink(path)? else {
        return Ok(false);
    };
    if !metadata.is_file() {
        return Err(invalid_path_error(path, "is not a regular file"));
    }

    #[cfg(unix)]
    {
        remove_group_world_permissions(path, &metadata)
    }

    #[cfg(not(unix))]
    {
        let _ = metadata;
        Ok(false)
    }
}

fn metadata_without_symlink(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(invalid_path_error(path, "is a symbolic link"))
        }
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn invalid_path_error(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("refusing to secure managed signing path {}: {reason}", path.display()),
    )
}

#[cfg(unix)]
fn remove_group_world_permissions(path: &Path, metadata: &fs::Metadata) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode();
    if mode & 0o077 == 0 {
        return Ok(false);
    }

    fs::set_permissions(path, fs::Permissions::from_mode(mode & !0o077))?;
    Ok(true)
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};

    use super::{ensure_signing_directory, repair_private_key_permissions, SigningDirectoryStatus};
    use crate::test_support::make_temp_dir;

    #[test]
    fn signing_directories_are_created_with_0700_and_repairs_preserve_owner_mode() {
        let root_dir = make_temp_dir("signing-directory-permissions");
        let root = root_dir.path();
        let signing_root = root.join("signing");
        let identity_root = signing_root.join("demo-app");

        assert_eq!(ensure_signing_directory(&signing_root).unwrap(), SigningDirectoryStatus::Created);
        assert_eq!(ensure_signing_directory(&identity_root).unwrap(), SigningDirectoryStatus::Created);
        assert_eq!(fs::metadata(&signing_root).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&identity_root).unwrap().permissions().mode() & 0o777, 0o700);

        fs::set_permissions(&signing_root, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&identity_root, fs::Permissions::from_mode(0o555)).unwrap();

        assert_eq!(ensure_signing_directory(&signing_root).unwrap(), SigningDirectoryStatus::Repaired);
        assert_eq!(ensure_signing_directory(&identity_root).unwrap(), SigningDirectoryStatus::Repaired);
        assert_eq!(fs::metadata(&signing_root).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&identity_root).unwrap().permissions().mode() & 0o777, 0o500);
    }

    #[test]
    fn existing_private_key_permissions_are_repaired() {
        let root_dir = make_temp_dir("private-key-permissions");
        let root = root_dir.path();
        let private_key = root.join("private.pem");
        fs::write(&private_key, b"private key").unwrap();
        fs::set_permissions(&private_key, fs::Permissions::from_mode(0o444)).unwrap();

        assert!(repair_private_key_permissions(&private_key).unwrap());
        assert_eq!(fs::metadata(&private_key).unwrap().permissions().mode() & 0o777, 0o400);
        assert!(!repair_private_key_permissions(&private_key).unwrap());
    }

    #[test]
    fn symlinked_managed_paths_are_rejected_without_changing_targets() {
        let root_dir = make_temp_dir("signing-permission-symlinks");
        let root = root_dir.path();

        let directory_target = root.join("directory-target");
        let directory_link = root.join("directory-link");
        fs::create_dir(&directory_target).unwrap();
        fs::set_permissions(&directory_target, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&directory_target, &directory_link).unwrap();

        let error = ensure_signing_directory(&directory_link).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(fs::metadata(&directory_target).unwrap().permissions().mode() & 0o777, 0o755);

        let key_target = root.join("key-target.pem");
        let key_link = root.join("key-link.pem");
        fs::write(&key_target, b"private key").unwrap();
        fs::set_permissions(&key_target, fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&key_target, &key_link).unwrap();

        let error = repair_private_key_permissions(&key_link).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(fs::metadata(&key_target).unwrap().permissions().mode() & 0o777, 0o644);
    }
}
