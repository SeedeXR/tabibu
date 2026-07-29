//! Integration tests that exercise the REAL command/OS paths the popover calls
//! (not just the pure parsers). Assertions are structural so they hold whether
//! or not the runner has Wi-Fi or internet — a CI runner with neither must
//! still pass: the point is that the code path runs and returns a well-formed
//! value instead of panicking.

use tabibu_net::{connection_test, ping_test, wifi_status, NetSampler};

#[test]
fn sampler_seeds_zero_then_reports_monotonic_totals() {
    let mut s = NetSampler::new();
    let a = s.sample();
    // First sample has no prior instant → rates are 0 (never a divide spike).
    assert_eq!(a.down_bps, 0);
    assert_eq!(a.up_bps, 0);
    let b = s.sample();
    // Cumulative byte counters never go backwards between samples.
    assert!(b.total_down_bytes >= a.total_down_bytes);
    assert!(b.total_up_bytes >= a.total_up_bytes);
}

#[test]
fn ping_returns_wellformed_stats() {
    // Loopback: exercises the real /sbin/ping path + parser.
    let p = ping_test("127.0.0.1");
    assert!((0.0..=100.0).contains(&p.loss_pct), "loss out of range: {}", p.loss_pct);
    assert!(p.received <= p.transmitted, "received > transmitted");
    // If any packet came back, loss is < 100 and there's an average RTT.
    if p.received > 0 {
        assert!(p.loss_pct < 100.0);
        assert!(p.avg_ms.is_some());
    }
}

#[test]
fn wifi_status_never_panics_and_is_consistent() {
    let w = wifi_status();
    // connected ⇔ we parsed an RSSI, and quality is bounded when present.
    assert_eq!(w.connected, w.rssi_dbm.is_some());
    if let Some(q) = w.quality_pct {
        assert!(q <= 100);
        assert!(w.connected);
    }
}

#[test]
fn connection_test_assembles_both_halves() {
    let t = connection_test("127.0.0.1");
    assert!((0.0..=100.0).contains(&t.ping.loss_pct));
    assert_eq!(t.wifi.connected, t.wifi.rssi_dbm.is_some());
}
