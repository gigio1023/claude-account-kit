//! Claude Desktop's own login.
//!
//! Claude Desktop runs its Code tab under its own login, which it hands to
//! Claude Code as `CLAUDE_CODE_OAUTH_TOKEN`, so switching the terminal login
//! never changes it. Reading which account Desktop last signed in with lets
//! `status` show the two logins side by side.

use crate::context;
use crate::domain::State;
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// How many leading characters of a uuid identify it in `status` output.
const UUID_PREFIX_LEN: usize = 8;

/// The one field read from Desktop's `config.json`.
///
/// The same file caches Desktop's OAuth tokens. Deserializing into a struct
/// that has only this field skips over them without keeping a copy.
#[derive(Deserialize)]
struct DesktopConfig {
    #[serde(rename = "lastKnownAccountUuid")]
    last_known_account_uuid: Option<String>,
}

/// The account Claude Desktop last signed in with, for the `status` line.
///
/// Names the saved account whose recorded uuid matches, and otherwise shows the
/// start of the uuid. `None` when Desktop is not installed, has recorded no
/// account, or runs on a platform whose config location is not known here.
pub(crate) fn describe_login(state: &State) -> Option<String> {
    let uuid = last_known_account_uuid(&context::desktop_config_path()?)?;
    let saved = state
        .accounts
        .iter()
        .find(|(_, entry)| entry.account_uuid.as_deref() == Some(uuid.as_str()))
        .map(|(name, _)| name.clone());
    Some(saved.unwrap_or_else(|| {
        let prefix = uuid.get(..UUID_PREFIX_LEN).unwrap_or(&uuid);
        format!("account id {prefix}, not linked to a saved account")
    }))
}

/// Read `lastKnownAccountUuid` and nothing else.
///
/// The file's bytes are dropped as soon as the parse finishes, and a parse
/// error is discarded rather than reported, so none of the cached tokens can
/// reach the output.
fn last_known_account_uuid(path: &Path) -> Option<String> {
    let config: DesktopConfig = {
        let bytes = fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()?
    };
    config
        .last_known_account_uuid
        .filter(|uuid| !uuid.is_empty())
}
