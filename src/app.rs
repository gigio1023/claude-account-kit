//! Application use cases.
//!
//! The `App` type is the orchestration layer. It coordinates modules with
//! side effects but leaves their implementation details in their own files.

use crate::claude;
use crate::cli::Commands;
use crate::context::AppContext;
use crate::domain::{
    AccountEntry, AccountKind, AccountName, Config, RateLimitSnapshot, RoutingMode, State,
};
use crate::credentials;
use crate::desktop;
use crate::routing;
use crate::sessions;
use crate::settings;
use crate::statusline;
use crate::storage;
use crate::switcher::{self, SwitchOptions};
use crate::time::now_epoch;
use crate::util::{confirm, display_pct, humanize};
use anyhow::{Context, Result, anyhow, bail};
use std::fs;

pub(crate) struct App {
    ctx: AppContext,
}

impl App {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            ctx: AppContext::new()?,
        })
    }

    pub(crate) fn handle(&self, command: Commands) -> Result<()> {
        if !matches!(command, Commands::Statusline) {
            self.ctx.report_migration();
        }
        match command {
            Commands::Setup { name, kind } => self.setup(name, kind),
            Commands::Switch { name, yes } => self.switch(&name, yes),
            Commands::Toggle { yes } => self.toggle(yes),
            Commands::List => self.list(),
            Commands::Remove { name } => self.remove(&name),
            Commands::Current => self.current(),
            Commands::Status => self.status(),
            Commands::Config {
                alert_at,
                mode,
                priority,
            } => self.config(alert_at, mode, priority),
            Commands::StatuslineInstall => settings::install_statusline(&self.ctx),
            Commands::StatuslineUninstall => settings::uninstall_statusline(&self.ctx),
            Commands::Purge { yes } => self.purge(yes),
            Commands::Statusline => statusline::handle(&self.ctx),
            Commands::Sessions { command } => sessions::run(&self.ctx, command),
        }
    }

    /// Save the active credential under an account name.
    ///
    /// The name defaults to the account's email address so that two logins are
    /// told apart by the thing that actually differs between them.
    fn setup(&self, name: Option<String>, kind: Option<AccountKind>) -> Result<()> {
        let status = claude::auth_status();
        let name = if let Some(value) = name {
            AccountName::parse(&value)?
        } else {
            let email = status
                .as_ref()
                .and_then(|status| status.email.as_deref())
                .ok_or_else(|| {
                    anyhow!("claude auth status reported no email; pass an account name")
                })?;
            AccountName::parse(email)?
        };
        self.ctx.ensure_app_dir()?;

        let mut state = storage::load_state(&self.ctx)?;
        let credential = credentials::read_active(&self.ctx)
            .context("failed to read the active Claude Code credential")?;
        if !claude::has_oauth_token(&credential) {
            bail!("Claude Code is signed out; run `claude auth login` and try again");
        }
        // Prefer an explicit CLI override for import and migration cases,
        // then what Claude Code reports, then the credential JSON shape.
        let kind = kind
            .or_else(|| status.as_ref().and_then(|status| status.kind))
            .or_else(|| claude::detect_account_kind_from_credential(&credential))
            .unwrap_or(AccountKind::Other);

        // `claude auth status` answers from a profile Claude Code caches between
        // refreshes, so shortly after a switch it can still name the account the
        // router just left. Saving then would file this credential under the
        // wrong login and make a later switch hand back the wrong token.
        if let Some(existing) = switcher::saved_account_for_credential(&self.ctx, &state, &credential)?
            && existing != name.as_str()
        {
            bail!(
                "the active credential is already saved as {existing}, but this would save it as \
                 {name}; log in as the account you mean, or run `remove {existing}` first"
            );
        }

        let login = claude::account_login(&credential);
        credentials::write_saved(&self.ctx, &name, &login)
            .with_context(|| format!("failed to save account credential for {name}"))?;

        let now = now_epoch();
        let created_at = state
            .accounts
            .get(name.as_str())
            .map_or(now, |entry| entry.created_at);
        state.current_account = Some(name.to_string());
        state.accounts.insert(
            name.clone().into_string(),
            AccountEntry {
                kind,
                created_at,
                updated_at: now,
                account_uuid: status
                    .as_ref()
                    .and_then(|status| status.email.as_deref())
                    .and_then(claude::profile_account_uuid),
                org_uuid: status.as_ref().and_then(|status| status.org_id.clone()),
            },
        );
        storage::save_state(&self.ctx, &state)?;

        println!("saved account: {name} ({})", kind.as_str());
        Ok(())
    }

    fn switch(&self, name: &str, yes: bool) -> Result<()> {
        let name = AccountName::parse(name)?;
        switcher::switch_to(
            &self.ctx,
            &name,
            SwitchOptions { yes, emit: true },
        )
    }

    fn toggle(&self, yes: bool) -> Result<()> {
        let state = storage::load_state(&self.ctx)?;
        let previous = state
            .previous_account
            .as_deref()
            .ok_or_else(|| anyhow!("no previous account recorded"))?;
        if !state.accounts.contains_key(previous) {
            bail!("previous account is not saved: {previous}");
        }
        self.switch(previous, yes)
    }

    /// Print saved accounts in routing order with their last known quota.
    fn list(&self) -> Result<()> {
        let state = storage::load_state(&self.ctx)?;
        if state.accounts.is_empty() {
            println!("no accounts saved");
            return Ok(());
        }

        let config = storage::load_config(&self.ctx)?;
        let cache = storage::load_rate_limits(&self.ctx)?;
        let now = now_epoch();

        for (index, name) in routing::ordered_accounts(&state, &config)
            .into_iter()
            .enumerate()
        {
            let marker = if state.current_account.as_deref() == Some(name.as_str()) {
                "*"
            } else {
                " "
            };
            let kind = state
                .accounts
                .get(&name)
                .map_or("other", |entry| entry.kind.as_str());
            let quota = cache
                .accounts
                .get(&name)
                .map_or_else(|| "-".to_string(), |snapshot| describe_quota(snapshot, now));
            println!("{marker} {}. {name}\t{kind}\t{quota}", index + 1);
        }
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<()> {
        let name = AccountName::parse(name)?;
        let mut state = storage::load_state(&self.ctx)?;
        if state.current_account.as_deref() == Some(name.as_str()) {
            bail!("cannot remove the current account; switch to another account first");
        }
        if state.accounts.remove(name.as_str()).is_none() {
            bail!("account is not saved: {name}");
        }
        credentials::delete_saved(&self.ctx, &name).ok();
        if state.previous_account.as_deref() == Some(name.as_str()) {
            state.previous_account = None;
        }
        storage::save_state(&self.ctx, &state)?;

        let mut cache = storage::load_rate_limits(&self.ctx)?;
        if cache.accounts.remove(name.as_str()).is_some() {
            storage::save_rate_limits(&self.ctx, &cache)?;
        }
        println!("removed account: {name}");
        Ok(())
    }

    fn current(&self) -> Result<()> {
        let mut state = storage::load_state(&self.ctx)?;
        if let Some(current) = state.current_account {
            println!("{current}");
            return Ok(());
        }

        if let Some(detected) =
            switcher::detect_current_account_by_credential(&self.ctx, &mut state)?
        {
            storage::save_state(&self.ctx, &state)?;
            println!("{detected}");
            return Ok(());
        }

        println!("unknown");
        Ok(())
    }

    fn status(&self) -> Result<()> {
        let state = storage::load_state(&self.ctx)?;
        let config = storage::load_config(&self.ctx)?;
        let cache = storage::load_rate_limits(&self.ctx)?;
        let now = now_epoch();

        println!(
            "current: {}",
            state.current_account.as_deref().unwrap_or("unknown")
        );
        println!(
            "previous: {}",
            state.previous_account.as_deref().unwrap_or("none")
        );
        println!("credential store: {}", credentials::describe(&self.ctx));
        println!("signed in: {}", describe_active(&self.ctx));
        if let Some(login) = desktop::describe_login(&state) {
            println!("desktop: {login}");
        }
        println!(
            "note: switch changes the terminal login only; Claude Desktop signs in on its own"
        );
        print_config(&config, &state);
        println!("accounts: {}", state.accounts.len());
        println!("desktop sessions: {}", describe_sessions(&self.ctx));

        if cache.accounts.is_empty() {
            println!("quota: none");
            return Ok(());
        }
        println!("quota:");
        for (name, snapshot) in &cache.accounts {
            println!("  {name}\t{}", describe_quota(snapshot, now));
        }
        Ok(())
    }

    /// The counterpart of `setup`.
    ///
    /// Removes the saved credentials and the state directory, which is
    /// everything this tool owns apart from the binary and the `PATH` line
    /// `install.sh` wrote. The credential Claude Code is logged in with is
    /// never touched, and neither is the statusLine setting.
    fn purge(&self, yes: bool) -> Result<()> {
        let state = storage::load_state(&self.ctx)?;
        confirm(
            &format!(
                "delete {} saved credential(s) and {}?",
                state.accounts.len(),
                self.ctx.app_dir.display()
            ),
            yes,
        )?;

        for name in state.accounts.keys() {
            let name = AccountName::parse(name)?;
            let removed = credentials::delete_saved(&self.ctx, &name)
                .with_context(|| format!("failed to remove the saved credential for {name}"))?;
            if removed {
                println!("removed saved credential: {name}");
            } else {
                println!("no stored credential found for {name}");
            }
        }
        if sessions::stop_background(&self.ctx)
            .context("failed to stop the sessions launch agent")?
        {
            println!("stopped the sessions launch agent");
        }
        if self.ctx.app_dir.exists() {
            fs::remove_dir_all(&self.ctx.app_dir)
                .with_context(|| format!("failed to remove {}", self.ctx.app_dir.display()))?;
            println!("removed {}", self.ctx.app_dir.display());
        }
        println!("the Claude Code login is unchanged; only this tool's copies are gone");
        Ok(())
    }

    fn config(
        &self,
        alert_at: Option<u8>,
        mode: Option<RoutingMode>,
        priority: Option<Vec<String>>,
    ) -> Result<()> {
        self.ctx.ensure_app_dir()?;
        let state = storage::load_state(&self.ctx)?;
        let mut config = storage::load_config(&self.ctx)?;
        if let Some(alert_at) = alert_at {
            if alert_at > 100 {
                bail!("--alert-at must be between 0 and 100");
            }
            config.alert_at = alert_at;
        }
        if let Some(mode) = mode {
            config.mode = mode;
        }
        if let Some(priority) = priority {
            config.priority = clean_priority(priority, &state)?;
        }
        storage::save_config(&self.ctx, &config)?;
        print_config(&config, &state);
        Ok(())
    }
}

/// Validate a user supplied account order.
///
/// Names are accepted before the account they refer to is saved, because the
/// order is easier to write once than to revisit after every `setup`. An unsaved
/// name is reported and then ignored by routing until it exists.
fn clean_priority(priority: Vec<String>, state: &State) -> Result<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::with_capacity(priority.len());
    for value in priority {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let name = AccountName::parse(value)?;
        if !state.accounts.contains_key(name.as_str()) {
            eprintln!("warning: account is not saved yet: {name}");
        }
        if !cleaned.iter().any(|seen| seen == name.as_str()) {
            cleaned.push(name.into_string());
        }
    }
    Ok(cleaned)
}

/// Whether Claude Code currently holds a usable credential.
///
/// Worth one Keychain read on an explicit `status`: a signed-out Claude Code
/// sends a statusline payload with no quota in it, which otherwise looks like
/// the router having nothing to say.
fn describe_active(ctx: &crate::context::AppContext) -> &'static str {
    match credentials::read_active(ctx) {
        Ok(credential) if claude::has_oauth_token(&credential) => "yes",
        Ok(_) => "no, Claude Code is signed out; run `claude auth login`",
        Err(_) => "unknown, the active credential could not be read",
    }
}

/// One line on Desktop session sharing for `status`.
///
/// A failure here is reported in the line rather than failing `status`,
/// whose other lines are about the terminal login and still worth showing.
fn describe_sessions(ctx: &crate::context::AppContext) -> String {
    let summary = match sessions::summary(ctx) {
        Ok(summary) => summary,
        Err(error) => return format!("unknown, {error:#}"),
    };
    let background = match summary.background {
        None => "background unavailable",
        Some(true) => "background on",
        Some(false) => "background off",
    };
    let last = summary
        .last_run_at
        .map_or_else(|| "never synced".to_string(), |at| format!("last sync {at}"));
    let hint = if summary.refresh_needed {
        "; switch Desktop accounts or restart it to see new entries"
    } else {
        ""
    };
    format!("{background}, {last}{hint}; details: `sessions status`")
}

fn describe_quota(snapshot: &RateLimitSnapshot, now: i64) -> String {
    let usage = match snapshot.peak_usage() {
        Some((label, pct)) => format!("{}({label})", display_pct(Some(pct))),
        None => "-".to_string(),
    };
    match snapshot.blocking_reset(now) {
        Some((label, reset)) => format!("{usage} reset in {}({label})", humanize(reset - now)),
        None => usage,
    }
}

fn print_config(config: &Config, state: &State) {
    println!("alert_at={}", config.alert_at);
    println!("mode={}", config.mode.as_str());
    if config.priority.is_empty() {
        println!("priority=(plan kind order: personal, team, enterprise, other)");
    } else {
        println!("priority={}", config.priority.join(","));
    }
    let order = routing::ordered_accounts(state, config);
    if !order.is_empty() {
        println!("order={}", order.join(" -> "));
    }
}
