//! macOS Keychain backend.
//!
//! This is the only module that talks to the `security` command. Keeping the
//! boundary narrow makes it clear where secret material can enter process memory
//! and keeps persistence code from accidentally writing credentials to disk.

use crate::context::AppContext;
use crate::domain::AccountName;
use anyhow::{Context, Result, anyhow, bail};
use std::process::Command;
use std::sync::OnceLock;

const ACTIVE_SERVICE: &str = "Claude Code-credentials";
/// The service saved accounts are filed under. It is the tool's name from
/// before the rename to claude-account-kit, kept as a persisted format: every
/// saved login on a machine lives under it, and a new name would mean
/// rewriting each of those credentials, where one failed write loses a login.
const ACCOUNT_SERVICE: &str = "claude-quota-router";

pub(crate) fn describe(_ctx: &AppContext) -> String {
    format!("macOS Keychain, services {ACTIVE_SERVICE} and {ACCOUNT_SERVICE}")
}

/// Read the credential Claude Code currently uses.
///
/// The returned string is secret material. Callers should pass it directly to
/// another credential operation or to a short-lived in-memory comparison.
pub(crate) fn read_active(_ctx: &AppContext) -> Result<String> {
    read(ACTIVE_SERVICE, active_account()?)
}

/// Replace the active Claude Code credential with a previously saved account.
pub(crate) fn write_active(_ctx: &AppContext, credential: &str) -> Result<()> {
    upsert(ACTIVE_SERVICE, active_account()?, credential)
}

pub(crate) fn read_saved(_ctx: &AppContext, name: &AccountName) -> Result<String> {
    read(ACCOUNT_SERVICE, &stored_account(name))
}

/// Store an account credential without writing it to the filesystem.
pub(crate) fn write_saved(_ctx: &AppContext, name: &AccountName, credential: &str) -> Result<()> {
    upsert(ACCOUNT_SERVICE, &stored_account(name), credential)
}

/// Remove a saved account credential, reporting whether one was there.
///
/// An account that was never stored is not an error, so a purge finishes
/// instead of stopping at the first missing item. Anything else is an error:
/// a locked or unavailable Keychain must not read as a completed removal.
pub(crate) fn delete_saved(_ctx: &AppContext, name: &AccountName) -> Result<bool> {
    delete(ACCOUNT_SERVICE, &stored_account(name))
}

/// The Keychain account name Claude Code files its credential under.
///
/// It does not change while the process runs, and resolving it costs a
/// `security` invocation, so it is resolved once.
fn active_account() -> Result<&'static str> {
    static ACCOUNT: OnceLock<Option<String>> = OnceLock::new();
    ACCOUNT
        .get_or_init(|| detect_active_account().ok())
        .as_deref()
        .ok_or_else(|| {
            anyhow!("Claude Code credential not found in Keychain; log in with claude first")
        })
}

fn detect_active_account() -> Result<String> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", ACTIVE_SERVICE])
        .output()
        .context("failed to run security to detect active Claude Code account")?;
    if !output.status.success() {
        bail!("Claude Code credential not found in Keychain; log in with claude first");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_keychain_account(&text).ok_or_else(|| anyhow!("failed to parse active Keychain account"))
}

fn read(service: &str, account: &str) -> Result<String> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .output()
        .with_context(|| format!("failed to run security for service {service}"))?;
    if !output.status.success() {
        bail!("security find-generic-password failed for service {service} account {account}");
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end_matches('\n')
        .to_string())
}

/// Write a credential, confirm it landed, and undo the write if it did not.
///
/// Only a verified write counts. `security` has stored a silently truncated
/// value and still exited 0, and either item is dangerous half-written: the
/// active one logs the user out, and a saved one becomes the fragment that the
/// next switch installs. So the previous value goes back, or the item goes away
/// if there was no previous value, before the failure is reported.
fn upsert(service: &str, account: &str, password: &str) -> Result<()> {
    let previous = read(service, account).ok();
    let result = write_and_verify(service, account, password);
    if result.is_err() {
        match previous {
            Some(previous) => {
                write_password(service, account, &previous).ok();
            }
            None => {
                delete(service, account).ok();
            }
        }
    }
    result
}

fn write_and_verify(service: &str, account: &str, password: &str) -> Result<()> {
    write_password(service, account, password)?;
    let stored = read(service, account)
        .with_context(|| format!("failed to read back service {service} account {account}"))?;
    if stored != password {
        bail!("Keychain kept a different value for service {service} account {account}");
    }
    Ok(())
}

/// Hand the credential to `security`.
///
/// The token travels in this child process's argv, which every process running
/// as the same user can read out of `ps` while the call lasts. That is not a
/// capability it gains: the Keychain items themselves are readable by any
/// process of this user through the same `security` command. The alternatives
/// were both worse. `-w` with no value makes `security` prompt on stdin, but
/// that prompt truncates at 128 bytes and a Claude Code credential is over two
/// kilobytes, so it silently stored a fragment. Calling the Security framework
/// in process avoids argv entirely, but the Keychain ACL is keyed on the
/// calling binary, so reading Claude Code's own item would raise a modal
/// authorization dialog, and the statusline renders too often for that.
fn write_password(service: &str, account: &str, password: &str) -> Result<()> {
    let output = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            service,
            "-a",
            account,
            "-w",
        ])
        .arg(password)
        .output()
        .with_context(|| format!("failed to run security for service {service}"))?;
    if !output.status.success() {
        bail!(
            "security add-generic-password failed for service {service} account {account}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// `security` exits 44 when the item is not in the keychain at all.
const ITEM_NOT_FOUND: i32 = 44;

fn delete(service: &str, account: &str) -> Result<bool> {
    let output = Command::new("security")
        .args(["delete-generic-password", "-s", service, "-a", account])
        .output()
        .with_context(|| format!("failed to run security for service {service}"))?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(ITEM_NOT_FOUND) => Ok(false),
        _ => bail!(
            "security delete-generic-password failed for service {service} account {account}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

fn stored_account(name: &AccountName) -> String {
    format!("account:{name}")
}

fn parse_keychain_account(output: &str) -> Option<String> {
    for line in output.lines() {
        if !line.contains("\"acct\"") {
            continue;
        }
        let (_, value) = line.split_once("=\"")?;
        return Some(value.trim_end_matches('"').to_string());
    }
    None
}
