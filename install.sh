#!/bin/sh
# Build claude-account-kit, put it on PATH, and keep it there across shells.
#
# The binary has to be on PATH before any of its own commands can run, so this
# bootstrap step lives in a script rather than in a subcommand.
set -eu

# CLAUDE_QUOTA_ROUTER_BIN_DIR is the variable's name from before the rename.
BIN_DIR="${CLAUDE_ACCOUNT_KIT_BIN_DIR:-${CLAUDE_QUOTA_ROUTER_BIN_DIR:-$HOME/.local/bin}}"
UPDATE_PROFILE=1
MARKER="# added by claude-account-kit install.sh"

usage() {
    cat <<'USAGE'
Usage: ./install.sh [options]

Options:
  --bin-dir DIR   Install into DIR instead of ~/.local/bin.
  --no-path       Install the binary without touching any shell profile.
  -h, --help      Show this message.

The install directory can also be set with CLAUDE_ACCOUNT_KIT_BIN_DIR.
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --bin-dir)
            [ $# -ge 2 ] || { echo "--bin-dir needs a directory" >&2; exit 2; }
            BIN_DIR="$2"
            shift 2
            ;;
        --no-path)
            UPDATE_PROFILE=0
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

# rustup installs into ~/.cargo/bin, which is not on PATH when rustup was told
# not to edit the shell profile.
if ! command -v cargo >/dev/null 2>&1; then
    if [ -x "$HOME/.cargo/bin/cargo" ]; then
        PATH="$HOME/.cargo/bin:$PATH"
        export PATH
    else
        echo "cargo not found. Install Rust from https://rustup.rs and run this again." >&2
        exit 1
    fi
fi

cargo build --release --manifest-path "$repo_root/Cargo.toml"
mkdir -p "$BIN_DIR"
install -m 755 "$repo_root/target/release/claude-account-kit" "$BIN_DIR/claude-account-kit"
echo "installed $BIN_DIR/claude-account-kit"

# Before the rename the binary was claude-quota-router, and Claude Code's
# statusLine setting still names that path. Replacing the old binary with a link
# to the new one keeps that statusline rendering, now against the migrated
# state. The link is made beside it and renamed over it, so the path never goes
# missing while a statusline render might be starting it.
legacy="$BIN_DIR/claude-quota-router"
if [ -f "$legacy" ] && [ ! -L "$legacy" ]; then
    ln -s claude-account-kit "$legacy.tmp.$$"
    mv -f "$legacy.tmp.$$" "$legacy"
    echo "replaced $legacy with a link to claude-account-kit"
    echo "run claude-account-kit statusline-install to point Claude Code's statusLine at the new name"
fi

if [ "$UPDATE_PROFILE" -eq 0 ]; then
    exit 0
fi

# Pick the file the login shell reads for PATH.
shell_name="$(basename "${SHELL:-/bin/sh}")"
profile=""
path_line=""
case "$shell_name" in
    zsh)
        profile="${ZDOTDIR:-$HOME}/.zprofile"
        path_line="export PATH=\"$BIN_DIR:\$PATH\""
        ;;
    bash)
        if [ -f "$HOME/.bash_profile" ]; then
            profile="$HOME/.bash_profile"
        else
            profile="$HOME/.profile"
        fi
        path_line="export PATH=\"$BIN_DIR:\$PATH\""
        ;;
    fish)
        profile="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
        path_line="fish_add_path $BIN_DIR"
        ;;
    ksh|dash|sh)
        profile="$HOME/.profile"
        path_line="export PATH=\"$BIN_DIR:\$PATH\""
        ;;
    *)
        echo "unrecognized shell: $shell_name"
        echo "add $BIN_DIR to PATH yourself."
        exit 0
        ;;
esac

case ":$PATH:" in
    *":$BIN_DIR:"*)
        echo "$BIN_DIR is already on PATH; left $profile alone"
        exit 0
        ;;
esac

if [ -f "$profile" ] && grep -qF -- "$BIN_DIR" "$profile"; then
    echo "$profile already mentions $BIN_DIR; left it alone"
    echo "open a new shell to pick it up"
    exit 0
fi

mkdir -p "$(dirname -- "$profile")"
{
    printf '\n%s\n' "$MARKER"
    printf '%s\n' "$path_line"
} >> "$profile"
echo "added $BIN_DIR to PATH in $profile"
echo "open a new shell to pick it up"
