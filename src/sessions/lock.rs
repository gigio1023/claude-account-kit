//! One sync at a time.
//!
//! The lock is a file created with `create_new`, holding this process's id
//! and a nonce. An advisory `flock` would be released by the kernel when a run
//! dies, but std's `File::try_lock` needs Rust 1.89 and this crate supports
//! 1.88, and adding `fs4` or `rustix` for one call is more than the lock is
//! worth: correctness never rests on it. Every Desktop write is a no-replace
//! link or a re-hashed rename, so two overlapping runs at worst repeat work.
//! The lock only keeps the background job from piling up.
//!
//! A lock left by a run that crashed is taken over once it is older than any
//! real run could be.

use super::platform;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A full run reads at most 256 MiB, which takes seconds, not minutes.
const STALE_AFTER: Duration = Duration::from_secs(10 * 60);

/// A held lock, released when dropped.
pub(super) struct RunLock {
    path: PathBuf,
    token: String,
}

impl RunLock {
    /// Take the lock, or `None` when a live run holds it.
    pub(super) fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let token = format!("{} {nanos}\n", process::id());
        for _ in 0..2 {
            match platform::create_private_new(path) {
                Ok(mut file) => {
                    file.write_all(token.as_bytes())?;
                    return Ok(Some(Self {
                        path: path.to_path_buf(),
                        token,
                    }));
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    if !is_stale(path)? {
                        return Ok(None);
                    }
                    match fs::remove_file(path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }
}

impl Drop for RunLock {
    /// Remove the lock only if it is still this run's; a run that took over a
    /// lock this one held too long keeps its own.
    fn drop(&mut self) {
        let ours = fs::read(&self.path).is_ok_and(|content| content == self.token.as_bytes());
        if ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn is_stale(path: &Path) -> io::Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "the sync lock is not a regular file",
        ));
    }
    let age = metadata
        .modified()?
        .elapsed()
        .unwrap_or(Duration::ZERO);
    Ok(age > STALE_AFTER)
}
