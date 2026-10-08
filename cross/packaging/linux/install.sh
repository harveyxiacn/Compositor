#!/bin/sh
# Installs Compositor for the current user: binaries in ~/.local/bin, plus a menu entry and icon.
set -e
here="$(cd "$(dirname "$0")" && pwd)"
prefix="${PREFIX:-$HOME/.local}"
install -Dm755 "$here/compositor" "$prefix/bin/compositor"
install -Dm755 "$here/compositor-cli" "$prefix/bin/compositor-cli"
install -Dm644 "$here/compositor.png" "$prefix/share/icons/hicolor/256x256/apps/compositor.png"
install -Dm644 "$here/compositor.desktop" "$prefix/share/applications/compositor.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/share/applications" || true
echo "Installed to $prefix. Make sure $prefix/bin is on your PATH."
