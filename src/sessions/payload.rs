//! The wrapper this tool writes into a partition.
//!
//! A written wrapper carries a fixed baseline plus the history fields that let
//! Desktop resume the right branch of a conversation. It deliberately leaves
//! out per-partition state such as permissions, connectors, and scheduled
//! work, which must not travel between accounts.

use super::ids::{SessionId, WrapperId};
use super::timestamp;
use super::transcripts::CliTranscript;
use serde_json::{Map, Value, json};
use std::path::Path;

/// Fields copied from a Desktop wrapper because they describe where the
/// conversation lives and how its transcript branches, not who may run it.
const HISTORY_FIELDS: [&str; 19] = [
    "worktreePath",
    "worktreeName",
    "folderWorktrees",
    "requestedAdditionalFolders",
    "worktreeLazy",
    "worktreePinned",
    "branch",
    "sourceBranch",
    "gitAnchors",
    "gitAnchorsLookupOnly",
    "gitAnchorsFolderRealpath",
    "priorCliSessionIds",
    "historyOnlyCliSessionId",
    "rewindEdges",
    "transcriptModelStates",
    "transcriptCuts",
    "forkedFromSessionId",
    "forkedAtMessageUuid",
    "lineageDetached",
];

/// Build the payload to publish from a Desktop wrapper.
///
/// `None` when the wrapper has no existing working folder or no valid times;
/// Desktop refuses to open a session whose folder is gone.
///
/// `is_dir` answers whether a working folder exists, which keeps the planning
/// that calls this free of filesystem access.
pub(super) fn from_wrapper(
    source: &Map<String, Value>,
    wrapper: &WrapperId,
    cli: &SessionId,
    is_dir: &dyn Fn(&str) -> bool,
) -> Option<Map<String, Value>> {
    let cwd = existing_dir(source.get("cwd"), is_dir)?;
    let created = timestamp::milliseconds(source.get("createdAt"))?;
    let activity = timestamp::milliseconds(source.get("lastActivityAt"))?;
    let origin = existing_dir(source.get("originCwd"), is_dir).unwrap_or(cwd);
    let mut payload = baseline(&Baseline {
        wrapper,
        cli,
        cwd,
        origin,
        created,
        activity,
        title: source.get("title").and_then(Value::as_str),
    });
    for (key, value) in source {
        if HISTORY_FIELDS.contains(&key.as_str()) {
            payload.insert(key.clone(), value.clone());
        }
    }
    Some(payload)
}

/// Build the payload for a transcript that has no Desktop entry anywhere.
pub(super) fn from_cli_transcript(transcript: &CliTranscript, cwd: &str) -> Map<String, Value> {
    let metadata = &transcript.metadata;
    let title = metadata
        .custom_title
        .as_deref()
        .or(metadata.summary.as_deref());
    let wrapper = WrapperId::for_cli_session(&transcript.id);
    let mut payload = baseline(&Baseline {
        wrapper: &wrapper,
        cli: &transcript.id,
        cwd,
        origin: cwd,
        created: metadata.created.unwrap_or(transcript.modified_ms),
        activity: metadata.modified.unwrap_or(transcript.modified_ms),
        title,
    });
    if let Some(branch) = &metadata.git_branch {
        payload.insert("branch".to_string(), json!(branch));
    }
    payload
}

struct Baseline<'a> {
    wrapper: &'a WrapperId,
    cli: &'a SessionId,
    cwd: &'a str,
    origin: &'a str,
    created: i64,
    activity: i64,
    title: Option<&'a str>,
}

/// The fields every written wrapper has, in Desktop's own key order.
///
/// A missing or blank title becomes the folder name and a short session id,
/// so two untitled sessions from one folder stay distinguishable.
fn baseline(fields: &Baseline<'_>) -> Map<String, Value> {
    let title = match fields.title {
        Some(title) if !title.trim().is_empty() => title.to_string(),
        _ => {
            let folder = Path::new(fields.origin).file_name().map_or_else(
                || "Local session".to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            format!("{folder} {}", &fields.cli.as_str()[..8])
        }
    };
    let mut payload = Map::new();
    payload.insert("sessionId".into(), json!(fields.wrapper.to_string()));
    payload.insert("cliSessionId".into(), json!(fields.cli.as_str()));
    payload.insert("cwd".into(), json!(fields.cwd));
    payload.insert("originCwd".into(), json!(fields.origin));
    payload.insert("createdAt".into(), json!(fields.created));
    payload.insert("lastActivityAt".into(), json!(fields.activity));
    payload.insert("title".into(), json!(title));
    payload.insert("isArchived".into(), json!(false));
    payload.insert("isHidden".into(), json!(false));
    payload.insert("sessionPermissionUpdates".into(), json!([]));
    payload.insert("alwaysAllowedReasons".into(), json!([]));
    payload
}

fn existing_dir<'a>(value: Option<&'a Value>, is_dir: &dyn Fn(&str) -> bool) -> Option<&'a str> {
    value.and_then(Value::as_str).filter(|path| is_dir(path))
}
