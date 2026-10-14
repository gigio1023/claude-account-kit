# Routing

## The account order

`config --priority` stores the order the router prefers, most preferred first.

```bash
claude-account-kit config --priority me@work.com,me@gmail.com
```

Accounts left out of the list fall in behind it by plan kind, in the order personal, team, enterprise, other, and alphabetically within a kind. Pass `--priority ''` to clear the list and fall back to plan kind alone. `list` and `status` both print the resulting order, so there is no need to work it out from the rules.

The plan kind comes from the `subscriptionType` that `claude auth status` reports when the account is saved.

| `subscriptionType` | Kind |
|---|---|
| `max`, `pro`, `free` | `personal` |
| `team` | `team` |
| `enterprise` | `enterprise` |
| anything else | `other` |

`setup --kind` overrides it, which is what to use when an account should be ranked as something other than the plan it is on.

## What counts as out of quota

Every statusline render carries the quota of the account Claude Code is signed in to. The router files that reading in `rate-limits.json` under the current account, so each saved account has one reading: the state it was in when the router last left it.

A reading blocks an account when any window is at 100% and its `resets_at` is still ahead. A window at 100% that arrives with no `resets_at` blocks for one hour from the moment it was read, which bounds the damage without stranding the account.

An account has recovered when a window that was at 100% carries a `resets_at` that has now passed. Recovery is what brings the router back, and only an account the router left at its limit can trigger it. A manual move down the order is not undone on the next render.

## Choosing

| State | Action |
|---|---|
| The current account reaches 100% | Switch to the first account in the order that is not blocked |
| An account above the current one has recovered | Switch back to it |
| Every account is blocked | Stay put and say so in the statusline |

In `manual` mode, the default, the statusline reports these states and waits. In `auto` mode the router performs the switch itself and asks you to restart Claude Code.

```bash
claude-account-kit config --mode auto
```

## Two rules against stale readings

Only the account Claude Code is signed in to reports its own quota, so every other account is judged by the reading taken when the router last left it. That is a deliberate limit, not an approximation to be improved: there is no way to ask about an account you are not on.

After a switch the router ignores quota readings for 90 seconds. A running Claude Code session keeps reporting the quota of the account it started with until it picks up the new credential, and the statusline payload carries no account identity, so an unguarded router would file the old account's numbers under the new one and immediately switch back out.

## What the statusline prints

The current account name is always first, followed by whatever is worth acting on.

```text
me@gmail.com
me@gmail.com 97%(5h)
me@gmail.com 97%(5h) me@work.com reset in 2h10m(5h)
me@gmail.com me@work.com reset done -> claude-account-kit list
me@gmail.com LIMIT(5h) -> claude-account-kit list
```

Usage appears once it reaches `--alert-at`, which defaults to 95. The window in parentheses is the one closest to its limit: `5h`, `7d`, or `spend` for the spend window that gateway-billed accounts report.

A failed automatic switch appends `route failed` instead of emptying the line, so a locked or missing credential cannot take your own statusline command down with it.
