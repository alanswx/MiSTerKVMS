#!/usr/bin/env bash
# Build the iPhone app: Rust core (device + simulator) → KVMSCore.xcframework
# → Swift bindings → xcodebuild.
# Usage: scripts/build-ios.sh [--core-only] [--device] [--testflight]
#   --core-only   stop after the XCFramework and bindings (then build in Xcode:
#                 open ios/MiSTerKVMS.xcodeproj)
#   --device      build for a real iPhone (needs DEVELOPMENT_TEAM, or a team
#                 picked in Xcode); default: the simulator
#   --testflight  archive, sign and upload to App Store Connect (TestFlight);
#                 needs DEVELOPMENT_TEAM and the ASC_* key below
#
# Env:
#   DEVELOPMENT_TEAM  Apple team id for signing device builds.
#   VERSION           CFBundleShortVersionString (default: from Cargo.toml).
#   BUILD_NUMBER      CFBundleVersion (default: git commit count); must grow
#                     with every TestFlight upload.
#   ASC_KEY_PATH, ASC_KEY_ID, ASC_ISSUER_ID
#                     App Store Connect API key (.p8 file, its id, the issuer
#                     id). Xcode signs in the cloud with it, so no certificate
#                     or profile has to be installed; the key needs the Admin
#                     role (or access to cloud-managed distribution
#                     certificates).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)}
BUILD_NUMBER=${BUILD_NUMBER:-$(git -C "$ROOT" rev-list --count HEAD 2>/dev/null || echo 1)}
OUT="$ROOT/build/ios"
cd "$ROOT"

CORE_ONLY=0; DEVICE=0; TESTFLIGHT=0
for arg in "$@"; do
  case "$arg" in
    --core-only) CORE_ONLY=1 ;;
    --device) DEVICE=1 ;;
    --testflight) TESTFLIGHT=1 ;;
    *) echo "unknown option $arg" >&2; exit 2 ;;
  esac
done

# rustup's toolchain: Homebrew's Rust has no iOS standard library.
export PATH="$HOME/.cargo/bin:$PATH"
export IPHONEOS_DEPLOYMENT_TARGET=17.0
for t in aarch64-apple-ios aarch64-apple-ios-sim; do
  rustup target list --installed | grep -qx "$t" || rustup target add "$t"
done

echo "==> Rust core ($VERSION build $BUILD_NUMBER)"
cargo build --release -p kvms-core --target aarch64-apple-ios
cargo build --release -p kvms-core --target aarch64-apple-ios-sim
# The bindings come from a host build of the same crate (same exports).
cargo build --release -p kvms-core --target aarch64-apple-darwin

echo "==> Swift bindings"
cargo run -q --release -p uniffi-bindgen --target aarch64-apple-darwin -- generate \
  --library target/aarch64-apple-darwin/release/libkvms_core.dylib --language swift --out-dir target/uniffi-swift-ios
mkdir -p ios/MiSTerKVMS/Generated "$OUT/headers"
cp target/uniffi-swift-ios/kvms_core.swift ios/MiSTerKVMS/Generated/
cp target/uniffi-swift-ios/kvms_coreFFI.h "$OUT/headers/"
cp target/uniffi-swift-ios/kvms_coreFFI.modulemap "$OUT/headers/module.modulemap"

echo "==> KVMSCore.xcframework"
rm -rf "$OUT/KVMSCore.xcframework"
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libkvms_core.a -headers "$OUT/headers" \
  -library target/aarch64-apple-ios-sim/release/libkvms_core.a -headers "$OUT/headers" \
  -output "$OUT/KVMSCore.xcframework" >/dev/null

[[ "$CORE_ONLY" == "1" ]] && { echo "Done: $OUT/KVMSCore.xcframework"; exit 0; }

# xcodebuild's full output goes to a log; errors and warnings are shown.
xcb() {
  local log="$OUT/xcodebuild-$1.log"; shift
  if ! xcodebuild "$@" > "$log" 2>&1; then
    grep -E "error:|warning: |ERROR|BUILD FAILED|EXPORT FAILED" "$log" | tail -40
    echo "xcodebuild failed; full log: $log" >&2
    exit 1
  fi
  grep -E "warning: " "$log" | grep -v appintentsmetadataprocessor | sort -u | head -20 || true
}
PROJECT=(-project ios/MiSTerKVMS.xcodeproj -scheme MiSTerKVMS -configuration Release)
VERSIONS=(MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="$BUILD_NUMBER")

if [[ "$TESTFLIGHT" == "1" ]]; then
  : "${DEVELOPMENT_TEAM:?set DEVELOPMENT_TEAM}" "${ASC_KEY_PATH:?set ASC_KEY_PATH}"
  : "${ASC_KEY_ID:?set ASC_KEY_ID}" "${ASC_ISSUER_ID:?set ASC_ISSUER_ID}"
  AUTH=(-allowProvisioningUpdates -authenticationKeyPath "$ASC_KEY_PATH"
        -authenticationKeyID "$ASC_KEY_ID" -authenticationKeyIssuerID "$ASC_ISSUER_ID")
  echo "==> Archive (team $DEVELOPMENT_TEAM)"
  rm -rf "$OUT/MiSTerKVMS.xcarchive" "$OUT/export"
  xcb archive "${PROJECT[@]}" -destination "generic/platform=iOS" -derivedDataPath "$OUT/DerivedData" \
    -archivePath "$OUT/MiSTerKVMS.xcarchive" DEVELOPMENT_TEAM="$DEVELOPMENT_TEAM" CODE_SIGN_STYLE=Automatic \
    "${VERSIONS[@]}" "${AUTH[@]}" archive
  cat > "$OUT/ExportOptions.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>method</key><string>app-store-connect</string>
	<key>destination</key><string>upload</string>
	<key>teamID</key><string>$DEVELOPMENT_TEAM</string>
	<key>signingStyle</key><string>automatic</string>
	<key>uploadSymbols</key><true/>
	<key>manageAppVersionAndBuildNumber</key><false/>
</dict>
</plist>
PLIST
  echo "==> Upload to App Store Connect ($VERSION build $BUILD_NUMBER)"
  xcb upload -exportArchive -archivePath "$OUT/MiSTerKVMS.xcarchive" \
    -exportOptionsPlist "$OUT/ExportOptions.plist" -exportPath "$OUT/export" "${AUTH[@]}"
  echo "Uploaded $VERSION ($BUILD_NUMBER): it shows up in TestFlight once Apple has processed it."
  exit 0
fi

echo "==> iPhone app"
if [[ "$DEVICE" == "1" ]]; then
  DEST="generic/platform=iOS"
  SIGN=(-allowProvisioningUpdates ${DEVELOPMENT_TEAM:+DEVELOPMENT_TEAM=$DEVELOPMENT_TEAM})
else
  DEST="generic/platform=iOS Simulator"
  SIGN=(CODE_SIGNING_ALLOWED=NO)
fi
xcb build "${PROJECT[@]}" -destination "$DEST" -derivedDataPath "$OUT/DerivedData" \
  "${VERSIONS[@]}" "${SIGN[@]}" build
APP=$(find "$OUT/DerivedData/Build/Products" -maxdepth 2 -name MiSTerKVMS.app | head -1)
[[ -n "$APP" ]] || { echo "build failed" >&2; exit 1; }
echo "Built $APP"
