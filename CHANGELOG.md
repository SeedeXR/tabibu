# Changelog

All notable changes to Tabibu are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project aims to adhere to [Semantic Versioning](https://semver.org/).
User-facing entries are written honestly — no inflated counts, no marketing
(see `memory/philosophy.md` §2). The canonical version lives in `VERSION` and
`core/Cargo.toml` (`workspace.package.version`); keep them in sync.

## [Unreleased]

Nothing yet.

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
  native menu-bar app. See `memory/test.md` and `docs/modules/monitor.md`.

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
