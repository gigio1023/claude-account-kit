//! This tool's own files: the state directory and its small JSON documents.
//!
//! The state directory must sit outside Desktop's data and the transcript
//! directory, so a sync can never mistake its own files for theirs, and none
//! of its files may be a symlink, which would let a write land somewhere else.

use super::bounded;
use super::paths::SessionPaths;
use super::platform;
use super::staging::StagedFile;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// State documents larger than this are refused.
const MAX_STATE_BYTES: u64 = 1024 * 1024;

/// Create the state directory, owner-only, after checking where it is.
pub(super) fn prepare_dir(paths: &SessionPaths) -> Result<()> {
    check_location(paths)?;
    let dir = &paths.state_dir;
    match fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => bail!("refusing to use {} because it is not a directory", dir.display()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", dir.display()));
        }
    }
    platform::restrict_dir(dir).with_context(|| format!("failed to restrict {}", dir.display()))?;
    // Checked before a run writes anything into Desktop's data, so a planted
    // symlink stops the run instead of failing it halfway.
    for file in [
        paths.ledger(),
        paths.last_run(),
        paths.fingerprint(),
        paths.seen_roots(),
        paths.lock(),
    ] {
        if fs::symlink_metadata(&file).is_ok_and(|metadata| !metadata.file_type().is_file()) {
            bail!("refusing to use {} because it is not a regular file", file.display());
        }
    }
    Ok(())
}

/// Refuse a state directory inside Desktop's data or the transcript directory,
/// comparing both the paths as given and the paths symlinks resolve to.
pub(super) fn check_location(paths: &SessionPaths) -> Result<()> {
    let state = resolved(&paths.state_dir);
    for protected in paths
        .desktop_roots()
        .iter()
        .chain(std::iter::once(&paths.projects_root))
    {
        if paths.state_dir.starts_with(protected) || state.starts_with(resolved(protected)) {
            bail!(
                "refusing to keep session state in {}, which is inside {}",
                paths.state_dir.display(),
                protected.display()
            );
        }
    }
    Ok(())
}

/// Read a state document; `None` when it does not exist.
pub(super) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match bounded::read_regular(path, MAX_STATE_BYTES) {
        Ok(file) => serde_json::from_slice(&file.bytes)
            .map(Some)
            .with_context(|| format!("failed to parse {}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// Replace a state document in one step, refusing to replace a symlink.
pub(super) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        bail!("refusing to replace {} because it is not a regular file", path.display());
    }
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    let staged = StagedFile::write(dir, text.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    staged
        .replace(path)
        .with_context(|| format!("failed to replace {}", path.display()))
}

/// The path with symlinks resolved as far as it exists, and the rest appended.
fn resolved(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    loop {
        if let Ok(canonical) = fs::canonicalize(&existing) {
            let mut result = canonical;
            for component in rest.iter().rev() {
                result.push(component);
            }
            return result;
        }
        match (
            existing.file_name().map(ToOwned::to_owned),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return lexical(path),
        }
    }
}

fn lexical(path: &Path) -> PathBuf {
    path.components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect()
}
