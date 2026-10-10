#!/usr/bin/env bash
set -e

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$DIR/target/release/azdl-desktop"

if [ ! -f "$BIN" ]; then
    BIN="$DIR/target/debug/azdl-desktop"
fi

if [ ! -f "$BIN" ]; then
    echo "Binary not found. Building with cargo build --release..."
    cargo build --release
    BIN="$DIR/target/release/azdl-desktop"
fi

# Ensure /tmp/webkit2gtk-4.1 symlink exists if ~/.local/lib/webkit2gtk-4.1 is present
if [ -d "$HOME/.local/lib/webkit2gtk-4.1" ] && [ ! -e "/tmp/webkit2gtk-4.1" ]; then
    ln -sfn "$HOME/.local/lib/webkit2gtk-4.1" "/tmp/webkit2gtk-4.1"
fi

exec "$BIN" "$@"
