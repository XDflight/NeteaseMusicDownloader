#!/usr/bin/env bash
# Build NeteaseMusicDownloader.app, a zip of it (used by the in-app updater) and a .dmg.
#
#   packaging/macos/bundle.sh <universal-binary> <version> <output-dir>
#
# The bundle is signed ad hoc (no developer certificate): Apple Silicon refuses unsigned
# code, and an ad hoc signature satisfies that. Gatekeeper will still ask the user to
# confirm the first launch (right-click → Open).
set -euo pipefail

BIN="${1:?path to the universal binary}"
VERSION="${2:?version, e.g. 0.1.0}"
OUT="${3:?output directory}"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
APP_NAME="NeteaseMusicDownloader"
APP="$OUT/$APP_NAME.app"
SLUG="netease-music-downloader"

mkdir -p "$OUT"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/$SLUG"
chmod 755 "$APP/Contents/MacOS/$SLUG"
cp "$ROOT/packaging/icons/app.icns" "$APP/Contents/Resources/app.icns"
sed "s/@VERSION@/$VERSION/g" "$ROOT/packaging/macos/Info.plist.in" > "$APP/Contents/Info.plist"

codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

# Update payload: the whole bundle, zipped the way Finder does.
ZIP="$OUT/$SLUG-$VERSION-macos-universal.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

# Disk image for first-time installs.
DMG="$OUT/$SLUG-$VERSION-macos-universal.dmg"
rm -f "$DMG"
if command -v create-dmg >/dev/null 2>&1; then
  create-dmg \
    --volname "云音下载姬" \
    --background "$ROOT/packaging/macos/dmg_background.png" \
    --window-pos 200 120 --window-size 660 400 \
    --icon-size 96 \
    --icon "$APP_NAME.app" 170 190 \
    --hide-extension "$APP_NAME.app" \
    --app-drop-link 490 190 \
    "$DMG" "$APP" || CREATE_DMG_FAILED=1
fi
if [ ! -f "$DMG" ]; then
  echo "create-dmg unavailable or failed; falling back to a plain disk image" >&2
  STAGE="$(mktemp -d)"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  hdiutil create -volname "云音下载姬" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
  rm -rf "$STAGE"
fi

echo "created:"
ls -lh "$ZIP" "$DMG"
