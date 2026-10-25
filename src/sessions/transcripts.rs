//! Claude Code transcripts: which exist, and what a CLI-only one says about itself.
//!
//! Only direct `projects/<dir>/<uuid>.jsonl` files count. Metadata comes from
//! the project's optional `sessions-index.json`, or else from fixed head and
//! tail windows of the transcript, read through the top-level key scanner so no
//! message or content value is ever decoded.

use super::bounded::{self, ReadBudget};
use super::ids::SessionId;
use super::json_scan;
use super::skips::{Skip, Skips};
use super::timestamp;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Transcript fields read from a JSONL record.
const RECORD_KEYS: [&str; 13] = [
    "cwd",
    "timestamp",
    "gitBranch",
    "entrypoint",
    "isSidechain",
    "sessionId",
    "type",
    "customTitle",
    "aiTitle",
    "movedToCloud",
    "cloudSessionId",
    "remoteSessionId",
    "connectionType",
];
/// Fields read from a `sessions-index.json` entry: the record fields plus these.
const INDEX_EXTRA_KEYS: [&str; 4] = ["projectPath", "summary", "created", "modified"];

/// Every transcript found under the projects root.
pub(super) struct TranscriptInventory {
    /// Regular transcript files by session id.
    pub(super) files: BTreeMap<SessionId, Vec<PathBuf>>,
    /// Every id with a transcript name, including remote mirrors and
    /// non-regular files. An id here is never treated as deleted.
    pub(super) seen: BTreeSet<SessionId>,
    /// False when a listing failed, so an absent id may still exist.
    pub(super) complete: bool,
}

/// What a transcript without a Desktop entry says about itself.
#[derive(Default)]
pub(super) struct CliMetadata {
    pub(super) project_path: Option<String>,
    pub(super) custom_title: Option<String>,
    pub(super) summary: Option<String>,
    pub(super) git_branch: Option<String>,
    pub(super) created: Option<i64>,
    pub(super) modified: Option<i64>,
    pub(super) sidechain: bool,
    /// A record in the transcript names a different session.
    pub(super) mismatched: bool,
    pub(super) remote: bool,
    /// The client that started the session, from the first record naming one.
    pub(super) entrypoint: Option<String>,
}

impl CliMetadata {
    /// Whether Claude Desktop started the session. Desktop then keeps its own
    /// entry for it, which may only lag behind the transcript.
    pub(super) fn desktop_started(&self) -> bool {
        self.entrypoint.as_deref() == Some("claude-desktop")
    }
}

/// A CLI-only transcript with its metadata.
pub(super) struct CliTranscript {
    pub(super) id: SessionId,
    pub(super) path: PathBuf,
    pub(super) metadata: CliMetadata,
    /// The file's modification time, used where the metadata has no times.
    pub(super) modified_ms: i64,
}

/// List `projects/<dir>/<uuid>.jsonl`, skipping `ssh-*` remote mirrors.
pub(super) fn inventory(root: &Path, skips: &mut Skips) -> TranscriptInventory {
    let mut inventory = TranscriptInventory {
        files: BTreeMap::new(),
        seen: BTreeSet::new(),
        complete: bounded::is_real_dir(root),
    };
    if !inventory.complete {
        return inventory;
    }
    let Ok(projects) = bounded::entries(root) else {
        skips.add(Skip::InventoryIncomplete);
        inventory.complete = false;
        return inventory;
    };
    for project in projects.iter().filter(|path| bounded::is_real_dir(path)) {
        let remote = file_name(project).starts_with("ssh-");
        if remote {
            skips.add(Skip::RemoteProject);
        }
        let Ok(files) = bounded::entries(project) else {
            skips.add(Skip::InventoryIncomplete);
            inventory.complete = false;
            continue;
        };
        for path in files {
            let Some(id) = transcript_id(&path) else {
                continue;
            };
            inventory.seen.insert(id.clone());
            if remote {
                continue;
            }
            if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file()) {
                inventory.files.entry(id).or_default().push(path);
            } else {
                skips.add(Skip::NonregularTranscript);
            }
        }
    }
    inventory
}

/// Read metadata for the transcripts no Desktop entry refers to.
///
/// `known` holds every transcript id that any Desktop entry names, current or
/// prior. Those are listed through their entry, never as a second row.
pub(super) fn read_cli_only(
    inventory: &TranscriptInventory,
    known: &BTreeSet<SessionId>,
    budget: &mut ReadBudget,
    skips: &mut Skips,
) -> Vec<CliTranscript> {
    let mut project_indexes: HashMap<PathBuf, BTreeMap<SessionId, CliMetadata>> = HashMap::new();
    let mut result = Vec::new();
    for (id, paths) in &inventory.files {
        if known.contains(id) {
            continue;
        }
        let [path] = paths.as_slice() else {
            skips.add(Skip::AmbiguousTranscript);
            continue;
        };
        let Some(project) = path.parent() else {
            continue;
        };
        let index = project_indexes.entry(project.to_path_buf()).or_insert_with(|| {
            read_index(project, budget).unwrap_or_else(|| {
                skips.add(Skip::IndexUnreadable);
                BTreeMap::new()
            })
        });
        let indexed = index.remove(id).unwrap_or_default();
        let Ok(stat) = fs::symlink_metadata(path) else {
            skips.add(Skip::TranscriptUnreadable);
            continue;
        };
        let modified_ms = modified_ms(&stat);
        let metadata = if indexed.project_path.as_deref().is_some_and(is_existing_dir) {
            indexed
        } else {
            let Ok(windows) = bounded::read_windows(path, budget) else {
                skips.add(Skip::TranscriptUnreadable);
                continue;
            };
            overlay(indexed, scan_windows(&windows, id, modified_ms))
        };
        result.push(CliTranscript {
            id: id.clone(),
            path: path.clone(),
            metadata,
            modified_ms,
        });
    }
    result
}

/// Whether a JSON field set marks a session that ran away from this machine.
///
/// Explicit SSH, WSL, and cloud transports count; so does an entrypoint or
/// connection type that names one.
pub(super) fn is_remote(fields: &Map<String, Value>) -> bool {
    let flagged = [
        "sshConfig",
        "wslConfig",
        "cloudSessionId",
        "remoteSessionId",
        "movedToCloud",
    ]
    .iter()
    .any(|key| fields.get(*key).is_some_and(truthy));
    let named = ["entrypoint", "connectionType"].iter().any(|key| {
        fields.get(*key).and_then(Value::as_str).is_some_and(|value| {
            let value = value.to_lowercase();
            ["ssh", "remote", "cloud", "wsl"]
                .iter()
                .any(|marker| value.contains(marker))
        })
    });
    flagged || named
}

/// Whether a path is an absolute, existing directory, following symlinks.
pub(super) fn is_existing_dir(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('\0')
        && Path::new(value).is_absolute()
        && Path::new(value).is_dir()
}

/// Read the index entries of one project, keeping no first-prompt text.
///
/// `None` means the index exists but could not be read; a missing index is
/// an empty one.
fn read_index(project: &Path, budget: &mut ReadBudget) -> Option<BTreeMap<SessionId, CliMetadata>> {
    let path = project.join("sessions-index.json");
    if fs::symlink_metadata(&path).is_err() {
        return Some(BTreeMap::new());
    }
    let file = bounded::read_metadata(&path, budget).ok()?;
    let mut entries = BTreeMap::new();
    let Some(raw) = json_scan::raw_field(&file.bytes, "entries") else {
        return Some(entries);
    };
    let keys: Vec<&str> = RECORD_KEYS.iter().chain(&INDEX_EXTRA_KEYS).copied().collect();
    for item in json_scan::array_items(raw) {
        let fields = json_scan::scalar_fields(item, &keys);
        if let Some(id) = SessionId::from_value(fields.get("sessionId")) {
            entries.insert(id, index_metadata(&fields));
        }
    }
    Some(entries)
}

fn index_metadata(fields: &Map<String, Value>) -> CliMetadata {
    CliMetadata {
        project_path: string(fields, "projectPath"),
        custom_title: string(fields, "customTitle").filter(|title| !title.is_empty()),
        summary: string(fields, "summary"),
        git_branch: string(fields, "gitBranch"),
        created: timestamp::milliseconds(fields.get("created")),
        modified: timestamp::milliseconds(fields.get("modified")),
        sidechain: fields.get("isSidechain") == Some(&Value::Bool(true)),
        mismatched: false,
        remote: is_remote(fields),
        entrypoint: string(fields, "entrypoint"),
    }
}

/// Collect metadata from the records in a transcript's read windows.
///
/// The first `cwd` wins, the last title and branch win, and the creation and
/// activity times are the earliest and latest record timestamps.
fn scan_windows(windows: &[Vec<u8>], expected: &SessionId, modified_ms: i64) -> CliMetadata {
    let mut metadata = CliMetadata::default();
    let mut times = Vec::new();
    for line in windows.iter().flat_map(|window| window.split(|byte| *byte == b'\n')) {
        let record = json_scan::scalar_fields(line, &RECORD_KEYS);
        if let Some(time) = timestamp::milliseconds(record.get("timestamp")) {
            times.push(time);
        }
        merge_record(&mut metadata, &record, expected);
    }
    metadata.created = Some(times.iter().copied().min().unwrap_or(modified_ms));
    metadata.modified = Some(times.iter().copied().max().unwrap_or(modified_ms));
    metadata
}

fn merge_record(metadata: &mut CliMetadata, record: &Map<String, Value>, expected: &SessionId) {
    if record.get("isSidechain") == Some(&Value::Bool(true)) {
        metadata.sidechain = true;
    }
    if is_remote(record) {
        metadata.remote = true;
    }
    if metadata.entrypoint.is_none() {
        metadata.entrypoint = string(record, "entrypoint");
    }
    if let Some(Value::String(id)) = record.get("sessionId")
        && id != expected.as_str()
    {
        metadata.mismatched = true;
        return;
    }
    if metadata.project_path.is_none() {
        metadata.project_path = string(record, "cwd").filter(|cwd| !cwd.is_empty());
    }
    let title_key = match record.get("type").and_then(Value::as_str) {
        Some("custom-title") => "customTitle",
        Some("ai-title") => "aiTitle",
        _ => "",
    };
    if let Some(title) = string(record, title_key).filter(|title| !title.is_empty()) {
        metadata.custom_title = Some(title);
    }
    if let Some(branch) = string(record, "gitBranch") {
        metadata.git_branch = Some(branch);
    }
}

/// Lay what the transcript says over what the index says.
fn overlay(mut indexed: CliMetadata, scanned: CliMetadata) -> CliMetadata {
    indexed.sidechain |= scanned.sidechain;
    indexed.mismatched |= scanned.mismatched;
    indexed.remote |= scanned.remote;
    if scanned.entrypoint.is_some() {
        indexed.entrypoint = scanned.entrypoint;
    }
    if scanned.project_path.is_some() {
        indexed.project_path = scanned.project_path;
    }
    if scanned.custom_title.is_some() {
        indexed.custom_title = scanned.custom_title;
    }
    if scanned.git_branch.is_some() {
        indexed.git_branch = scanned.git_branch;
    }
    indexed.created = scanned.created;
    indexed.modified = scanned.modified;
    indexed
}

fn transcript_id(path: &Path) -> Option<SessionId> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
        return None;
    }
    SessionId::parse(path.file_stem()?.to_str()?)
}

fn file_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
}

fn string(fields: &Map<String, Value>, key: &str) -> Option<String> {
    fields.get(key).and_then(Value::as_str).map(str::to_string)
}

/// JSON truthiness as the earlier Python tool applied it.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

/// A file's modification time in Unix milliseconds, or 0 when unknown.
pub(super) fn modified_ms(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}
