# ADR-0003: Tauri shell replaces the SwiftUI shell

Date: 2026-06-13 · Status: Accepted · **Supersedes ADR-0001 and ADR-0002**

## Context

The SwiftUI shell (ADR-0002) reached the UI layer and proved buggy and slow to
iterate on — layout instability, fragile state, and a heavy C-FFI/JSON-string
bridge (ADR-0001) just to move data between Rust and Swift. The core is
already Rust; the bridge was overhead that existed solely because the shell was
a different language.

## Decision

Replace the entire Swift shell (`Tabibu/`, `TabibuMonitor/`, `TabibuHelper/`)
and the C-FFI crate (`tabibu-ffi`, `core/include/`) with a **Tauri v2**
application in `app/`:

- **`app/src-tauri/`** — a standalone Rust binary (not a member of the `core/`
  workspace) that depends on the core crates *by path* and exposes them through
  `#[tauri::command]` functions. **No FFI**: Tauri serializes command
  arguments and return values with serde, which the core types already derive.
  Streaming scan results use a Tauri `Channel`.
- **`app/src/`** — a static HTML/CSS/JS frontend (no bundler, no Node build
  step; `withGlobalTauri` exposes `window.__TAURI__`). Premium design system,
  Lucide icons (ISC) inlined.

## Rationale

- **The bridge disappears.** The core is Rust; the Tauri backend is Rust; they
  link directly. ADR-0001's hand-written C ABI and JSON-string marshalling are
  gone, along with an entire class of ownership/lifetime/version-drift bugs.
- **One language end to end** (Rust) for all logic; the view layer is the web
  platform, which is fast to build and style consistently.
- **Feasibility verified** before committing: Tauri v2 + its macOS WebView
  deps fetch and compile here; the app bundles (`Tabibu.app` + DMG) and
  launches with the full UI rendering and IPC working (`system_info` drives the
  Full-Disk-Access indicator).
- **Workspace stays lean**: `app/src-tauri` is outside the `core/` workspace,
  so the heavy Tauri tree never slows `cargo test --workspace` in `core/`.

## Consequences

- **Removed:** `Tabibu/`, `TabibuMonitor/`, `TabibuHelper/`,
  `core/crates/tabibu-ffi/`, `core/include/`, and the Swift-era packaging
  scripts (`build-core.sh`, `make-app.sh`, `make-dmg.sh` — Tauri's
  `tauri build` handles universal compile + `.app`/DMG bundling).
- **CI/release** retargeted: the `swift build` job becomes a `cargo build` of
  `app/src-tauri`; release runs `tauri build --target universal-apple-darwin`.
- **The menu-bar monitor and privileged helper are not yet reimplemented.**
  Tauri has a system-tray API (replaces TabibuMonitor) and a privileged helper
  would be Rust; both are follow-ups. Monitoring currently lives in the app's
  Memory & CPU view.
- **`docs/ffi.md`, ADR-0001, ADR-0002 are superseded** — kept for history with
  a superseded banner.
- Reclaim and all scanning remain entirely user-space (the `trash` crate +
  read-only scanners), so no privileged helper is required for current features.

## Verification

`cargo build` of `app/src-tauri` is clean; `npx tauri build --debug` produces
`Tabibu.app` + DMG; the app launches and renders the sidebar, nav, icons, and
FDA state. Core workspace unchanged: 95 tests still pass.
