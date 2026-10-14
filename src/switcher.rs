//! Account switching transaction.
//!
//! Switching touches both secret storage and local metadata. Keeping the flow in
//! one module makes it easier to see the ordering: refresh the current backup,
//! put the target account's login into the active credential, update state,
//! then reset quota side effects.

use crate::claude;
use crate::context::AppContext;
use crate::credentials;
use crate::domain::{AccountName, State};
use crate::notification;
use crate::storage;
use crate::time::now_epoch;
use crate::util::confirm;
use anyhow::{Context, Result, bail};

#[derive(Clone, Copy, Debug)]
pub(crate) struct SwitchOptions {
    pub(crate) yes: bool,
    pub(crate) emit: bool,
}

pub(crate) fn switch_to(
    ctx: &AppContext,
    name: &AccountName,
    options: SwitchOptions,
) -> Result<()> {
    let mut state = storage::load_state(ctx)?;
    if !state.accounts.contains_key(name.as_str()) {
        bail!("account is not saved: {name}");
    }
    confirm(&format!("switch to {name}?"), options.yes)?;

    let active_credential = credentials::read_active(ctx).unwrap_or_default();
    refresh_current_backup(ctx, &state, name, &active_credential)?;

    let target_credential = credentials::read_saved(ctx, name)
        .with_context(|| format!("failed to read saved credential for {name}"))?;
    if !claude::has_oauth_token(&target_credential) {
        bail!(
            "the saved credential for {name} carries no token, so activating it would log you out; \
             sign in as that account and run `setup {name}` again"
        );
    }
    let old_current = state.current_account.clone();

    if claude::same_login(&active_credential, &target_credential) {
        state.current_account = Some(name.to_string());
        storage::save_state(ctx, &state)?;
        if options.emit {
            println!("already using account: {name}");
        }
        return Ok(());
    }

    let switched = claude::with_login(&active_credential, &target_credential);
    credentials::write_active(ctx, &switched)
        .with_context(|| format!("failed to activate account {name}"))?;

    if old_current.as_deref() != Some(name.as_str()) {
        state.previous_account = old_current;
    }
    state.current_account = Some(name.to_string());
    state.switched_at = Some(now_epoch());
    storage::save_state(ctx, &state)?;

    clear_quota_side_effects(ctx, name)?;

    if options.emit {
        println!("switched to account: {name}");
        println!("restart Claude Code if the running session does not pick up the new credential");
        println!("Claude Desktop keeps its own login; switch it in the Desktop app");
    }
    Ok(())
}

pub(crate) fn detect_current_account_by_credential(
    ctx: &AppContext,
    state: &mut State,
) -> Result<Option<String>> {
    let active = credentials::read_active(ctx)?;
    let found = saved_account_for_credential(ctx, state, &active)?;
    if let Some(name) = found.as_ref() {
        state.current_account = Some(name.clone());
    }
    Ok(found)
}

/// The saved account holding this credential's login, if one does.
///
/// Logins are compared rather than trusted from a name, because the only thing
/// that reliably identifies a login here is the token itself.
pub(crate) fn saved_account_for_credential(
    ctx: &AppContext,
    state: &State,
    credential: &str,
) -> Result<Option<String>> {
    for name in state.accounts.keys() {
        let account_name = AccountName::parse(name)?;
        if let Ok(saved) = credentials::read_saved(ctx, &account_name)
            && claude::same_login(&saved, credential)
        {
            return Ok(Some(name.clone()));
        }
    }
    Ok(None)
}

/// Refresh the saved credential for the currently active account before switching.
///
/// Claude Code may refresh OAuth tokens while the account is active. Capturing the
/// current credential immediately before switching prevents restoring an older
/// token the next time the user switches back to this account.
fn refresh_current_backup(
    ctx: &AppContext,
    state: &State,
    target: &AccountName,
    active_credential: &str,
) -> Result<()> {
    let Some(current_account) = state.current_account.as_deref() else {
        return Ok(());
    };
    if current_account == target.as_str() || !state.accounts.contains_key(current_account) {
        return Ok(());
    }
    // After `claude auth logout` the active record is still there with its
    // tokens blanked. Copying that over the saved account would destroy the
    // only working copy of that login.
    if !claude::has_oauth_token(active_credential) {
        return Ok(());
    }
    // A credential carries no account identity, so the only proof available that
    // the active one is not some other login is that it is not already filed
    // under another name. That happens when the user signs in as an account the
    // router already knows and does not run `setup`.
    if let Some(owner) = saved_account_for_credential(ctx, state, active_credential)?
        && owner != current_account
    {
        bail!(
            "the active credential is saved as {owner}, but the router has {current_account} as \
             the current account; run `setup` to record the account you are signed in to"
        );
    }

    let current_name = AccountName::parse(current_account)?;
    let login = claude::account_login(active_credential);
    credentials::write_saved(ctx, &current_name, &login)
        .with_context(|| format!("failed to update current account backup for {current_account}"))
}

/// Drop the quota state that the switch invalidates.
///
/// The target's cached reading describes the moment the router left it, so it is
/// discarded and re-observed. Readings for every other account are kept: they are
/// what the statusline counts down and what selection skips over. Notification
/// markers are cleared so the next quota event is announced again.
fn clear_quota_side_effects(ctx: &AppContext, target: &AccountName) -> Result<()> {
    let mut cache = storage::load_rate_limits(ctx)?;
    if cache.accounts.remove(target.as_str()).is_some() {
        storage::save_rate_limits(ctx, &cache)?;
    }
    notification::clear_notified(ctx)
}
