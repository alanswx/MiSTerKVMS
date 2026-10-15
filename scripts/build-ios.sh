#!/usr/bin/env bash
# Build the iPhone app: Rust core (device + simulator) → KVMSCore.xcframework
# → Swift bindings → xcodebuild.
# Usage: scripts/build-ios.sh [--core-only] [--device]
#   --core-only  stop after the XCFramework and bindings (then build in Xcode:
#                open ios/MiSTerKVMS.xcodeproj)
#   --device     build for a real iPhone (needs DEVELOPMENT_TEAM, or a team
#                picked in Xcode); default: the simulator
#
# Env:
#   DEVELOPMENT_TEAM  Apple team id for signing device builds.
#   VERSION           CFBundleShortVersionString (default: from Cargo.toml).
#   BUILD_NUMBER      CFBundleVersion (default: git commit count).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)}
BUILD_NUMBER=${BUILD_NUMBER:-$(git -C "$ROOT" rev-list --count HEAD 2>/dev/null || echo 1)}
OUT="$ROOT/build/ios"
cd "$ROOT"

CORE_ONLY=0; DEVICE=0
for arg in "$@"; do
  case "$arg" in
    --core-only) CORE_ONLY=1 ;;
    --device) DEVICE=1 ;;
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

echo "==> iPhone app"
if [[ "$DEVICE" == "1" ]]; then
  DEST="generic/platform=iOS"
  SIGN=(-allowProvisioningUpdates ${DEVELOPMENT_TEAM:+DEVELOPMENT_TEAM=$DEVELOPMENT_TEAM})
else
  DEST="generic/platform=iOS Simulator"
  SIGN=(CODE_SIGNING_ALLOWED=NO)
fi
xcodebuild -project ios/MiSTerKVMS.xcodeproj -scheme MiSTerKVMS -configuration Release \
  -destination "$DEST" -derivedDataPath "$OUT/DerivedData" \
  MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="$BUILD_NUMBER" "${SIGN[@]}" \
  build | grep -E "error|warning: |BUILD" || true
APP=$(find "$OUT/DerivedData/Build/Products" -maxdepth 2 -name MiSTerKVMS.app | head -1)
[[ -n "$APP" ]] || { echo "build failed" >&2; exit 1; }
echo "Built $APP"
