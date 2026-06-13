// swift-tools-version: 5.10
// TabibuHelper — privileged XPC helper. Build-verifiable here; it can only be
// *installed* via SMAppService from a signed, notarized app bundle (ADR-0002,
// docs/modules/helper.md). No Developer ID on this machine → install/runtime
// are externally blocked, documented honestly.
import PackageDescription

let package = Package(
    name: "TabibuHelper",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(name: "TabibuHelper", path: "Sources/TabibuHelper")
    ]
)
