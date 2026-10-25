//! The record of every Desktop file this tool wrote, refreshed, or removed.
//!
//! Ownership is the reason the ledger exists: this tool may replace or delete
//! a Desktop wrapper only when the file's current SHA-256 equals the latest
//! record for its path. Once Desktop rewrites a file, the hashes differ and
//! the file is Desktop's again. The ledger is append-only JSON lines, so a
//! crash can at most lose the last line, which only shrinks ownership.
//!
//! The earlier Python tool kept `created-records.jsonl` with the same paths
//! and hashes. When this ledger does not exist yet, those records are imported
//! once as `created`; the Python file itself is never changed.

use super::bounded;
use super::platform;
use super::staging::StagedFile;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::Path;

/// A ledger larger than this is refused rather than read without a bound.
const MAX_LEDGER_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LedgerAction {
    Created,
    Refreshed,
    Removed,
}

/// One line of `ledger.jsonl`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct LedgerRecord {
    /// RFC 3339 UTC time of the write.
    pub(super) at: String,
    pub(super) action: LedgerAction,
    /// `<accountUuid>/<orgUuid>/local_<uuid>.json` for the primary root;
    /// additional roots prefix that path with `profiles/<root SHA-256>/`.
    pub(super) relative_path: String,
    pub(super) cli_session_id: String,
    /// SHA-256 of the bytes written, or of the bytes removed.
    pub(super) sha256: String,
}

/// One line of the Python tool's `created-records.jsonl`.
#[derive(Deserialize)]
struct LegacyRecord {
    created_at: String,
    relative_path: String,
    cli_session_id: String,
    sha256: String,
}

/// The latest record per path, which is all ownership needs.
#[derive(Default)]
pub(super) struct Ledger {
    latest: HashMap<String, (LedgerAction, String)>,
    /// Legacy records read but not yet written as this ledger.
    pending_import: Vec<LedgerRecord>,
}

impl Ledger {
    /// Whether this tool owns the file at `relative_path` with these contents.
    pub(super) fn owns(&self, relative_path: &str, sha256: &str) -> bool {
        matches!(
            self.latest.get(relative_path),
            Some((LedgerAction::Created | LedgerAction::Refreshed, recorded)) if recorded == sha256
        )
    }

    /// Apply a record to the in-memory view after it was appended.
    pub(super) fn apply(&mut self, record: &LedgerRecord) {
        self.latest.insert(
            record.relative_path.clone(),
            (record.action, record.sha256.clone()),
        );
    }
}

/// Read the ledger, or the legacy records when the ledger does not exist yet.
///
/// Reading never writes; a legacy import stays pending until
/// [`persist_import`]. A line that does not parse is skipped, which can only
/// make this tool own fewer files.
pub(super) fn load(ledger_path: &Path, legacy_path: &Path) -> Result<Ledger> {
    let mut ledger = Ledger::default();
    if let Some(bytes) = read_optional(ledger_path)? {
        for record in parse_lines::<LedgerRecord>(&bytes) {
            ledger.apply(&record);
        }
        return Ok(ledger);
    }
    if let Some(bytes) = read_optional(legacy_path)? {
        for legacy in parse_lines::<LegacyRecord>(&bytes) {
            let record = LedgerRecord {
                at: legacy.created_at,
                action: LedgerAction::Created,
                relative_path: legacy.relative_path,
                cli_session_id: legacy.cli_session_id,
                sha256: legacy.sha256,
            };
            ledger.apply(&record);
            ledger.pending_import.push(record);
        }
    }
    Ok(ledger)
}

/// Write pending legacy records as the new ledger, once and in one step.
///
/// The import is linked into place without replacing, so a ledger that
/// appeared in the meantime is never overwritten.
pub(super) fn persist_import(
    ledger: &mut Ledger,
    ledger_path: &Path,
    state_dir: &Path,
) -> Result<()> {
    if ledger.pending_import.is_empty() {
        return Ok(());
    }
    let mut text = String::new();
    for record in &ledger.pending_import {
        text.push_str(&serde_json::to_string(record)?);
        text.push('\n');
    }
    let staged = StagedFile::write(state_dir, text.as_bytes()).with_context(|| {
        format!(
            "failed to stage the ledger import in {}",
            state_dir.display()
        )
    })?;
    match staged.link_new(ledger_path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            bail!(
                "{} appeared while importing the earlier tool's records; run sync again",
                ledger_path.display()
            )
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to write {}", ledger_path.display()));
        }
    }
    drop(staged);
    platform::sync_dir(state_dir).ok();
    ledger.pending_import.clear();
    Ok(())
}

/// Append one record and flush it, refusing a ledger that is not a regular file.
pub(super) fn append(ledger_path: &Path, record: &LedgerRecord) -> io::Result<()> {
    let before = match fs::symlink_metadata(ledger_path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(bounded::invalid("the ledger is not a regular file"));
        }
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut file = platform::open_private_append(ledger_path)?;
    if let Some(before) = before
        && !platform::same_file(&before, &file.metadata()?)
    {
        return Err(bounded::invalid(
            "the ledger changed while it was being opened",
        ));
    }
    let mut line = serde_json::to_string(record).map_err(io::Error::other)?;
    line.push('\n');
    file.write_all(line.as_bytes())?;
    file.sync_data()
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match bounded::read_regular(path, MAX_LEDGER_BYTES) {
        Ok(file) => Ok(Some(file.bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn parse_lines<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> impl Iterator<Item = T> + '_ {
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
}
