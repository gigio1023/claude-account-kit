//! Why a session, file, or target was left out of a run.
//!
//! The reasons are the only per-item detail a run records, so they never carry
//! a title, path, or message. Their names are stable because `last-run.json`
//! and `--json` output expose them.

use serde::{Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Skip {
    /// A directory listing failed or passed its entry limit.
    InventoryIncomplete,
    /// An `ssh-*` project directory, which mirrors a remote machine.
    RemoteProject,
    /// A transcript name that is not a regular file.
    NonregularTranscript,
    /// A Desktop entry that could not be read; its partition gets no writes.
    MetadataUnreadable,
    /// A Desktop entry without a valid `cliSessionId`.
    MissingCliId,
    /// The newest copy of a conversation is archived or hidden.
    ArchivedOrHiddenSource,
    /// The newest copy of a conversation runs over SSH, WSL, or the cloud.
    RemoteSource,
    /// The transcript of the newest copy is gone, though an older one exists.
    MissingTranscript,
    /// Two project directories hold a transcript with the same id.
    AmbiguousTranscript,
    /// The newest copy has no existing working folder or no valid timestamps.
    InvalidLocalMetadata,
    /// A project's `sessions-index.json` could not be read.
    IndexUnreadable,
    /// A transcript could not be read within the run's limits.
    TranscriptUnreadable,
    /// A subagent transcript, or one whose records name another session.
    SidechainOrMismatchedTranscript,
    /// A CLI transcript that ran over SSH, WSL, or the cloud.
    RemoteTranscript,
    /// A CLI transcript whose working folder no longer exists.
    UnknownCwd,
    /// A transcript Desktop started that no Desktop entry names yet. Desktop
    /// writes the transcript before it points its entry at it, and an entry
    /// made here in between would list the conversation twice.
    DesktopTranscript,
    /// A CLI transcript whose `local_<id>` name a Desktop entry already uses.
    WrapperIdTaken,
    /// A target partition whose metadata could not be read completely.
    IncompleteTarget,
    /// The account holds a `deleted_*` marker for the conversation.
    DeletedInAccount,
    /// Every transcript is gone, but the transcript listing was incomplete or empty.
    OrphanUnverified,
}

impl Skip {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::InventoryIncomplete => "inventory_incomplete",
            Self::RemoteProject => "remote_project",
            Self::NonregularTranscript => "nonregular_transcript",
            Self::MetadataUnreadable => "metadata_unreadable",
            Self::MissingCliId => "missing_cli_id",
            Self::ArchivedOrHiddenSource => "archived_or_hidden_source",
            Self::RemoteSource => "remote_source",
            Self::MissingTranscript => "missing_transcript",
            Self::AmbiguousTranscript => "ambiguous_transcript",
            Self::InvalidLocalMetadata => "invalid_local_metadata",
            Self::IndexUnreadable => "index_unreadable",
            Self::TranscriptUnreadable => "transcript_unreadable",
            Self::SidechainOrMismatchedTranscript => "sidechain_or_mismatched_transcript",
            Self::RemoteTranscript => "remote_transcript",
            Self::UnknownCwd => "unknown_cwd",
            Self::DesktopTranscript => "desktop_transcript",
            Self::WrapperIdTaken => "wrapper_id_taken",
            Self::IncompleteTarget => "incomplete_target",
            Self::DeletedInAccount => "deleted_in_account",
            Self::OrphanUnverified => "orphan_unverified",
        }
    }

    /// Whether the reason means the run could not see everything it needed.
    pub(super) fn signals_incomplete_input(self) -> bool {
        matches!(
            self,
            Self::InventoryIncomplete
                | Self::MetadataUnreadable
                | Self::IndexUnreadable
                | Self::TranscriptUnreadable
        )
    }
}

/// Counts per reason, serialized as `{"reason": count}`.
#[derive(Clone, Debug, Default)]
pub(super) struct Skips(BTreeMap<Skip, u64>);

impl Skips {
    pub(super) fn add(&mut self, reason: Skip) {
        *self.0.entry(reason).or_insert(0) += 1;
    }

    pub(super) fn any_incomplete_input(&self) -> bool {
        self.0.keys().any(|reason| reason.signals_incomplete_input())
    }

    pub(super) fn to_map(&self) -> BTreeMap<String, u64> {
        self.0
            .iter()
            .map(|(reason, count)| (reason.as_str().to_string(), *count))
            .collect()
    }
}

impl Serialize for Skips {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_map().serialize(serializer)
    }
}
