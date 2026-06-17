# ADR-0001: Hand-written C ABI with JSON payloads for the Rust↔Swift bridge

Date: 2026-06-13 · Status: **Superseded by ADR-0003** (the Swift shell and its
C-FFI bridge were removed in favour of a Tauri shell that calls the Rust core
directly). Kept for history.

## Context

The Rust core (`libtabibu_ffi.a`) must be callable from Swift. Candidates:
`swift-bridge` (codegen, Swift-friendly types), `cbindgen` (generates a C
header from Rust), or a hand-written `extern "C"` surface with a
hand-maintained header. Payload candidates: C structs vs serialized JSON.

## Decision

**Hand-written `extern "C"` functions (~10) + hand-maintained header
(`core/include/tabibu_core.h`) + JSON payloads** for all composite types.
Cancellation via opaque `u64` op handles; streaming via C callbacks;
ownership rule *Rust allocates, Rust frees* (`tabibu_string_free`); ABI
version asserted by Swift at launch (`tabibu_ffi_version`).

## Rationale

- The surface is deliberately narrow (engineering guide §5); at ~10 functions
  a codegen tool adds build complexity (extra toolchain step, generated-code
  drift, two build graphs) without paying for itself. Neither `cbindgen` nor
  `swift-bridge` was present on the build machine; zero new build deps.
- JSON crossing the boundary is **UI-rate data** (review items, monitor
  samples, reports). Hot loops (hashing, walking) never cross the boundary —
  they live entirely in Rust. Serde derives already exist on every type;
  Swift `Codable` mirrors them. Debuggability is excellent (`--version`-style
  probes, loggable payloads).
- C structs would buy throughput we don't need at the cost of manual layout
  stability and lifetime bugs — the classic FFI failure mode.

## Consequences

- Header and `tabibu-ffi/src/lib.rs` must stay in lockstep manually; guarded
  by the FFI round-trip tests (`crates/tabibu-ffi/tests/roundtrip.rs`) and
  the version assert. Any breaking change bumps `FFI_VERSION`.
- If profiling ever shows JSON encode/decode as a real cost on a UI path
  (unlikely: items stream incrementally), the escape hatch is a binary
  payload for that one call — not a wholesale redesign.

## Verification

6 round-trip tests drive the ABI exactly as Swift does (C strings, callbacks,
`user_data`, null/garbage inputs). Swift `--version` proves the link.
