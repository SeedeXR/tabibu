# Tabibu — working TODO

Roadmap for `tabibu-cli` (built on the existing core crates) plus holistic
Tabibu improvements. Grounded on crates that already exist and are tested; each
task ships with unit/integration/regression tests and doc updates. Worked
top-down; check items off as they land. (Previous list archived as
`do_archived_1.md`.)

Legend: `[ ]` todo · `[~]` in progress · `[x]` done · `(safe)` read-only ·
`(destructive)` gated behind `--yes`, always to Trash.

## Phase 1 — insight commands (read-only, safe) — do first
- [x] `tabibu space <path> [--depth N]` — disk-usage tree (wraps `tabibu-walk::size_tree`). (safe)
- [x] `tabibu scan large` — big/old files in Downloads (wraps `tabibu-junk::LargeOldScanner` via `smart_scan`; read-only, guard-enforced). (safe)
- [x] `tabibu scan dupes <path> [--min-size N]` — byte-identical duplicates (wraps `tabibu-dupes` collect_candidates + find_duplicates; keep newest). (safe)
- [x] `tabibu scan junk` — reclaimable junk by category, report only (runs all `tabibu-junk` scanners via `smart_scan`; app-parity allowed-roots). (safe)
- [x] `tabibu scan malware` — adware/rogue-profile heuristics, report only (runs `tabibu-malware` scanners via `smart_scan`, app-parity roots). (safe)

**Phase 1 COMPLETE** ✓ (2026-08-20) — all insight commands shipped, tested, documented. Loop stopped here; Phase 2 (`clean`, destructive) awaits review.

## Phase 2 — cleanup (report first, `--yes` to act, always → Trash/reversible)
- [x] `tabibu clean junk [--yes]` — report by category, then reclaim to Trash via `tabibu-engine`. (destructive)
- [x] `tabibu clean dev-artifacts [PATH] [--global] [--yes]` — report then Trash rebuildable build dirs (new `tabibu-devscan`). (destructive)
- [x] **Dev-artifacts scanner** — `scan dev-artifacts [PATH] [--global]`: cross-stack (Rust target, node_modules, dist/build, __pycache__/.venv, .gradle, .dart_tool, Pods, DerivedData, .terraform, …), directory-level or global, manifest-gated so hand-made `build`/`dist` are never flagged; each with a rebuild hint. Verified live: 15.9 GB across this repo. (safe/report)
- [x] **Wire dev-artifacts into the app** — new "Build artifacts" Developer view (`scan_dev_artifacts` command → review list with checkboxes → reclaim to Trash, reuses the app's `reclaim`). Builds; live GUI render not headlessly verifiable.
- [x] `tabibu clean caches|logs|all` — per-category clean (caches = user+dev cache scanners; logs = log scanner; all = junk + dev-artifacts across home). Report-first + `--yes`. Verified reports: caches 3.2 GB, all 74.6 GB/1884 items. (destructive; report-first)

**Phase 2 COMPLETE** ✓ — all `clean` targets shipped (report-first, `--yes` → Trash, reversible).
- [x] `tabibu trash empty --yes` — already shipped. (destructive)
- [x] `tabibu slim <app> --yes` — already shipped. (destructive)
- [x] **Flush DNS cache** — shipped in BOTH: CLI `tabibu flush-dns` (sudo if not root) + app **Junk** landing "Flush DNS cache" button (`flush_dns` command via one admin prompt). Runs `dscacheutil -flushcache` + `killall -HUP mDNSResponder`; non-destructive maintenance. Live root run is user-triggered (can't verify headlessly).

## Phase 3 — targeted tools
- [x] `tabibu brew status|clean|autoremove` — wraps `tabibu-brew`. Status read-only (analyze: version/prefix/packages/reclaimable/orphans). clean→`brew cleanup`, autoremove→`brew autoremove`: report-first + `--yes`, removal **delegated to brew** (the one non-Trash cleanup — Homebrew owns reversibility). Verified live: 234 pkgs, 2 MB reclaimable, 0 orphans. (destructive; report-first)
- [x] `tabibu docker status|prune` — wraps `tabibu-docker`. Status read-only (analyze: daemon state + per-category reclaimable). prune→`docker builder prune` + `image prune -a`: report-first + `--yes`, delegated to docker, **build cache + unused images only** (both regenerate; volumes/containers untouched, so no irreversible data loss). Verified live: 2.7 GB reclaimable (build cache). (destructive; report-first)
- [x] `tabibu uninstall <app>` — app + leftovers (wraps `tabibu-uninstall`). Resolve a `.app` path or a name (exact stem, then substring) → report the bundle (`dir_size`, UnusedApp/Review) + `find_remnants` leftovers, largest-first; `--yes` moves ALL to Trash via shared `run_reclaim` (reversible — engine permits Trash for every tier, Delete/Truncate Safe-only). No bundle id → bundle-only report. Verified live: report-only run resolved "Adobe Lightroom" (3.7 GB) + a real remnant container, every item action=Trash, nothing touched without --yes; bogus name → exit 1. (destructive; report-first)

**Phase 3 COMPLETE** ✓ — `brew`, `docker`, `uninstall` all shipped: read verbs safe, destructive verbs report-first + `--yes`, removal delegated to brew/docker or reversible Trash. Loop stopped here; Phase 4 (privacy/VPN) awaits review.

## Phase 4 — privacy / VPN — ~~SKIPPED for CLI~~ (desktop-app only, 2026-08-26)
Decision: encrypted-DNS toggle and VPN connect/disconnect stay a **desktop-app**
feature (they need a persistent root helper + one admin prompt; the app already
owns that flow). The CLI keeps `privacy` (read-only exposure/DNS status) only.
- [x] ~~`tabibu privacy dns on|off`~~ — app-only (Network view Salama section).
- [x] ~~`tabibu vpn status|connect|disconnect`~~ — app-only (Network → Salama VPN).

## Phase 5 — UX, safety & holistic Tabibu improvements
- [x] `tabibu protect list|add|remove <path>` — a protected-paths list **shared by app + CLI** (single safety source of truth). New `tabibu_engine::protect` module (file `~/.config/tabibu/protected.list`, keyed off injected `home`); enforced INSIDE `reclaim` (the one mutating path) so the app honors it for free — no app code change. Overlap refused in BOTH directions (protecting a child blocks trashing its ancestor); component-wise (no false prefix). Regression: a protected file survives `reclaim` (engine test). Verified live: add/idempotent/list/remove lifecycle + `~` expansion. (safety)
- [x] `tabibu completions bash|zsh|fish` — shell completions (clap_complete). Generated at runtime from the SAME `Cli::command()` (no drift from --help); arg is `clap_complete::Shell` (bash/zsh/fish + more free). clap_complete added to BOTH deps + build-deps (cli.rs is include!d into build.rs). Verified: bash 1788 / zsh 1509 / fish 248 lines, all mention protect/uninstall/docker; invalid shell → exit 2. (safe)
- [x] `tabibu report [--json]` — one health+space+privacy snapshot for CI/cron. Reuses monitor Sampler (double-sample CPU%) + salama exposure/dns_status; disk from NEW shared `tabibu_monitor::disk_space()` (moved out of app-only commands.rs → app now delegates to it, single source of truth). JSON = {health,disk,privacy}. Verified live: 31.3 GB free/494.4 GB, DNS encrypted, app still compiles. (safe)
- [x] Global flags: `--quiet`, `--no-color`; document exit codes. `--quiet` implemented by shadowing `println!` with a macro gated on a `QUIET` atomic (zero per-site churn); `print_json` uses `std::println!` so JSON data + stderr errors survive quiet. `--no-color` = documented compatibility no-op (CLI emits zero ANSI — grounded, not assumed). Exit codes documented 0/1/2 (clap gives 2). Verified live: `--quiet` empties human stdout, keeps JSON + stderr errors (exit 1); `--no-color` accepted. (safe)
- [x] Config file (`~/.config/tabibu/config.toml`) for defaults (depth, min-size). New CLI-local `config` module (NOT a new dep — no toml crate in the tree; hand-rolled flat `key = value` reader, a TOML subset, `#`/`[section]` ignored). `depth`/`min_size` made `Option` in cli.rs so precedence works: explicit flag > config > built-in (1/4096). OPTIONAL — absent file = today's behavior. Bad/unknown keys ignored (never breaks the CLI). **Protected paths deliberately NOT here** — one source of truth stays `tabibu protect`/`protected.list`. Verified live: config depth=3 → 3 levels, `--depth 1` overrides → 1, no config → 1. (safe)
- [x] Schedule recipe: `tabibu clean --yes` via `launchd`/cron — documented, opt-in. Sample LaunchAgent `scripts/xr.seede.tabibu.maintenance.plist` (Label uses the app namespace `xr.seede.tabibu`; runs `clean caches --yes` weekly, logs to ~/Library/Logs; RunAtLoad false, Background). NOT auto-installed — docs show launchctl bootstrap/kickstart/bootout + a cron alternative (incl. hourly `report --json` health log). Safe to automate ONLY because clean = reversible Trash move; noted brew/docker prunes are NOT Trash. Docs-only + sample data file → no unit tests; plist validated with `plutil -lint` (OK). All referenced subcommands verified to exist. (docs)
- [x] Consistent JSON schema across app commands + CLI (scripting parity). GROUNDED: both front-ends serialize the SAME core `#[derive(Serialize)]` types (one source, no parallel schema). Verified struct-direct parity: CleanupItem (scans/clean items), UniversalReport (slim), DirNode (space), DuplicateGroup (dupes), DevArtifact (scan dev-artifacts), PrivacyStatus (privacy), Report + ActionOutcome (brew/docker) — each maps to an app command returning the identical type. Small safe alignment done: CLI `privacy`/`report` now serialize `tabibu_salama::status()`→`PrivacyStatus` directly (was hand-built {exposure,dns} — same shape, now same TYPE by construction). Documented CLI-only envelopes (dry_run/would_free_bytes/item_count, trash_bytes/flushed/installed/running, protect keys) + intentional differences (status/report health uses reader-friendly keys not raw SystemSample; clean shows ReclaimReport subset; scan dev-artifacts=DevArtifact vs app's converted CleanupItem). No real drift needing a fix. New "JSON output & app parity" doc section. (safe)

**Phase 5 COMPLETE** ✓ (2026-08-26) — protect · completions · report · global flags/exit codes · config.toml · schedule recipe · JSON parity. All shipped, tested, documented. Loop stopped.

## Cross-cutting (every task)
- [x] Tests: unit (arg parse + pure formatters) + integration (temp fixture) + regression (safety rule: protected paths survive reclaim; dry-run changes nothing). Verified 2026-08-26 — full `cargo test --workspace` green (tabibu-cli 26, tabibu-engine 21, tabibu-monitor 3, tabibu-devscan 3, + all others), `cargo fmt --all --check` clean, `cargo clippy --workspace --all-targets -D warnings` clean, app `cargo check` clean.
- [x] Docs: `docs/tabibu-cli.md` (all commands + config/scheduling/JSON-parity sections + mermaid) and `README.md` updated throughout.
- [x] Man page: regenerated (`scripts/gen-man.sh`) — auto-derives from the clap def; current.
