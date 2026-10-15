# Desktop session sharing

Claude Desktop lists Code sessions per account and organization. After you switch accounts in Desktop, the local sessions you started under the other account disappear from the sidebar, although their transcripts are still on disk in `~/.claude/projects`. `claude-account-kit sessions` puts every local session back in every account's list, on this machine only.

## How it works

Desktop keeps one small file per session and account:

```text
~/Library/Application Support/Claude/claude-code-sessions/<account uuid>/<org uuid>/local_<id>.json
```

Each file points at a Claude Code transcript through `cliSessionId`. A sync adds the files an account is missing, each pointing at the same transcript. No transcript is copied or edited, and nothing leaves the machine.

- **Sources.** Sessions Desktop already lists in any account, and sessions started from the terminal with `claude`, read from their transcript.
- **Skipped.** Archived and hidden sessions, SSH, WSL, and cloud sessions, subagent transcripts, and sessions whose working folder no longer exists. Desktop cannot open a session without its folder, so these come back on their own once the folder does.
- **Deletions.** A session you deleted in one account is never added back to that account.
- **Rewind and fork.** Desktop moves a session to a new transcript in the account where you rewound it and leaves the other accounts on the old one. A sync points the copies it wrote at the newest transcript.
- **Removed transcripts.** When every transcript of a session is gone, the copies the sync wrote are deleted. The entry Desktop wrote stays, and Desktop shows it as unavailable. When `~/.claude/projects` holds no transcript at all, nothing is deleted, because an empty or unmounted folder looks the same as every session being gone.

## What it never changes

The sync changes a Desktop file only when it can prove it wrote that file. Every write is recorded with the file's SHA-256 in `ledger.jsonl` in the tool's state directory. An entry Desktop wrote, or one Desktop changed after the sync wrote it, is left as it is; `sessions status` counts these as left alone. New files are published with a hard link, which fails instead of replacing a file Desktop created at the same moment.

The sync adds entries only to account folders Desktop has already created. Desktop creates one the first time an account is active.

## Commands

```bash
claude-account-kit sessions sync --dry-run   # what would change, without writing
claude-account-kit sessions sync             # apply it now
claude-account-kit sessions status           # background state and the last sync
claude-account-kit sessions background on    # sync at login and every 30 seconds
claude-account-kit sessions background off
```

`background on` installs a launch agent at `~/Library/LaunchAgents/io.github.gigio1023.claude-account-kit.sessions.plist` that runs the binary it was installed from. It also runs when a new account folder appears. A background run that finds no change in the account and project folders since the last full sync exits without reading any session, and a full sync runs at least every five minutes.

Desktop reads an account's list when it opens that account, so new entries show up after the next account switch or restart. The sync does not restart Desktop.

`claude-account-kit status` shows a one-line summary next to the terminal account, and `purge` turns the background sync off before it removes the state directory.

## Limits

- macOS only. The background job uses launchd, and the Desktop paths are the macOS ones.
- The sync reads the session file format of Claude Desktop 2.37. A Desktop update that changes it needs a recheck with `sessions sync --dry-run`.
- Titles and sort order are not kept in step across accounts; each account keeps what Desktop shows there.
- A brand-new account gets entries after Desktop has opened it once, and they appear after the next switch.

## Moving from claude-local-sessions

The sync takes over from the earlier `claude-local-sessions` script. On its first write run it imports that script's record of the files it created from `~/Library/Application Support/Claude Local Sessions/created-records.jsonl`, so those files count as the sync's own. Turn the old launch agent off before turning this one on.
