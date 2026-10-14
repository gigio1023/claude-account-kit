//! Claude Code account inspection.
//!
//! `claude auth status` is the CLI's own view of the account that the active
//! credential belongs to, so it is the source for both the account's email and
//! its plan. The credential JSON is a fallback for tests and offline setup flows
//! where the command is unavailable. The account uuid, which the command does
//! not report, comes from the profile Claude Code caches in its global config.

use crate::context;
use crate::domain::AccountKind;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::fs;
use std::process::Command;

#[derive(Clone, Debug, Default)]
pub(crate) struct AuthStatus {
    pub(crate) email: Option<String>,
    pub(crate) kind: Option<AccountKind>,
    /// The organization uuid, reported as `orgId`.
    pub(crate) org_id: Option<String>,
}

/// Ask Claude Code who is logged in.
///
/// One invocation answers everything `setup` asks of it, which keeps the
/// command from being spawned more than once for a single save.
pub(crate) fn auth_status() -> Option<AuthStatus> {
    let output = Command::new("claude")
        .args(["auth", "status"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_status(&output.stdout)
}

fn parse_status(payload: &[u8]) -> Option<AuthStatus> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    Some(AuthStatus {
        email: value
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|email| !email.is_empty()),
        kind: value
            .get("subscriptionType")
            .and_then(Value::as_str)
            .map(AccountKind::from_subscription),
        org_id: value
            .get("orgId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|org| !org.is_empty()),
    })
}

/// The slice of Claude Code's global config that names the signed-in account.
///
/// That file also holds project history, MCP server settings, and other data
/// that must not be logged, so these are the only fields deserialized from it.
#[derive(Deserialize)]
struct GlobalConfig {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<OauthAccount>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OauthAccount {
    account_uuid: Option<String>,
    email_address: Option<String>,
}

/// The account uuid in Claude Code's cached profile, if that profile is `email`'s.
///
/// The profile lags a switch until Claude Code refetches it, so right after one
/// it still describes the account that was left. Its uuid is used only when its
/// email is the one `claude auth status` reports for the login being saved.
pub(crate) fn profile_account_uuid(email: &str) -> Option<String> {
    let text = fs::read(context::claude_global_config_path().ok()?).ok()?;
    let config: GlobalConfig = serde_json::from_slice(&text).ok()?;
    let account = config.oauth_account?;
    let same_email = account
        .email_address
        .is_some_and(|address| address.eq_ignore_ascii_case(email));
    if !same_email {
        return None;
    }
    account.account_uuid.filter(|uuid| !uuid.is_empty())
}

/// Whether a credential actually carries an OAuth token.
///
/// `claude auth logout` leaves the credential record in place with its metadata
/// and empty token strings, and Claude Code reports that state as logged out.
/// Treating it as a credential would let the router back it up over a saved
/// account, or activate it and log the user out.
pub(crate) fn has_oauth_token(credential: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(credential) else {
        return false;
    };
    value
        .pointer("/claudeAiOauth/accessToken")
        .and_then(Value::as_str)
        .is_some_and(|token| !token.is_empty())
}

/// The keys of the credential record that belong to the signed-in account.
///
/// Claude Code keeps one record for the whole machine: next to the account's
/// login it holds MCP server OAuth tokens, plugin secrets, and gateway pins.
/// These are the keys Claude Code itself drops when another account signs in,
/// so they are the only ones a switch may replace. Every other key survives it.
const ACCOUNT_KEYS: [&str; 5] = [
    "claudeAiOauth",
    "organizationUuid",
    "trustedDeviceToken",
    "enterpriseGateway",
    "designOauth",
];

fn account_part(credential: &str) -> Map<String, Value> {
    let mut record = parse_record(credential);
    record.retain(|key, _| ACCOUNT_KEYS.contains(&key.as_str()));
    record
}

fn parse_record(credential: &str) -> Map<String, Value> {
    match serde_json::from_str(credential) {
        Ok(Value::Object(record)) => record,
        _ => Map::new(),
    }
}

/// The account's own part of a credential record, which is what gets saved.
///
/// A saved copy of the whole record would carry the machine's MCP tokens along,
/// and restoring it later would roll them back to that moment.
pub(crate) fn account_login(credential: &str) -> String {
    Value::Object(account_part(credential)).to_string()
}

/// Whether two credential records hold the same account login.
///
/// The rest of the record changes whenever an MCP server is authorized, so it
/// cannot take part in telling logins apart.
pub(crate) fn same_login(left: &str, right: &str) -> bool {
    let left = account_part(left);
    !left.is_empty() && left == account_part(right)
}

/// The active record with its account login replaced by a saved one.
pub(crate) fn with_login(active: &str, saved: &str) -> String {
    let mut record = parse_record(active);
    record.retain(|key, _| !ACCOUNT_KEYS.contains(&key.as_str()));
    record.extend(account_part(saved));
    Value::Object(record).to_string()
}

pub(crate) fn detect_account_kind_from_credential(credential: &str) -> Option<AccountKind> {
    let value: Value = serde_json::from_str(credential).ok()?;
    value
        .pointer("/claudeAiOauth/subscriptionType")
        .and_then(Value::as_str)
        .map(AccountKind::from_subscription)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_credential_whose_tokens_were_cleared() {
        assert!(has_oauth_token(r#"{"claudeAiOauth":{"accessToken":"abc"}}"#));
        // What `claude auth logout` leaves behind.
        assert!(!has_oauth_token(
            r#"{"claudeAiOauth":{"accessToken":"","subscriptionType":"max"}}"#
        ));
        assert!(!has_oauth_token("{}"));
        assert!(!has_oauth_token("not json"));
    }

    #[test]
    fn a_switch_replaces_only_the_account_login() {
        let active = r#"{"mcpOAuth":{"linear":"now"},"claudeAiOauth":{"accessToken":"a"},"trustedDeviceToken":"a"}"#;
        let saved = r#"{"mcpOAuth":{"linear":"then"},"claudeAiOauth":{"accessToken":"b"}}"#;
        let switched: Value = serde_json::from_str(&with_login(active, saved)).unwrap();
        assert_eq!(
            switched,
            serde_json::json!({"mcpOAuth":{"linear":"now"},"claudeAiOauth":{"accessToken":"b"}})
        );
        assert!(same_login(&switched.to_string(), saved));
        assert!(!same_login(active, saved));
        assert_eq!(
            account_login(saved),
            r#"{"claudeAiOauth":{"accessToken":"b"}}"#
        );
    }
}
