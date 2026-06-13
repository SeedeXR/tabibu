# Tabibu — Build & Setup

From clean machine to running app. Verified on macOS 26.5 / Xcode 26.5 /
Rust 1.94 (2026-06-13). Re-verify each milestone (docs.md rule 4).

## Prerequisites

- Xcode (full, not just CLT) — `xcodebuild -version`
- Rust via rustup with both targets:
  `rustup target add aarch64-apple-darwin x86_64-apple-darwin`

## Build everything

```sh
# 1. Rust core → universal static lib staged into build/
scripts/build-core.sh                 # add --debug for fast iteration

# 2. Main app (SwiftPM)
swift build --package-path Tabibu
Tabibu/.build/debug/Tabibu --version   # must print "Tabibu 0.1.0 (ffi v1)"

# 3. Menu-bar agent + helper
swift build --package-path TabibuMonitor
swift build --package-path TabibuHelper

# 4. App bundle + DMG (ad-hoc signed locally; see docs/release.md)
scripts/make-icon.sh                  # build/AppIcon.icns
scripts/make-app.sh                   # build/Tabibu.app
scripts/make-dmg.sh                   # build/Tabibu-<ver>.dmg (LZMA)
```

## Test & verify

```sh
cd core
cargo test --workspace                # all suites
cargo clippy --workspace --all-targets   # deny(warnings) + pedantic
cargo bench -p tabibu-walk -p tabibu-dupes   # criterion benches
scripts/bench-gate.sh                 # fails on >5% regression vs baseline
TabibuMonitor/budget-test.sh          # monitor CPU/RSS budget
```

## Gotchas

- **Deployment target:** `build-core.sh` exports
  `MACOSX_DEPLOYMENT_TARGET=14.0` to match `Package.swift`; building the Rust
  lib by hand without it causes ld version warnings.
- **FDA:** scans of `~/Library/Safari` etc. return empty without Full Disk
  Access — that's the TCC design, not a bug; the app detects and deep-links.
- **No Developer ID on this machine:** bundles are ad-hoc signed; Gatekeeper
  (`spctl -a`) will reject them on other Macs. External blocker tracked in
  `docs/release.md`.
