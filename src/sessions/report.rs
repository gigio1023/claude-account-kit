//! The summary of one sync, as printed and as kept in `last-run.json`.
//!
//! It holds counts and reason names only. No title, path, transcript id, or
//! message ever goes into it, so the file can sit in the state directory and
//! be shown in `status` without exposing a conversation.

use super::plan::PlanCounts;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RunStatus {
    #[default]
    Ok,
    /// A dry run: changes were planned, nothing was written.
    Preview,
    /// At least one write failed or could not be recorded in the ledger.
    Error,
}

impl RunStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Preview => "preview",
            Self::Error => "error",
        }
    }
}

/// Wrapper files added, refreshed, and removed.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Changes {
    pub(super) add: u64,
    pub(super) refresh: u64,
    pub(super) remove: u64,
}

impl Changes {
    pub(super) fn total(self) -> u64 {
        self.add + self.refresh + self.remove
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct RunReport {
    pub(super) status: RunStatus,
    /// RFC 3339 UTC time the run finished.
    pub(super) checked_at: String,
    /// Whether the launch agent started the run.
    pub(super) background: bool,
    #[serde(flatten)]
    pub(super) counts: PlanCounts,
    /// What the plan called for.
    pub(super) planned: Changes,
    /// What was written. Always zero for a dry run.
    pub(super) applied: Changes,
    pub(super) skipped: BTreeMap<String, u64>,
    /// Outcome name to count for each attempted write.
    pub(super) publication: BTreeMap<String, u64>,
    pub(super) warnings: Vec<String>,
    /// Desktop reads a partition again only on an account switch or a restart.
    pub(super) refresh_needed: bool,
    pub(super) elapsed_ms: u64,
}

/// Print a sync run for a person.
pub(super) fn print(report: &RunReport) {
    print_overview(report);
    if report.status == RunStatus::Preview {
        let planned = report.planned;
        println!(
            "would add {}, refresh {}, remove {}; nothing was written",
            planned.add, planned.refresh, planned.remove
        );
    } else {
        print_changes(report.applied);
    }
    print_details(report);
    if report.refresh_needed {
        print_refresh_hint();
    }
}

/// How many conversations and partitions the run saw.
pub(super) fn print_overview(report: &RunReport) {
    let counts = &report.counts;
    println!(
        "conversations: {} across {} Desktop partition(s); {} entr{} already up to date",
        counts.sessions,
        counts.partitions,
        counts.already_registered,
        if counts.already_registered == 1 { "y" } else { "ies" }
    );
}

pub(super) fn print_changes(applied: Changes) {
    println!(
        "added {}, refreshed {}, removed {}",
        applied.add, applied.refresh, applied.remove
    );
}

/// What was left alone or skipped, and anything that limits the result.
pub(super) fn print_details(report: &RunReport) {
    let counts = &report.counts;
    if counts.diverged_not_owned + counts.orphan_not_owned > 0 {
        println!(
            "left alone: {} copy(ies) on an older transcript and {} orphaned copy(ies), \
             because Desktop wrote or changed them",
            counts.diverged_not_owned, counts.orphan_not_owned
        );
    }
    if !report.skipped.is_empty() {
        let skipped: Vec<String> = report
            .skipped
            .iter()
            .map(|(reason, count)| format!("{reason} {count}"))
            .collect();
        println!("skipped: {}", skipped.join(", "));
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
    if report.status == RunStatus::Error {
        println!("some writes failed; run `sessions sync --json` for the outcome counts");
    }
    if counts.partitions == 0 {
        println!("no Desktop session lists found; sign in to Claude Desktop and run sync again");
    }
}

pub(super) fn print_refresh_hint() {
    println!("Claude Desktop shows the changes after the next account switch or restart");
}
