#!/usr/bin/env bash
# Build the macOS app: Rust core → Swift bindings → SwiftPM → .app bundle.
# Usage: scripts/build-mac.sh [--run]
#
# Env:
#   SIGN_IDENTITY  codesign identity (default "Apple Development"; "-" for
#                  ad-hoc). A "Developer ID Application" identity also turns
#                  on the hardened runtime + timestamp needed for notarization.
#                  Ad-hoc signatures change every build, so macOS forgets the
#                  Accessibility grant each time.
#   UNIVERSAL=1    build arm64 + x86_64 (needs rustup with both targets).
#   VERSION        CFBundleShortVersionString (default: from Cargo.toml).
#   BUILD_NUMBER   CFBundleVersion (default: git commit count).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
APP="$ROOT/build/MiSTerKVMS.app"
SIGN_IDENTITY=${SIGN_IDENTITY:-Apple Development}
VERSION=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)}
BUILD_NUMBER=${BUILD_NUMBER:-$(git -C "$ROOT" rev-list --count HEAD 2>/dev/null || echo 1)}
cd "$ROOT"
# Match LSMinimumSystemVersion. Homebrew's Rust ships a std built for the host
# macOS; release builds use rustup's toolchain, which honours this.
export MACOSX_DEPLOYMENT_TARGET=14.0

echo "==> Rust core ($VERSION build $BUILD_NUMBER)"
if [[ "${UNIVERSAL:-}" == "1" ]]; then
  ARCHS=(aarch64-apple-darwin x86_64-apple-darwin)
  for t in "${ARCHS[@]}"; do cargo build --release -p kvms-core -p kvms-cli --target "$t"; done
  mkdir -p target/universal
  lipo -create -output target/universal/kvms \
    target/aarch64-apple-darwin/release/kvms target/x86_64-apple-darwin/release/kvms
  CLI_BIN=target/universal/kvms
  BINDINGS_LIB=target/aarch64-apple-darwin/release/libkvms_core.dylib
else
  cargo build --release -p kvms-core -p kvms-cli
  BINDINGS_LIB=target/release/libkvms_core.dylib
  CLI_BIN=target/release/kvms
fi

echo "==> Swift bindings"
cargo run -q --release -p uniffi-bindgen -- generate \
  --library "$BINDINGS_LIB" --language swift --out-dir target/uniffi-swift
mkdir -p apple/Sources/KVMSCore/Generated apple/Sources/kvms_coreFFI/include
cp target/uniffi-swift/kvms_core.swift apple/Sources/KVMSCore/Generated/
cp target/uniffi-swift/kvms_coreFFI.h apple/Sources/kvms_coreFFI/include/
cp target/uniffi-swift/kvms_coreFFI.modulemap apple/Sources/kvms_coreFFI/include/module.modulemap

echo "==> Swift app"
if [[ "${UNIVERSAL:-}" == "1" ]]; then
  # One build per architecture (multi-arch SwiftPM builds use a different
  # build system that rejects per-target language modes), then lipo.
  for pair in arm64:aarch64-apple-darwin x86_64:x86_64-apple-darwin; do
    arch=${pair%%:*}; triple=${pair#*:}
    (cd apple && KVMS_RUST_LIB="$ROOT/target/$triple/release/libkvms_core.a" \
      swift build -c release --arch "$arch" --scratch-path ".build-$arch")
  done
  mkdir -p target/universal
  BIN=target/universal/MiSTerKVMS
  lipo -create -output "$BIN" \
    "$(cd apple && swift build -c release --arch arm64 --scratch-path .build-arm64 --show-bin-path)/MiSTerKVMS" \
    "$(cd apple && swift build -c release --arch x86_64 --scratch-path .build-x86_64 --show-bin-path)/MiSTerKVMS"
else
  (cd apple && swift build -c release)
  BIN=$(cd apple && swift build -c release --show-bin-path)/MiSTerKVMS
fi

echo "==> Bundle"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/MiSTerKVMS"
# The command-line tool ships inside the app (signed and notarized with it).
cp "$CLI_BIN" "$APP/Contents/MacOS/kvms"
cp apple/Info.plist "$APP/Contents/Info.plist"
cp apple/Icon/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $BUILD_NUMBER" "$APP/Contents/Info.plist"

SIGN_ARGS=(--force --sign "$SIGN_IDENTITY")
if [[ "$SIGN_IDENTITY" == Developer\ ID* ]]; then
  SIGN_ARGS+=(--options runtime --timestamp --entitlements apple/MiSTerKVMS.entitlements)
fi
# Nested executables are signed before the bundle.
codesign "${SIGN_ARGS[@]}" --identifier io.github.misterkvms.kvms "$APP/Contents/MacOS/kvms"
codesign "${SIGN_ARGS[@]}" "$APP"
lipo -archs "$APP/Contents/MacOS/MiSTerKVMS" | sed 's/^/architectures: /'
echo "Built $APP"

if [[ "${1:-}" == "--run" ]]; then
  open "$APP"
fi
