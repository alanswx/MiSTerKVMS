#!/usr/bin/env bash
# Build the Linux app (GTK 4 + libadwaita), the misterkvms-cli CLI and the
# misterkvms-capture server, and package them as
# build/MiSTerKVMS-<version>-linux-<arch>.tar.gz.
# Usage: scripts/build-linux.sh [--run]
#
# Needs: Rust, pkg-config, GTK 4.14+ and libadwaita 1.5+ development files,
# ALSA development files (Ubuntu/Debian:
#   sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libasound2-dev).
# Env: VERSION (default: from Cargo.toml).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)}
ARCH=$(uname -m)
cd "$ROOT"

echo "==> Build $VERSION ($ARCH)"
cargo build --release -p kvms-linux -p kvms-cli -p misterkvms-capture
TARGET_DIR=$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

NAME="MiSTerKVMS-$VERSION-linux-$ARCH"
OUT="build/$NAME"
rm -rf "$OUT"
mkdir -p "$OUT"
cp "$TARGET_DIR/release/misterkvms" "$TARGET_DIR/release/misterkvms-cli" "$TARGET_DIR/release/misterkvms-capture" "$OUT/"
ln -sf misterkvms-cli "$OUT/kvms" # old name, kept for one release
cp -r linux/data/icons "$OUT/icons"
cp linux/data/io.github.misterkvms.MiSTerKVMS.desktop "$OUT/"
cp LICENSE "$OUT/LICENSE.txt"
cat > "$OUT/install.sh" <<'INSTALL'
#!/bin/sh
# Install for the current user: ~/.local/bin, the app menu and the icon.
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p ~/.local/bin ~/.local/share/applications ~/.local/share/icons
cp "$HERE/misterkvms" "$HERE/misterkvms-cli" "$HERE/misterkvms-capture" ~/.local/bin/
ln -sf misterkvms-cli ~/.local/bin/kvms
cp -r "$HERE/icons/hicolor" ~/.local/share/icons/
cp "$HERE/io.github.misterkvms.MiSTerKVMS.desktop" ~/.local/share/applications/
echo "Installed. Start MiSTerKVMS from the app menu, or run: misterkvms"
INSTALL
chmod +x "$OUT/install.sh"
tar -C build -czf "build/$NAME.tar.gz" "$NAME"
echo "Built build/$NAME.tar.gz"

if [[ "${1:-}" == "--run" ]]; then
  "$OUT/misterkvms"
fi
