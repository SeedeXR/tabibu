//! Network status for the menu-bar popover: live throughput (download/upload
//! rate + cumulative totals) and an on-demand connection test (Wi-Fi signal
//! strength + packet loss + latency).
//!
//! Split like the other command-delegating crates (`tabibu-docker`): the shell
//! commands (`system_profiler`, `ping`) are thin wrappers around **pure
//! parsers** that are unit-tested without a network. Throughput comes from
//! `sysinfo`'s per-interface byte counters (the same numbers the OS reports),
//! turned into a rate over the wall-clock gap between samples.
//!
//! Design choices:
//!   - Throughput is cheap + local → sampled live in the popover.
//!   - Wi-Fi RSSI (`system_profiler`, ~1–2s) and packet loss (`ping`, outward
//!     network I/O) are **on-demand** behind a "Test Connection" button, never
//!     on a timer — no surprise egress, no slow poll.

use serde::Serialize;
use std::process::Command;
use std::time::{Duration, Instant};

use sysinfo::Networks;

// ---------------------------------------------------------------------------
// Throughput (live, sysinfo)
// ---------------------------------------------------------------------------

/// A download/upload snapshot for the popover.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Throughput {
    /// Bytes/sec received since the previous sample (0 on the seed sample).
    pub down_bps: u64,
    /// Bytes/sec transmitted since the previous sample (0 on the seed sample).
    pub up_bps: u64,
    /// Cumulative bytes received across all interfaces since boot.
    pub total_down_bytes: u64,
    /// Cumulative bytes transmitted across all interfaces since boot.
    pub total_up_bytes: u64,
}

/// Stateful throughput sampler. Rates are a delta over the wall-clock gap
/// between [`Self::sample`] calls, so drive it on a fixed cadence (the popover
/// polls every few seconds). The first call seeds and reports `0` rates.
pub struct NetSampler {
    nets: Networks,
    last: Option<Instant>,
}

impl Default for NetSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl NetSampler {
    #[must_use]
    pub fn new() -> Self {
        Self {
            nets: Networks::new_with_refreshed_list(),
            last: None,
        }
    }

    /// Refresh counters and return the current throughput. Loopback (`lo*`) is
    /// excluded so local-only traffic doesn't masquerade as internet activity.
    pub fn sample(&mut self) -> Throughput {
        let now = Instant::now();
        self.nets.refresh(false);
        let (mut down_delta, mut up_delta, mut total_down, mut total_up) = (0u64, 0u64, 0u64, 0u64);
        for (name, data) in self.nets.list() {
            if name.starts_with("lo") {
                continue;
            }
            down_delta += data.received();
            up_delta += data.transmitted();
            total_down += data.total_received();
            total_up += data.total_transmitted();
        }
        let dt = self.last.replace(now).map(|prev| now.duration_since(prev));
        compute_throughput(down_delta, up_delta, total_down, total_up, dt)
    }
}

/// Turn byte deltas + the elapsed gap into a [`Throughput`]. `dt == None` (the
/// seed sample) or a near-zero gap yields `0` rates — never a divide-by-zero or
/// an absurd spike. Pure so the rate math is unit-tested without a NIC.
#[must_use]
pub fn compute_throughput(
    down_delta: u64,
    up_delta: u64,
    total_down: u64,
    total_up: u64,
    dt: Option<Duration>,
) -> Throughput {
    let secs = dt.map_or(0.0, |d| d.as_secs_f64());
    let rate = |bytes: u64| -> u64 {
        if secs <= 0.05 {
            return 0;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            (bytes as f64 / secs) as u64
        }
    };
    Throughput {
        down_bps: rate(down_delta),
        up_bps: rate(up_delta),
        total_down_bytes: total_down,
        total_up_bytes: total_up,
    }
}

// ---------------------------------------------------------------------------
// Connection test (on-demand): Wi-Fi signal + packet loss
// ---------------------------------------------------------------------------

/// Wi-Fi signal, parsed from the connected network in `system_profiler`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct Wifi {
    pub connected: bool,
    /// Received signal strength (dBm; closer to 0 is stronger, e.g. -43).
    pub rssi_dbm: Option<i32>,
    pub noise_dbm: Option<i32>,
    /// A 0–100 quality reading derived from RSSI (see [`quality_from_rssi`]).
    pub quality_pct: Option<u8>,
    /// Negotiated transmit rate in Mbps (integer as `system_profiler` reports).
    pub tx_rate_mbps: Option<u32>,
    pub phy_mode: Option<String>,
}

/// Packet-loss / latency, parsed from `ping` statistics.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Ping {
    pub transmitted: u32,
    pub received: u32,
    /// Percentage of packets lost (0.0–100.0).
    pub loss_pct: f64,
    /// Average round-trip time in ms (`None` when every packet was lost).
    pub avg_ms: Option<f64>,
}

/// A full on-demand connection test.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionTest {
    pub wifi: Wifi,
    pub ping: Ping,
}

/// Wi-Fi signal via `system_profiler SPAirPortDataType`. Returns
/// `Wifi::default()` (disconnected) if the command fails or Wi-Fi is off.
#[must_use]
pub fn wifi_status() -> Wifi {
    let out = Command::new("/usr/sbin/system_profiler")
        .args(["SPAirPortDataType"])
        .output();
    match out {
        Ok(o) if o.status.success() => parse_wifi(&String::from_utf8_lossy(&o.stdout)),
        _ => Wifi::default(),
    }
}

/// Packet loss + latency via `ping`. Five quick pings with a short deadline so
/// the button never hangs. `host` should be an IP literal (no name lookup
/// dependency) — the caller passes a public resolver.
#[must_use]
pub fn ping_test(host: &str) -> Ping {
    let out = Command::new("/sbin/ping")
        .args(["-c", "5", "-t", "5", host])
        .output();
    match out {
        // ping exits non-zero on 100% loss, so parse stdout regardless of code.
        Ok(o) => parse_ping(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => Ping {
            transmitted: 0,
            received: 0,
            loss_pct: 100.0,
            avg_ms: None,
        },
    }
}

/// Run the full on-demand test. `host` is an IPv4 literal (a public resolver).
// ponytail: v4 literal avoids a DNS-lookup dependency; the cost is that an
// IPv6-only network reports loss even when v6 connectivity is fine. Acceptable
// for a quick check — revisit with a dual-stack hostname if v6-only becomes common.
#[must_use]
pub fn connection_test(host: &str) -> ConnectionTest {
    ConnectionTest {
        wifi: wifi_status(),
        ping: ping_test(host),
    }
}

// ---------------------------------------------------------------------------
// Pure parsers (unit-tested without a network)
// ---------------------------------------------------------------------------

/// Map RSSI (dBm) to a 0–100 quality reading. The common linear heuristic:
/// −50 dBm or stronger ≈ 100%, −100 dBm or weaker ≈ 0%.
#[must_use]
pub fn quality_from_rssi(rssi: i32) -> u8 {
    let q = 2 * (rssi + 100);
    q.clamp(0, 100) as u8
}

/// Parse the CONNECTED network's fields from `system_profiler SPAirPortDataType`.
///
/// The output lists the current network under `Current Network Information:`
/// and then every visible network under `Other Local Wi-Fi Networks:` — both
/// carry `Signal / Noise:` lines, so we must read only the window between the
/// two headers (the connected network), never the scan list.
///
// ponytail: reads the FIRST current-network block. A disconnected interface
// prints no such block, so the common single-Wi-Fi case is correct; a Mac with
// two simultaneously-connected Wi-Fi interfaces would report the first one's
// signal (not disambiguated — vanishingly rare).
#[must_use]
pub fn parse_wifi(text: &str) -> Wifi {
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines
        .iter()
        .position(|l| l.trim_start().starts_with("Current Network Information:"))
    else {
        return Wifi::default(); // no current-network block → not connected
    };
    // End the window at the "Other …" scan list (or end of output).
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with("Other Local Wi-Fi Networks:"))
        .map_or(lines.len(), |off| start + 1 + off);

    let mut w = Wifi::default();
    for line in &lines[start..end] {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Signal / Noise:") {
            // "-43 dBm / -96 dBm"
            let mut parts = v.split('/');
            w.rssi_dbm = parts.next().and_then(parse_dbm);
            w.noise_dbm = parts.next().and_then(parse_dbm);
        } else if let Some(v) = t.strip_prefix("Transmit Rate:") {
            w.tx_rate_mbps = v.trim().parse().ok();
        } else if let Some(v) = t.strip_prefix("PHY Mode:") {
            w.phy_mode = Some(v.trim().to_string());
        }
    }
    w.connected = w.rssi_dbm.is_some();
    w.quality_pct = w.rssi_dbm.map(quality_from_rssi);
    w
}

/// Parse "-43 dBm" (or " -43 dBm ") to `-43`.
fn parse_dbm(s: &str) -> Option<i32> {
    s.split_whitespace().next().and_then(|n| n.parse().ok())
}

/// Parse `ping` statistics. Reads the "N packets transmitted, M packets
/// received, X% packet loss" line and the "round-trip min/avg/max…" line.
/// Tolerates the 100%-loss case (no round-trip line, ping exited non-zero).
#[must_use]
pub fn parse_ping(text: &str) -> Ping {
    let mut p = Ping {
        transmitted: 0,
        received: 0,
        loss_pct: 100.0,
        avg_ms: None,
    };
    for line in text.lines() {
        let t = line.trim();
        if t.contains("packet loss") {
            // "5 packets transmitted, 5 packets received, 0.0% packet loss"
            for field in t.split(',') {
                let f = field.trim();
                if let Some(n) = f.strip_suffix("packets transmitted").map(str::trim) {
                    p.transmitted = n.parse().unwrap_or(0);
                } else if let Some(n) = f
                    .strip_suffix("packets received")
                    .or_else(|| f.strip_suffix("packet received"))
                    .map(str::trim)
                {
                    p.received = n.parse().unwrap_or(0);
                } else if let Some(n) = f.strip_suffix("% packet loss").map(str::trim) {
                    p.loss_pct = n.parse().unwrap_or(100.0);
                }
            }
        } else if let Some(v) = t
            .strip_prefix("round-trip")
            .or_else(|| t.strip_prefix("rtt"))
        {
            // "min/avg/max/stddev = 20.782/26.072/34.204/5.836 ms"
            if let Some(nums) = v.split('=').nth(1) {
                p.avg_ms = nums
                    .trim()
                    .split('/')
                    .nth(1)
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse().ok());
            }
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throughput_seed_and_rate() {
        // Seed sample (no dt): rates are 0, totals pass through.
        let seed = compute_throughput(1000, 500, 1000, 500, None);
        assert_eq!(seed.down_bps, 0);
        assert_eq!(seed.up_bps, 0);
        assert_eq!(seed.total_down_bytes, 1000);
        // 2 MB down / 0.5 MB up over 2s → 1 MB/s down, 0.25 MB/s up.
        let t = compute_throughput(
            2_000_000,
            500_000,
            9_000_000,
            3_000_000,
            Some(Duration::from_secs(2)),
        );
        assert_eq!(t.down_bps, 1_000_000);
        assert_eq!(t.up_bps, 250_000);
        assert_eq!(t.total_down_bytes, 9_000_000);
        // A near-zero gap must not blow up into a spike.
        let z = compute_throughput(1_000_000, 0, 0, 0, Some(Duration::from_millis(10)));
        assert_eq!(z.down_bps, 0);
    }

    #[test]
    fn rssi_quality_mapping() {
        assert_eq!(quality_from_rssi(-43), 100); // excellent, clamped
        assert_eq!(quality_from_rssi(-50), 100);
        assert_eq!(quality_from_rssi(-75), 50);
        assert_eq!(quality_from_rssi(-90), 20);
        assert_eq!(quality_from_rssi(-100), 0);
        assert_eq!(quality_from_rssi(-120), 0); // weaker than floor, clamped
    }

    #[test]
    fn parse_wifi_reads_connected_not_scan_list() {
        // The connected block's Signal is -43; a DIFFERENT network in the scan
        // list below reports -70. We must return -43, never -70.
        let text = "\
          Interfaces:
            en0:
              Status: Connected
              Current Network Information:
                MyNet:
                  PHY Mode: 802.11ax
                  Channel: 48 (5GHz, 160MHz)
                  Signal / Noise: -43 dBm / -96 dBm
                  Transmit Rate: 1921
              Other Local Wi-Fi Networks:
                Neighbor:
                  PHY Mode: 802.11ac
                  Signal / Noise: -70 dBm / -92 dBm
                  Transmit Rate: 300";
        let w = parse_wifi(text);
        assert!(w.connected);
        assert_eq!(w.rssi_dbm, Some(-43));
        assert_eq!(w.noise_dbm, Some(-96));
        assert_eq!(w.tx_rate_mbps, Some(1921));
        assert_eq!(w.phy_mode.as_deref(), Some("802.11ax"));
        assert_eq!(w.quality_pct, Some(100));
    }

    #[test]
    fn parse_wifi_not_connected() {
        let w = parse_wifi("      Interfaces:\n        en0:\n          Status: Off");
        assert!(!w.connected);
        assert_eq!(w.rssi_dbm, None);
        assert_eq!(w.quality_pct, None);
    }

    #[test]
    fn parse_ping_normal() {
        let out = "\
PING 1.1.1.1 (1.1.1.1): 56 data bytes
64 bytes from 1.1.1.1: icmp_seq=0 ttl=57 time=21.0 ms

--- 1.1.1.1 ping statistics ---
5 packets transmitted, 5 packets received, 0.0% packet loss
round-trip min/avg/max/stddev = 20.782/26.072/34.204/5.836 ms";
        let p = parse_ping(out);
        assert_eq!(p.transmitted, 5);
        assert_eq!(p.received, 5);
        assert_eq!(p.loss_pct, 0.0);
        assert_eq!(p.avg_ms, Some(26.072));
    }

    #[test]
    fn parse_ping_total_loss() {
        let out = "\
PING 10.0.0.9 (10.0.0.9): 56 data bytes

--- 10.0.0.9 ping statistics ---
5 packets transmitted, 0 packets received, 100.0% packet loss";
        let p = parse_ping(out);
        assert_eq!(p.transmitted, 5);
        assert_eq!(p.received, 0);
        assert_eq!(p.loss_pct, 100.0);
        assert_eq!(p.avg_ms, None); // no round-trip line
    }

    #[test]
    fn parse_ping_partial_loss() {
        let out = "\
--- 1.1.1.1 ping statistics ---
5 packets transmitted, 3 packets received, 40.0% packet loss
round-trip min/avg/max/stddev = 18.0/22.5/30.0/4.0 ms";
        let p = parse_ping(out);
        assert_eq!(p.received, 3);
        assert_eq!(p.loss_pct, 40.0);
        assert_eq!(p.avg_ms, Some(22.5));
    }
}
