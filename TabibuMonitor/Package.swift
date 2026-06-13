// swift-tools-version: 5.10
// TabibuMonitor — menu-bar agent, SwiftPM instead of .xcodeproj (ADR-0002),
// mirroring Tabibu/Package.swift. Links the prebuilt Rust staticlib in
// <root>/build (scripts/build-core.sh). Bundled as a login item by
// scripts/make-app.sh.
import PackageDescription
import Foundation

// Absolute path to the repo root (this file lives at <root>/TabibuMonitor/).
let repoRoot = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()   // Package.swift
    .deletingLastPathComponent()   // TabibuMonitor/
    .path

let package = Package(
    name: "TabibuMonitor",
    platforms: [.macOS(.v14)],
    targets: [
        // C bridge: the hand-maintained header for libtabibu_ffi.a.
        // Duplicated from Tabibu/Sources/CTabibuCore on purpose — each
        // package stays self-contained (no cross-package path coupling).
        .target(
            name: "CTabibuCore",
            path: "Sources/CTabibuCore"
        ),
        .executableTarget(
            name: "TabibuMonitor",
            dependencies: ["CTabibuCore"],
            path: "Sources/TabibuMonitor",
            linkerSettings: [
                .unsafeFlags(["-L\(repoRoot)/build"]),
                .linkedLibrary("tabibu_ffi"),
                // Drop unreferenced static-lib code (the monitor calls only
                // tabibu_monitor_sample, not the dupes/walk/junk surface).
                .unsafeFlags(["-Xlinker", "-dead_strip"]),
            ]
        ),
    ]
)
