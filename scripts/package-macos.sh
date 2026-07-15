#!/usr/bin/env bash
# Build, ad-hoc codesign, and zip jotainchatttttttt for distribution to other Macs.
# Usage:  npm run package:mac
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="jotainchatttttttt"
BUNDLE_ID="com.jotain.jotainchatttttttt"
APP="$ROOT/src-tauri/target/release/bundle/macos/${APP_NAME}.app"
ENTITLEMENTS="$ROOT/src-tauri/entitlements.plist"
OUT_DIR="$ROOT/packages"
STAGE="$OUT_DIR/_stage"
ARCH="$(uname -m)"
STAMP="$(date +%Y%m%d)"
# Prefer package.json version (e.g. 0.1.1-paste-dnd); fall back to 0.1.0
VER="$(node -p "require('./package.json').version" 2>/dev/null || echo "0.1.0")"
ZIP_NAME="${APP_NAME}-macos-${ARCH}-v${VER}-${STAMP}.zip"
ZIP_PATH="$OUT_DIR/$ZIP_NAME"

echo "==> Building release .app"
npm run tauri:build

if [[ ! -d "$APP" ]]; then
  echo "ERROR: app bundle missing at $APP" >&2
  exit 1
fi

echo "==> Clearing quarantine on local build"
xattr -cr "$APP" 2>/dev/null || true

# macOS CFBundleIconFile must be the name WITHOUT extension. Tauri often writes
# "icon.icns" which breaks icon lookup and leaves LaunchServices on a cached/default icon.
# Also rename to a versioned icon name so Dock/Finder cannot reuse a stale cache entry.
ICON_BASE="AppIcon-v$(echo "$VER" | tr -c 'A-Za-z0-9._-' '_')"
RES="$APP/Contents/Resources"
PLIST="$APP/Contents/Info.plist"
if [[ -f "$RES/icon.icns" ]]; then
  echo "==> Fix app icon ($ICON_BASE.icns, CFBundleIconFile without extension)"
  rm -f "$RES/${ICON_BASE}.icns" "$RES/AppIcon.icns"
  mv "$RES/icon.icns" "$RES/${ICON_BASE}.icns"
  # Keep a plain AppIcon.icns copy too (some tools look for it)
  cp "$RES/${ICON_BASE}.icns" "$RES/AppIcon.icns"
  /usr/libexec/PlistBuddy -c "Set :CFBundleIconFile ${ICON_BASE}" "$PLIST" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string ${ICON_BASE}" "$PLIST"
  /usr/libexec/PlistBuddy -c "Set :CFBundleVersion ${VER}" "$PLIST" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Add :CFBundleVersion string ${VER}" "$PLIST"
  # Drop any Finder custom-icon flag that can pin an old resource-fork icon
  if command -v SetFile >/dev/null 2>&1; then
    SetFile -a c "$APP" 2>/dev/null || true
  fi
  rm -f "$APP/Icon"$'\r' 2>/dev/null || true
fi

echo "==> Ad-hoc codesign (deep, with entitlements)"
codesign --force --deep --sign - \
  --identifier "$BUNDLE_ID" \
  --entitlements "$ENTITLEMENTS" \
  "$APP"

echo "==> Verify signature"
codesign --verify --deep --strict --verbose=2 "$APP"
codesign -dv --verbose=2 "$APP" 2>&1 | head -25

echo "==> Architecture"
file "$APP/Contents/MacOS/$APP_NAME"
lipo -info "$APP/Contents/MacOS/$APP_NAME" 2>/dev/null || true

# Keep prior zips for rollback; only refresh staging area.
mkdir -p "$OUT_DIR"
rm -rf "$STAGE"
mkdir -p "$STAGE"

echo "==> Stage release folder"
ditto "$APP" "$STAGE/${APP_NAME}.app"
xattr -cr "$STAGE/${APP_NAME}.app" 2>/dev/null || true

# ASCII names only — avoids zip encoding corruption on other Macs
cat > "$STAGE/Open-Me-First.command" << 'HELPER'
#!/bin/bash
cd "$(dirname "$0")"
APP="jotainchatttttttt.app"
if [[ ! -d "$APP" ]]; then
  echo "ERROR: $APP not found. Unzip the full package first."
  read -r -p "Press Enter to close…"
  exit 1
fi
echo "Removing macOS quarantine (common after AirDrop/download)…"
xattr -cr "$APP" 2>/dev/null || true
echo "Opening $APP …"
open "$APP"
sleep 1
echo ""
echo "If it still will not open:"
echo "  1) Right-click $APP → Open → Open"
echo "  2) System Settings → Privacy & Security → Open Anyway"
read -r -p "Press Enter to close…"
HELPER
chmod +x "$STAGE/Open-Me-First.command"

# UTF-8 README with English filename
cat > "$STAGE/README.txt" << 'TXT'
jotainchatttttttt — open on the other Mac
==========================================

1. Unzip this zip completely
2. Double-click:  Open-Me-First.command
   (if blocked: right-click → Open)
3. Or right-click jotainchatttttttt.app → Open → Open
4. Privacy & Security → Open Anyway if needed
5. Terminal fallback:
     xattr -cr ~/Downloads/jotainchatttttttt.app
     open ~/Downloads/jotainchatttttttt.app

Notes
-----
- arm64 / Apple Silicon (latest Mac Pro, M-series)
- Send/share this whole zip, not a single inner file
- No auto-update; replace the whole .app for new versions

Ports (allow if firewall asks)
  UDP 48765  discovery
  TCP 48766  chat
  TCP 48767  files
TXT

echo "==> Zip (flat root: no nested stage/)"
rm -f "$ZIP_PATH" # avoid stale entries when re-zipping same day's name
(
  cd "$STAGE"
  # -y store symlinks as links; -r recursive
  zip -ry "$ZIP_PATH" \
    "${APP_NAME}.app" \
    "Open-Me-First.command" \
    "README.txt"
)

ditto "$APP" "$OUT_DIR/${APP_NAME}.app"
xattr -cr "$OUT_DIR/${APP_NAME}.app" 2>/dev/null || true
rm -rf "$STAGE"

echo "==> Verify zip listing"
unzip -l "$ZIP_PATH"
codesign --verify --deep --strict "$OUT_DIR/${APP_NAME}.app"
echo "codesign_OK"

cat <<EOF

============================================================
SEND THIS FILE TO THE OTHER MAC:

  $ZIP_PATH

Other Mac:
  1. Unzip
  2. Double-click Open-Me-First.command
  3. If blocked: right-click .app → Open

Local app copy:
  $OUT_DIR/${APP_NAME}.app

Arch: $ARCH (Mac Pro / Apple Silicon OK)
Sign: ad-hoc (needs Open-Me-First or right-click Open)
============================================================
EOF
