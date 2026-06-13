<div align="center">

# Tabibu

**A high-performance, honest macOS optimization tool.**
Rust core + Swift/SwiftUI shell · benchmark-gated · safety-first.

</div>

---

Tabibu (*Swahili: physician / healer*) is a CleanMyMac-class utility that wins
on **honesty, safety, and measurable speed** — no scareware, no fake "GB freed",
no resident background hog. The cleanup logic is the easy 20%; the discipline
around it (safety invariants, benchmark gates, honest UX) is the product.

> **macOS only** (Apple Silicon + Intel). Target: macOS 14+. Distribution:
> notarized, non-sandboxed direct download (signing/notarization pending a
> Developer ID — see [Limitations](#current-limitations)).

This README is for developers. The deeper "why" lives in
[`memory/`](#the-knowledge-base-memory); architecture and contracts live in
[`docs/`](#documentation-map).

## Table of contents

- [Quick start](#quick-start)
- [Repository layout](#repository-layout)
- [Architecture](#architecture)
- [Building](#building)
- [Testing & quality gates](#testing--quality-gates)
- [Packaging & release](#packaging--release)
- [The knowledge base (`memory/`)](#the-knowledge-base-memory)
- [Documentation map](#documentation-map)
- [Contributing workflow](#contributing-workflow)
- [Current limitations](#current-limitations)

## Quick start

```sh
# Prerequisites: Xcode (full), Rust via rustup with both Apple targets
rustup target add aarch64-apple-darwin x86_64-apple-darwin

# 1. Build the Rust core → universal static lib staged into build/
scripts/build-core.sh                      # --debug for fast iteration

# 2. Build & smoke-test the app (links the static lib)
swift build --package-path Tabibu
Tabibu/.build/debug/Tabibu --version        # -> "Tabibu 0.1.0 (ffi v1)"

# 3. Run the full core test suite
cd core && cargo test --workspace           # 95 tests
```

To produce a runnable bundle + DMG, see [Packaging & release](#packaging--release).

## Repository layout

```
tabibu/
├── core/                       # Rust workspace → libtabibu_ffi.a (universal)
│   ├── Cargo.toml              # [workspace] + shared deps, lints, release profile
│   ├── deny.toml               # cargo-deny: licenses + advisories
│   ├── rustfmt.toml            # stable-only formatting config
│   ├── include/tabibu_core.h   # hand-maintained C ABI header (lockstep w/ -ffi)
│   └── crates/
│       ├── tabibu-engine/      # traits, SafetyTier, denylist, undo, orchestration
│       ├── tabibu-walk/        # parallel fs traversal + size tree
│       ├── tabibu-dupes/       # 3-stage blake3 duplicate funnel
│       ├── tabibu-junk/        # cache/temp/log/trash/large-old scanners
│       ├── tabibu-uninstall/   # remnants, orphans, unused apps, stale binaries
│       ├── tabibu-malware/     # adware/profile heuristics + quarantine vault
│       ├── tabibu-monitor/     # sysinfo system + per-process sampling
│       ├── tabibu-ffi/         # the ONLY C ABI surface (unsafe lives here)
│       └── tabibu-bench/       # (criterion benches live per-crate in benches/)
├── Tabibu/                     # SwiftUI app (SwiftPM)
│   └── Sources/
│       ├── CTabibuCore/        # C target exposing the FFI header to Swift
│       └── Tabibu/             # Core/ Model/ Views/ Components/ Services/
├── TabibuMonitor/              # menu-bar agent (pure AppKit, SwiftPM)
├── TabibuHelper/               # privileged XPC helper skeleton (SwiftPM)
├── scripts/                    # build-core, make-icon/app/dmg, bench-gate, uninstall
├── docs/                       # ADRs, FFI contract, module guides (mermaid)
├── memory/                     # the project "brain" — read this first
├── build/                      # generated artifacts (gitignored)
└── .github/workflows/          # CI + release pipelines
```

## Architecture

Two languages, split along their strengths (full rationale: ADR-0001/0002 and
`memory/architecture.md`):

- **Rust core** does all throughput work — walking, hashing, scanning,
  reclaiming — and owns the safety invariants. Compiled to a universal static
  library `libtabibu_ffi.a`.
- **Swift/SwiftUI shell** does the native macOS UI and platform access (IOKit,
  TCC/FDA, `SMAppService`, XPC). Four signed binaries: the app, the menu-bar
  monitor, the privileged helper, and the linked-in Rust lib.

```
┌─ Tabibu.app (SwiftUI) ─┐   C ABI / JSON   ┌─ libtabibu_ffi.a (Rust) ─┐
│ views, review, reclaim │◄────────────────►│ engine + scanners (in-proc)│
└───────────┬────────────┘   (ADR-0001)     └────────────────────────────┘
            │ XPC (allowlisted)
   ┌────────▼─────────┐                      ┌──────────────────────────┐
   │ TabibuHelper(root)│                     │ TabibuMonitor (menu-bar) │
   └──────────────────┘                      └──────────────────────────┘
```

**Safety backbone** (engine, property- and golden-image-tested): scanning is
read-only behind a denylist guard; reclaim is the *only* mutating path, writes
an undo manifest before touching anything, trashes rather than deletes below
`Safe`, and reports *measured* freed bytes. The denylist invariant — *no
returned path escapes the allowed roots* — is property-tested against a
deliberately malicious scanner.

**FFI** (`tabibu-ffi`, the only crate allowed `unsafe`): ~10 hand-written
`extern "C"` functions, composite data as JSON, cancellation via opaque op
handles, streaming via C callbacks, `Rust allocates / Rust frees`, ABI version
asserted by Swift at launch. Contract + diagrams: `docs/ffi.md`.

## Building

All commands assume repo root unless noted. Verified on macOS 26.5 / Xcode 26.5
/ Rust 1.94.

| Goal | Command |
|---|---|
| Rust core (universal, staged to `build/`) | `scripts/build-core.sh` |
| Rust core (fast, debug) | `scripts/build-core.sh --debug` |
| App (single-arch, dev) | `swift build --package-path Tabibu` |
| App (universal, release) | `swift build -c release --arch arm64 --arch x86_64 --package-path Tabibu` |
| Menu-bar agent | `swift build -c release --arch arm64 --arch x86_64 --package-path TabibuMonitor` |
| Helper (build-verify only) | `swift build --package-path TabibuHelper` |

The Swift packages link the Rust lib via `linkerSettings` (`-L<root>/build
-ltabibu_ffi`); run `build-core.sh` first. `build-core.sh` exports
`MACOSX_DEPLOYMENT_TARGET=14.0` to match `Package.swift` — building the Rust
lib by hand without it produces ld version warnings.

## Testing & quality gates

These are merge requirements, not suggestions (`memory/test.md`):

```sh
cd core
cargo test --workspace            # 95 tests: unit, integration, property, golden-image
cargo clippy --workspace --all-targets   # deny(warnings) + pedantic
cargo fmt --check                 # rustfmt clean
cargo build -p tabibu-malware --features clamav   # feature-gated path compiles

# Performance: criterion benches + >5% regression gate
scripts/bench-gate.sh --update-baseline   # bless current numbers into core/benches-baseline/
scripts/bench-gate.sh                      # fail if any bench regresses >5% vs baseline

# Monitor resource budget (hard CPU gate; honest RSS ceiling)
swift build -c release --arch arm64 --arch x86_64 --package-path TabibuMonitor
TabibuMonitor/budget-test.sh
```

What's enforced and why:
- **Safety invariants** are property-tested (`tabibu-engine/tests/denylist_prop.rs`)
  and golden-image-tested (`golden_reclaim.rs`: snapshot → reclaim → assert
  exactly the intended files changed; plus fault injection).
- **FFI** has round-trip contract tests (`tabibu-ffi/tests/roundtrip.rs`) that
  drive the C ABI exactly as Swift does. Changing the surface means changing
  that test + the header + `CoreBridge.swift` + the version constant together.
- **Benchmarks** gate merges: `bench-gate.sh` parses criterion's
  `estimates.json` and fails on >5% mean regression.

## Packaging & release

```sh
scripts/make-icon.sh     # build/AppIcon.icns (generated programmatically)
scripts/make-app.sh      # build/Tabibu.app (universal, icon, monitor login item, signed)
scripts/make-dmg.sh      # build/Tabibu-<VERSION>.dmg (LZMA/ULMO, mount-verified)
```

- `SIGN_IDENTITY` defaults to `-` (ad-hoc). With a real Developer ID it enables
  Hardened Runtime; set `NOTARIZE=1` on `make-dmg.sh` once `notarytool`
  credentials exist.
- DMG uses ULMO (LZMA) for best ratio, falling back to ULFO/UDZO if
  unsupported, and prints the achieved ratio.
- `scripts/uninstall-tabibu.sh` is the honest self-uninstaller (`--dry-run` by
  default; `--yes` to act).
- Full pipeline and the external blockers: `docs/release.md`.

## The knowledge base (`memory/`)

`memory/` is the project's shared brain — read it at the start of any session
(`memory/session_start.md`). Key files:

| File | What it holds |
|---|---|
| `tabibu-engineering-guide.md` | The technical spine: platform constraints, features, milestones |
| `agent_profile.md` | Operating contract: zero-hallucination, testing culture |
| `architecture.md` / `mindmap.md` | System structure (deep / at-a-glance) |
| `philosophy.md` | Why safety > honesty > performance > features |
| `mac_pain_points.md` | The real Mac complaints each feature targets |
| `test.md` | Testing methodology + resource budgets |
| `design.md` | Design system, the Scan→Review→Reclaim flow, tokens, a11y |
| `todo.md` | Roadmap M0–M8 + honest status of what's done |
| `handover_session.md` | Timestamped dev log — newest first |

## Documentation map

| Location | Content |
|---|---|
| `docs/adr/` | Architecture Decision Records (FFI choice, SwiftPM) |
| `docs/ffi.md` | The C ABI contract + scan/reclaim flow diagrams |
| `docs/setup.md` | Clean-machine build instructions + gotchas |
| `docs/release.md` | Signing, notarization, compression, external blockers |
| `docs/modules/*.md` | Per-crate/component guides with mermaid diagrams |

## Contributing workflow

1. **Start** by reading the last 2–3 `memory/handover_session.md` entries and
   the active milestone in `memory/todo.md`.
2. **Branch** `m<N>/<topic>`; commit messages scoped (`engine:`, `dupes:`,
   `shell:`, `ci:`).
3. **For a new scanner:** implement `Scanner`, register in your crate's
   `scanners()`, add fixtures + tests, add a bench if it's on a hot path, wire
   the id through `tabibu-ffi`'s scan registry and the UI.
4. **For any mutating change:** design the undo first; add a golden-image test
   before implementing.
5. **Before pushing:** the gates in [Testing](#testing--quality-gates) must be
   green, and the relevant `docs/` / `memory/` files updated in the same change
   (stale docs are bugs).
6. **End** with a timestamped `handover_session.md` entry.

## Current limitations

Honest, and tracked in `memory/todo.md`:

- **Not distributable yet** — no Developer ID / notarization credentials in the
  build environment; builds are ad-hoc signed (Gatekeeper rejects on other
  Macs). The pipeline is wired and conditional.
- **ClamAV** is a feature-gated stub; v1 ships native adware heuristics.
  Real-time (Endpoint Security) scanning is deferred (Apple entitlement).
- **SMART** status is a helper stub; Rosetta flagging, free-space *trend*, GPU
  `powermetrics`, and deselection telemetry are not yet built.
- The privileged **helper** is build-verified only (install needs the signed
  app via `SMAppService`).

## License

License TBD — note the GPL boundary around any future `libclamav` bundling
(`memory/project_context.md`).
