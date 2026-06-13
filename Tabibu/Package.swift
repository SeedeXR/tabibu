// swift-tools-version: 5.10
// Tabibu main app — SwiftPM instead of .xcodeproj (ADR-0002): fully
// scriptable, diffable, no hand-maintained pbxproj. The .app bundle is
// assembled by scripts/make-app.sh.
import PackageDescription
import Foundation

// Absolute path to the repo root (this file lives at <root>/Tabibu/).
let repoRoot = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()   // Package.swift
    .deletingLastPathComponent()   // Tabibu/
    .path

let package = Package(
    name: "Tabibu",
    platforms: [.macOS(.v14)],
    targets: [
        // C bridge: just the hand-maintained header for libtabibu_ffi.a
        // (built by scripts/build-core.sh into <root>/build/).
        .target(
            name: "CTabibuCore",
            path: "Sources/CTabibuCore"
        ),
        .executableTarget(
            name: "Tabibu",
            dependencies: ["CTabibuCore"],
            path: "Sources/Tabibu",
            resources: [.copy("Resources/Icons")],
            linkerSettings: [
                .unsafeFlags(["-L\(repoRoot)/build"]),
                .linkedLibrary("tabibu_ffi"),
            ]
        ),
    ]
)
