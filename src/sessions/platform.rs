//! The filesystem calls whose form differs by platform.
//!
//! Session sharing is a macOS feature, but `sync` runs wherever the paths
//! exist, so the few calls that need Unix permission bits or file identity
//! live here instead of being spread through the read and write paths.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

/// Whether two metadata records describe the same file.
///
/// Used after opening a path that was checked with `lstat`: if the leaf was
/// swapped for a symlink in between, the opened file is a different one.
#[cfg(unix)]
pub(super) fn same_file(before: &Metadata, after: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev() && before.ino() == after.ino()
}

/// Stable Rust has no file identity on Windows, so the check there is the
/// `lstat` type check alone.
#[cfg(not(unix))]
pub(super) fn same_file(_before: &Metadata, _after: &Metadata) -> bool {
    true
}

/// Create a new owner-only file, failing if the name is taken.
pub(super) fn create_private_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    restrict_new_file(&mut options);
    options.open(path)
}

/// Open an owner-only file for appending, creating it when missing.
pub(super) fn open_private_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    restrict_new_file(&mut options);
    options.open(path)
}

#[cfg(unix)]
fn restrict_new_file(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

/// Windows has no mode bits; a new file inherits the user profile's ACL.
#[cfg(not(unix))]
fn restrict_new_file(_options: &mut OpenOptions) {}

/// Make a directory owner-only.
#[cfg(unix)]
pub(super) fn restrict_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
pub(super) fn restrict_dir(path: &Path) -> io::Result<()> {
    fs::metadata(path).map(|_| ())
}

/// Flush a directory entry change, such as a new link or a rename, to disk.
#[cfg(unix)]
pub(super) fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Windows cannot open a directory as a file without extra flags, and its
/// rename is already journaled, so there is nothing to flush.
#[cfg(not(unix))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature has to match the unix version"
)]
pub(super) fn sync_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}
