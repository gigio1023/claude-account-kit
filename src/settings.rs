//! Claude Code settings mutation.
//!
//! This module owns `settings.json` edits so the app can install a statusline
//! wrapper without losing a user's existing statusline command.

use crate::context::AppContext;
use crate::shell;
use crate::time::now_epoch;
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn install_statusline(ctx: &AppContext) -> Result<()> {
    ctx.ensure_app_dir()?;
    ctx.ensure_claude_dir()?;

    let settings_path = ctx.settings_path();
    let mut settings = load_settings(&settings_path)?;
    let command = statusline_command()?;
    let backup = back_up(&settings_path)?;

    if let Some(existing) = current_command(&settings)
        && !is_our_statusline_command(&existing)
    {
        fs::write(ctx.inner_statusline_path(), &existing)
            .context("failed to save prior statusline command")?;
    }

    // Only the command is replaced. Claude Code accepts other keys on this
    // object, such as `padding`, and they belong to the user rather than to the
    // wrapper.
    match settings.get_mut("statusLine").filter(|value| value.is_object()) {
        Some(entry) => {
            entry["type"] = json!("command");
            entry["command"] = json!(command);
        }
        None => {
            settings["statusLine"] = json!({
                "type": "command",
                "command": command,
            });
        }
    }
    save_settings(&settings_path, &settings)?;
    if let Some(backup) = backup {
        println!("backed up {} to {}", settings_path.display(), backup.display());
    }
    println!("installed Claude Code statusLine wrapper");
    Ok(())
}

pub(crate) fn uninstall_statusline(ctx: &AppContext) -> Result<()> {
    let settings_path = ctx.settings_path();
    let mut settings = load_settings(&settings_path)?;
    let current = current_command(&settings).unwrap_or_default();

    if !is_our_statusline_command(&current) {
        println!("statusLine wrapper is not installed");
        return Ok(());
    }

    let backup = back_up(&settings_path)?;
    let inner_path = ctx.inner_statusline_path();
    if inner_path.exists() {
        let inner = fs::read_to_string(&inner_path)
            .context("failed to read saved prior statusline command")?;
        settings["statusLine"]["command"] = json!(inner.trim());
    } else if let Some(object) = settings.as_object_mut() {
        object.remove("statusLine");
    }

    save_settings(&settings_path, &settings)?;
    if let Some(backup) = backup {
        println!("backed up {} to {}", settings_path.display(), backup.display());
    }
    println!("uninstalled Claude Code statusLine wrapper");
    Ok(())
}

/// Copy `settings.json` aside before editing it.
///
/// This file is hand-maintained and holds far more than the statusline, so a
/// bad edit is expensive. The name matches the `settings.json.bak-*` files
/// Claude Code itself leaves behind, and a fresh one is written per run rather
/// than overwriting the previous copy.
fn back_up(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    // Two edits in the same second would otherwise share a name, and the
    // second copy would bury the state the first one preserved.
    let stamp = now_epoch();
    let mut backup = path.with_extension(format!("json.bak-{stamp}"));
    for attempt in 1..100 {
        if !backup.exists() {
            break;
        }
        backup = path.with_extension(format!("json.bak-{stamp}-{attempt}"));
    }
    fs::copy(path, &backup)
        .with_context(|| format!("failed to back up {} to {}", path.display(), backup.display()))?;
    Ok(Some(backup))
}

fn current_command(settings: &Value) -> Option<String> {
    settings
        .get("statusLine")
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn statusline_command() -> Result<String> {
    let exe = env::current_exe().context("failed to find current executable")?;
    Ok(format!("{} statusline", shell::quote(&exe)))
}

/// Binary names whose `statusline` command is this tool's wrapper.
///
/// The second is the name before the rename. A wrapper installed under it must
/// still be recognized, or `statusline-install` would save it as the user's own
/// command and the wrapper would end up running itself, and
/// `statusline-uninstall` would leave it in place.
const OUR_BINARY_NAMES: [&str; 2] = ["claude-account-kit", "claude-quota-router"];

fn is_our_statusline_command(command: &str) -> bool {
    command.contains("statusline") && OUR_BINARY_NAMES.iter().any(|name| command.contains(name))
}

fn load_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

fn save_settings(path: &Path, settings: &Value) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(settings)?),
    )
    .with_context(|| format!("failed to write {}", path.display()))
}
