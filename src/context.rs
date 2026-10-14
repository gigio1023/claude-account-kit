//! Filesystem context for Claude Quota Router.
//!
//! Paths are centralized here so command logic can refer to semantic locations
//! rather than rebuilding path strings across modules, and so the per platform
//! differences live in one place.

use anyhow::{Context, Result, anyhow};
use std::env;
use std::fs;
use std::path::PathBuf;

const APP_HOME_ENV: &str = "CLAUDE_QUOTA_ROUTER_HOME";
const CLAUDE_HOME_ENV: &str = "CLAUDE_HOME";
/// Claude Code's own override for its config directory. Honoring it keeps
/// `statusline-install` from writing a `settings.json` Claude Code never reads.
const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

#[derive(Clone, Debug)]
pub(crate) struct AppContext {
    pub(crate) app_dir: PathBuf,
    pub(crate) claude_dir: PathBuf,
}

impl AppContext {
    pub(crate) fn new() -> Result<Self> {
        let app_dir = match env::var_os(APP_HOME_ENV) {
            Some(path) => PathBuf::from(path),
            None => default_app_dir()?,
        };

        let claude_dir = match env::var_os(CLAUDE_HOME_ENV)
            .or_else(|| env::var_os(CLAUDE_CONFIG_DIR_ENV))
        {
            Some(path) => PathBuf::from(path),
            None => home_dir()?.join(".claude"),
        };

        Ok(Self {
            app_dir,
            claude_dir,
        })
    }

    pub(crate) fn ensure_app_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.app_dir)
            .with_context(|| format!("failed to create {}", self.app_dir.display()))
    }

    pub(crate) fn ensure_claude_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.claude_dir)
            .with_context(|| format!("failed to create {}", self.claude_dir.display()))
    }

    pub(crate) fn state_path(&self) -> PathBuf {
        self.app_dir.join("state.json")
    }

    pub(crate) fn config_path(&self) -> PathBuf {
        self.app_dir.join("config.json")
    }

    pub(crate) fn rate_limits_path(&self) -> PathBuf {
        self.app_dir.join("rate-limits.json")
    }

    pub(crate) fn notified_path(&self) -> PathBuf {
        self.app_dir.join("notified.json")
    }

    pub(crate) fn inner_statusline_path(&self) -> PathBuf {
        self.app_dir.join("inner-statusline.txt")
    }

    pub(crate) fn settings_path(&self) -> PathBuf {
        self.claude_dir.join("settings.json")
    }

    /// Where `sessions` keeps its ledger, last run summary, fingerprint, and lock.
    pub(crate) fn sessions_dir(&self) -> PathBuf {
        self.app_dir.join("sessions")
    }

    /// Where saved account credentials live when the platform has no Keychain.
    pub(crate) fn accounts_dir(&self) -> PathBuf {
        self.app_dir.join("accounts")
    }

    /// Claude Code's own credential file on platforms without a Keychain.
    pub(crate) fn active_credential_path(&self) -> PathBuf {
        self.claude_dir.join(".credentials.json")
    }
}

/// The per platform configuration directory.
///
/// macOS keeps the historical `~/.config` location rather than following
/// `XDG_CONFIG_HOME`, so an existing install does not lose its state when that
/// variable happens to be set.
fn default_app_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    if let Some(appdata) = non_empty("APPDATA") {
        return Ok(PathBuf::from(appdata).join("claude-quota-router"));
    }

    #[cfg(target_os = "linux")]
    if let Some(xdg) = non_empty("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("claude-quota-router"));
    }

    Ok(home_dir()?.join(".config").join("claude-quota-router"))
}

#[cfg(any(windows, target_os = "linux"))]
fn non_empty(key: &str) -> Option<std::ffi::OsString> {
    env::var_os(key).filter(|value| !value.is_empty())
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("neither HOME nor USERPROFILE is set"))
}
