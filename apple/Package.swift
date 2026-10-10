// swift-tools-version:6.0
import Foundation
import PackageDescription

// Built by scripts/build-mac.sh, which runs cargo first. KVMS_RUST_LIB points
// at a universal (lipo'd) library for release builds.
let rustLib = Context.environment["KVMS_RUST_LIB"] ?? URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()
    .appendingPathComponent("../target/release/libkvms_core.a")
    .standardizedFileURL.path

let package = Package(
    name: "MiSTerKVMS",
    platforms: [.macOS(.v14)],
    targets: [
        // C module for the UniFFI-generated header (kvms_coreFFI.h).
        .target(
            name: "kvms_coreFFI",
            path: "Sources/kvms_coreFFI",
            // Frameworks the Rust core needs (from `--print native-static-libs`).
            linkerSettings: [
                .unsafeFlags([rustLib]),
                .linkedFramework("QuartzCore"), .linkedFramework("CoreGraphics"),
                .linkedFramework("Metal"), .linkedFramework("AudioToolbox"),
                .linkedFramework("CoreAudio"), .linkedFramework("Foundation"),
                .linkedFramework("CoreFoundation"), .linkedLibrary("objc"),
                .linkedLibrary("iconv"),
            ]
        ),
        // UniFFI-generated Swift bindings for the Rust core.
        .target(
            name: "KVMSCore",
            dependencies: ["kvms_coreFFI"],
            path: "Sources/KVMSCore",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
        .executableTarget(
            name: "MiSTerKVMS",
            dependencies: ["KVMSCore"],
            path: "Sources/MiSTerKVMS",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
