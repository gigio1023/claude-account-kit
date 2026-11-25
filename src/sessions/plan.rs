//! What a run should change, decided from what discovery read.
//!
//! A conversation is identified by its wrapper id, `local_<uuid>`, not by the
//! transcript id inside it: Desktop's rewind and fork move one partition's copy
//! to a new transcript while the other partitions keep the old one. For each
//! wrapper id one copy is canonical, and every partition should hold a copy
//! that points at the canonical transcript.
//!
//! - A partition without the wrapper gets the canonical payload (an addition).
//! - A partition whose copy points elsewhere is moved to the canonical
//!   transcript (a refresh) when this tool wrote that copy and nobody changed
//!   it since, or when the canonical copy lists the copy's transcript as an
//!   earlier one. Desktop rewrites a copy when an app opens or quits, so
//!   lineage, not ownership, proves that nobody continued the copy.
//! - A copy that already points at the canonical transcript gets a field
//!   update, whoever wrote it, that changes only these fields:
//!   - `lastActivityAt` rises to the conversation's latest activity, the later
//!     of the canonical copy's value and the transcript's last record. Desktop
//!     sorts its list by that field, so without it a conversation continued in
//!     one account stays buried in the others.
//!   - `title`, with `titleSource`, takes the name [`title::decide`] picks, so
//!     a rename in one app reaches the others.
//!   - `isArchived` turns false when the copy was archived before the
//!     conversation's latest activity: Desktop archives a copy whose own
//!     activity time is older than its inactivity limit, and before the
//!     activity update a copy in one app aged while the conversation went on in
//!     another. An archive after the latest activity is a person's choice and
//!     stays. The archive time is `archiveSetAtMs`, or the file's modification
//!     time when Desktop wrote none; Desktop rewrites the file on any later
//!     change, so that time is never earlier than the archive.
//!
//!   Hidden copies, and archived copies that stay archived, keep every field.
//! - When no transcript of the conversation exists any more, the copies this
//!   tool owns are deleted (a removal), and Desktop's own copies are left.
//! - An entry this tool named after a transcript (`local_<transcript id>`) is
//!   deleted when another conversation lists the same transcript: Desktop had
//!   written that transcript before its own entry pointed at it.
//!
//! Nothing here touches the filesystem; whether a folder exists is asked
//! through the `is_dir` input.

use super::desktop::{DesktopScan, WrapperCopy};
use super::ids::{SessionId, WrapperId};
use super::ledger::Ledger;
use super::payload;
use super::skips::{Skip, Skips};
use super::title::{self, TitleDecision, TitleTarget};
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

/// A copy that points at an older transcript than the canonical one, and
/// that this tool owns or that the canonical copy's lineage covers.
pub(super) struct Refresh {
    pub(super) target: Target,
    pub(super) cli: SessionId,
    pub(super) transcript: PathBuf,
    pub(super) payload: Map<String, Value>,
    /// The hash the plan read; the file must still have it.
    pub(super) expected_sha256: String,
    /// Whether this tool wrote the copy. A copy Desktop wrote is refreshed
    /// without becoming the tool's, so it is never removed as an orphan.
    pub(super) owned: bool,
    /// Which of the fields a field update owns this write changes.
    pub(super) fields: FieldChanges,
}

/// A change to an existing copy: a [`Refresh`] that moves it to the canonical
/// transcript, or a field update that changes only the fields in
/// [`FieldChanges`]. A field update of a copy Desktop wrote keeps `owned`
/// false, so the copy never becomes the tool's to remove.
pub(super) type FieldUpdate = Refresh;

/// The fields a write raises, renames, or restores, counted in the report.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FieldChanges {
    pub(super) activity: bool,
    pub(super) title: bool,
    pub(super) restore: bool,
}

/// A tool-owned copy of a conversation whose transcripts are all gone, or any
/// copy of a duplicate entry.
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
    /// Partition copies that already point at the canonical transcript and
    /// need no change.
    pub(super) already_registered: u64,
    /// Conversations with at least one copy to refresh.
    pub(super) refresh_sessions: u64,
    /// Conversations whose transcripts are all gone.
    pub(super) orphaned_sessions: u64,
    /// Copies pointing at another transcript that Desktop wrote or changed.
    pub(super) diverged_not_owned: u64,
    /// Copies of orphaned conversations that Desktop wrote or changed.
    pub(super) orphan_not_owned: u64,
    /// Entries named after a transcript that another conversation lists.
    pub(super) duplicate_sessions: u64,
    /// Conversations whose copies hold different names set in Desktop, none
    /// of them provably the latest, so every copy kept its title.
    pub(super) title_conflicts: u64,
}

pub(super) struct Plan {
    pub(super) additions: Vec<Addition>,
    pub(super) refreshes: Vec<Refresh>,
    pub(super) updates: Vec<FieldUpdate>,
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
        updates: Vec::new(),
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
    let claims = transcript_claims(&input.desktop.copies);
    for (wrapper, copies) in &groups {
        if is_duplicate(wrapper, copies, &claims) {
            plan_duplicate(input, copies, &mut plan, skips);
        } else {
            plan_desktop_session(input, wrapper, copies, &mut plan, skips);
        }
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
    let mut canonical_payload = canonical.payload.clone();
    payload::fill_links(
        &mut canonical_payload,
        copies.iter().map(|copy| &copy.payload),
    );
    let activity = latest_activity(input, canonical);
    payload::set_activity(&mut canonical_payload, activity);
    let tail_titles = input.transcripts.tail_titles.get(&canonical.cli);
    let title = match title::decide(copies, tail_titles.map_or(&[], Vec::as_slice)) {
        TitleDecision::Set(target) => Some(target),
        TitleDecision::Conflict => {
            plan.counts.title_conflicts += 1;
            None
        }
        TitleDecision::Keep => None,
    };
    if let Some(target) = &title {
        title::apply(&mut canonical_payload, target);
    }
    let canonical = Canonical {
        source: canonical,
        payload: canonical_payload,
        activity,
        title,
    };

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
        let owned = |copy: &WrapperCopy| input.ledger.owns(&target.ledger_key, &copy.sha256);
        let current = held
            .filter(|copy| copy.fields.cli_session_id.as_ref() == Some(&canonical.source.cli));
        if let Some(copy) = current {
            refreshed |= plan_current(input, index, copy, target, &canonical, plan, skips);
            continue;
        }
        if let Some(reason) = blocked_reason(input, index, &tombstone_names) {
            skips.add(reason);
            continue;
        }
        match held {
            None => plan.additions.push(Addition {
                target,
                cli: canonical.source.cli.clone(),
                transcript: canonical.source.transcript.clone(),
                payload: canonical.payload.clone(),
            }),
            Some(copy) if owned(copy) || continues(canonical.source.copy, copy) => {
                refreshed = true;
                let mut rebased = payload::rebase(&copy.payload, &canonical.payload);
                let retitled = retitle(&mut rebased, copy, &canonical);
                plan.refreshes.push(Refresh {
                    owned: owned(copy),
                    target,
                    cli: canonical.source.cli.clone(),
                    transcript: canonical.source.transcript.clone(),
                    payload: rebased,
                    expected_sha256: copy.sha256.clone(),
                    fields: FieldChanges {
                        title: retitled,
                        ..FieldChanges::default()
                    },
                });
            }
            Some(_) => plan.counts.diverged_not_owned += 1,
        }
    }
    if refreshed {
        plan.counts.refresh_sessions += 1;
    }
}

/// The canonical copy with what every partition should get from it.
struct Canonical<'s, 'a> {
    source: &'s Source<'a>,
    /// The canonical payload with the newest link fields and the latest activity.
    payload: Map<String, Value>,
    /// The conversation's latest activity, see [`latest_activity`].
    activity: i64,
    /// The title every listed copy should show, when one is decided.
    title: Option<TitleTarget>,
}

/// Give a listed copy's new payload the conversation's title, and say
/// whether that changed it. Archived and hidden copies keep theirs.
fn retitle(
    payload: &mut Map<String, Value>,
    copy: &WrapperCopy,
    canonical: &Canonical<'_, '_>,
) -> bool {
    let Some(target) = &canonical.title else {
        return false;
    };
    if copy.fields.archived_or_hidden() || !title::differs(payload, target) {
        return false;
    }
    title::apply(payload, target);
    true
}

/// Plan a copy that already points at the canonical transcript, and return
/// whether it is refreshed.
///
/// A copy this tool owns that lacks a link field is refreshed. Otherwise a
/// local copy that is listed, or archived before the latest activity, gets a
/// field update when its activity time, title, or archive flag is behind,
/// whoever wrote it.
fn plan_current(
    input: &PlanInput<'_>,
    index: usize,
    copy: &WrapperCopy,
    target: Target,
    canonical: &Canonical<'_, '_>,
    plan: &mut Plan,
    skips: &mut Skips,
) -> bool {
    let owned = input.ledger.owns(&target.ledger_key, &copy.sha256);
    let blocked = blocked_reason(input, index, &target.tombstone_names);
    let source = canonical.source;
    if owned && payload::missing_links(&copy.payload, &canonical.payload) && blocked.is_none() {
        let mut rebased = payload::rebase(&copy.payload, &canonical.payload);
        let retitled = retitle(&mut rebased, copy, canonical);
        plan.refreshes.push(Refresh {
            owned: true,
            target,
            cli: source.cli.clone(),
            transcript: source.transcript.clone(),
            payload: rebased,
            expected_sha256: copy.sha256.clone(),
            fields: FieldChanges {
                title: retitled,
                ..FieldChanges::default()
            },
        });
        return true;
    }
    let fields = &copy.fields;
    let local = !fields.remote && !fields.hidden;
    let restore = local && fields.archived && archived_before(copy, canonical.activity);
    let listed = local && (!fields.archived || restore);
    let behind = listed
        && fields
            .last_activity_at
            .is_none_or(|own| own < canonical.activity);
    let mut updated = copy.payload.clone();
    if restore {
        updated.insert("isArchived".to_string(), Value::Bool(false));
    }
    if behind {
        payload::set_activity(&mut updated, canonical.activity);
    }
    let retitled = listed
        && canonical.title.as_ref().is_some_and(|target| {
            let differs = title::differs(&updated, target);
            if differs {
                title::apply(&mut updated, target);
            }
            differs
        });
    if !(restore || behind || retitled) {
        plan.counts.already_registered += 1;
        return false;
    }
    if let Some(reason) = blocked {
        skips.add(reason);
        return false;
    }
    plan.updates.push(FieldUpdate {
        owned,
        target,
        cli: source.cli.clone(),
        transcript: source.transcript.clone(),
        payload: updated,
        expected_sha256: copy.sha256.clone(),
        fields: FieldChanges {
            activity: behind,
            title: retitled,
            restore,
        },
    });
    false
}

/// Whether an archived copy was archived before `activity`.
///
/// The archive time is `archiveSetAtMs`, or else the file's modification
/// time, which is never earlier than the archive. A copy with neither stays
/// archived.
fn archived_before(copy: &WrapperCopy, activity: i64) -> bool {
    let archived_at = copy
        .fields
        .archive_set_at
        .or((copy.modified_ms > 0).then_some(copy.modified_ms));
    archived_at.is_some_and(|archived_at| archived_at < activity)
}

/// The conversation's latest activity: the canonical copy's `lastActivityAt`
/// or the transcript's last record, whichever is later.
fn latest_activity(input: &PlanInput<'_>, canonical: &Source<'_>) -> i64 {
    let recorded = input.transcripts.last_record_ms.get(&canonical.cli).copied();
    [canonical.copy.fields.last_activity_at, recorded]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or_default()
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
    if metadata.desktop_started() {
        skips.add(Skip::DesktopTranscript);
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
    if copy.fields.archived_or_hidden() {
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
    let candidate_later = continues(candidate, other);
    let other_later = continues(other, candidate);
    if candidate_later != other_later {
        return candidate_later;
    }
    (candidate.fields.last_activity_at, candidate.modified_ms)
        > (other.fields.last_activity_at, other.modified_ms)
}

/// Whether `later` lists `earlier`'s transcript as one it moved on from.
fn continues(later: &WrapperCopy, earlier: &WrapperCopy) -> bool {
    earlier
        .fields
        .cli_session_id
        .as_ref()
        .is_some_and(|cli| later.fields.prior_cli_session_ids.contains(cli))
}

/// Every entry that names each transcript, current or earlier.
fn transcript_claims(copies: &[WrapperCopy]) -> BTreeMap<&SessionId, BTreeSet<&WrapperId>> {
    let mut claims: BTreeMap<&SessionId, BTreeSet<&WrapperId>> = BTreeMap::new();
    for copy in copies {
        for id in &copy.fields.transcript_ids {
            claims.entry(id).or_default().insert(&copy.wrapper);
        }
    }
    claims
}

/// Whether an entry is this tool's second name for another conversation.
///
/// Every copy must still point at the transcript the entry is named after;
/// a copy that moved on was continued under this name and is kept.
fn is_duplicate(
    wrapper: &WrapperId,
    copies: &[&WrapperCopy],
    claims: &BTreeMap<&SessionId, BTreeSet<&WrapperId>>,
) -> bool {
    let own = wrapper.uuid();
    copies
        .iter()
        .all(|copy| copy.fields.cli_session_id.as_ref() == Some(own))
        && claims
            .get(own)
            .is_some_and(|wrappers| wrappers.iter().any(|other| *other != wrapper))
}

/// Remove every copy of a duplicate entry. The transcript stays, listed under
/// the conversation that names it.
fn plan_duplicate(
    input: &PlanInput<'_>,
    copies: &[&WrapperCopy],
    plan: &mut Plan,
    skips: &mut Skips,
) {
    plan.counts.duplicate_sessions += 1;
    for copy in copies {
        if is_blocked(input, copy.partition) {
            skips.add(Skip::IncompleteTarget);
            continue;
        }
        let partition = &input.desktop.partitions[copy.partition];
        plan.removals.push(Removal {
            target: Target {
                desktop_root: partition.desktop_root.clone(),
                path: partition.dir.join(copy.wrapper.file_name()),
                relative: partition.relative_path(&copy.wrapper),
                ledger_key: partition.ledger_key(&copy.wrapper),
                tombstone_names: BTreeSet::new(),
            },
            cli: copy.wrapper.uuid().to_string(),
            expected_sha256: copy.sha256.clone(),
        });
    }
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
