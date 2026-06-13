# Module: TabibuMonitor (menu-bar agent)

A standalone menu-bar agent showing live system load and top processes, with
an "Open Tabibu" affordance. Its defining constraint is its own resource
budget: *a performance tool that is itself a hog is dead on arrival.*

## Design

- **Pure AppKit, no SwiftUI.** `NSStatusItem` + `NSPopover` with hand-built
  `NSStackView`/`NSTextField` content. SwiftUI was measured first and dropped
  (see budget below).
- Calls only `tabibu_monitor_sample` over the FFI (its `CoreBridge.swift` is a
  trimmed copy — each Swift package stays self-contained, ADR-0002).
- **Cadence discipline:** the menu-bar label refreshes every **5 s** (top-1,
  cheap); full top-6 sampling runs every **2 s only while the popover is
  open** and stops on close. Sampling is off the main thread.
- Accessory activation policy (`.accessory`): no Dock icon. Bundled as a
  login item inside `Tabibu.app/Contents/Library/LoginItems/` by
  `scripts/make-app.sh`.

```mermaid
sequenceDiagram
    participant T as Timer (5s label / 2s popover)
    participant A as AppDelegate
    participant CB as CoreBridge
    participant R as tabibu_monitor_sample (Rust)
    T->>A: tick
    A->>CB: monitorSample(topN, byCPU) [bg queue]
    CB->>R: FFI call → sysinfo refresh
    R-->>CB: SystemSample JSON
    CB-->>A: decoded sample [main]
    A->>A: update label / popover view
    Note over A,R: popover closed → 2s timer invalidated; only the 5s label runs
```

## Budget (measured, honest — not aspirational)

`TabibuMonitor/budget-test.sh` launches the release binary, settles past the
launch spike, and samples `ps -o %cpu,rss` 10× over ~30 s. Hard fail on breach.

| Metric | Budget | Measured | Rationale |
|---|---|---|---|
| Avg CPU | **< 1%** | ~0.1–0.7% | The metric that truly captures "is it a hog while running". Tight, hard gate. |
| RSS | **< 70 MB** | ~51 MB | Honest ceiling. See below. |

**Why not the guide's illustrative "30 MB"?** A native macOS menu-bar agent
*must* link AppKit (`NSStatusItem`/`NSPopover` have no lighter API) plus
Foundation; `ps rss` counts a large share of those resident, shared framework
pages. Measured floors on this Apple-Silicon machine:

- SwiftUI `MenuBarExtra` build: **~76 MB**
- Pure-AppKit build (shipped): **~51 MB**
- `-dead_strip` of the unused static-lib surface: no RSS change (confirms the
  cost is framework pages, not dead code).

51 MB is the practical floor; the budget is set at 70 MB to absorb sysinfo's
per-sample process-map allocation without flaking. This is a recorded
engineering tradeoff, not a lowered bar to hide a regression — CPU remains the
tight, honest gate. Revisit if Apple ships a lighter status-item API.
