//! Bounded reads of files this tool does not own.
//!
//! Every read has a size limit, and every read of Desktop or Claude Code data
//! also draws on one budget shared by the whole run, so a huge or growing file
//! cannot stall a background run. A path is checked with `lstat` and then
//! compared with what was opened, so a leaf symlink is never followed.
//!
//! Opening a path that turns into a FIFO between the check and the open could
//! block; only someone who can already write the user's own directories could
//! arrange that, and std has no portable `O_NOFOLLOW | O_NONBLOCK` to rule it out.

use super::platform;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Desktop allows 10 MiB wrapper records; connector metadata can fill them.
pub(super) const MAX_METADATA: u64 = 10 * 1024 * 1024;
/// One allowance for the whole run, across all partitions and transcripts.
const MAX_READ_BYTES: u64 = 256 * 1024 * 1024;
/// The start of a transcript, where its first `cwd` and title records are.
const HEAD_BYTES: u64 = 32 * 1024;
/// The end of a transcript, where its latest title and timestamps are.
const TAIL_BYTES: u64 = 64 * 1024;
/// Entries read from one directory before the listing counts as incomplete.
const MAX_ENTRIES: usize = 20_000;

/// The run's remaining read allowance in bytes.
pub(super) struct ReadBudget {
    remaining: u64,
}

impl ReadBudget {
    pub(super) fn new() -> Self {
        Self {
            remaining: MAX_READ_BYTES,
        }
    }

    /// Reserve bytes before reading them, failing before any over-budget read.
    fn claim(&mut self, amount: u64) -> io::Result<()> {
        if amount > self.remaining {
            return Err(invalid("the run's read budget is exhausted"));
        }
        self.remaining -= amount;
        Ok(())
    }
}

/// A file's bytes with the metadata it was opened under.
pub(super) struct ReadFile {
    pub(super) bytes: Vec<u8>,
    pub(super) metadata: Metadata,
}

/// Whether the path is a directory itself, not a symlink to one.
pub(super) fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

/// List a directory in sorted order, failing past the entry limit.
pub(super) fn entries(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(path)? {
        if result.len() >= MAX_ENTRIES {
            return Err(invalid("directory has more entries than the listing limit"));
        }
        result.push(entry?.path());
    }
    result.sort();
    Ok(result)
}

/// Read a regular file of at most `limit` bytes, outside the run budget.
///
/// For this tool's own state files, which must also never be symlinks.
pub(super) fn read_regular(path: &Path, limit: u64) -> io::Result<ReadFile> {
    let (file, metadata) = open_regular(path, limit)?;
    let bytes = read_to_size(file, metadata.len())?;
    Ok(ReadFile { bytes, metadata })
}

/// Read a Desktop metadata file, charging its size to the run budget.
pub(super) fn read_metadata(path: &Path, budget: &mut ReadBudget) -> io::Result<ReadFile> {
    let (file, metadata) = open_regular(path, MAX_METADATA)?;
    budget.claim(metadata.len() + 1)?;
    let bytes = read_to_size(file, metadata.len())?;
    Ok(ReadFile { bytes, metadata })
}

/// Read a transcript's head and tail windows, charging them to the budget.
///
/// The tail window starts at the first full line inside it, so a fragment of
/// an opaque message value is never mistaken for a top-level record.
pub(super) fn read_windows(path: &Path, budget: &mut ReadBudget) -> io::Result<Vec<Vec<u8>>> {
    let (mut file, metadata) = open_regular(path, u64::MAX)?;
    let size = metadata.len();
    let head_size = size.min(HEAD_BYTES);
    let tail_start = head_size.max(size.saturating_sub(TAIL_BYTES));
    budget.claim(head_size + (size - tail_start))?;

    let mut head = Vec::new();
    (&mut file).take(head_size).read_to_end(&mut head)?;
    file.seek(SeekFrom::Start(tail_start))?;
    let mut tail = Vec::new();
    file.take(size - tail_start).read_to_end(&mut tail)?;

    if tail_start > 0 && (tail_start > head_size || !head.ends_with(b"\n")) {
        tail = match tail.iter().position(|byte| *byte == b'\n') {
            Some(newline) => tail.split_off(newline + 1),
            None => Vec::new(),
        };
    }
    let mut windows = vec![head];
    if !tail.is_empty() {
        windows.push(tail);
    }
    Ok(windows)
}

fn open_regular(path: &Path, limit: u64) -> io::Result<(File, Metadata)> {
    let before = fs::symlink_metadata(path)?;
    if !before.file_type().is_file() {
        return Err(invalid("not a regular file"));
    }
    if before.len() > limit {
        return Err(invalid("file is larger than its read limit"));
    }
    let file = File::open(path)?;
    let after = file.metadata()?;
    if !after.is_file() || !platform::same_file(&before, &after) {
        return Err(invalid("file changed while it was being opened"));
    }
    Ok((file, before))
}

/// Read at most `size` bytes, failing if the file grew past its checked size.
fn read_to_size(file: File, size: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    file.take(size + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > size {
        return Err(invalid("file grew while it was being read"));
    }
    Ok(bytes)
}

pub(super) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
