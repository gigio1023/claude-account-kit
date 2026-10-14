# Claude Account Kit

Claude Account Kit is for using several Claude accounts on one machine. It moves Claude Code's terminal login between saved accounts by the quota each has left, and it shares local Code sessions across the accounts you sign in to in Claude Desktop, as [docs/desktop-sessions.md](docs/desktop-sessions.md) describes.

Account routing keeps Claude Code on an account that still has quota. It reads the quota Claude Code hands to the statusline, remembers what each saved account had left when you were last on it, and moves Claude Code's login down an order you set. Only the login changes: MCP servers and plugins you authorized stay authorized on every account. Personal, team, and enterprise plans are all routed the same way.

![Claude Code runs claude-account-kit as its statusLine.command and hands it rate_limits. claude-account-kit files the reading under the current account in rate-limits.json, picks the first account the order in config.json still allows, and puts that account's saved login into Claude Code's credential, which Claude Code picks up at the next session.](docs/figures/switch-loop.svg)

## Install

Building needs Rust 1.88 or newer. On macOS and Linux:

```bash
./install.sh
```

The script builds the binary, copies it to `~/.local/bin`, and adds that directory to `PATH` in the file your login shell reads. It leaves the profile alone when the directory is already there. Open a new shell, then confirm:

```bash
claude-account-kit --version
```

[docs/install.md](docs/install.md) covers the options, a build by hand, and Windows.

## Renamed from claude-quota-router

This tool was called claude-quota-router. An existing setup carries over without signing in again.

- **Configuration directory.** The first run of the new binary renames `~/.config/claude-quota-router` to `~/.config/claude-account-kit`, with the saved account list, order, quota cache, and your previous statusline command inside. `CLAUDE_ACCOUNT_KIT_HOME` replaces `CLAUDE_QUOTA_ROUTER_HOME`, which still works when the new variable is unset.
- **Old binary path.** `install.sh` replaces a `claude-quota-router` binary in the install directory with a link to `claude-account-kit`, so the statusline command Claude Code already has keeps working.
- **Statusline setting.** `claude-account-kit statusline-install` rewrites the command to the new path and keeps the command it was wrapping.
- **Keychain service.** Saved logins stay under the service `claude-quota-router`. Moving them would mean rewriting every saved credential, so the old name is kept.

## Set up your accounts

Run `setup` once per account, each time while Claude Code is signed in to that account. The name defaults to the email, because that is what tells two logins apart.

1. Save the account you are signed in to now.

   ```bash
   claude auth status          # check which account this is
   claude-account-kit setup
   ```

2. Sign in as the next account and save that one too. Repeat for every account you want in the rotation.

   ```bash
   claude auth login
   claude-account-kit setup
   ```

   Do not run `claude auth logout` for this. Logging out empties the credential Claude Code is holding without removing the record, which leaves Claude Code signed out and its statusline with no quota to report. `setup` refuses to save a credential in that state, and `claude-account-kit switch <name>` puts a working one back.

3. Put the accounts in the order you want them used.

   ```bash
   claude-account-kit config --priority me@work.com,me@gmail.com
   ```

   An account left out of the list falls in behind it by plan kind, in the order personal, team, enterprise, other.

4. Hook up the statusline, then restart Claude Code.

   ```bash
   claude-account-kit statusline-install
   ```

   This wraps the statusline command you already have rather than replacing it. Your command keeps running on every render and its output is printed after claude-account-kit's own. Only `command` on the `statusLine` object changes, so keys such as `padding` survive, and `settings.json` is copied to `settings.json.bak-<timestamp>` first.

   That puts this tool's segment at the front of the line, which is one opinion about a setting that is yours. If you want it somewhere else, or your statusline is built in a way the wrapper does not suit, skip `statusline-install` and paste this into Claude Code instead:

   ```text
   I installed a CLI called claude-account-kit and want its segment in my Claude Code statusline.

   The command is `claude-account-kit statusline`. It reads the same JSON payload on stdin that
   Claude Code gives any statusLine command, prints one short segment on stdout, prints nothing
   when it has nothing to report, and always exits 0.

   Please wire it into statusLine.command in ~/.claude/settings.json. If I already have a
   statusLine command, keep it and show both parts, and put its segment wherever suits
   my line best. Watch out for stdin: it can only be read once, so buffer the payload and give
   the same text to each command rather than piping stdin straight through to both.

   Back the file up before you edit it, and show me the command you ended up with.
   ```

   Wired this way claude-account-kit prints only its own segment, because the command it would otherwise re-run is the one `statusline-install` records and you never ran it.

5. Move to the account you want to start on, then check the result.

   ```bash
   claude-account-kit switch me@work.com
   claude-account-kit list
   ```

   ```
   * 1. me@work.com     team        42%(5h)
     2. me@gmail.com    personal    -
   ```

   The star marks the account in use, the number is its place in the order, and the last column is the most recent quota reading for that account. A dash means nothing has been read yet, which is normal until you have spent a session on it.

## Everyday use

| Command | What it does |
|---|---|
| `claude-account-kit list` | Saved accounts in routing order, with their last quota reading |
| `claude-account-kit status` | Current account, the account Claude Desktop is on, settings, and the whole quota cache |
| `claude-account-kit current` | The current account name only |
| `claude-account-kit switch <name>` | Move to a saved account, asking first |
| `claude-account-kit toggle` | Move back to the account you came from |
| `claude-account-kit setup <name>` | Save again under a name you choose instead of the email |
| `claude-account-kit remove <name>` | Forget a saved account and delete its stored credential |

Add `--yes` to `switch` and `toggle` to skip the prompt. Restart Claude Code after a switch: a running session keeps using the credential it started with.

## What the statusline shows

claude-account-kit prints its own part first and your old command's output after it, separated by `|`.

| Segment | Meaning |
|---|---|
| `me@work.com` | The account in use |
| `97%(5h)` | Usage has passed the alert threshold, in the five hour window |
| `LIMIT(7d)` | That window is full |
| `me@gmail.com reset in 2h10m(5h)` | A higher ranked account is waiting on a reset |
| `me@gmail.com reset done` | That account is usable again |
| `route failed` | An automatic switch could not complete |
| `-> claude-account-kit list` | Something needs your attention |

`claude-account-kit config --alert-at 90` moves the threshold, which is 95 by default.

## Automatic switching

```bash
claude-account-kit config --mode auto
```

In `auto` the statusline switches on its own: off an account that has hit its limit, onto the next usable one in the order, and back up to a higher ranked account once its reset has passed. It never pulls you back to an account below the one you moved to by hand. The default is `manual`, which only prints.

Readings are ignored for 90 seconds after any switch. The statusline payload carries no account identity, so a reading arriving right after a switch still belongs to the account you left.

## Removing it

```bash
./uninstall.sh --purge
```

This restores your old statusline command, deletes the saved credentials and this tool's directory, removes the binary, and takes out the `PATH` line the installer added. That line is matched by its marker comment, so a `PATH` line you wrote by hand survives.

| Command | Removes |
|---|---|
| `./uninstall.sh --purge` | Everything: the wrapper, the saved accounts, the binary, and the `PATH` line |
| `./uninstall.sh` | The same, but keeps the saved accounts for later |
| `claude-account-kit statusline-uninstall` | The wrapper, putting your previous command back |
| `claude-account-kit purge` | The saved credentials and the state directory |

A purge asks before it deletes; `--yes` answers for it. None of these touch the account Claude Code is signed in to, so removing the tool logs you out of nothing.

## Good to know

- **Restart Claude Code after a switch.** A running session holds the credential it started with. The statusline says so when it switches for you.
- **A switch changes the account and nothing else.** Only the Claude login inside the active credential is replaced, so MCP servers and plugins you authorized stay authorized. [Platforms](docs/platforms.md#what-a-switch-does-not-change) lists the keys.
- **The account Claude Code displays lags behind.** The `oauthAccount` block in `~/.claude.json` keeps the previous email, organization, and seat tier until Claude Code refetches the profile, which it does at most once a day. Requests go to the new account regardless, because the OAuth token decides that.
- **That lag reaches `claude auth status`,** which is where `setup` gets its default name. Running `setup` right after a switch would file the new credential under the previous account's name, so it refuses when the active credential is already saved under another name. Sign in properly with `claude auth login`, or pass the name yourself.
- **Claude Desktop keeps its own login.** Desktop runs its Code tab with a login it passes to Claude Code itself, so a `switch` changes the terminal login and leaves Desktop on its account. `status` prints a `desktop:` line naming the account Desktop last signed in with, matched to a saved account when `setup` recorded that account's id.
- **Saved credentials are real credentials.** On macOS they are Keychain items under the service `claude-quota-router`, the tool's name before the rename; elsewhere they are owner-only files. Each holds only that account's Claude login, not your MCP or plugin tokens, but it carries a live refresh token, so treat them the way you treat the login itself.
- **A broken state file costs you this tool's segment, not your statusline.** If this tool's own files cannot be read, it prints nothing and your original command still renders.
- **No quota in the statusline usually means Claude Code is signed out.** Claude Code leaves `rate_limits` out of the statusline payload entirely when it has no live quota window, and neither this tool nor your own command can show a number that is not in the payload. `claude-account-kit status` prints a `signed in:` line, and `claude auth status` reports `loggedIn`.

## Documentation

| Document | Contents |
|---|---|
| [docs/routing.md](docs/routing.md) | How the order and the cached readings pick an account, and what the statusline prints |
| [docs/platforms.md](docs/platforms.md) | Credential stores, notifications, and file locations per platform |
| [docs/install.md](docs/install.md) | Install options, a build by hand, the statusline wrapper, and removal |
| [docs/development.md](docs/development.md) | Module layout, checks, and adding a platform backend |

## License

MIT. See [LICENSE](LICENSE).
