//! Deterministic account selection across plans.
//!
//! Every saved account is a candidate, whatever plan it is on. The user ranks
//! accounts with `config --priority`, and anything left unranked falls in behind
//! by plan kind. Selection then reads the per-account quota cache: an account
//! whose last reading was at its limit is skipped until that limit resets, and
//! the router returns to a higher ranked account once its reset has passed.

use crate::domain::{Config, RateLimitCache, State};

/// Saved accounts in the order the router should prefer them.
pub(crate) fn ordered_accounts(state: &State, config: &Config) -> Vec<String> {
    let mut order: Vec<String> = Vec::with_capacity(state.accounts.len());
    for name in &config.priority {
        if state.accounts.contains_key(name) && !order.iter().any(|seen| seen == name) {
            order.push(name.clone());
        }
    }

    let mut rest: Vec<&String> = state
        .accounts
        .keys()
        .filter(|name| !order.iter().any(|seen| &seen == name))
        .collect();
    rest.sort_by_key(|name| {
        (
            state.accounts.get(*name).map(|entry| entry.kind.rank()),
            (*name).clone(),
        )
    });
    order.extend(rest.into_iter().cloned());
    order
}

/// The account to move to when the current one cannot be used.
pub(crate) fn next_target(
    state: &State,
    config: &Config,
    cache: &RateLimitCache,
    current: &str,
    now: i64,
) -> Option<String> {
    ordered_accounts(state, config)
        .into_iter()
        .find(|name| name != current && !is_blocked(cache, name, now))
}

/// A higher ranked account whose quota has come back.
pub(crate) fn return_target(
    state: &State,
    config: &Config,
    cache: &RateLimitCache,
    current: &str,
    now: i64,
) -> Option<String> {
    // Only accounts the router left at their limit come back into play here.
    // Without that condition a manual move down the list would be undone on the
    // next statusline render.
    higher_ranked(state, config, current)
        .into_iter()
        .find(|name| {
            cache
                .accounts
                .get(name)
                .is_some_and(|snapshot| snapshot.is_recovered(now))
        })
}

/// A higher ranked account that is still waiting for its limit to reset.
pub(crate) fn pending_recovery(
    state: &State,
    config: &Config,
    cache: &RateLimitCache,
    current: &str,
    now: i64,
) -> Option<(String, &'static str, i64)> {
    higher_ranked(state, config, current)
        .into_iter()
        .find_map(|name| {
            let snapshot = cache.accounts.get(&name)?;
            let (label, reset) = snapshot.blocking_reset(now)?;
            Some((name, label, reset))
        })
}

fn higher_ranked(state: &State, config: &Config, current: &str) -> Vec<String> {
    let order = ordered_accounts(state, config);
    let position = order.iter().position(|name| name == current);
    match position {
        Some(index) => order.into_iter().take(index).collect(),
        None => order,
    }
}

fn is_blocked(cache: &RateLimitCache, name: &str, now: i64) -> bool {
    cache
        .accounts
        .get(name)
        .is_some_and(|snapshot| snapshot.is_blocked(now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AccountEntry, AccountKind, LimitWindow, RateLimitSnapshot, RoutingMode};
    use std::collections::BTreeMap;

    fn state_with(accounts: &[(&str, AccountKind)]) -> State {
        let mut entries = BTreeMap::new();
        for (name, kind) in accounts {
            entries.insert(
                (*name).to_string(),
                AccountEntry {
                    kind: *kind,
                    created_at: 1,
                    updated_at: 1,
                    account_uuid: None,
                    org_uuid: None,
                },
            );
        }
        State {
            current_account: None,
            previous_account: None,
            accounts: entries,
            switched_at: None,
        }
    }

    fn config_with(priority: &[&str]) -> Config {
        Config {
            alert_at: 95,
            mode: RoutingMode::Manual,
            priority: priority.iter().map(|name| (*name).to_string()).collect(),
        }
    }

    fn cache_with(entries: &[(&str, u8, i64)]) -> RateLimitCache {
        let mut accounts = BTreeMap::new();
        for (name, pct, reset) in entries {
            accounts.insert(
                (*name).to_string(),
                RateLimitSnapshot {
                    detected_at: 100,
                    five_hour: LimitWindow {
                        used_percentage: Some(*pct),
                        resets_at: Some(*reset),
                    },
                    seven_day: LimitWindow::default(),
                    spend_limit: LimitWindow::default(),
                },
            );
        }
        RateLimitCache { accounts }
    }

    #[test]
    fn skips_accounts_that_are_still_at_their_limit() {
        let state = state_with(&[
            ("personal-main", AccountKind::Personal),
            ("team-main", AccountKind::Team),
            ("enterprise-main", AccountKind::Enterprise),
        ]);
        let config = config_with(&["personal-main", "team-main", "enterprise-main"]);
        let cache = cache_with(&[("personal-main", 100, 5000), ("team-main", 100, 5000)]);

        assert_eq!(
            next_target(&state, &config, &cache, "personal-main", 1000).as_deref(),
            Some("enterprise-main")
        );
    }

    #[test]
    fn returns_to_a_higher_ranked_account_after_its_reset() {
        let state = state_with(&[
            ("personal-main", AccountKind::Personal),
            ("team-main", AccountKind::Team),
        ]);
        let config = config_with(&["team-main", "personal-main"]);
        let cache = cache_with(&[("team-main", 100, 2000)]);

        assert_eq!(
            return_target(&state, &config, &cache, "personal-main", 1500),
            None
        );
        assert_eq!(
            return_target(&state, &config, &cache, "personal-main", 2500).as_deref(),
            Some("team-main")
        );
    }

    #[test]
    fn does_not_pull_back_to_a_lower_ranked_account() {
        let state = state_with(&[
            ("personal-main", AccountKind::Personal),
            ("team-main", AccountKind::Team),
        ]);
        let config = config_with(&["personal-main", "team-main"]);
        let cache = cache_with(&[("team-main", 100, 2000)]);

        assert_eq!(
            return_target(&state, &config, &cache, "personal-main", 2500),
            None
        );
    }
}
