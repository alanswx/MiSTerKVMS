#!/usr/bin/env bash
# Package build/MiSTerKVMS.app into a drag-to-install DMG (app + Applications
# shortcut), then sign, notarize and staple it when credentials are available.
# Usage: scripts/package-dmg.sh <output.dmg>
#
# Env:
#   SIGN_IDENTITY        sign the DMG with this identity (Developer ID).
#   NOTARY_KEY_PATH      App Store Connect API key (.p8) for notarytool,
#   NOTARY_KEY_ID        with its key ID
#   NOTARY_ISSUER_ID     and issuer ID. All three needed to notarize.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
APP="$ROOT/build/MiSTerKVMS.app"
OUT=${1:?usage: package-dmg.sh <output.dmg>}
[[ -d "$APP" ]] || { echo "Build the app first (scripts/build-mac.sh)"; exit 1; }

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"

rm -f "$OUT"
hdiutil create -volname "MiSTerKVMS" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$OUT"

if [[ "${SIGN_IDENTITY:-}" == Developer\ ID* ]]; then
  codesign --force --sign "$SIGN_IDENTITY" --timestamp "$OUT"
fi

if [[ -n "${NOTARY_KEY_PATH:-}" && -n "${NOTARY_KEY_ID:-}" && -n "${NOTARY_ISSUER_ID:-}" ]]; then
  echo "==> Notarizing"
  xcrun notarytool submit "$OUT" --wait \
    --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID"
  xcrun stapler staple "$OUT"
else
  echo "==> Not notarizing (no App Store Connect API key provided)"
fi
echo "Packaged $OUT"
