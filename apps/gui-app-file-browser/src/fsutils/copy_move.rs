// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Instant;

use fs::adapter::FsAdapter;
use fs::{Error, OpenFlags, MAX_ASYNC_LEN};
use slint_keyos_platform::async_scalar;
use whence::WhenceExt;

use crate::fs_permissions::FileSystemPermissions;
use crate::fsutils::join_path;
use crate::FileSystem;

pub async fn move_entries(
    fs: FileSystem,
    source_location: fs::Location,
    dest_location: fs::Location,
    source_dir: &str,
    dest_dir: &str,
    names: &[String],
    mut progress: impl FnMut(u64, u64),
) -> whence::Result<(), Error> {
    if names.is_empty() {
        progress(0, 0);
        return Ok(());
    }

    log::info!(
        "move_entries start names={} source_dir={source_dir} dest_dir={dest_dir} locations={source_location:?}->{dest_location:?}",
        names.len()
    );
    let start = Instant::now();

    let (planned, total_bytes) = plan_entries(&fs, source_location, source_dir, names).await?;
    if total_bytes == 0 {
        progress(0, 0);
    }

    let mut completed = 0u64;
    let mut report = |delta: u64| {
        completed = completed.saturating_add(delta);
        progress(completed, total_bytes);
    };
    for entry in planned {
        let resolved = resolve_unique_name(&fs, dest_location, dest_dir, &entry.name)?;
        let dest_root = join_path(dest_dir, &resolved);

        if source_location == dest_location {
            let from = entry.source_path;
            let to = dest_root;
            fs.rename(&from, &to, dest_location).whence()?;
            report(entry.size);
            continue;
        }

        if entry.is_dir {
            copy_dir(&fs, &entry.source_path, &dest_root, source_location, dest_location, &mut report)
                .await?;
        } else {
            copy_file(&fs, &entry.source_path, &dest_root, source_location, dest_location, &mut report)
                .await?;
        }

        fs.remove(&entry.source_path, source_location).whence()?;
    }

    log::info!("move_entries done elapsed_ms={}", start.elapsed().as_millis());

    Ok(())
}

pub async fn copy_entries(
    fs: FileSystem,
    source_location: fs::Location,
    dest_location: fs::Location,
    source_dir: &str,
    dest_dir: &str,
    names: &[String],
    mut progress: impl FnMut(u64, u64),
) -> whence::Result<(), Error> {
    if names.is_empty() {
        progress(0, 0);
        return Ok(());
    }

    log::info!(
        "copy_entries start names={} source_dir={source_dir} dest_dir={dest_dir} locations={source_location:?}->{dest_location:?}",
        names.len()
    );
    let start = Instant::now();

    let (planned, total_bytes) = plan_entries(&fs, source_location, source_dir, names).await?;
    if total_bytes == 0 {
        progress(0, 0);
    }

    let mut completed = 0u64;
    let mut report = |delta: u64| {
        completed = completed.saturating_add(delta);
        progress(completed, total_bytes);
    };
    for entry in planned {
        let resolved = resolve_unique_name(&fs, dest_location, dest_dir, &entry.name)?;
        let dest_root = join_path(dest_dir, &resolved);

        if entry.is_dir {
            copy_dir(&fs, &entry.source_path, &dest_root, source_location, dest_location, &mut report)
                .await?;
        } else {
            copy_file(&fs, &entry.source_path, &dest_root, source_location, dest_location, &mut report)
                .await?;
        }
    }

    log::info!("copy_entries done elapsed_ms={}", start.elapsed().as_millis());

    Ok(())
}

struct PlannedEntry {
    name: String,
    source_path: String,
    is_dir: bool,
    size: u64,
}

async fn plan_entries(
    fs: &FileSystem,
    source_location: fs::Location,
    source_dir: &str,
    names: &[String],
) -> whence::Result<(Vec<PlannedEntry>, u64), Error> {
    let mut planned = Vec::with_capacity(names.len());
    let mut total_bytes = 0u64;

    for name in names {
        let source_path = join_path(source_dir, name);
        log::debug!("plan_entries entry name={name} source_path={source_path} location={source_location:?}");
        let (is_dir, entry_size) = entry_info(fs, &source_path, source_location)?;
        total_bytes += entry_size;
        planned.push(PlannedEntry { name: name.clone(), source_path, is_dir, size: entry_size });
    }

    log::debug!(
        "plan_entries done entries={} total_bytes={} location={source_location:?}",
        planned.len(),
        total_bytes
    );

    Ok((planned, total_bytes))
}

async fn copy_dir(
    fs: &FileSystem,
    source_root: &str,
    dest_root: &str,
    source_location: fs::Location,
    dest_location: fs::Location,
    report: &mut impl FnMut(u64),
) -> whence::Result<(), Error> {
    fs.create_dir(dest_root, dest_location).whence()?;

    let walker = fs.walk_dir(source_root, source_location).whence()?;
    for entry in walker {
        let (path, entry) = entry.whence()?;
        let relative = strip_prefix(&path, source_root);
        let dest_path = join_path(dest_root, &relative);
        if entry.is_dir {
            fs.create_dir(&dest_path, dest_location).whence()?;
        } else if entry.is_file {
            copy_file(fs, &path, &dest_path, source_location, dest_location, report).await?;
        }
    }

    Ok(())
}

async fn copy_file(
    fs: &FileSystem,
    source_path: &str,
    dest_path: &str,
    source_location: fs::Location,
    dest_location: fs::Location,
    report: &mut impl FnMut(u64),
) -> whence::Result<(), Error> {
    let metadata = fs.metadata(source_path, source_location).whence()?;
    log::debug!(
        "copy_file metadata source={source_path} location={source_location:?} size={}",
        metadata.size
    );
    let mut src = fs.open_file(source_path, source_location, OpenFlags::READ_ONLY).whence()?;
    let mut dst = fs.open_file(dest_path, dest_location, OpenFlags::CREATE).whence()?;

    let mut remaining = metadata.size as usize;
    while remaining > 0 {
        let req = src.async_copy_block_to(&mut dst, MAX_ASYNC_LEN);
        let written = async_scalar::<FileSystemPermissions, _>(req).await.whence()?;
        if written == 0 {
            break;
        }
        remaining = remaining.saturating_sub(written);
        report(written as u64);
    }

    log::debug!("copy_file done source={source_path} dest={dest_path} bytes={}", metadata.size);

    Ok(())
}

fn resolve_unique_name(
    fs: &FileSystem,
    location: fs::Location,
    dest_dir: &str,
    name: &str,
) -> whence::Result<String, Error> {
    for suffix in 0usize.. {
        let candidate = if suffix == 0 { name.to_string() } else { with_suffix(name, suffix) };
        let candidate_path = join_path(dest_dir, &candidate);
        match fs.metadata(&candidate_path, location) {
            Ok(_) => continue,
            Err(Error::FileNotFound) => return Ok(candidate),
            Err(e) => return Err(e).whence(),
        }
    }

    Err(Error::InternalError).whence()
}

fn dir_size(fs: &FileSystem, source_path: &str, location: fs::Location) -> whence::Result<u64, Error> {
    log::debug!("dir_size start path={source_path} location={location:?}");
    let walker = fs.walk_dir(source_path, location).whence()?;
    let mut total = 0u64;
    for entry in walker {
        let (_path, entry) = entry.whence()?;
        if entry.is_file {
            total = total.saturating_add(entry.len);
        }
    }
    log::debug!("dir_size done path={source_path} location={location:?} total_bytes={total}");
    Ok(total)
}

fn entry_info(fs: &FileSystem, path: &str, location: fs::Location) -> whence::Result<(bool, u64), Error> {
    log::debug!("entry_info metadata start path={path} location={location:?}");
    let metadata = fs.metadata(path, location).whence()?;
    log::debug!(
        "entry_info metadata done path={path} location={location:?} is_dir={} size={}",
        metadata.is_dir,
        metadata.size
    );
    if metadata.is_dir {
        let size = dir_size(fs, path, location)?;
        Ok((true, size))
    } else {
        Ok((false, metadata.size))
    }
}

fn strip_prefix(path: &str, prefix: &str) -> String {
    let trimmed_prefix = prefix.trim_end_matches('/');
    let mut relative = path.strip_prefix(trimmed_prefix).unwrap_or(path).trim_start_matches('/');
    if relative.is_empty() {
        relative = path;
    }
    relative.to_string()
}

fn with_suffix(name: &str, suffix: usize) -> String {
    match name.rsplit_once('.') {
        Some((base, ext)) => format!("{base}-{suffix}.{ext}"),
        None => format!("{name}-{suffix}"),
    }
}
