#!/bin/sh
# Take claude-account-kit back off this machine.
#
# The mirror of install.sh: it undoes the two things that script did, the binary
# in BIN_DIR and the PATH line in the login shell's profile. Everything else the
# tool created is behind --purge, because that deletes saved credentials.
set -eu

# CLAUDE_QUOTA_ROUTER_BIN_DIR is the variable's name from before the rename.
BIN_DIR="${CLAUDE_ACCOUNT_KIT_BIN_DIR:-${CLAUDE_QUOTA_ROUTER_BIN_DIR:-$HOME/.local/bin}}"
UPDATE_PROFILE=1
PURGE=0
MARKER="# added by claude-account-kit install.sh"
# What install.sh wrote before the rename. A profile edited then still carries it.
LEGACY_MARKER="# added by claude-quota-router install.sh"

usage() {
    cat <<'USAGE'
Usage: ./uninstall.sh [options]

Options:
  --purge         Also delete saved account credentials and the state directory.
  --bin-dir DIR   Look for the binary in DIR instead of ~/.local/bin.
  --keep-path     Leave the shell profile alone.
  -h, --help      Show this message.

Without --purge the saved accounts stay, so reinstalling picks up where you
left off. The credential Claude Code is logged in with is never touched.
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --purge)
            PURGE=1
            shift
            ;;
        --bin-dir)
            [ $# -ge 2 ] || { echo "--bin-dir needs a directory" >&2; exit 2; }
            BIN_DIR="$2"
            shift 2
            ;;
        --keep-path)
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

binary="$BIN_DIR/claude-account-kit"
# install.sh replaces a pre-rename binary with this link to the new one.
legacy="$BIN_DIR/claude-quota-router"

# Restore the statusline through the binary while it is still there. Its own
# uninstall is what knows which command it replaced.
if [ -x "$binary" ]; then
    "$binary" statusline-uninstall
    if [ "$PURGE" -eq 1 ]; then
        "$binary" purge --yes
    fi
else
    echo "no binary at $binary; skipped the statusLine step"
    if [ "$PURGE" -eq 1 ]; then
        echo "saved credentials were left in place"
    fi
fi

if [ -e "$binary" ]; then
    rm -f "$binary"
    echo "removed $binary"
fi

# Only the link install.sh made is removed. A regular file under the old name is
# a pre-rename install this script did not create.
if [ -L "$legacy" ]; then
    case "$(readlink "$legacy")" in
        claude-account-kit|"$binary")
            rm -f "$legacy"
            echo "removed $legacy"
            ;;
    esac
fi

if [ "$UPDATE_PROFILE" -eq 0 ]; then
    exit 0
fi

# Only the block install.sh wrote is removed, matched by its marker, so a PATH
# line the user wrote by hand survives.
for profile in \
    "${ZDOTDIR:-$HOME}/.zprofile" \
    "$HOME/.bash_profile" \
    "$HOME/.profile" \
    "${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
do
    [ -f "$profile" ] || continue
    grep -qF -e "$MARKER" -e "$LEGACY_MARKER" -- "$profile" || continue

    tmp="$profile.cak-tmp.$$"
    awk -v marker="$MARKER" -v legacy="$LEGACY_MARKER" '
        $0 == marker || $0 == legacy { skip = 1; next }
        skip > 0                     { skip--; next }
        { print }
    ' "$profile" > "$tmp"
    cat "$tmp" > "$profile"
    rm -f "$tmp"
    echo "removed the PATH line from $profile"
done

echo "open a new shell to drop $BIN_DIR from PATH"
