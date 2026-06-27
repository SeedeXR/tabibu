# Changelog

All notable changes to Tabibu are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project aims to adhere to [Semantic Versioning](https://semver.org/).
User-facing entries are written honestly — no inflated counts, no marketing.
The canonical version lives in the root `VERSION` file; `scripts/sync-version.sh`
propagates it into every manifest, so all builds stamp the same version.

## [Unreleased]

Nothing yet.

## [0.1.4] — 2026-06-27

### Added
- **Developer / CLI** view: Homebrew analysis + safe cleanup — `brew cleanup`
  and `autoremove` previews, plus every installed formula/cask sized with its
  install date and dependency status. All removal is delegated to `brew`
  itself; Tabibu never deletes Homebrew files directly.
- New gourd app icon and a themeable in-app brand mark.
- Designed, interactive docs site (`docs/`) generated from Markdown on publish.

### Changed
- New live-scan loader: an animated radar with a running total and per-category
  chips that light up as junk is found.
- Homebrew scan is ~2.4× faster (reads on-disk install receipts instead of the
  slow `brew info --json`).
- Full Disk Access is now granted from **Settings** (was in the Uninstaller).
- Version is single-sourced from the root `VERSION` file.

### Fixed
- Commands run off the main thread, so heavy scans no longer freeze the window
  and Stop works mid-scan.
- Universal (Intel + Apple Silicon) release build compiles cleanly.

## [0.1.3] — 2026-06-16

### Added
- **Duplicates: whole-home scan** — no folder pick required; "Scan Home" by
  default (or choose a folder). Documents/Desktop/Pictures/Mail/iCloud stay
  protected by the denylist even here (reported as skipped, never touched).
- **Force-quit from Memory & CPU** — per-process Quit (SIGTERM); on failure,
  offer Force Quit (SIGKILL). Confirms and warns about unsaved work.
- **Thermal pressure** card (Dashboard + Memory) from `pmset -g therm` —
  honest: exact CPU die temperature needs root, so we show the real
  thermal/throttle signal instead, with a note.
- **Dashboard line graphs** — colorful live CPU% and memory% time-series
  (gradient-filled SVG), plus a **free-space trend** persisted across launches.
- **Uninstaller → Find Leftovers** — disk-wide orphan scan for support files
  whose owning app is gone (Risky tier, never pre-selected); review + trash.
- **Security scan UI** — runs adware/rogue-profile heuristics; review and send
  detections to the locked **quarantine vault** (move + lock, never delete).
- **SMART status** on the Disk view (`diskutil info -plist /`).
- **Full Disk Access onboarding** — explains FDA is the one-time universal
  grant (no per-folder shortcut, no self-grant possible) with a re-check button.
- **Per-volume trashes** — `TrashScanner` now also lists
  `/Volumes/*/.Trashes/<uid>` (external-drive Trash), hidden entries included.

### Notes / honest limits
- Exact CPU **die temperature** and **GPU `powermetrics`** require root (a
  privileged helper we don't ship); thermal *pressure* is shown instead.
- The menu-bar tray remains icon + live tooltip + menu; a **rich popover
  window** is still a follow-up.

## [0.1.2] — 2026-06-16

### Added
- **Rosetta flagging** — `tabibu-monitor` detects translated (x86_64-on-arm64)
  processes via per-pid `sysctl` (`P_TRANSLATED`, empirically verified:
  `p_flag` `0x34004` translated vs `0x4004` native); Memory & CPU shows a
  "Rosetta" badge.
- **Deselection telemetry** — new `tabibu-telemetry` crate: opt-in (default
  OFF), privacy-respecting. Records only {category, tier, size bucket, ts} when
  a user unchecks a `Safe` suggestion — never paths/contents. Turning it off
  deletes collected data. Settings toggle with a plain-language explanation.
- **Mac Health dashboard** (new default view) — honest, measured cards:
  storage free/used, memory %, CPU %, battery (charge/cycles/health), last
  Smart Scan. No invented metrics.
- **Themed hero landings** — per-section accent gradients, glyph, feature list,
  and a prominent Scan button (CleanMyMac-inspired, honest copy).
- **Menu-bar tray** — Tauri tray with a live CPU/memory tooltip and Open/Quit
  menu, replacing the removed Swift `TabibuMonitor` (Swift-free).

### Changed
- **Shell is Tauri v2** (ADR-0003), finalized from the prior SwiftUI shell. The Rust backend
  (`app/src-tauri`) now calls the core crates directly through
  `#[tauri::command]`s — the C-FFI/JSON bridge is gone. Frontend is a static
  HTML/CSS/JS app (`app/src`) using `window.__TAURI__`, with the same design
  system, Lucide icons, and feature views (Smart Scan, Junk, Large & Old,
  Duplicates, Disk treemap, Memory & CPU, Battery, Uninstaller, Startup,
  Security placeholder). Battery/startup facts come from `ioreg`/`pmset`/plist
  parsing (no Objective-C bindings).
- Cache sizing parallelized with `rayon` (`tabibu-junk`): real-home scan
  ~7.8s → ~0.26s.
- CI/release retargeted: `swift build` job → `cargo build` of `app/src-tauri`;
  release runs `tauri build --target universal-apple-darwin`.
- **Zero Swift:** `scripts/make-icon.sh` rewritten to generate the icon via
  SVG + `qlmanage` + `tauri icon` (no `swift` CLI). Removed the dead Swift
  module docs. The only remaining "swift" is `swift-rs`, a transitive crate
  inside Tauri's own macOS plumbing (not our code).

### Removed
- `Tabibu/`, `TabibuMonitor/`, `TabibuHelper/` (Swift shell);
  `core/crates/tabibu-ffi/` + `core/include/` (C ABI); the Swift-era packaging
  scripts (`build-core.sh`, `make-app.sh`, `make-dmg.sh`). ADR-0001/0002 and
  `docs/ffi.md` superseded.

### Fixed
- CI failures: cargo-deny wildcard-path bans (crates marked `publish = false`),
  clippy pedantic version-drift (no longer gated), bench gate cross-machine
  baseline (now `--smoke` on CI).

## [0.1.0] — 2026-06-13

First end-to-end vertical slice: Rust core ↔ Swift shell, building, tested,
and packaged. Not yet distributable (no Developer ID / notarization — see
*Known limitations*).

### Added

**Core engine (`tabibu-engine`)**
- `Scanner` (read-only) / `Reclaimer` (mutating) separation; `SafetyTier`
  (`Safe`/`Review`/`Risky`) and `CleanupItem` model.
- Hard denylist (SIP paths, user data, iCloud, Keychains, Mail, traversal),
  enforced by a guarded sink and property-tested against an adversarial scanner.
- Undo manifest written + fsynced before any mutation; reclaim measures freed
  bytes rather than estimating; tier rules enforced in the engine, not the UI.
- `smart_scan` orchestration: concurrent scanners, per-scanner honest outcomes.

**Scanners**
- Junk (`tabibu-junk`): Trash, user caches (running-app guard), dev caches
  (Xcode/npm/pnpm/yarn/pip/Cargo/Homebrew), temp, logs, large & old files.
- Duplicates (`tabibu-dupes`): 3-stage funnel (size → 16 KiB head/tail sample →
  full `blake3`), keeps newest copy, Review tier.
- Uninstaller (`tabibu-uninstall`): bundle-ID + fuzzy remnant hunt, orphan
  sweep, unused-app detection (`kMDItemLastUsedDate`), broken-symlink audit.
- Walk (`tabibu-walk`): parallel size tree for the space map.
- Malware (`tabibu-malware`): native adware launch-agent + rogue managed-profile
  heuristics; quarantine vault (move + lock `0o000`, never delete, refuses
  system paths).
- Monitor (`tabibu-monitor`): system + per-process sampling via `sysinfo`.

**Bridge & app**
- Hand-written C ABI (`tabibu-ffi`, ADR-0001): universal `libtabibu_ffi.a`,
  JSON payloads, Rust-allocates/Rust-frees, versioned (`FFI_VERSION = 1`),
  6 round-trip contract tests.
- SwiftUI app: sidebar IA; reusable Scan → Review → Reclaim → Result flow
  (Smart Scan, Junk, Large & Old); Duplicates; Disk treemap with free-space +
  APFS-snapshot reporting; Memory & CPU with real memory-pressure dial and
  honest daemon explainers (no "free RAM" button); Battery (IOKit); Uninstaller;
  Startup Items. Lucide icons (ISC) as tintable templates.
- `TabibuMonitor`: pure-AppKit menu-bar agent (login item).
- `TabibuHelper`: XPC helper skeleton with allowlisted protocol and
  audit-token code-sign validation (DEBUG fallback gated).

**Tooling & docs**
- Packaging: programmatic app icon, universal `.app` assembly, LZMA (ULMO) DMG,
  self-uninstaller, criterion-based regression bench gate (>5%).
- CI workflows, `cargo deny` + `rustfmt` config.
- ADR-0001 (FFI), ADR-0002 (SwiftPM), `docs/ffi.md`, `setup.md`, `release.md`,
  and per-module docs with mermaid diagrams.

### Changed

- Bundle identifier set to `xr.seede.tabibu`.
- Monitor RSS budget recorded as a measured **70 MB** ceiling (CPU < 1% stays
  the hard gate); the guide's 30 MB was illustrative and unachievable for a
  native menu-bar app.

### Security

- Privileged work is confined to a tiny XPC helper with a fixed, allowlisted
  command surface — no "run arbitrary path as root".

### Known limitations

- **Not distributable yet:** no Developer ID certificate or notarization
  credentials in the build environment, so builds are ad-hoc signed and
  Gatekeeper rejects them on other Macs. The signing/notarization/Sparkle
  pipeline is wired and conditional (`docs/release.md`).
- **ClamAV** ships as a feature-gated stub (`engine_available() → false`);
  v1 relies on native heuristics. **Endpoint Security** real-time scanning is
  deferred (Apple entitlement).
- **SMART** disk status is a helper stub; Rosetta-process flagging, free-space
  *trend*, GPU `powermetrics`, and deselection telemetry are not yet built.
- Dedupe benchmark fixture is 2k files (scale to 100k before release).

[Unreleased]: https://example.com/tabibu/compare/v0.1.0...HEAD
[0.1.0]: https://example.com/tabibu/releases/tag/v0.1.0
