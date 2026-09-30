#!/usr/bin/env bash
# Build the Linux release artifacts: a tarball (also the updater's payload) and an AppImage.
#
#   packaging/linux/package.sh <arch: x86_64|aarch64> <version> <binary> <output-dir>
#
# Needs `appimagetool` on PATH (CI downloads it) for the AppImage.
set -euo pipefail

ARCH="${1:?x86_64 or aarch64}"
VERSION="${2:?version}"
BIN="${3:?path to the release binary}"
OUT="${4:?output directory}"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SLUG="netease-music-downloader"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

# ---- tarball ------------------------------------------------------------------------------
STAGE="$(mktemp -d)"
DIR="$STAGE/$SLUG-$VERSION"
mkdir -p "$DIR"
install -m 755 "$BIN" "$DIR/$SLUG"
install -m 644 "$ROOT/packaging/linux/$SLUG.desktop" "$DIR/"
install -m 644 "$ROOT/packaging/icons/png/icon-256.png" "$DIR/$SLUG.png"
install -m 644 "$ROOT/LICENSE" "$DIR/"
cat > "$DIR/README.txt" <<EOF
云音下载姬 / Netease Music Downloader $VERSION

Run ./$SLUG. To add it to your application menu, copy $SLUG.desktop to
~/.local/share/applications and $SLUG.png to ~/.local/share/icons, and point the
Exec= line at the full path of the binary.

Chinese text needs a CJK font, e.g. fonts-noto-cjk.
EOF
tar -C "$STAGE" -czf "$OUT/$SLUG-$VERSION-linux-$ARCH.tar.gz" "$SLUG-$VERSION"

# ---- AppImage -----------------------------------------------------------------------------
APPDIR="$STAGE/AppDir"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/256x256/apps"
install -m 755 "$BIN" "$APPDIR/usr/bin/$SLUG"
install -m 644 "$ROOT/packaging/linux/$SLUG.desktop" "$APPDIR/usr/share/applications/"
install -m 644 "$ROOT/packaging/linux/$SLUG.desktop" "$APPDIR/"
install -m 644 "$ROOT/packaging/icons/png/icon-256.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/$SLUG.png"
install -m 644 "$ROOT/packaging/icons/png/icon-256.png" "$APPDIR/$SLUG.png"
cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/netease-music-downloader" "$@"
EOF
chmod 755 "$APPDIR/AppRun"

# `--appimage-extract-and-run` lets the tool run on machines without FUSE (CI containers).
ARCH="$ARCH" appimagetool --appimage-extract-and-run "$APPDIR" "$OUT/$SLUG-$VERSION-linux-$ARCH.AppImage"

rm -rf "$STAGE"
ls -lh "$OUT"/$SLUG-$VERSION-linux-$ARCH.*
