#!/usr/bin/env bash
# Build Wren for macOS (Apple Silicon), sign locally, install to /Applications/Wren.app, and run --selftest.
#
# Usage:
#   bash scripts/release.sh                 # build, sign, install, self-test
#   bash scripts/release.sh --launch        # ...and start Wren quietly in the menu bar
#   bash scripts/release.sh --no-build      # reinstall the last staged build
#   bash scripts/release.sh --no-selftest   # skip --selftest
#   bash scripts/release.sh --sign "-"      # force ad-hoc signing instead of Keychain Apple Development cert
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALL_APP="/Applications/Wren.app"
STAGE_DIR="${WREN_STAGE_DIR:-$(dirname "$REPO_ROOT")/wren-release-macos}"

NO_BUILD=0
NO_INSTALL=0
NO_SELFTEST=0
LAUNCH=0
SIGN_IDENTITY=""

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) NO_BUILD=1 ;;
    --no-install) NO_INSTALL=1 ;;
    --no-selftest) NO_SELFTEST=1 ;;
    --launch) LAUNCH=1 ;;
    --sign) SIGN_IDENTITY="${2:-}"; shift ;;
    *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

step() { printf '\n== %s\n' "$1"; }

# Auto-detect an Apple Development identity in Keychain so macOS Accessibility & Microphone
# permissions persist cleanly across local rebuilds; fall back to ad-hoc "-" if none exists.
if [ -z "$SIGN_IDENTITY" ]; then
  DETECTED="$(security find-identity -v -p codesigning 2>/dev/null | awk -F'"' '/Apple Development:/ {print $2; exit}' || true)"
  if [ -n "$DETECTED" ]; then
    SIGN_IDENTITY="$DETECTED"
  else
    SIGN_IDENTITY="-"
  fi
fi

mkdir -p "$STAGE_DIR"
VERSION="$(node -e "console.log(JSON.parse(require('fs').readFileSync('$REPO_ROOT/src-tauri/tauri.conf.json','utf8')).version)")"
BUILD_ID_FILE="$STAGE_DIR/build-id.txt"
STAGED_APP="$STAGE_DIR/Wren.app"
STAGED_DMG="$STAGE_DIR/Wren_${VERSION}_aarch64.dmg"

if [ "$NO_BUILD" -eq 0 ]; then
  step "Frontend unit tests"
  (cd "$REPO_ROOT" && node scripts/unit-tests.mjs)

  step "Build macOS release bundle (Wren.app)"
  WREN_BUILD_ID="rel-$(date +%Y%m%d-%H%M%S)-$(uuidgen | tr -d '-' | cut -c1-8 | tr '[:upper:]' '[:lower:]')"
  export WREN_BUILD_ID
  echo "WREN_BUILD_ID = $WREN_BUILD_ID"
  (cd "$REPO_ROOT" && npx tauri build --bundles app)

  BUILT_APP="$REPO_ROOT/src-tauri/target/release/bundle/macos/Wren.app"
  if [ ! -d "$BUILT_APP" ]; then
    echo "RELEASE FAIL: Wren.app not found at $BUILT_APP" >&2
    exit 1
  fi

  step "Code-sign bundle ($SIGN_IDENTITY)"
  # Sign without Hardened Runtime so local development builds keep full mic/AX access without extra
  # entitlements, while preserving TCC permissions across rebuilds via the Keychain cert identity.
  codesign --force --deep --sign "$SIGN_IDENTITY" "$BUILT_APP"
  codesign --verify --deep --strict "$BUILT_APP"

  rm -rf "$STAGED_APP"
  ditto "$BUILT_APP" "$STAGED_APP"

  step "Package signed DMG"
  DMG_STAGE="$(mktemp -d)"
  trap 'rm -rf "$DMG_STAGE"' EXIT
  ditto "$STAGED_APP" "$DMG_STAGE/Wren.app"
  ln -s /Applications "$DMG_STAGE/Applications"
  rm -f "$STAGED_DMG"
  hdiutil create -volname "Wren" -srcfolder "$DMG_STAGE" -ov -format UDZO "$STAGED_DMG" >/dev/null
  rm -rf "$DMG_STAGE"
  trap - EXIT

  printf '%s\n' "$WREN_BUILD_ID" > "$BUILD_ID_FILE"
  echo "Staged: $STAGED_APP"
  [ -f "$STAGED_DMG" ] && echo "DMG:    $STAGED_DMG ($(du -sh "$STAGED_DMG" | awk '{print $1}'))"
fi

if [ ! -d "$STAGED_APP" ] || [ ! -f "$BUILD_ID_FILE" ]; then
  echo "RELEASE FAIL: no staged build in $STAGE_DIR" >&2
  exit 1
fi
BUILD_ID="$(tr -d '\r\n' < "$BUILD_ID_FILE")"

if [ "$NO_INSTALL" -eq 1 ]; then
  echo "Built $BUILD_ID; skipped install (--no-install)"
  exit 0
fi

step "Install to $INSTALL_APP"
WAS_RUNNING=0
if pgrep -x "wren" >/dev/null 2>&1; then
  WAS_RUNNING=1
  if [ -x "$INSTALL_APP/Contents/MacOS/wren" ]; then
    "$INSTALL_APP/Contents/MacOS/wren" --quit || true
  fi
  for _ in $(seq 1 20); do
    pgrep -x "wren" >/dev/null 2>&1 || break
    sleep 0.25
  done
  pkill -x "wren" 2>/dev/null || true
fi

TMP_INSTALL="/Applications/Wren.app.wren-new"
rm -rf "$TMP_INSTALL"
ditto "$STAGED_APP" "$TMP_INSTALL"
rm -rf "$INSTALL_APP"
mv "$TMP_INSTALL" "$INSTALL_APP"
xattr -dr com.apple.quarantine "$INSTALL_APP" 2>/dev/null || true

INSTALLED_BIN="$INSTALL_APP/Contents/MacOS/wren"
if ! LC_ALL=C grep -q "wren-build-id:${BUILD_ID}" "$INSTALLED_BIN"; then
  echo "RELEASE FAIL: installed binary does not carry build marker $BUILD_ID" >&2
  exit 1
fi
echo "Installed $INSTALL_APP (verified build $BUILD_ID)"

if [ "$NO_SELFTEST" -eq 0 ]; then
  step "Self-test ($INSTALLED_BIN --selftest)"
  if ! "$INSTALLED_BIN" --selftest; then
    echo "RELEASE FAIL: --selftest reported failures (check Accessibility/Microphone permissions if needed)" >&2
    exit 1
  fi
fi

if [ "$LAUNCH" -eq 1 ] || [ "$WAS_RUNNING" -eq 1 ]; then
  step "Launching Wren in menu bar"
  open -a "$INSTALL_APP" --args --hidden
fi

echo ""
echo "RELEASE OK ($BUILD_ID)"
