//! A cheap signal that nothing a sync looks at has changed.
//!
//! The background job runs every 30 seconds, and a full run reads around a
//! hundred megabytes of Desktop metadata. A directory's modification time
//! changes whenever an entry is added, removed, or renamed in it, which covers
//! a new partition, Desktop rewriting a wrapper through a rename, and a new
//! transcript. The digest of those times is all that is stored, so the
//! fingerprint file holds no names.
//!
//! A message appended to an existing transcript does not change any of these
//! times; the five-minute ceiling on skipped runs covers that case.

use super::bounded;
use super::desktop::sha256_hex;
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The fingerprint a full run started from, and when that run happened.
#[derive(Serialize, Deserialize)]
pub(super) struct StoredFingerprint {
    pub(super) digest: String,
    /// Unix seconds.
    pub(super) full_run_at: i64,
}

/// Digest the partition directories, the project directories, and their times.
pub(super) fn compute(desktop_roots: &[PathBuf], projects_root: &Path) -> String {
    let mut lines = String::new();
    for root in desktop_roots {
        let sessions_root: PathBuf = root.join("claude-code-sessions");
        describe(&mut lines, "S", &sessions_root, &root.to_string_lossy());
        for account in listing(&mut lines, &sessions_root) {
            let account_name = name(&account);
            describe(&mut lines, "A", &account, &account_name);
            for partition in listing(&mut lines, &account) {
                let label = format!("{account_name}/{}", name(&partition));
                describe(&mut lines, "P", &partition, &label);
            }
        }
    }
    describe(&mut lines, "R", projects_root, "");
    for project in listing(&mut lines, projects_root) {
        describe(&mut lines, "D", &project, &name(&project));
    }
    sha256_hex(lines.as_bytes())
}

/// The real subdirectories of `dir`, noting a failed listing in the digest.
fn listing(lines: &mut String, dir: &Path) -> Vec<std::path::PathBuf> {
    if !bounded::is_real_dir(dir) {
        return Vec::new();
    }
    let Ok(entries) = bounded::entries(dir) else {
        lines.push_str("E\n");
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|path| bounded::is_real_dir(path))
        .collect()
}

fn describe(lines: &mut String, kind: &str, path: &Path, label: &str) {
    let nanos = fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| metadata.file_type().is_dir())
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos());
    let _ = match nanos {
        Some(nanos) => writeln!(lines, "{kind} {label} {nanos}"),
        None => writeln!(lines, "{kind} {label} missing"),
    };
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
