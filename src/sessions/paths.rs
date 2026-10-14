//! Where session sharing reads and writes.
//!
//! Claude Desktop's data is under `~/Library/Application Support/Claude`, and
//! the transcripts it resumes are under `~/.claude/projects` whatever
//! `CLAUDE_CONFIG_DIR` says, because Desktop starts the CLI without that
//! variable. This tool's own state is in the app directory, never inside
//! either of those roots.

use crate::context::AppContext;
use anyhow::{Context, Result, anyhow};
use std::env;
use std::path::{Path, PathBuf};

/// The launch agent label, also its plist's file name.
pub(super) const LAUNCH_AGENT_LABEL: &str = "io.github.gigio1023.claude-account-kit.sessions";

pub(super) struct SessionPaths {
    /// Claude Desktop's application data directory.
    pub(super) desktop_root: PathBuf,
    /// Claude Code's transcript directory.
    pub(super) projects_root: PathBuf,
    /// This tool's state directory.
    pub(super) state_dir: PathBuf,
    /// The earlier Python tool's record of the files it created.
    pub(super) legacy_ledger: PathBuf,
    pub(super) launch_agent: PathBuf,
}

impl SessionPaths {
    /// Resolve the default paths, with optional roots for fixture runs.
    pub(super) fn resolve(
        ctx: &AppContext,
        desktop_root: Option<&Path>,
        projects_root: Option<&Path>,
    ) -> Result<Self> {
        let home = home_dir()?;
        let support = home.join("Library").join("Application Support");
        let desktop_root = match desktop_root {
            Some(path) => absolute(path)?,
            None => support.join("Claude"),
        };
        let projects_root = match projects_root {
            Some(path) => absolute(path)?,
            None => home.join(".claude").join("projects"),
        };
        Ok(Self {
            desktop_root,
            projects_root,
            state_dir: absolute(&ctx.sessions_dir())?,
            legacy_ledger: support
                .join("Claude Local Sessions")
                .join("created-records.jsonl"),
            launch_agent: home
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{LAUNCH_AGENT_LABEL}.plist")),
        })
    }

    /// The directory of per-account session lists.
    pub(super) fn sessions_root(&self) -> PathBuf {
        self.desktop_root.join("claude-code-sessions")
    }

    pub(super) fn ledger(&self) -> PathBuf {
        self.state_dir.join("ledger.jsonl")
    }

    pub(super) fn last_run(&self) -> PathBuf {
        self.state_dir.join("last-run.json")
    }

    pub(super) fn fingerprint(&self) -> PathBuf {
        self.state_dir.join("fingerprint.json")
    }

    pub(super) fn lock(&self) -> PathBuf {
        self.state_dir.join("sync.lock")
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("failed to resolve {}", path.display()))
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("neither HOME nor USERPROFILE is set"))
}
