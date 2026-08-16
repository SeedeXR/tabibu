//! Salama route selection — the part that's actually ours.
//!
//! Root-free, network-free, fully testable. Given per-PoP probe samples and the
//! inter-PoP mesh, it scores every candidate, finds the best (possibly 2-hop)
//! path to each allowed exit, and applies switch hysteresis so the "optimizer"
//! never flaps the user's TCP connections. The transport is injected via
//! [`Prober`] so the daemon owns the sockets and this crate owns the maths. See
//! `tabibu-vpn-notes/vpn-client-integration.md` §3.

use std::time::{Duration, Instant};

use tabibu_vpnproto::{ErrCode, MeshLink, Pop, ProbeSample};

/// Injected transport. The real `UdpProber` lives in the daemon; tests use a
/// fixture-driven mock. Implementations must respect the deadline and never
/// panic on partial results.
pub trait Prober {
    fn probe(
        &self,
        pops: &[Pop],
        count: u8,
        spacing: Duration,
        deadline: Duration,
    ) -> Vec<ProbeSample>;
}

/// Scoring weights. Lower score is better; units are notional "cost
/// milliseconds". Three fixed profiles are exposed in the UI — never raw
/// weights.
#[derive(Debug, Clone, Copy)]
pub struct Weights {
    pub rtt: f64,
    pub jitter: f64,
    pub loss: f64,
    pub load: f64,
}

impl Weights {
    pub const BALANCED: Self = Self {
        rtt: 1.0,
        jitter: 1.5,
        loss: 1.0,
        load: 0.4,
    };
    pub const LOW_LATENCY: Self = Self {
        rtt: 1.0,
        jitter: 4.0,
        loss: 2.5,
        load: 0.3,
    };
    pub const THROUGHPUT: Self = Self {
        rtt: 0.4,
        jitter: 0.3,
        loss: 1.5,
        load: 1.6,
    };

    /// Resolve a UI profile name to its weights (default `BALANCED`).
    #[must_use]
    pub fn profile(name: &str) -> Self {
        match name {
            "low-latency" | "low_latency" => Self::LOW_LATENCY,
            "throughput" => Self::THROUGHPUT,
            _ => Self::BALANCED,
        }
    }
}

/// Lower is better. A dead PoP (no replies) scores `+∞` so it can never win.
/// Loss is superlinear — a lossy link is disproportionately bad for real use.
#[must_use]
pub fn score(s: &ProbeSample, w: &Weights) -> f64 {
    if s.replies == 0 {
        return f64::INFINITY;
    }
    w.rtt * f64::from(s.rtt_p50_ms)
        + w.jitter * f64::from(s.jitter_ms)
        + w.loss * (f64::from(s.loss) * 100.0).powi(2)
        + w.load * f64::from(s.load) * 100.0
}

/// Cost of an inter-PoP mesh hop, in the same "cost ms" units as [`score`]
/// (mesh links carry no jitter/load, only rtt + loss).
fn mesh_cost(link: &MeshLink, w: &Weights) -> f64 {
    f64::from(link.rtt_ms) + w.loss * (f64::from(link.loss) * 100.0).powi(2)
}

/// Fixed penalty per extra hop — covers re-encryption + queueing so a 2-hop
/// path must be *meaningfully* better than direct to be chosen.
pub const HOP_PENALTY_MS: f64 = 8.0;

/// A candidate route to one exit PoP.
#[derive(Debug, Clone)]
pub struct Path {
    /// First PoP the client connects to (== `exit` for a direct path).
    pub entry: u16,
    pub exit: u16,
    pub cost: f64,
    /// Ordered PoP ids, `[exit]` (direct) or `[transit, exit]` (2-hop).
    pub hops: Vec<u16>,
}

impl Path {
    /// Two paths are the "same route" if they enter and exit at the same PoPs —
    /// used by the selector to compare against the current choice.
    #[must_use]
    pub fn eq_route(&self, other: &Path) -> bool {
        self.entry == other.entry && self.exit == other.exit
    }
}

/// Best path to every allowed exit, cheapest first. Capped at 2 hops for v1:
/// each exit is reached either directly (`client → exit`) or via one transit
/// PoP (`client → transit → exit`) — whichever is cheaper.
///
/// Invariants (property-tested): the `exit` of every returned path is a
/// `can_exit` PoP whose country is in `allowed` (empty `allowed` = no filter).
/// A `can_exit == false` PoP can only ever appear as a transit hop.
#[must_use]
pub fn select_paths(
    pops: &[Pop],
    samples: &[ProbeSample],
    mesh: &[MeshLink],
    w: &Weights,
    allowed: &[String],
) -> Vec<Path> {
    let sc = |id: u16| -> f64 {
        samples
            .iter()
            .find(|s| s.pop_id == id)
            .map_or(f64::INFINITY, |s| score(s, w))
    };
    let allow = |c: &str| allowed.is_empty() || allowed.iter().any(|a| a.eq_ignore_ascii_case(c));

    let mut paths = Vec::new();
    for e in pops.iter().filter(|p| p.can_exit && allow(&p.country)) {
        let mut best_cost = sc(e.id); // direct
        let mut best_hops = vec![e.id];
        // Try each transit hop T with a mesh link T → exit.
        for link in mesh.iter().filter(|l| l.to == e.id && l.from != e.id) {
            let via = sc(link.from) + mesh_cost(link, w) + HOP_PENALTY_MS;
            if via < best_cost {
                best_cost = via;
                best_hops = vec![link.from, e.id];
            }
        }
        if best_cost.is_finite() {
            paths.push(Path {
                entry: best_hops[0],
                exit: e.id,
                cost: best_cost,
                hops: best_hops,
            });
        }
    }
    paths.sort_by(|a, b| a.cost.total_cmp(&b.cost));
    paths
}

/// The challenger must be at least this fraction cheaper than the current route
/// to justify a switch.
pub const SWITCH_MARGIN: f64 = 0.15;
/// Minimum time on a route before we'll switch away — stops flapping.
pub const MIN_DWELL: Duration = Duration::from_secs(90);

#[derive(Debug, Clone)]
pub enum Decision {
    Connect(Path),
    Switch(Path),
    Stay,
}

/// Stateful path selector with hysteresis. Guarantees at most one route change
/// per [`MIN_DWELL`] (property-tested), so the optimizer can't drop the user's
/// connections every probe cycle.
#[derive(Debug, Default)]
pub struct Selector {
    current: Option<(Path, Instant)>,
}

impl Selector {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The route currently in use, if any.
    #[must_use]
    pub fn current(&self) -> Option<&Path> {
        self.current.as_ref().map(|(p, _)| p)
    }

    /// Decide whether to connect, switch, or stay given fresh candidates.
    pub fn choose(&mut self, cands: &[Path], now: Instant) -> Decision {
        let Some(best) = cands.iter().min_by(|a, b| a.cost.total_cmp(&b.cost)) else {
            return Decision::Stay; // nothing reachable — hold what we have
        };
        match &self.current {
            None => {
                self.current = Some((best.clone(), now));
                Decision::Connect(best.clone())
            }
            Some((cur, since)) => {
                // Dwell: never switch within MIN_DWELL of the last change.
                if now.duration_since(*since) < MIN_DWELL {
                    return Decision::Stay;
                }
                let cur_cost = cands
                    .iter()
                    .find(|c| c.eq_route(cur))
                    .map_or(f64::INFINITY, |c| c.cost);
                // Margin: challenger must be >=15% cheaper AND a different route.
                if !best.eq_route(cur) && best.cost < cur_cost * (1.0 - SWITCH_MARGIN) {
                    self.current = Some((best.clone(), now));
                    Decision::Switch(best.clone())
                } else {
                    Decision::Stay
                }
            }
        }
    }
}

/// Turn silence into a real diagnosis. WireGuard can't tell you why it failed,
/// so we infer from probe results: nothing answered anywhere ⇒ UDP is blocked;
/// PoPs reachable but the tunnel refused ⇒ handshake timeout (bad key / down).
#[must_use]
pub fn diagnose(samples: &[ProbeSample], handshake_failed: bool) -> ErrCode {
    let any_reply = samples.iter().any(|s| s.replies > 0);
    match (any_reply, handshake_failed) {
        (false, _) => ErrCode::UdpBlocked,
        (true, _) => ErrCode::HandshakeTimeout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::net::SocketAddr;

    fn pop(id: u16, country: &str, can_exit: bool) -> Pop {
        let a: SocketAddr = "203.0.113.10:51820".parse().unwrap();
        Pop {
            id,
            country: country.into(),
            city: "X".into(),
            endpoint: a,
            probe: a,
            quic: a,
            public_key: [0u8; 32],
            can_exit,
        }
    }

    fn sample(id: u16, rtt: f32, jitter: f32, loss: f32, load: f32) -> ProbeSample {
        ProbeSample {
            pop_id: id,
            rtt_p50_ms: rtt,
            rtt_p95_ms: rtt + jitter,
            jitter_ms: jitter,
            loss,
            load,
            replies: if loss >= 1.0 { 0 } else { 10 },
        }
    }

    /// A fixture-driven Prober — proves the injected-transport shape works and
    /// keeps route logic runnable without a socket (spec §3.1).
    struct MockProber(Vec<ProbeSample>);
    impl Prober for MockProber {
        fn probe(&self, _: &[Pop], _: u8, _: Duration, _: Duration) -> Vec<ProbeSample> {
            self.0.clone()
        }
    }

    #[test]
    fn mock_prober_returns_fixtures() {
        let m = MockProber(vec![sample(1, 40.0, 5.0, 0.0, 0.2)]);
        let out = m.probe(
            &[],
            10,
            Duration::from_millis(20),
            Duration::from_millis(1500),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pop_id, 1);
    }

    #[test]
    fn dead_pop_scores_infinite() {
        let s = sample(1, 10.0, 1.0, 1.0, 0.0); // loss 1.0 → replies 0
        assert!(score(&s, &Weights::BALANCED).is_infinite());
    }

    /// Spec §9.1: worsening any single input never lowers the score.
    #[test]
    fn score_is_monotonic_in_each_input() {
        let w = Weights::BALANCED;
        let base = sample(1, 40.0, 8.0, 0.02, 0.3);
        let b = score(&base, &w);
        let worse = [
            ProbeSample {
                rtt_p50_ms: 60.0,
                ..base
            },
            ProbeSample {
                jitter_ms: 20.0,
                ..base
            },
            ProbeSample { loss: 0.10, ..base },
            ProbeSample { load: 0.9, ..base },
        ];
        for s in worse {
            assert!(score(&s, &w) >= b, "worsening an input lowered the score");
        }
    }

    #[test]
    fn select_excludes_transit_only_as_exit() {
        // PoP 2 is transit-only but has a fantastic direct score; it must never
        // be returned as an exit, only as a hop toward a real exit.
        let pops = vec![pop(1, "DE", true), pop(2, "KE", false)];
        let samples = vec![
            sample(1, 90.0, 10.0, 0.0, 0.3),
            sample(2, 5.0, 1.0, 0.0, 0.1),
        ];
        let mesh = vec![MeshLink {
            from: 2,
            to: 1,
            rtt_ms: 20.0,
            loss: 0.0,
        }];
        let paths = select_paths(&pops, &samples, &mesh, &Weights::BALANCED, &[]);
        assert!(
            paths.iter().all(|p| p.exit == 1),
            "exit must be the can_exit PoP"
        );
        // The cheap transit-only PoP is used as an entry hop instead.
        assert_eq!(paths[0].hops, vec![2, 1]);
        assert_eq!(paths[0].entry, 2);
    }

    #[test]
    fn select_honors_country_filter_against_hostile_scores() {
        // Forbidden country "RU" advertises an unbeatable score; the filter must
        // still never return it as an exit (spec §9.1 hostile-mock shape).
        let pops = vec![pop(1, "DE", true), pop(2, "RU", true)];
        let samples = vec![
            sample(1, 80.0, 10.0, 0.0, 0.3),
            sample(2, 1.0, 0.0, 0.0, 0.0),
        ];
        let paths = select_paths(&pops, &samples, &[], &Weights::BALANCED, &["DE".into()]);
        assert!(!paths.is_empty());
        assert!(
            paths.iter().all(|p| p.exit == 1),
            "no exit outside the allowed set"
        );
    }

    #[test]
    fn two_hop_beats_direct_when_transit_is_much_better() {
        // Direct to exit is awful; a nearby transit with a good mesh link wins
        // even after the hop penalty.
        let pops = vec![pop(1, "DE", true), pop(2, "KE", false)];
        let samples = vec![
            sample(1, 300.0, 40.0, 0.05, 0.5),
            sample(2, 10.0, 2.0, 0.0, 0.1),
        ];
        let mesh = vec![MeshLink {
            from: 2,
            to: 1,
            rtt_ms: 60.0,
            loss: 0.0,
        }];
        let paths = select_paths(&pops, &samples, &mesh, &Weights::BALANCED, &[]);
        assert_eq!(
            paths[0].hops,
            vec![2, 1],
            "should route via the good transit"
        );
    }

    #[test]
    fn selector_connects_then_dwells() {
        let mut sel = Selector::new();
        let t0 = Instant::now();
        let p1 = Path {
            entry: 1,
            exit: 1,
            cost: 100.0,
            hops: vec![1],
        };
        let p2 = Path {
            entry: 2,
            exit: 2,
            cost: 10.0,
            hops: vec![2],
        }; // way better
           // First choice connects.
        assert!(matches!(
            sel.choose(std::slice::from_ref(&p1), t0),
            Decision::Connect(_)
        ));
        // A far-better challenger within MIN_DWELL must NOT switch.
        let within = t0 + MIN_DWELL - Duration::from_secs(1);
        assert!(matches!(
            sel.choose(&[p1.clone(), p2.clone()], within),
            Decision::Stay
        ));
        // After the dwell, the 15%-better challenger switches.
        let after = t0 + MIN_DWELL + Duration::from_secs(1);
        assert!(matches!(sel.choose(&[p1, p2], after), Decision::Switch(_)));
    }

    #[test]
    fn selector_ignores_marginal_challenger() {
        let mut sel = Selector::new();
        let t0 = Instant::now();
        let cur = Path {
            entry: 1,
            exit: 1,
            cost: 100.0,
            hops: vec![1],
        };
        let marginal = Path {
            entry: 2,
            exit: 2,
            cost: 90.0,
            hops: vec![2],
        }; // only 10% better
        sel.choose(std::slice::from_ref(&cur), t0);
        let after = t0 + MIN_DWELL + Duration::from_secs(1);
        // 90 is not < 100*0.85 = 85, so we stay.
        assert!(matches!(
            sel.choose(&[cur, marginal], after),
            Decision::Stay
        ));
    }

    #[test]
    fn diagnose_distinguishes_blocked_from_handshake() {
        let none = vec![sample(1, 0.0, 0.0, 1.0, 0.0)]; // replies 0
        assert_eq!(diagnose(&none, false), ErrCode::UdpBlocked);
        let some = vec![sample(1, 40.0, 5.0, 0.0, 0.2)];
        assert_eq!(diagnose(&some, true), ErrCode::HandshakeTimeout);
    }

    // Spec §9.1: the anti-flap property. For ANY sequence of candidate sets and
    // time steps, the selector never changes route more than once per MIN_DWELL.
    proptest! {
        #[test]
        fn selector_never_flaps_faster_than_min_dwell(
            steps in proptest::collection::vec((0u64..400, 0.0f64..200.0, 0u16..4), 1..40)
        ) {
            let mut sel = Selector::new();
            let t0 = Instant::now();
            let mut clock = t0;
            let mut last_change: Option<Instant> = None;
            for (secs, cost, exit) in steps {
                clock += Duration::from_secs(secs);
                // Two candidates that vary by the fuzzed cost/exit so switches
                // are actually tempted.
                let cands = vec![
                    Path { entry: exit, exit, cost, hops: vec![exit] },
                    Path { entry: 9, exit: 9, cost: 100.0, hops: vec![9] },
                ];
                match sel.choose(&cands, clock) {
                    Decision::Connect(_) | Decision::Switch(_) => {
                        if let Some(prev) = last_change {
                            prop_assert!(
                                clock.duration_since(prev) >= MIN_DWELL,
                                "route changed twice within MIN_DWELL"
                            );
                        }
                        last_change = Some(clock);
                    }
                    Decision::Stay => {}
                }
            }
        }
    }
}
