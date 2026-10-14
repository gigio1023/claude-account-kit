//! Desktop session sharing: every local Claude Code session in every Desktop account.
//!
//! Claude Desktop lists Code-tab sessions per account and organization, so a
//! session started under one account is hidden after switching to another,
//! although its transcript is shared in `~/.claude/projects`. A sync adds the
//! missing list entries to each partition Desktop already created. It never
//! copies or edits a transcript, and it changes a Desktop file only when its
//! ledger proves this tool wrote that file and nobody changed it since.
//!
//! Desktop reads a partition again only on an account switch or a restart, so
//! a run reports `refresh_needed` instead of forcing a reload.

mod background;
mod bounded;
mod desktop;
mod fingerprint;
mod ids;
mod json_scan;
mod ledger;
mod lock;
mod paths;
mod payload;
mod plan;
mod platform;
mod publish;
mod report;
mod skips;
mod staging;
mod state;
mod sync;
mod timestamp;
mod transcripts;

use crate::cli::{BackgroundAction, SessionsCommand};
use crate::context::AppContext;
use anyhow::{Result, bail};
use background::BackgroundStatus;
use paths::SessionPaths;
use report::{RunReport, RunStatus};
use serde_json::json;
use sync::RunKind;

/// What `status` shows about session sharing.
pub(crate) struct SessionsSummary {
    /// Whether the background job is installed and loaded; `None` where it
    /// is unavailable, which is anywhere but macOS.
    pub(crate) background: Option<bool>,
    pub(crate) interval_seconds: Option<u64>,
    /// RFC 3339 UTC time of the last full sync, manual or background.
    pub(crate) last_run_at: Option<String>,
    pub(crate) added: u64,
    pub(crate) refreshed: u64,
    pub(crate) removed: u64,
    /// The last sync changed entries that Desktop shows only after an
    /// account switch or a restart.
    pub(crate) refresh_needed: bool,
}

/// Read the background job's state and the last sync's summary.
///
/// # Errors
///
/// Fails when `HOME` is unset, when a state file is unreadable or a symlink,
/// or when `launchctl` cannot be run on macOS.
pub(crate) fn summary(ctx: &AppContext) -> Result<SessionsSummary> {
    let paths = SessionPaths::resolve(ctx, None, None)?;
    let background = background::status(&paths)?;
    let last_run: Option<RunReport> = state::read_json(&paths.last_run())?;
    let applied = last_run.as_ref().map(|report| report.applied).unwrap_or_default();
    Ok(SessionsSummary {
        background: background
            .supported
            .then_some(background.installed && background.loaded),
        interval_seconds: background.interval,
        last_run_at: last_run.as_ref().map(|report| report.checked_at.clone()),
        added: applied.add,
        refreshed: applied.refresh,
        removed: applied.remove,
        refresh_needed: last_run.is_some_and(|report| report.refresh_needed),
    })
}

/// Run one `sessions` subcommand.
pub(crate) fn run(ctx: &AppContext, command: SessionsCommand) -> Result<()> {
    match command {
        SessionsCommand::Sync {
            dry_run,
            json,
            background,
            desktop_root,
            projects_root,
        } => {
            let paths =
                SessionPaths::resolve(ctx, desktop_root.as_deref(), projects_root.as_deref())?;
            if background {
                run_background(&paths);
                return Ok(());
            }
            sync_now(&paths, dry_run, json)
        }
        SessionsCommand::Status { json } => print_status(ctx, json),
        SessionsCommand::Background { action } => {
            let paths = SessionPaths::resolve(ctx, None, None)?;
            match action {
                BackgroundAction::On { interval } => turn_on(&paths, interval),
                BackgroundAction::Off => turn_off(&paths),
                BackgroundAction::Plist { interval } => {
                    let program = background::current_program()?;
                    print!(
                        "{}",
                        background::render_plist(&program, interval, &paths.sessions_root())
                    );
                    Ok(())
                }
            }
        }
    }
}

fn sync_now(paths: &SessionPaths, dry_run: bool, json: bool) -> Result<()> {
    let kind = if dry_run {
        RunKind::DryRun
    } else {
        RunKind::Write { background: false }
    };
    let Some(report) = sync::full_run(paths, kind)? else {
        bail!("another sessions sync is running; try again in a moment");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        report::print(&report);
    }
    if report.status == RunStatus::Error {
        bail!("some session entries could not be written");
    }
    Ok(())
}

/// The launch agent's entry point: no output, and a failing exit code on error.
///
/// launchd discards output anyway; printing nothing also keeps a manual
/// `--background` run from showing anything a log could capture.
fn run_background(paths: &SessionPaths) {
    let ok = sync::background_run(paths).unwrap_or(false);
    if !ok {
        // Every guard, including the run lock, was dropped inside the run.
        std::process::exit(1);
    }
}

fn print_status(ctx: &AppContext, json: bool) -> Result<()> {
    if json {
        let paths = SessionPaths::resolve(ctx, None, None)?;
        let background: BackgroundStatus = background::status(&paths)?;
        let last_run: Option<RunReport> = state::read_json(&paths.last_run())?;
        let value = json!({ "background": background, "last_run": last_run });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let summary = summary(ctx)?;
    match (summary.background, summary.interval_seconds) {
        (None, _) => println!("background: not available on this platform"),
        (Some(true), Some(interval)) => println!("background: on, every {interval}s"),
        (Some(true), None) => println!("background: on"),
        (Some(false), _) => println!("background: off"),
    }
    let Some(last_run_at) = summary.last_run_at else {
        println!("last sync: never; run `sessions sync`");
        return Ok(());
    };
    let paths = SessionPaths::resolve(ctx, None, None)?;
    let last_run: Option<RunReport> = state::read_json(&paths.last_run())?;
    let status = last_run
        .as_ref()
        .map_or("unknown", |report| report.status.as_str());
    println!("last sync: {last_run_at} ({status})");
    if let Some(report) = &last_run {
        report::print_overview(report);
    }
    report::print_changes(report::Changes {
        add: summary.added,
        refresh: summary.refreshed,
        remove: summary.removed,
    });
    if let Some(report) = &last_run {
        report::print_details(report);
    }
    if summary.refresh_needed {
        report::print_refresh_hint();
    }
    Ok(())
}

fn turn_on(paths: &SessionPaths, interval: u32) -> Result<()> {
    let status = background::turn_on(paths, interval)?;
    if !status.loaded {
        bail!("the launch agent was installed but launchd did not load it");
    }
    println!(
        "background sync on: at login, every {interval}s, and when a Desktop account appears"
    );
    Ok(())
}

fn turn_off(paths: &SessionPaths) -> Result<()> {
    let status = background::turn_off(paths)?;
    if status.installed || status.loaded {
        bail!("the launch agent is still present; check `sessions status`");
    }
    println!("background sync off; `sessions sync` still works by hand");
    Ok(())
}
