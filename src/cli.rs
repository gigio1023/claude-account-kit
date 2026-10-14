//! Command-line surface for the binary.
//!
//! This module owns only argument parsing. It deliberately delegates behavior to
//! [`App`](crate::app::App) so clap-specific structs do not become the place
//! where business rules accumulate.

use crate::app::App;
use crate::domain::{AccountKind, RoutingMode};
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Save the active Claude Code credential as a named account.
    Setup {
        /// Account name. Defaults to the email claude auth status reports.
        name: Option<String>,
        /// Override the plan kind. If omitted, claude auth status is used.
        #[arg(long, value_enum)]
        kind: Option<AccountKind>,
    },
    /// Switch the active Claude Code credential to a saved account.
    Switch {
        name: String,
        #[arg(short, long)]
        yes: bool,
    },
    /// Switch back to the previous account.
    Toggle {
        #[arg(short, long)]
        yes: bool,
    },
    /// List saved accounts.
    List,
    /// Remove a saved account.
    Remove { name: String },
    /// Print the current account name.
    Current,
    /// Show state, config, and cached quota information.
    Status,
    /// Read or update alert, routing, and account order settings.
    Config {
        #[arg(long)]
        alert_at: Option<u8>,
        #[arg(long, value_enum)]
        mode: Option<RoutingMode>,
        /// Account order, for example personal-main,team-main,enterprise-main.
        /// Pass an empty value to clear it.
        #[arg(long, value_delimiter = ',')]
        priority: Option<Vec<String>>,
    },
    /// Install the Claude Code statusLine wrapper.
    ///
    /// Named apart from `install.sh`, which only puts the binary on PATH and
    /// never edits Claude Code's settings.
    StatuslineInstall,
    /// Remove the Claude Code statusLine wrapper and restore the prior command.
    StatuslineUninstall,
    /// Delete every saved account credential and the router's own directory.
    ///
    /// Separate from `statusline-uninstall` because the two undo different
    /// things: one a setting in Claude Code, the other this tool's own copies.
    /// Neither removes the binary, which is `uninstall.sh`'s job.
    Purge {
        #[arg(short, long)]
        yes: bool,
    },
    /// Internal command used by Claude Code statusLine.
    Statusline,
    /// Show every local Claude Code session in every Claude Desktop account.
    Sessions {
        #[command(subcommand)]
        command: SessionsCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum SessionsCommand {
    /// Add missing Desktop entries, refresh stale copies, remove orphaned ones.
    Sync {
        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Print the run summary as JSON.
        #[arg(long)]
        json: bool,
        /// Internal: the launch agent's run. Skips work when nothing changed
        /// and prints nothing; the exit code reports failure.
        #[arg(long, hide = true, conflicts_with_all = ["dry_run", "json"])]
        background: bool,
        /// Claude Desktop's data directory, for runs against a copy.
        #[arg(long, hide = true)]
        desktop_root: Option<PathBuf>,
        /// Claude Code's transcript directory, for runs against a copy.
        #[arg(long, hide = true)]
        projects_root: Option<PathBuf>,
    },
    /// Show the background job and the last sync.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Turn the macOS background sync on or off.
    Background {
        #[command(subcommand)]
        action: BackgroundAction,
    },
}

#[derive(Subcommand)]
pub(crate) enum BackgroundAction {
    /// Install a launch agent that syncs at login, on an interval, and when an account appears.
    On {
        /// Seconds between runs.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..))]
        interval: u32,
    },
    /// Stop and remove the launch agent; entries already added stay.
    Off,
    /// Print the launch agent plist without installing it.
    #[command(hide = true)]
    Plist {
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..))]
        interval: u32,
    },
}

/// Parse CLI input and execute the selected command.
///
/// Keeping this as a single public entrypoint prevents the rest of the crate
/// from exporting clap-specific details as public API.
///
/// # Errors
///
/// Returns the error of the selected command: an invalid argument, a missing
/// account, a Keychain or filesystem failure, or an unreadable settings file.
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let app = App::new()?;
    app.handle(cli.command)
}
