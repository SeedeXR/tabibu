<div align="center">

# Tabibu

**A high-performance, honest macOS optimization tool.**
Rust core + Tauri shell · benchmark-gated · safety-first.

</div>

---

Tabibu (*Swahili: physician / healer*) is a CleanMyMac-class utility that wins
on **honesty, safety, and measurable speed** — no scareware, no fake "GB freed",
no resident background hog. The cleanup logic is the easy 20%; the discipline
around it (safety invariants, benchmark gates, honest UX) is the product.

> **macOS only** (Apple Silicon + Intel). Target: macOS 13+. Distribution:
> notarized direct download (signing/notarization pending a Developer ID —
> see [Limitations](#current-limitations)).

This README is for developers. The deeper "why" lives in
[`memory/`](#the-knowledge-base-memory); architecture decisions live in
[`docs/adr/`](#documentation-map).

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
# Prerequisites: Rust (rustup), Node + npm (for the Tauri CLI only).
cd app
npm install                 # installs @tauri-apps/cli locally

# Run the app with hot-reload (compiles the Rust backend + core crates):
npm run dev                 # = tauri dev

# Or just compile the backend to check it builds:
cargo build --manifest-path src-tauri/Cargo.toml

# Run the core test suite (fast — the Tauri tree is not in this workspace):
cd ../core && cargo test --workspace      # 95 tests
```

## Repository layout

```
tabibu/
├── core/                       # Rust workspace — the engine + scanners
│   ├── Cargo.toml              # [workspace] + shared deps, lints, release profile
│   ├── deny.toml               # cargo-deny: licenses + advisories
│   ├── rustfmt.toml            # stable-only formatting config
│   └── crates/
│       ├── tabibu-engine/      # traits, SafetyTier, denylist, undo, orchestration
│       ├── tabibu-walk/        # parallel fs traversal + size tree
│       ├── tabibu-dupes/       # 3-stage blake3 duplicate funnel
│       ├── tabibu-junk/        # cache/temp/log/trash/large-old scanners (rayon-parallel)
│       ├── tabibu-uninstall/   # remnants, orphans, unused apps, stale binaries
│       ├── tabibu-malware/     # adware/profile heuristics + quarantine vault
│       ├── tabibu-monitor/     # sysinfo system + per-process sampling
│       └── tabibu-bench/       # criterion benches (also live per-crate in benches/)
├── app/                        # Tauri v2 desktop shell
│   ├── package.json            # @tauri-apps/cli (CLI only; frontend has no bundler)
│   ├── src/                    # static frontend: index.html, styles.css, main.js, icons.js
│   └── src-tauri/              # Rust backend — calls the core crates directly (no FFI)
│       ├── Cargo.toml          # standalone package, path deps into ../../core/crates
│       ├── tauri.conf.json     # window, bundle, CSP
│       ├── capabilities/       # Tauri v2 permissions
│       ├── icons/              # app icon set
│       └── src/                # main.rs, commands.rs, system.rs
├── scripts/                    # make-icon, bench-gate, uninstall-tabibu
├── docs/                       # ADRs, module guides (mermaid)
├── memory/                     # the project "brain" — read this first
└── .github/workflows/          # CI + release pipelines
```

## Architecture

One language end to end (Rust) for all logic; the view layer is the web
platform. Full rationale: **ADR-0003** (and `memory/architecture.md`). The
project began with a SwiftUI shell over a C-FFI bridge — that was replaced by
Tauri, which removed the bridge entirely (ADR-0001/0002 are superseded).

- **Rust core** (`core/`) does all throughput work — walking, hashing,
  scanning, reclaiming — and owns the safety invariants.
- **Tauri shell** (`app/`): the Rust backend (`src-tauri`) depends on the core
  crates *by path* and exposes them as `#[tauri::command]`s; the frontend
  (`src`) is static HTML/CSS/JS using `window.__TAURI__`.

```
┌─ app/src (web UI) ─┐  invoke / Channel  ┌─ app/src-tauri (Rust) ─┐  path dep  ┌─ core crates ─┐
│ sidebar, scan flow │◄──────────────────►│ #[tauri::command]s     │◄──────────►│ engine+scanners│
│ review, treemap…   │  (serde, no FFI)   │ system.rs (macOS facts)│            │ (in-process)   │
└────────────────────┘                    └────────────────────────┘            └────────────────┘
```

**Safety backbone** (in the engine, property- and golden-image-tested):
scanning is read-only behind a denylist guard; reclaim is the *only* mutating
path, writes an undo manifest before touching anything, trashes rather than
deletes below `Safe`, and reports *measured* freed bytes. The denylist
invariant — *no returned path escapes the allowed roots* — is property-tested
against a deliberately malicious scanner.

**Backend ↔ core**: no FFI. Tauri serializes command args/returns with serde,
which the core types already derive. Streaming scan results use a Tauri
`Channel`; `system.rs` gathers macOS facts (home, Full Disk Access probe,
running-app bundle IDs, battery via `ioreg`/`pmset`, launch agents) without
Objective-C bindings.

## Building

| Goal | Command |
|---|---|
| Run the app (hot reload) | `cd app && npm run dev` |
| Compile the backend only | `cargo build --manifest-path app/src-tauri/Cargo.toml` |
| Bundle `.app` + DMG (debug) | `cd app && npx tauri build --debug` |
| Bundle universal (release) | `cd app && npx tauri build --target universal-apple-darwin` |
| App icon set | `scripts/make-icon.sh` then copy into `app/src-tauri/icons/` |

The frontend is static — no Node bundler, no build step. `npm install` in
`app/` exists only to provide the Tauri CLI. The first backend build compiles
the Tauri dependency tree (one-time, cached thereafter).

## Testing & quality gates

These are merge requirements, not suggestions (`memory/test.md`):

```sh
cd core
cargo test --workspace            # 95 tests: unit, integration, property, golden-image
cargo clippy --workspace --all-targets   # clippy::all denied; pedantic advisory (CI-portable)
cargo fmt --check                 # rustfmt clean

# Performance: criterion benches + >5% regression gate (consistent machine only)
scripts/bench-gate.sh --update-baseline   # bless numbers locally (gitignored, hardware-specific)
scripts/bench-gate.sh                      # compare vs baseline
scripts/bench-gate.sh --smoke              # CI mode: run-only, no cross-machine comparison
```

What's enforced and why:
- **Safety invariants** are property-tested (`tabibu-engine/tests/denylist_prop.rs`)
  and golden-image-tested (`golden_reclaim.rs`: snapshot → reclaim → assert
  exactly the intended files changed; plus fault injection).
- **clippy** gates on `clippy::all` (correctness); `pedantic` is advisory
  because its lint set drifts between toolchain versions and would flap on CI.
- **cargo-deny** (CI) checks licenses, advisories, bans, sources.
- **Benchmarks**: `bench-gate.sh` is hardware-specific, so CI runs `--smoke`
  (compile + run, no comparison); the real >5% gate runs on a consistent box.

## Packaging & release

`tauri build` produces the universal `.app` and DMG directly — there is no
hand-rolled bundler. Push a `v*` tag and `.github/workflows/release.yml` runs
`npx tauri build --target universal-apple-darwin`; signing and notarization
activate automatically when the Apple secrets exist (see the workflow header).
`scripts/uninstall-tabibu.sh` is the honest self-uninstaller (`--dry-run` by
default; `--yes` to act).

## The knowledge base (`memory/`)

`memory/` is the project's shared brain — read it at the start of any session
(`memory/session_start.md`). Key files:

| File | What it holds |
|---|---|
| `tabibu-engineering-guide.md` | Platform constraints, features, milestones |
| `agent_profile.md` | Operating contract: zero-hallucination, testing culture |
| `architecture.md` / `mindmap.md` | System structure (deep / at-a-glance) |
| `philosophy.md` | Why safety > honesty > performance > features |
| `mac_pain_points.md` | The real Mac complaints each feature targets |
| `test.md` | Testing methodology + resource budgets |
| `design.md` | Design system, the Scan→Review→Reclaim flow, tokens, a11y |
| `todo.md` | Roadmap + honest status of what's done |
| `handover_session.md` | Timestamped dev log — newest first |

## Documentation map

| Location | Content |
|---|---|
| `docs/adr/` | Architecture Decision Records (0003 = Tauri; 0001/0002 superseded) |
| `docs/setup.md` | Clean-machine build instructions |
| `docs/release.md` | Signing, notarization, external blockers |
| `docs/modules/*.md` | Per-crate guides with mermaid diagrams |

## Contributing workflow

1. **Start** by reading the last 2–3 `memory/handover_session.md` entries and
   the active milestone in `memory/todo.md`.
2. **For a new scanner:** implement `Scanner`, register in your crate's
   `scanners()`, add fixtures + tests, add a bench if it's on a hot path, then
   expose it through a command in `app/src-tauri/src/commands.rs` and the UI.
3. **For any mutating change:** design the undo first; add a golden-image test
   before implementing.
4. **Before pushing:** the gates above must be green, and the relevant `docs/`
   / `memory/` files updated in the same change (stale docs are bugs).
5. **End** with a timestamped `handover_session.md` entry.

## Current limitations

Honest, and tracked in `memory/todo.md`:

- **Not distributable yet** — no Developer ID / notarization credentials, so
  bundles are unsigned (Gatekeeper rejects on other Macs without right-click →
  Open). The pipeline is wired and conditional.
- **ClamAV** is a feature-gated stub; v1 ships native adware heuristics.
  Real-time (Endpoint Security) scanning is deferred (Apple entitlement).
- **Tray is minimal** — a Tauri status item with a live CPU/memory tooltip and
  Open/Quit menu. A rich health *popover window* (CleanMyMac-style) is a
  follow-up. No privileged helper (not needed — all features are user-space).
- **Exact CPU die temperature and GPU `powermetrics` need root** — Tabibu
  ships the honest **thermal pressure** signal (`pmset -g therm`) instead; true
  per-degree readings would require the deferred privileged helper.
- **Install-time artifact monitoring** (FSEvents) is a follow-up — the
  Uninstaller's disk-wide leftover/orphan scan covers post-uninstall artifacts.
  (v0.1.3 shipped: whole-home duplicates, force-quit, thermal pressure,
  dashboard line graphs + free-space trend, leftovers scan, Security scan UI,
  SMART status, FDA onboarding, per-volume trashes.)

## License

License TBD — note the GPL boundary around any future `libclamav` bundling
(`memory/project_context.md`).
