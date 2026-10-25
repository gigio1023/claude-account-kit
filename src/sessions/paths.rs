//! Where session sharing reads and writes.
//!
//! Claude Desktop's default data is under `~/Library/Application Support/Claude`;
//! additional profile roots must be selected explicitly. The
//! transcripts it resumes are under `~/.claude/projects` whatever
//! `CLAUDE_CONFIG_DIR` says, because Desktop starts the CLI without that
//! variable. This tool's own state is in the app directory, never inside
//! any of those roots.

use crate::context::AppContext;
use anyhow::{Context, Result, anyhow};
use std::env;
use std::path::{Component, Path, PathBuf};

/// The launch agent label, also its plist's file name.
pub(super) const LAUNCH_AGENT_LABEL: &str = "io.github.gigio1023.claude-account-kit.sessions";

pub(super) struct SessionPaths {
    /// Claude Desktop's application data directory.
    pub(super) desktop_root: PathBuf,
    /// Explicit additional Desktop user-data directories, never auto-discovered.
    pub(super) additional_desktop_roots: Vec<PathBuf>,
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
            additional_desktop_roots: Vec::new(),
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

    /// Add explicit user-data roots, deduplicated without creating directories.
    pub(super) fn with_additional_roots(mut self, roots: &[PathBuf]) -> Result<Self> {
        for root in roots {
            let root: PathBuf = absolute(root)?;
            if root != self.desktop_root && !self.additional_desktop_roots.contains(&root) {
                self.additional_desktop_roots.push(root);
            }
        }
        Ok(self)
    }

    /// The primary root followed by explicitly selected profile roots.
    pub(super) fn desktop_roots(&self) -> Vec<PathBuf> {
        std::iter::once(self.desktop_root.clone())
            .chain(self.additional_desktop_roots.iter().cloned())
            .collect()
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

    /// Hashes of profile roots whose session directories have been observed.
    pub(super) fn seen_roots(&self) -> PathBuf {
        self.state_dir.join("seen-desktop-roots.json")
    }

    pub(super) fn lock(&self) -> PathBuf {
        self.state_dir.join("sync.lock")
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let absolute: PathBuf = std::path::absolute(path)
        .with_context(|| format!("failed to resolve {}", path.display()))?;
    let mut normalized: PathBuf = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => normalized.push(component),
        }
    }
    Ok(normalized)
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("neither HOME nor USERPROFILE is set"))
}
