//! A private temporary file beside the name it will be published under.
//!
//! Every file this tool writes into a directory it shares with another
//! program is first written here in full and flushed, then linked or renamed
//! into place, so a reader never sees half a file. The name never ends in
//! `.json.tmp`, which Desktop treats as its own recovery file.

use super::platform;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A fully written, flushed, owner-only file that is removed unless published.
pub(super) struct StagedFile {
    path: PathBuf,
    published: bool,
}

impl StagedFile {
    /// Write `bytes` to a new uniquely named file in `dir`.
    pub(super) fn write(dir: &Path, bytes: &[u8]) -> io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            ".claude-account-kit-{}-{nanos}-{sequence}.partial",
            process::id()
        ));
        let mut file = platform::create_private_new(&path)?;
        let staged = Self {
            path,
            published: false,
        };
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(staged)
    }

    /// Publish under `target` only if that name is free.
    ///
    /// A hard link fails when the target exists, whoever created it and
    /// however recently, which a check followed by a rename cannot promise.
    pub(super) fn link_new(&self, target: &Path) -> io::Result<()> {
        std::fs::hard_link(&self.path, target)
    }

    /// Publish under `target`, replacing whatever file is there in one step.
    pub(super) fn replace(mut self, target: &Path) -> io::Result<()> {
        std::fs::rename(&self.path, target)?;
        self.published = true;
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
