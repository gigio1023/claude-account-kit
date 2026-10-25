//! Every write this tool makes inside Claude Desktop's data.
//!
//! Desktop does not share this tool's lock, so each write re-checks what it
//! depends on immediately before acting. A new wrapper is published with a
//! hard link, which fails on an existing name even if Desktop created it a
//! moment ago. A refresh or removal re-hashes the file first and goes ahead
//! only if it still holds the bytes this tool recorded. Every completed write
//! is appended to the ledger, and only a counted outcome leaves this module,
//! never a title, path, or payload.

use super::bounded::{self, MAX_METADATA};
use super::desktop::sha256_hex;
use super::ids::{SessionId, WrapperId};
use super::ledger::{self, Ledger, LedgerAction, LedgerRecord};
use super::plan::{Addition, Refresh, Removal, Target};
use super::platform;
use super::staging::StagedFile;
use super::timestamp;
use crate::time::now_epoch;
use serde_json::{Map, Value};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// What happened to one planned change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Created,
    Refreshed,
    Removed,
    /// The write happened, but its ledger line could not be appended.
    Unlogged(LedgerAction),
    /// The target name was taken after the scan, so nothing was written.
    AlreadyExists,
    /// The file to remove was already gone.
    AlreadyGone,
    /// A `deleted_*` marker for the conversation appeared after the scan.
    DeletedSinceScan,
    /// The transcript the entry would point at is gone.
    TranscriptDisappeared,
    /// The file no longer has the bytes this tool wrote; it was left alone.
    ChangedSinceScan,
    WriteError,
}

impl Outcome {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Refreshed => "refreshed",
            Self::Removed => "removed",
            Self::Unlogged(LedgerAction::Created) => "created_unlogged",
            Self::Unlogged(LedgerAction::Refreshed) => "refreshed_unlogged",
            Self::Unlogged(LedgerAction::Removed) => "removed_unlogged",
            Self::AlreadyExists => "already_exists",
            Self::AlreadyGone => "already_gone",
            Self::DeletedSinceScan => "deleted_since_scan",
            Self::TranscriptDisappeared => "transcript_disappeared",
            Self::ChangedSinceScan => "changed_since_scan",
            Self::WriteError => "write_error",
        }
    }

    /// The ledger action this outcome completed, logged or not.
    pub(super) fn completed(self) -> Option<LedgerAction> {
        match self {
            Self::Created => Some(LedgerAction::Created),
            Self::Refreshed => Some(LedgerAction::Refreshed),
            Self::Removed => Some(LedgerAction::Removed),
            Self::Unlogged(action) => Some(action),
            _ => None,
        }
    }

    pub(super) fn is_error(self) -> bool {
        matches!(self, Self::Unlogged(_) | Self::WriteError)
    }
}

/// Writes into `claude-code-sessions`, each recorded in the ledger.
pub(super) struct Publisher<'a> {
    pub(super) desktop_roots: &'a [PathBuf],
    pub(super) ledger_path: &'a Path,
    pub(super) ledger: &'a mut Ledger,
}

impl Publisher<'_> {
    /// Publish a new wrapper without replacing anything at its name.
    pub(super) fn add(&mut self, addition: &Addition) -> Outcome {
        self.try_add(addition).unwrap_or(Outcome::WriteError)
    }

    /// Replace a tool-owned copy with the canonical payload.
    pub(super) fn refresh(&mut self, refresh: &Refresh) -> Outcome {
        self.try_refresh(refresh).unwrap_or(Outcome::WriteError)
    }

    /// Delete a tool-owned copy of a conversation whose transcripts are gone.
    pub(super) fn remove(&mut self, removal: &Removal) -> Outcome {
        self.try_remove(removal).unwrap_or(Outcome::WriteError)
    }

    fn try_add(&mut self, addition: &Addition) -> io::Result<Outcome> {
        let target = &addition.target;
        let partition = self.check_target(target)?;
        if fs::symlink_metadata(&target.path).is_ok() {
            return Ok(Outcome::AlreadyExists);
        }
        if deleted_in_account(target, self.desktop_roots) {
            return Ok(Outcome::DeletedSinceScan);
        }
        if !is_regular_file(&addition.transcript) {
            return Ok(Outcome::TranscriptDisappeared);
        }
        let bytes = encode(&addition.payload)?;
        let staged = StagedFile::write(partition, &bytes)?;
        if deleted_in_account(target, self.desktop_roots) {
            return Ok(Outcome::DeletedSinceScan);
        }
        match staged.link_new(&target.path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                return Ok(Outcome::AlreadyExists);
            }
            Err(error) => return Err(error),
        }
        drop(staged);
        platform::sync_dir(partition).ok();
        Ok(self.log(LedgerAction::Created, target, &addition.cli, &bytes))
    }

    fn try_refresh(&mut self, refresh: &Refresh) -> io::Result<Outcome> {
        let target = &refresh.target;
        let partition = self.check_target(target)?;
        if deleted_in_account(target, self.desktop_roots) {
            return Ok(Outcome::DeletedSinceScan);
        }
        if !is_regular_file(&refresh.transcript) {
            return Ok(Outcome::TranscriptDisappeared);
        }
        let bytes = encode(&refresh.payload)?;
        let staged = StagedFile::write(partition, &bytes)?;
        // Hash as late as possible, so Desktop has the least time to rewrite
        // the file between the check and the rename. A file Desktop deleted
        // in the meantime has changed too, and is not recreated here.
        match still_holds(&target.path, &refresh.expected_sha256) {
            Ok(true) => {}
            Ok(false) => return Ok(Outcome::ChangedSinceScan),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Outcome::ChangedSinceScan);
            }
            Err(error) => return Err(error),
        }
        staged.replace(&target.path)?;
        platform::sync_dir(partition).ok();
        Ok(self.log(LedgerAction::Refreshed, target, &refresh.cli, &bytes))
    }

    fn try_remove(&mut self, removal: &Removal) -> io::Result<Outcome> {
        let target = &removal.target;
        let partition = self.check_target(target)?;
        let outcome = match still_holds(&target.path, &removal.expected_sha256) {
            Ok(false) => return Ok(Outcome::ChangedSinceScan),
            Ok(true) => match fs::remove_file(&target.path) {
                Ok(()) => Outcome::Removed,
                Err(error) if error.kind() == ErrorKind::NotFound => Outcome::AlreadyGone,
                Err(error) => return Err(error),
            },
            Err(error) if error.kind() == ErrorKind::NotFound => Outcome::AlreadyGone,
            Err(error) => return Err(error),
        };
        platform::sync_dir(partition).ok();
        // Ownership ends either way; the record says so for a later run.
        let record = LedgerRecord {
            at: timestamp::format_utc(now_epoch()),
            action: LedgerAction::Removed,
            relative_path: target.ledger_key.clone(),
            cli_session_id: removal.cli.clone(),
            sha256: removal.expected_sha256.clone(),
        };
        if ledger::append(self.ledger_path, &record).is_err() {
            return Ok(if outcome == Outcome::Removed {
                Outcome::Unlogged(LedgerAction::Removed)
            } else {
                outcome
            });
        }
        self.ledger.apply(&record);
        Ok(outcome)
    }

    /// Confirm the target is a wrapper path inside real Desktop directories,
    /// and return its partition directory.
    fn check_target<'t>(&self, target: &'t Target) -> io::Result<&'t Path> {
        let parts: Vec<&str> = target.relative.split('/').collect();
        let well_formed = match parts.as_slice() {
            [account, org, name] => {
                SessionId::parse(account).is_some()
                    && SessionId::parse(org).is_some()
                    && name
                        .strip_suffix(".json")
                        .and_then(WrapperId::parse_stem)
                        .is_some()
            }
            _ => false,
        };
        let sessions_root: PathBuf = target.desktop_root.join("claude-code-sessions");
        if !well_formed
            || !self.desktop_roots.contains(&target.desktop_root)
            || target.path != sessions_root.join(&target.relative)
        {
            return Err(bounded::invalid("unexpected Desktop session path"));
        }
        let partition = target
            .path
            .parent()
            .ok_or_else(|| bounded::invalid("Desktop session path has no partition"))?;
        let account = partition
            .parent()
            .ok_or_else(|| bounded::invalid("Desktop session path has no account"))?;
        let real = [
            target.desktop_root.as_path(),
            sessions_root.as_path(),
            account,
            partition,
        ]
        .into_iter()
        .all(bounded::is_real_dir);
        if !real {
            return Err(bounded::invalid(
                "Desktop session directories must be real directories",
            ));
        }
        Ok(partition)
    }

    fn log(
        &mut self,
        action: LedgerAction,
        target: &Target,
        cli: &SessionId,
        bytes: &[u8],
    ) -> Outcome {
        let record = LedgerRecord {
            at: timestamp::format_utc(now_epoch()),
            action,
            relative_path: target.ledger_key.clone(),
            cli_session_id: cli.to_string(),
            sha256: sha256_hex(bytes),
        };
        if ledger::append(self.ledger_path, &record).is_err() {
            return Outcome::Unlogged(action);
        }
        self.ledger.apply(&record);
        match action {
            LedgerAction::Created => Outcome::Created,
            LedgerAction::Refreshed => Outcome::Refreshed,
            LedgerAction::Removed => Outcome::Removed,
        }
    }
}

/// Whether any partition of the target's account holds a `deleted_*` marker
/// for the conversation. An unreadable account directory counts as yes.
fn deleted_in_account(target: &Target, desktop_roots: &[PathBuf]) -> bool {
    let Some(account) = target.relative.split('/').next() else {
        return true;
    };
    desktop_roots.iter().any(|root| {
        let account_dir: PathBuf = root.join("claude-code-sessions").join(account);
        if fs::symlink_metadata(&account_dir)
            .is_err_and(|error| error.kind() == ErrorKind::NotFound)
        {
            return false;
        }
        if ![
            root.as_path(),
            root.join("claude-code-sessions").as_path(),
            account_dir.as_path(),
        ]
        .into_iter()
        .all(bounded::is_real_dir)
        {
            return true;
        }
        let Ok(partitions) = bounded::entries(&account_dir) else {
            return true;
        };
        partitions
            .iter()
            .filter(|partition| bounded::is_real_dir(partition))
            .any(|partition| {
                target.tombstone_names.iter().any(|name| {
                    fs::symlink_metadata(partition.join(format!("deleted_{name}"))).is_ok()
                })
            })
        })
}

/// Whether the file still has the recorded bytes, read without following a symlink.
fn still_holds(path: &Path, expected_sha256: &str) -> io::Result<bool> {
    let current = bounded::read_regular(path, MAX_METADATA)?;
    Ok(sha256_hex(&current.bytes) == expected_sha256)
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn encode(payload: &Map<String, Value>) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(payload).map_err(io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}
