#!/usr/bin/env bash
# Generate immutable file-delta artifacts from two final signed macOS bundles; never uploads.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BASE= TARGET= NOTES= URL_PREFIX= OUTPUT="$ROOT/dist/patch"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --base) BASE="$2"; shift 2 ;;
    --target) TARGET="$2"; shift 2 ;;
    --output) OUTPUT="$2"; shift 2 ;;
    --notes) NOTES="$2"; shift 2 ;;
    --url-prefix) URL_PREFIX="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
if [ -z "$BASE" ] || [ -z "$TARGET" ] || [ -z "$NOTES" ]; then
  echo 'usage: patch/package.sh --base OLD.app --target NEW.app --notes "..." [--output DIR] [--url-prefix URL]' >&2
  exit 2
fi
(cd "$ROOT" && cargo build --release --locked -p photocraft-updater)
UPDATER="${CARGO_TARGET_DIR:-$ROOT/target}/release/photocraft-updater"
CONFIG="$TARGET/Contents/Resources/patch-config.json"
REVISION="$(plutil -extract revision raw -o - "$CONFIG")"
PLATFORM="$(plutil -extract platform raw -o - "$CONFIG")"
CHANNEL="$(plutil -extract channel raw -o - "$CONFIG")"
# Metadata becomes artifact names; reject separators and non-macOS platforms before redirection.
case "$PLATFORM" in macos-aarch64|macos-x86_64|macos-universal) ;; *) echo 'error: unsupported patch platform' >&2; exit 2 ;; esac
case "$REVISION" in ''|*[!0-9]*) echo 'error: invalid patch revision' >&2; exit 2 ;; esac
case "$CHANNEL" in ''|*[!a-zA-Z0-9-]*) echo 'error: invalid patch channel' >&2; exit 2 ;; esac
URL_PREFIX="${URL_PREFIX:-https://github.com/iricwang/photocraft/releases/download/patch-$CHANNEL-$REVISION}"
mkdir -p "$OUTPUT"
PACKAGE="$OUTPUT/photocraft-patch-$REVISION-$PLATFORM.pcp"
BOOTSTRAP="$OUTPUT/photocraft-bootstrap-$REVISION-$PLATFORM.zip"
INVENTORY="$OUTPUT/inventory-$REVISION-$PLATFORM.json"
MANIFEST="$OUTPUT/manifest-$PLATFORM.json"
if [ -e "$PACKAGE" ] || [ -e "$BOOTSTRAP" ] || [ -e "$INVENTORY" ]; then
  echo 'error: revision artifacts already exist; preserve them and choose another revision/output' >&2
  exit 1
fi
codesign --verify --strict --deep "$BASE"
codesign --verify --strict --deep "$TARGET"
"$UPDATER" inventory "$TARGET" >"$INVENTORY"
ditto -c -k --keepParent "$TARGET" "$BOOTSTRAP"
"$UPDATER" package "$BASE" "$TARGET" "$PACKAGE" "${URL_PREFIX%/}/$(basename "$PACKAGE")" "$MANIFEST" "$NOTES"
echo "Created $PACKAGE, $BOOTSTRAP, $INVENTORY and $MANIFEST"
