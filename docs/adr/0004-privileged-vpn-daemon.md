# ADR-0004: Salama VPN uses a root LaunchDaemon (not a NetworkExtension)

Date: 2026-07-30 · Status: Proposed

## Context

"Salama" (Swahili: *safe / at peace*) is Tabibu's WARP-equivalent: it hides the
user's public IP and encrypts their traffic so the ISP, router, local Wi-Fi,
and on-path observers see only opaque UDP to a chosen exit — plus an opt-in,
country-selectable VPN. A VPN cannot be user-space on macOS: creating a `utun`
interface, installing routes, changing DNS, and loading `pf` rules all require
root.

This **ends Tabibu's "no privileged helper — all features are user-space"
invariant** (README). That is an architectural change, so it is recorded here
before the privileged code is written, per the contributing rule that treats
stale/absent design docs as bugs.

## Decision

1. **Privilege model — a root `LaunchDaemon` (`tabibud`), not
   `NEPacketTunnelProvider`.**
   - The daemon is a plain Rust binary bundled in `Contents/MacOS/tabibud`,
     registered via `SMAppService.daemon` (macOS 13+, our minimum).
   - Rejected: `NEPacketTunnelProvider`. It needs Apple to *grant* the
     `com.apple.developer.networking.networkextension` entitlement, ships as an
     `.appex` Tauri v2 can't bundle, and buys us little that `pf` +
     `includeAllNetworks`-style rules don't. Revisit only for App Store
     distribution.

2. **Trust boundary — the daemon owns packets, nothing else.**
   The unprivileged Tauri backend owns auth, the control-plane API, and the
   session grant; tokens and credentials never reach root code. The daemon
   listens on a unix socket and **verifies every peer's code signature** via the
   audit token (`LOCAL_PEERTOKEN`, never the racy `LOCAL_PEERPID`) against
   `anchor apple generic and identifier "…" and certificate leaf[subject.OU] =
   "<TEAM_ID>"`. This is the single highest-severity control in the feature and
   must be reviewed by someone other than its author.

3. **The Tabibu undo-manifest discipline extends to network state.**
   Before the first mutation the daemon snapshots default route, DNS service
   keys, and `pf` state to `/var/db/tabibu/netstate.json` (root, `0600`).
   `PurgeConfig` replays it in reverse and is **idempotent** — safe after a
   crash, safe when nothing was installed. The daemon runs it on startup if it
   finds a manifest with no live tunnel.

4. **Layering keeps the interesting logic root-free and tested.**
   - `tabibu-vpnproto` — pure serde wire types (**built, tested**).
   - `tabibu-route` — probe scoring, 2-hop path search, switch hysteresis;
     transport injected via a `Prober` trait (**built, tested**, incl. an
     anti-flap property test).
   - Deferred (need root / Developer ID / a server fleet): `tabibu-tunnel`
     (utun + boringtun), `tabibu-netcfg` (routes/DNS/`pf` kill switch), the
     `tabibud` daemon, packaging, and the UI. Tracked in `notes.md`.

## New attack surface (stated plainly)

- A root daemon that reconfigures all network traffic. Mitigation: minimal root
  surface, mandatory peer-codesign check, no user credentials in root code,
  rate-limited socket.
- A modified `/etc/pf.conf` + a DNS override + a persistent private key. These
  are exactly what makes an app *look* like malware if left behind — hence the
  uninstall guarantee below is non-negotiable.
- One `unsafe`/Objective-C seam (`SMAppService` via `objc2`), the first in the
  app; noted as a deliberate exception to the "no ObjC bindings" convention.

## Uninstall guarantee (non-negotiable)

`scripts/uninstall-tabibu.sh` must fully reverse everything: `PurgeConfig`,
unregister the daemon, remove the plist, restore `/etc/pf.conf` from the hashed
backup, drop the DNS override, release the `pf` token, and **shred** the
WireGuard key in `/var/db/tabibu/`. A golden test runs install → connect →
uninstall → asserts the machine is byte-identical to the pre-install snapshot.

## Honesty constraints (brand fit)

Tabibu's pitch is "no scareware, no fake GB freed, no resident background hog." A
VPN *is* a resident process. To stay honest: the daemon idles at ~0 CPU when
disconnected (blocking `accept()`, no timers) and the UI publishes the
**measured** idle footprint; speed claims are only ever *measured* before/after
RTT with timestamps, never "up to N× faster". If it can't be done honestly it
doesn't ship — and whether a root daemon belongs in Tabibu at all, versus a
sibling app sharing the core crates, is an open question flagged for decision
before milestone 4.

## Status / gating

**Proposed**, not Accepted: shipping is blocked on a Developer ID (unsigned
bundles cannot register an `SMAppService` daemon) and on the brand-fit decision
above. Milestones 1–3 (the two built crates + this ADR) need none of that and
are done; everything past them is gated. See `notes.md` and
`tabibu-vpn-notes/` for the full plan.
