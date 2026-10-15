# Install

```bash
./install.sh
```

The script builds the release binary, copies it to `~/.local/bin`, and adds that directory to `PATH` in the file your login shell reads.

| Login shell | File it writes | Line it adds |
|---|---|---|
| zsh | `${ZDOTDIR:-$HOME}/.zprofile` | `export PATH=...` |
| bash | `~/.bash_profile`, else `~/.profile` | `export PATH=...` |
| fish | `~/.config/fish/config.fish` | `fish_add_path ...` |
| anything else | none | printed for you to add |

It leaves the profile alone when the directory is already on `PATH` or the file already mentions it, so rerunning it does not stack up lines.

| Option | Effect |
|---|---|
| `--bin-dir DIR` | Install into `DIR` instead of `~/.local/bin` |
| `--no-path` | Install the binary and touch no shell profile |

`CLAUDE_ACCOUNT_KIT_BIN_DIR` sets the same directory as `--bin-dir`. `CLAUDE_QUOTA_ROUTER_BIN_DIR`, its name before the rename, still does when the new variable is unset.

When the directory holds a `claude-quota-router` binary from before the rename, the script replaces it with a link to `claude-account-kit`. Claude Code's statusline setting still names that path, so the link keeps the statusline running until `claude-account-kit statusline-install` points the setting at the new name.

Building needs Rust 1.88 or newer. The script falls back to `~/.cargo/bin/cargo` when `cargo` is not on `PATH`, which is where rustup puts it after an install that left the shell profile alone.

## By hand

```bash
cargo build --release
cp target/release/claude-account-kit ~/.local/bin/
```

`install.sh` is a POSIX shell script and covers macOS and Linux. On Windows, build with `cargo build --release`, copy `target\release\claude-account-kit.exe` to a directory on `PATH`, and add that directory with `setx PATH`.

## The statusline wrapper

```bash
claude-account-kit statusline-install
```

This writes `statusLine.command` in Claude Code's `settings.json` and saves whatever command was there. The wrapper runs your old command on every render, feeds it the same JSON Claude Code sent, and prints its output after its own, so the statusline you already had keeps working.

Only `command` is replaced. Other keys on the `statusLine` object, such as `padding`, are left as they are, and the rest of `settings.json` keeps its key order.

The name keeps it apart from `install.sh`, which only builds the binary, puts it on `PATH`, and never touches `settings.json`. Run it from the copy on `PATH` rather than from `target/release`, because it records its own path in the setting. `claude-account-kit statusline-uninstall` puts the saved command back.

When `CLAUDE_CONFIG_DIR` is set, both commands follow it, because that is the directory Claude Code reads its settings from.

## Removing it

```bash
./uninstall.sh --purge
```

The mirror of `install.sh`. It puts your old statusline command back, deletes the saved account credentials and this tool's configuration directory, removes the binary, and takes out the `PATH` line `install.sh` added, matched by its marker comment so a line you wrote by hand survives.

| Option | Effect |
|---|---|
| `--purge` | Also delete the saved credentials and the configuration directory |
| `--bin-dir DIR` | Look for the binary in `DIR` instead of `~/.local/bin` |
| `--keep-path` | Leave the shell profile alone |

Without `--purge` the saved accounts stay, so reinstalling later picks up where you left off.

The script also removes the `claude-quota-router` link `install.sh` made. A regular file under that name is a pre-rename install it did not create, so it stays.

The same removal is available from the binary when you want to keep it on `PATH`:

```bash
claude-account-kit statusline-uninstall  # the wrapper, putting your previous command back
claude-account-kit purge                 # the saved credentials and the state directory
```

These are separate because they undo different things: one a setting inside Claude Code, the other this tool's own copies. Neither removes the binary.

A purge asks before it deletes; `--yes` answers for it. Neither form touches the account Claude Code is signed in to, so removing the router logs you out of nothing. `settings.json` is copied to `settings.json.bak-<timestamp>` before either command edits it.
