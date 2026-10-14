//! Filesystem context for Claude Account Kit.
//!
//! Paths are centralized here so command logic can refer to semantic locations
//! rather than rebuilding path strings across modules, and so the per platform
//! differences live in one place.

use anyhow::{Context, Result, anyhow};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const APP_HOME_ENV: &str = "CLAUDE_ACCOUNT_KIT_HOME";
/// The override's name before the tool was renamed from claude-quota-router.
/// Still honored when the new one is unset, so a setup that relies on it keeps
/// its directory.
const LEGACY_APP_HOME_ENV: &str = "CLAUDE_QUOTA_ROUTER_HOME";
const APP_DIR_NAME: &str = "claude-account-kit";
/// The default directory's name before the rename, moved to `APP_DIR_NAME` on
/// first use.
const LEGACY_APP_DIR_NAME: &str = "claude-quota-router";
const CLAUDE_HOME_ENV: &str = "CLAUDE_HOME";
/// Claude Code's own override for its config directory. Honoring it keeps
/// `statusline-install` from writing a `settings.json` Claude Code never reads.
const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

#[derive(Clone, Debug)]
pub(crate) struct AppContext {
    pub(crate) app_dir: PathBuf,
    pub(crate) claude_dir: PathBuf,
    /// What this run did with the pre-rename directory, if anything.
    pub(crate) migration: Option<LegacyMigration>,
}

/// What happened to the pre-rename default directory on this run.
#[derive(Clone, Debug)]
pub(crate) enum LegacyMigration {
    /// The directory was renamed to the new default location.
    Moved { from: PathBuf, to: PathBuf },
    /// The rename failed, so this run keeps using the old location.
    Kept { at: PathBuf, reason: String },
}

impl AppContext {
    pub(crate) fn new() -> Result<Self> {
        let app_home = env::var_os(APP_HOME_ENV).or_else(|| env::var_os(LEGACY_APP_HOME_ENV));
        let (app_dir, migration) = if let Some(path) = app_home {
            (PathBuf::from(path), None)
        } else {
            let parent = default_app_parent()?;
            resolve_default_app_dir(
                &parent.join(APP_DIR_NAME),
                &parent.join(LEGACY_APP_DIR_NAME),
            )
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
            migration,
        })
    }

    /// Tell the user, on stderr, what happened to the pre-rename directory.
    ///
    /// Only interactive commands call this. The statusline runs on every render
    /// and has to stay silent, so a move it performs goes unannounced.
    pub(crate) fn report_migration(&self) {
        match &self.migration {
            Some(LegacyMigration::Moved { from, to }) => eprintln!(
                "moved {} to {}: claude-quota-router is now claude-account-kit",
                from.display(),
                to.display()
            ),
            Some(LegacyMigration::Kept { at, reason }) => eprintln!(
                "warning: still using {}, because moving it to its new name failed: {reason}",
                at.display()
            ),
            None => {}
        }
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

/// Claude Code's global config file, which holds its cached account profile.
///
/// It sits beside the config directory rather than inside it: `~/.claude.json`
/// by default, and `<dir>/.claude.json` when `CLAUDE_CONFIG_DIR` relocates it.
pub(crate) fn claude_global_config_path() -> Result<PathBuf> {
    let base = match env::var_os(CLAUDE_CONFIG_DIR_ENV) {
        Some(path) => PathBuf::from(path),
        None => home_dir()?,
    };
    Ok(base.join(".claude.json"))
}

/// Claude Desktop's own `config.json`, on the platform where its layout is known.
///
/// `None` elsewhere, so callers skip anything that depends on Desktop.
pub(crate) fn desktop_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    if let Ok(home) = home_dir() {
        return Some(
            home.join("Library")
                .join("Application Support")
                .join("Claude")
                .join("config.json"),
        );
    }
    None
}

/// The per platform parent of the configuration directory.
///
/// macOS keeps the historical `~/.config` location rather than following
/// `XDG_CONFIG_HOME`, so an existing install does not lose its state when that
/// variable happens to be set.
fn default_app_parent() -> Result<PathBuf> {
    #[cfg(windows)]
    if let Some(appdata) = non_empty("APPDATA") {
        return Ok(PathBuf::from(appdata));
    }

    #[cfg(target_os = "linux")]
    if let Some(xdg) = non_empty("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg));
    }

    Ok(home_dir()?.join(".config"))
}

/// Pick the default directory, moving the pre-rename one into place first.
///
/// The statusline runs this on every render, so the settled case costs one
/// `exists` check. Both paths share a parent, which makes the move a single
/// atomic rename: a concurrent reader sees the old directory or the new one,
/// never a half-copied tree. The two are never merged, because a merge could
/// pick the wrong copy of a file and nothing would say so.
fn resolve_default_app_dir(new: &Path, legacy: &Path) -> (PathBuf, Option<LegacyMigration>) {
    if new.exists() || !legacy.exists() {
        return (new.to_path_buf(), None);
    }
    match fs::rename(legacy, new) {
        Ok(()) => (
            new.to_path_buf(),
            Some(LegacyMigration::Moved {
                from: legacy.to_path_buf(),
                to: new.to_path_buf(),
            }),
        ),
        // Another process, such as a second statusline render, moved it first.
        Err(_) if new.exists() => (new.to_path_buf(), None),
        // The old directory is still intact, so keep using it rather than
        // starting over in an empty one.
        Err(error) => (
            legacy.to_path_buf(),
            Some(LegacyMigration::Kept {
                at: legacy.to_path_buf(),
                reason: error.to_string(),
            }),
        ),
    }
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
