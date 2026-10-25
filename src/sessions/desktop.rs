//! Claude Desktop's per-account session lists, read without changing them.
//!
//! Desktop keeps one directory per account and organization under
//! `claude-code-sessions/<accountUuid>/<orgUuid>/`, called a partition here.
//! Each `local_<uuid>.json` in it is a wrapper: a list entry pointing at a
//! Claude Code transcript through `cliSessionId`. A `deleted_<id>` file marks a
//! conversation the user deleted, and it holds for the whole account.

use super::bounded::{self, ReadBudget};
use super::ids::{SessionId, WrapperId};
use super::skips::{Skip, Skips};
use super::timestamp;
use super::transcripts;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// One account and organization's session list directory.
pub(super) struct Partition {
    pub(super) desktop_root: PathBuf,
    /// Empty for the primary root, preserving existing ledger ownership.
    pub(super) ledger_prefix: String,
    pub(super) account: SessionId,
    pub(super) org: SessionId,
    pub(super) dir: PathBuf,
}

impl Partition {
    /// The path of an entry relative to `claude-code-sessions`, as the ledger records it.
    pub(super) fn relative_path(&self, wrapper: &WrapperId) -> String {
        format!("{}/{}/{}", self.account, self.org, wrapper.file_name())
    }

    /// Ownership key unique to this root, account, organization, and wrapper.
    pub(super) fn ledger_key(&self, wrapper: &WrapperId) -> String {
        format!("{}{}", self.ledger_prefix, self.relative_path(wrapper))
    }
}

/// The fields of a wrapper this tool reads.
///
/// Desktop owns the schema, so a field of an unexpected type reads as absent
/// rather than failing the file, the way the earlier Python tool treated it.
pub(super) struct WrapperFields {
    pub(super) cli_session_id: Option<SessionId>,
    pub(super) prior_cli_session_ids: BTreeSet<SessionId>,
    /// Every transcript id the wrapper names, current or earlier.
    pub(super) transcript_ids: BTreeSet<SessionId>,
    pub(super) last_activity_at: Option<i64>,
    pub(super) archived_or_hidden: bool,
    pub(super) remote: bool,
}

impl WrapperFields {
    fn read(payload: &Map<String, Value>) -> Self {
        let cli_session_id = SessionId::from_value(payload.get("cliSessionId"));
        let prior_cli_session_ids: BTreeSet<SessionId> = payload
            .get("priorCliSessionIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|value| SessionId::from_value(Some(value)))
            .collect();
        let mut transcript_ids = prior_cli_session_ids.clone();
        for key in [
            "cliSessionId",
            "unarchivedCliSessionId",
            "preClearCliSessionId",
            "historyOnlyCliSessionId",
        ] {
            transcript_ids.extend(SessionId::from_value(payload.get(key)));
        }
        let flag = |key: &str| payload.get(key) == Some(&Value::Bool(true));
        Self {
            cli_session_id,
            prior_cli_session_ids,
            transcript_ids,
            last_activity_at: timestamp::milliseconds(payload.get("lastActivityAt")),
            archived_or_hidden: flag("isArchived") || flag("isHidden"),
            remote: transcripts::is_remote(payload),
        }
    }
}

/// One wrapper file in one partition.
pub(super) struct WrapperCopy {
    pub(super) wrapper: WrapperId,
    /// Index into [`DesktopScan::partitions`].
    pub(super) partition: usize,
    /// The whole wrapper, unknown fields included.
    pub(super) payload: Map<String, Value>,
    pub(super) fields: WrapperFields,
    /// SHA-256 of the bytes as read, which decides whether this tool owns them.
    pub(super) sha256: String,
    pub(super) modified_ms: i64,
}

/// Everything read from `claude-code-sessions`.
pub(super) struct DesktopScan {
    pub(super) partitions: Vec<Partition>,
    pub(super) copies: Vec<WrapperCopy>,
    /// Partitions that must not be written: their listing or a wrapper failed.
    pub(super) blocked: BTreeSet<usize>,
    /// `deleted_*` marker names, without the prefix, per account.
    pub(super) tombstones: BTreeMap<SessionId, BTreeSet<String>>,
    /// False when the account or organization listing failed, so a partition
    /// may be missing and with it the entries that name some transcripts.
    pub(super) complete: bool,
}

impl DesktopScan {
    /// Every transcript id any wrapper names.
    pub(super) fn known_transcript_ids(&self) -> BTreeSet<SessionId> {
        self.copies
            .iter()
            .flat_map(|copy| copy.fields.transcript_ids.iter().cloned())
            .collect()
    }
}

/// Find the partitions and read every wrapper and marker in them.
pub(super) fn scan(
    desktop_roots: &[PathBuf],
    budget: &mut ReadBudget,
    skips: &mut Skips,
) -> DesktopScan {
    let mut partitions: Vec<Partition> = Vec::new();
    let mut complete: bool = true;
    for (index, root) in desktop_roots.iter().enumerate() {
        let (found, finished): (Vec<Partition>, bool) = find_partitions(root, index == 0, skips);
        partitions.extend(found);
        complete &= finished;
    }
    let mut scan = DesktopScan {
        partitions,
        copies: Vec::new(),
        blocked: BTreeSet::new(),
        tombstones: BTreeMap::new(),
        complete,
    };
    for index in 0..scan.partitions.len() {
        let account = scan.partitions[index].account.clone();
        scan.tombstones.entry(account.clone()).or_default();
        let Ok(files) = bounded::entries(&scan.partitions[index].dir) else {
            // The markers of this account are unknown now, so no partition of
            // the account may receive an entry the user may have deleted.
            skips.add(Skip::InventoryIncomplete);
            let same_account = scan.partitions.iter().enumerate();
            let blocked: Vec<usize> = same_account
                .filter(|(_, partition)| partition.account == account)
                .map(|(other, _)| other)
                .collect();
            scan.blocked.extend(blocked);
            continue;
        };
        for path in files {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(marker) = tombstone_marker(name) {
                scan.tombstones.entry(account.clone()).or_default().insert(marker);
                continue;
            }
            let Some(wrapper) = name.strip_suffix(".json").and_then(WrapperId::parse_stem) else {
                continue;
            };
            if let Some(copy) = read_copy(&path, &wrapper, index, budget) {
                scan.copies.push(copy);
            } else {
                skips.add(Skip::MetadataUnreadable);
                scan.blocked.insert(index);
            }
        }
    }
    scan
}

/// Real `<uuid>/<uuid>` directories under the root, and whether listing finished.
fn find_partitions(
    desktop_root: &Path,
    primary: bool,
    skips: &mut Skips,
) -> (Vec<Partition>, bool) {
    let root: PathBuf = desktop_root.join("claude-code-sessions");
    let mut partitions = Vec::new();
    let parent_ok = root.parent().is_some_and(bounded::is_real_dir);
    if !parent_ok || !bounded::is_real_dir(&root) {
        return (partitions, true);
    }
    let Ok(accounts) = bounded::entries(&root) else {
        skips.add(Skip::InventoryIncomplete);
        return (partitions, false);
    };
    for account_dir in accounts.iter().filter(|path| bounded::is_real_dir(path)) {
        let Some(account) = uuid_name(account_dir) else {
            continue;
        };
        let Ok(orgs) = bounded::entries(account_dir) else {
            skips.add(Skip::InventoryIncomplete);
            return (partitions, false);
        };
        for org_dir in orgs.into_iter().filter(|path| bounded::is_real_dir(path)) {
            if let Some(org) = uuid_name(&org_dir) {
                partitions.push(Partition {
                    desktop_root: desktop_root.to_path_buf(),
                    ledger_prefix: if primary {
                        String::new()
                    } else {
                        format!(
                            "profiles/{}/",
                            sha256_hex(desktop_root.as_os_str().as_encoded_bytes())
                        )
                    },
                    account: account.clone(),
                    org,
                    dir: org_dir,
                });
            }
        }
    }
    (partitions, true)
}

/// Read one wrapper; `None` when it is unreadable or names another session.
fn read_copy(
    path: &Path,
    wrapper: &WrapperId,
    partition: usize,
    budget: &mut ReadBudget,
) -> Option<WrapperCopy> {
    let file = bounded::read_metadata(path, budget).ok()?;
    let payload: Map<String, Value> = serde_json::from_slice(&file.bytes).ok()?;
    if payload.get("sessionId").and_then(Value::as_str) != Some(wrapper.to_string().as_str()) {
        return None;
    }
    Some(WrapperCopy {
        wrapper: wrapper.clone(),
        partition,
        fields: WrapperFields::read(&payload),
        payload,
        sha256: sha256_hex(&file.bytes),
        modified_ms: transcripts::modified_ms(&file.metadata),
    })
}

/// The marker name of a `deleted_<uuid>` or `deleted_local_<uuid>` file.
pub(super) fn tombstone_marker(name: &str) -> Option<String> {
    let marker = name.strip_prefix("deleted_")?;
    SessionId::parse(marker.strip_prefix("local_").unwrap_or(marker))?;
    Some(marker.to_string())
}

fn uuid_name(path: &Path) -> Option<SessionId> {
    SessionId::parse(path.file_name()?.to_str()?)
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}
