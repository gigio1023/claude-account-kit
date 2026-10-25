//! What a run should change, decided from what discovery read.
//!
//! A conversation is identified by its wrapper id, `local_<uuid>`, not by the
//! transcript id inside it: Desktop's rewind and fork move one partition's copy
//! to a new transcript while the other partitions keep the old one. For each
//! wrapper id one copy is canonical, and every partition should hold a copy
//! that points at the canonical transcript.
//!
//! - A partition without the wrapper gets the canonical payload (an addition).
//! - A partition whose copy points elsewhere gets the canonical payload only
//!   when this tool wrote that copy and nobody changed it since (a refresh).
//! - When no transcript of the conversation exists any more, the copies this
//!   tool owns are deleted (a removal), and Desktop's own copies are left.
//!
//! Nothing here touches the filesystem; whether a folder exists is asked
//! through the `is_dir` input.

use super::desktop::{DesktopScan, WrapperCopy};
use super::ids::{SessionId, WrapperId};
use super::ledger::Ledger;
use super::payload;
use super::skips::{Skip, Skips};
use super::transcripts::{CliTranscript, TranscriptInventory};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// A Desktop file this tool would write or remove.
pub(super) struct Target {
    pub(super) desktop_root: PathBuf,
    pub(super) path: PathBuf,
    /// The path under `claude-code-sessions`, as the ledger records it.
    pub(super) relative: String,
    /// Root-qualified ownership key; legacy primary-root keys stay unchanged.
    pub(super) ledger_key: String,
    /// Every name a `deleted_*` marker for this conversation could use.
    pub(super) tombstone_names: BTreeSet<String>,
}

/// A new wrapper in a partition that lacks the conversation.
pub(super) struct Addition {
    pub(super) target: Target,
    pub(super) cli: SessionId,
    pub(super) transcript: PathBuf,
    pub(super) payload: Map<String, Value>,
}

/// A tool-owned copy that points at an older transcript than the canonical one.
pub(super) struct Refresh {
    pub(super) target: Target,
    pub(super) cli: SessionId,
    pub(super) transcript: PathBuf,
    pub(super) payload: Map<String, Value>,
    /// The hash that proved ownership; the file must still have it.
    pub(super) expected_sha256: String,
}

/// A tool-owned copy of a conversation whose transcripts are all gone.
pub(super) struct Removal {
    pub(super) target: Target,
    pub(super) cli: String,
    pub(super) expected_sha256: String,
}

/// Counts that describe the run apart from the changes themselves.
#[derive(Clone, Debug, Default, Serialize, serde::Deserialize)]
#[serde(default)]
pub(super) struct PlanCounts {
    pub(super) partitions: u64,
    /// Conversations with a usable source, from Desktop or from the CLI.
    pub(super) sessions: u64,
    /// Partition copies that already point at the canonical transcript.
    pub(super) already_registered: u64,
    /// Conversations with at least one copy to refresh.
    pub(super) refresh_sessions: u64,
    /// Conversations whose transcripts are all gone.
    pub(super) orphaned_sessions: u64,
    /// Copies pointing at another transcript that Desktop wrote or changed.
    pub(super) diverged_not_owned: u64,
    /// Copies of orphaned conversations that Desktop wrote or changed.
    pub(super) orphan_not_owned: u64,
}

pub(super) struct Plan {
    pub(super) additions: Vec<Addition>,
    pub(super) refreshes: Vec<Refresh>,
    pub(super) removals: Vec<Removal>,
    pub(super) counts: PlanCounts,
}

pub(super) struct PlanInput<'a> {
    pub(super) desktop: &'a DesktopScan,
    pub(super) transcripts: &'a TranscriptInventory,
    pub(super) cli_only: &'a [CliTranscript],
    pub(super) ledger: &'a Ledger,
    pub(super) is_dir: &'a dyn Fn(&str) -> bool,
}

/// A copy that may serve as the canonical source, with what it would publish.
struct Source<'a> {
    copy: &'a WrapperCopy,
    cli: SessionId,
    transcript: PathBuf,
    payload: Map<String, Value>,
}

pub(super) fn build(input: &PlanInput<'_>, skips: &mut Skips) -> Plan {
    let mut plan = Plan {
        additions: Vec::new(),
        refreshes: Vec::new(),
        removals: Vec::new(),
        counts: PlanCounts {
            partitions: input.desktop.partitions.len() as u64,
            ..PlanCounts::default()
        },
    };
    let mut groups: BTreeMap<&WrapperId, Vec<&WrapperCopy>> = BTreeMap::new();
    for copy in &input.desktop.copies {
        groups.entry(&copy.wrapper).or_default().push(copy);
    }
    for (wrapper, copies) in &groups {
        plan_desktop_session(input, wrapper, copies, &mut plan, skips);
    }
    for transcript in input.cli_only {
        let wrapper = WrapperId::for_cli_session(&transcript.id);
        if groups.contains_key(&wrapper) {
            skips.add(Skip::WrapperIdTaken);
            continue;
        }
        plan_cli_session(input, &wrapper, transcript, &mut plan, skips);
    }
    plan
}

fn plan_desktop_session(
    input: &PlanInput<'_>,
    wrapper: &WrapperId,
    copies: &[&WrapperCopy],
    plan: &mut Plan,
    skips: &mut Skips,
) {
    let ids: BTreeSet<&SessionId> = copies
        .iter()
        .flat_map(|copy| &copy.fields.transcript_ids)
        .collect();
    let tombstone_names = tombstone_names(wrapper, ids.iter().copied());
    if !ids.is_empty() && ids.iter().all(|id| !input.transcripts.seen.contains(*id)) {
        plan_orphan(input, copies, &tombstone_names, plan, skips);
        return;
    }

    let sources: Vec<Source<'_>> = copies
        .iter()
        .filter_map(|copy| source(input, copy).ok())
        .collect();
    let Some(canonical) = newest(&sources, |source| source.copy) else {
        if let Some(newest_copy) = newest(copies, |copy| *copy) {
            skips.add(
                source(input, newest_copy)
                    .err()
                    .unwrap_or(Skip::MissingTranscript),
            );
        }
        return;
    };
    plan.counts.sessions += 1;

    let mut refreshed = false;
    for (index, partition) in input.desktop.partitions.iter().enumerate() {
        let target = Target {
            desktop_root: partition.desktop_root.clone(),
            path: partition.dir.join(wrapper.file_name()),
            relative: partition.relative_path(wrapper),
            ledger_key: partition.ledger_key(wrapper),
            tombstone_names: tombstone_names.clone(),
        };
        let held = copies.iter().find(|copy| copy.partition == index);
        if held.is_some_and(|copy| copy.fields.cli_session_id.as_ref() == Some(&canonical.cli)) {
            plan.counts.already_registered += 1;
            continue;
        }
        if let Some(reason) = blocked_reason(input, index, &tombstone_names) {
            skips.add(reason);
            continue;
        }
        match held {
            None => plan.additions.push(Addition {
                target,
                cli: canonical.cli.clone(),
                transcript: canonical.transcript.clone(),
                payload: canonical.payload.clone(),
            }),
            Some(copy) if input.ledger.owns(&target.ledger_key, &copy.sha256) => {
                refreshed = true;
                plan.refreshes.push(Refresh {
                    target,
                    cli: canonical.cli.clone(),
                    transcript: canonical.transcript.clone(),
                    payload: canonical.payload.clone(),
                    expected_sha256: copy.sha256.clone(),
                });
            }
            Some(_) => plan.counts.diverged_not_owned += 1,
        }
    }
    if refreshed {
        plan.counts.refresh_sessions += 1;
    }
}

/// Every transcript of the conversation is gone: remove the copies this tool owns.
///
/// Only a complete transcript listing can prove a transcript is gone; an
/// incomplete one removes nothing. Neither does an empty one, which is more
/// likely a moved or unmounted directory than a user who deleted every
/// transcript, and would otherwise remove every entry this tool ever wrote.
fn plan_orphan(
    input: &PlanInput<'_>,
    copies: &[&WrapperCopy],
    tombstone_names: &BTreeSet<String>,
    plan: &mut Plan,
    skips: &mut Skips,
) {
    if !input.transcripts.complete || input.transcripts.seen.is_empty() {
        skips.add(Skip::OrphanUnverified);
        return;
    }
    plan.counts.orphaned_sessions += 1;
    for copy in copies {
        let partition = &input.desktop.partitions[copy.partition];
        let relative = partition.relative_path(&copy.wrapper);
        let ledger_key: String = partition.ledger_key(&copy.wrapper);
        if !input.ledger.owns(&ledger_key, &copy.sha256) {
            plan.counts.orphan_not_owned += 1;
            continue;
        }
        if is_blocked(input, copy.partition) {
            skips.add(Skip::IncompleteTarget);
            continue;
        }
        plan.removals.push(Removal {
            target: Target {
                desktop_root: partition.desktop_root.clone(),
                path: partition.dir.join(copy.wrapper.file_name()),
                relative,
                ledger_key,
                tombstone_names: tombstone_names.clone(),
            },
            cli: copy
                .fields
                .cli_session_id
                .as_ref()
                .map_or_else(String::new, ToString::to_string),
            expected_sha256: copy.sha256.clone(),
        });
    }
}

fn plan_cli_session(
    input: &PlanInput<'_>,
    wrapper: &WrapperId,
    transcript: &CliTranscript,
    plan: &mut Plan,
    skips: &mut Skips,
) {
    let metadata = &transcript.metadata;
    if metadata.sidechain || metadata.mismatched {
        skips.add(Skip::SidechainOrMismatchedTranscript);
        return;
    }
    if metadata.remote {
        skips.add(Skip::RemoteTranscript);
        return;
    }
    let Some(cwd) = metadata
        .project_path
        .as_deref()
        .filter(|cwd| (input.is_dir)(cwd))
    else {
        skips.add(Skip::UnknownCwd);
        return;
    };
    plan.counts.sessions += 1;
    let payload = payload::from_cli_transcript(transcript, cwd);
    let tombstone_names = tombstone_names(wrapper, [&transcript.id]);
    for (index, partition) in input.desktop.partitions.iter().enumerate() {
        if let Some(reason) = blocked_reason(input, index, &tombstone_names) {
            skips.add(reason);
            continue;
        }
        plan.additions.push(Addition {
            target: Target {
                desktop_root: partition.desktop_root.clone(),
                path: partition.dir.join(wrapper.file_name()),
                relative: partition.relative_path(wrapper),
                ledger_key: partition.ledger_key(wrapper),
                tombstone_names: tombstone_names.clone(),
            },
            cli: transcript.id.clone(),
            transcript: transcript.path.clone(),
            payload: payload.clone(),
        });
    }
}

/// Check one copy as a canonical source, or say why it cannot be one.
fn source<'a>(input: &PlanInput<'_>, copy: &'a WrapperCopy) -> Result<Source<'a>, Skip> {
    let cli = copy.fields.cli_session_id.clone().ok_or(Skip::MissingCliId)?;
    if copy.fields.archived_or_hidden {
        return Err(Skip::ArchivedOrHiddenSource);
    }
    if copy.fields.remote {
        return Err(Skip::RemoteSource);
    }
    let transcript = match input.transcripts.files.get(&cli).map(Vec::as_slice) {
        Some([path]) => path.clone(),
        Some([_, _, ..]) => return Err(Skip::AmbiguousTranscript),
        _ => return Err(Skip::MissingTranscript),
    };
    let payload = payload::from_wrapper(&copy.payload, &copy.wrapper, &cli, input.is_dir)
        .ok_or(Skip::InvalidLocalMetadata)?;
    Ok(Source {
        copy,
        cli,
        transcript,
        payload,
    })
}

/// The newest of several copies of one conversation.
///
/// Lineage decides first: a copy whose `priorCliSessionIds` holds another
/// copy's transcript came after it. Without a lineage link, the later
/// `lastActivityAt` wins, then the later file modification time.
fn newest<'a, T>(items: &'a [T], copy_of: impl Fn(&'a T) -> &'a WrapperCopy) -> Option<&'a T> {
    items.iter().reduce(|best, item| {
        if is_newer(copy_of(item), copy_of(best)) {
            item
        } else {
            best
        }
    })
}

fn is_newer(candidate: &WrapperCopy, other: &WrapperCopy) -> bool {
    let continues = |later: &WrapperCopy, earlier: &WrapperCopy| {
        earlier
            .fields
            .cli_session_id
            .as_ref()
            .is_some_and(|cli| later.fields.prior_cli_session_ids.contains(cli))
    };
    let candidate_later = continues(candidate, other);
    let other_later = continues(other, candidate);
    if candidate_later != other_later {
        return candidate_later;
    }
    (candidate.fields.last_activity_at, candidate.modified_ms)
        > (other.fields.last_activity_at, other.modified_ms)
}

/// Why a partition may not receive a write for this conversation, if it may not.
fn blocked_reason(
    input: &PlanInput<'_>,
    partition: usize,
    tombstone_names: &BTreeSet<String>,
) -> Option<Skip> {
    if is_blocked(input, partition) {
        return Some(Skip::IncompleteTarget);
    }
    let account = &input.desktop.partitions[partition].account;
    let deleted = input
        .desktop
        .tombstones
        .get(account)
        .is_some_and(|markers| !markers.is_disjoint(tombstone_names));
    deleted.then_some(Skip::DeletedInAccount)
}

/// A partition is closed to writes when it could not be read completely, or
/// when the partition listing itself was incomplete.
fn is_blocked(input: &PlanInput<'_>, partition: usize) -> bool {
    !input.desktop.complete || input.desktop.blocked.contains(&partition)
}

/// The marker names that would mean the user deleted this conversation.
fn tombstone_names<'a>(
    wrapper: &WrapperId,
    ids: impl IntoIterator<Item = &'a SessionId>,
) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = ids.into_iter().map(ToString::to_string).collect();
    names.insert(wrapper.uuid().to_string());
    names.insert(wrapper.to_string());
    names
}
