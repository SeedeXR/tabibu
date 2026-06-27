# Tabibu — Build & Setup

From a clean machine to a running app. Verified on macOS 26.5 / Rust 1.94 /
Node 22 (2026-06-13). The shell is Tauri v2 (ADR-0003).

## Prerequisites

- **Rust** via rustup. For a universal release build also:
  `rustup target add aarch64-apple-darwin x86_64-apple-darwin`
- **Node + npm** — only to install the Tauri CLI (the frontend itself has no
  bundler/build step).
- **Xcode Command Line Tools** (`xcode-select --install`) for the macOS WebView
  SDK and `cc`.

## Build & run

```sh
cd app
npm install                 # installs @tauri-apps/cli locally (one-time)

# Develop with hot reload (compiles the Rust backend + core crates):
npm run dev                 # = npx tauri dev

# Compile-only check of the backend:
cargo build --manifest-path src-tauri/Cargo.toml

# Bundle a .app + DMG:
npx tauri build --debug                              # fast, for testing
npx tauri build --target universal-apple-darwin      # release, universal
```

The first backend build compiles the Tauri dependency tree (wry/objc2/…) —
slow once, cached after.

## Test & verify (core)

```sh
cd core
cargo test --workspace                  # 110 tests
cargo clippy --workspace --all-targets  # clippy::all denied
cargo fmt --check
cargo bench -p tabibu-walk -p tabibu-dupes   # criterion benches
scripts/bench-gate.sh --smoke           # runner-aware bench run
```

## App icon

Run `npx tauri icon <1024.png>` (from `app/`) to regenerate the full icon set
into `app/src-tauri/icons/` (`icon.icns`, `icon.png`, sized PNGs, Square/Store
logos) whenever the brand mark changes.

## Gotchas

- **FDA:** scans of `~/Library/Safari` etc. return empty without Full Disk
  Access — that's the TCC design, not a bug; the app detects it
  (`system_info` → the sidebar "Limited access" indicator) and deep-links to
  Privacy settings.
- **No Developer ID on this machine:** `tauri build` produces an unsigned
  bundle; Gatekeeper rejects it on other Macs without right-click → Open.
  Signing/notarization are wired and conditional — see `docs/release.md`.
- **Backend ↔ core is not FFI:** edit `app/src-tauri/src/commands.rs` to add a
  command; the core types serialize via serde automatically.
