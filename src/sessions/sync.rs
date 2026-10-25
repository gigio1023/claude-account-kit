//! One sync run: discover, plan, publish, record.
//!
//! A dry run reads and plans only. A write run holds the lock, imports the
//! earlier tool's ledger once, applies the plan, and stores an aggregate
//! summary. A background run first compares a cheap fingerprint with the last
//! full run and stops there when nothing changed recently.

use super::bounded::{self, ReadBudget};
use super::desktop::{self, DesktopScan};
use super::fingerprint::{self, StoredFingerprint};
use super::ledger::{self, Ledger, LedgerAction};
use super::lock::RunLock;
use super::paths::SessionPaths;
use super::plan::{self, Plan, PlanInput};
use super::publish::{Outcome, Publisher};
use super::report::{Changes, RunReport, RunStatus};
use super::skips::Skips;
use super::state;
use super::timestamp;
use super::transcripts::{self, CliTranscript, TranscriptInventory};
use crate::time::now_epoch;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

/// A background run skips planning when the fingerprint matches a full run
/// younger than this, in seconds.
const UNCHANGED_WINDOW: i64 = 5 * 60;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RunKind {
    /// Plan and report; write nothing, not even this tool's state.
    DryRun,
    /// Apply the plan. `background` marks a run the launch agent started.
    Write { background: bool },
}

/// Run a full sync. `None` means another run holds the lock.
pub(super) fn full_run(paths: &SessionPaths, kind: RunKind) -> Result<Option<RunReport>> {
    let started = Instant::now();
    if kind == RunKind::DryRun {
        state::check_location(paths)?;
        let ledger = ledger::load(&paths.ledger(), &paths.legacy_ledger)?;
        let mut skips = Skips::default();
        let discovery = discover(paths, &mut skips)?;
        let plan = plan::build(&discovery.input(&ledger), &mut skips);
        let warnings = warnings(&discovery, &skips);
        let mut report = summarize(&plan, &skips, warnings, &[], started);
        report.status = RunStatus::Preview;
        return Ok(Some(report));
    }

    state::prepare_dir(paths)?;
    let digest = fingerprint::compute(&paths.desktop_roots(), &paths.projects_root);
    let Some(_lock) = RunLock::try_acquire(&paths.lock())? else {
        return Ok(None);
    };
    let mut ledger = ledger::load(&paths.ledger(), &paths.legacy_ledger)?;
    ledger::persist_import(&mut ledger, &paths.ledger(), &paths.state_dir)?;

    let mut skips = Skips::default();
    let discovery = discover(paths, &mut skips)?;
    let plan = plan::build(&discovery.input(&ledger), &mut skips);
    let warnings = warnings(&discovery, &skips);
    // Remember observed roots before publishing. A temporarily missing profile
    // may hold the only deletion marker for an account in another profile.
    state::write_json(&paths.seen_roots(), &discovery.seen_roots)?;
    let outcomes = publish(paths, &mut ledger, &plan);

    let mut report = summarize(&plan, &skips, warnings, &outcomes, started);
    report.background = kind == RunKind::Write { background: true };
    state::write_json(&paths.last_run(), &report)?;
    if report.status == RunStatus::Ok {
        let stored = StoredFingerprint {
            digest,
            full_run_at: now_epoch(),
        };
        state::write_json(&paths.fingerprint(), &stored)?;
    }
    Ok(Some(report))
}

/// The launch agent's run. Returns whether it ended without an error.
///
/// Busy and unchanged both count as success: another run is doing the work,
/// or there is none to do.
pub(super) fn background_run(paths: &SessionPaths) -> Result<bool> {
    state::prepare_dir(paths)?;
    let digest = fingerprint::compute(&paths.desktop_roots(), &paths.projects_root);
    if recently_unchanged(paths, &digest) {
        return Ok(true);
    }
    let report = full_run(paths, RunKind::Write { background: true })?;
    Ok(report.is_none_or(|report| report.status == RunStatus::Ok))
}

/// Whether the last full run saw the same fingerprint less than five minutes ago.
///
/// An unreadable fingerprint file reads as changed, which only costs a full run.
fn recently_unchanged(paths: &SessionPaths, digest: &str) -> bool {
    let Ok(Some(stored)) = state::read_json::<StoredFingerprint>(&paths.fingerprint()) else {
        return false;
    };
    let age = now_epoch() - stored.full_run_at;
    stored.digest == digest && (0..UNCHANGED_WINDOW).contains(&age)
}

/// Everything read before planning.
struct Discovery {
    desktop: DesktopScan,
    transcripts: TranscriptInventory,
    cli_only: Vec<CliTranscript>,
    seen_roots: BTreeSet<String>,
}

impl Discovery {
    fn input<'a>(&'a self, ledger: &'a Ledger) -> PlanInput<'a> {
        PlanInput {
            desktop: &self.desktop,
            transcripts: &self.transcripts,
            cli_only: &self.cli_only,
            ledger,
            is_dir: &transcripts::is_existing_dir,
        }
    }
}

fn discover(paths: &SessionPaths, skips: &mut Skips) -> Result<Discovery> {
    let mut budget = ReadBudget::new();
    let mut desktop = desktop::scan(&paths.desktop_roots(), &mut budget, skips);
    let mut seen_roots: BTreeSet<String> = state::read_json(&paths.seen_roots())?.unwrap_or_default();
    for root in paths.desktop_roots() {
        let key: String = desktop::sha256_hex(root.as_os_str().as_encoded_bytes());
        if bounded::is_real_dir(&root) && bounded::is_real_dir(&root.join("claude-code-sessions")) {
            seen_roots.insert(key);
        } else if seen_roots.contains(&key) {
            desktop.complete = false;
        }
    }
    let transcripts = transcripts::inventory(&paths.projects_root, skips);
    let known = desktop.known_transcript_ids();
    let cli_only = transcripts::read_cli_only(&transcripts, &known, &mut budget, skips);
    Ok(Discovery {
        desktop,
        transcripts,
        cli_only,
        seen_roots,
    })
}

fn publish(paths: &SessionPaths, ledger: &mut Ledger, plan: &Plan) -> Vec<Outcome> {
    let desktop_roots = paths.desktop_roots();
    let ledger_path = paths.ledger();
    let mut publisher = Publisher {
        desktop_roots: &desktop_roots,
        ledger_path: &ledger_path,
        ledger,
    };
    let mut outcomes = Vec::new();
    outcomes.extend(
        plan.removals
            .iter()
            .map(|removal| publisher.remove(removal)),
    );
    outcomes.extend(
        plan.refreshes
            .iter()
            .map(|refresh| publisher.refresh(refresh)),
    );
    outcomes.extend(
        plan.additions
            .iter()
            .map(|addition| publisher.add(addition)),
    );
    outcomes
}

/// Conditions a reader should know about, phrased without any path or title.
fn warnings(discovery: &Discovery, skips: &Skips) -> Vec<String> {
    let mut warnings = Vec::new();
    if !discovery.desktop.complete {
        warnings.push(
            "the Desktop account listing could not be read completely, so nothing was written"
                .to_string(),
        );
    } else if !discovery.desktop.blocked.is_empty() {
        warnings.push(format!(
            "{} Desktop session list(s) could not be read completely and were left unchanged",
            discovery.desktop.blocked.len()
        ));
    }
    if !discovery.transcripts.complete {
        warnings.push(
            "the transcript listing was incomplete, so no orphaned entries were removed".to_string(),
        );
    } else if discovery.transcripts.seen.is_empty() && !discovery.desktop.copies.is_empty() {
        warnings.push("no transcripts were found, so no orphaned entries were removed".to_string());
    }
    if skips.any_incomplete_input() {
        warnings.push(
            "some inputs could not be read within the read limits; this run may be incomplete"
                .to_string(),
        );
    }
    warnings
}

fn summarize(
    plan: &Plan,
    skips: &Skips,
    warnings: Vec<String>,
    outcomes: &[Outcome],
    started: Instant,
) -> RunReport {
    let mut publication: BTreeMap<String, u64> = BTreeMap::new();
    let mut applied = Changes::default();
    for outcome in outcomes {
        *publication.entry(outcome.as_str().to_string()).or_insert(0) += 1;
        match outcome.completed() {
            Some(LedgerAction::Created) => applied.add += 1,
            Some(LedgerAction::Refreshed) => applied.refresh += 1,
            Some(LedgerAction::Removed) => applied.remove += 1,
            None => {}
        }
    }
    let failed = outcomes.iter().any(|outcome| outcome.is_error());
    RunReport {
        status: if failed { RunStatus::Error } else { RunStatus::Ok },
        checked_at: timestamp::format_utc(now_epoch()),
        background: false,
        counts: plan.counts.clone(),
        planned: Changes {
            add: plan.additions.len() as u64,
            refresh: plan.refreshes.len() as u64,
            remove: plan.removals.len() as u64,
        },
        applied,
        skipped: skips.to_map(),
        publication,
        warnings,
        refresh_needed: applied.total() > 0,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}
