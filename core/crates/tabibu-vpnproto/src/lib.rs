//! Salama VPN — shared wire types (app ⇄ daemon ⇄ control plane ⇄ PoP).
//!
//! "Salama" (Swahili: *safe / at peace*) is Tabibu's WARP-equivalent: it hides
//! the user's IP and encrypts their traffic against the ISP, router, and local
//! network, plus an opt-in country-selectable VPN. This crate is the **pure,
//! root-free, serde-only** vocabulary every layer speaks — no I/O, no sockets,
//! no privilege. It exists first so the daemon, the app backend, and the
//! control plane can't drift out of sync. See
//! `tabibu-vpn-notes/vpn-client-integration.md` §2.
//!
//! Nothing here touches the network; the parts that do (the utun data plane,
//! routes/DNS/pf kill-switch, the root daemon) are deferred behind a Developer
//! ID + a server fleet — see `notes.md` and `docs/adr/0004-*`.

use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr};

/// A single exit/entry location, fetched from the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pop {
    pub id: u16,
    /// ISO 3166-1 alpha-2, e.g. `"KE"`.
    pub country: String,
    pub city: String,
    /// WireGuard endpoint, UDP 51820.
    pub endpoint: SocketAddr,
    /// Probe responder, UDP 51821.
    pub probe: SocketAddr,
    /// 443 fallback + enrollment.
    pub quic: SocketAddr,
    /// WireGuard server public key.
    pub public_key: [u8; 32],
    /// `false` = transit-only PoP (entry hop, never an exit).
    pub can_exit: bool,
}

/// Inter-PoP link, measured server-side and gossiped to clients.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MeshLink {
    pub from: u16,
    pub to: u16,
    pub rtt_ms: f32,
    pub loss: f32,
}

/// Signed by the control plane, verified offline by each PoP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGrant {
    pub client_pub: [u8; 32],
    pub tunnel_ip: Ipv4Addr,
    pub entry_pop: u16,
    pub exit_pop: u16,
    pub tier: u8,
    /// Unix secs; keep short (~600) so a leaked grant expires fast.
    pub not_after: u64,
    pub nonce: [u8; 16],
}

/// Payload of [`Request::Connect`]. Boxed in the enum because it dwarfs the
/// other variants (it carries a full [`Pop`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectReq {
    pub grant_blob: Vec<u8>,
    pub entry: Pop,
    pub exit_pop: u16,
    pub mtu: u16,
    pub dns: Vec<Ipv4Addr>,
}

/// App → daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Status,
    Probe {
        pops: Vec<Pop>,
    },
    Connect(Box<ConnectReq>),
    Disconnect,
    /// Idempotent teardown of every network change we ever made. Used by
    /// uninstall and by crash recovery on daemon startup.
    PurgeConfig,
}

/// Daemon → app (streamed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    Status(TunnelStatus),
    ProbeResult(ProbeSample),
    Throughput {
        rx_bps: u64,
        tx_bps: u64,
        rtt_ms: f32,
    },
    Error {
        code: ErrCode,
        detail: String,
    },
}

/// A real diagnosis synthesised from probe results — WireGuard itself is silent
/// by design (an unknown key, a down server, and blocked UDP are
/// indistinguishable on the wire), so without this the user gets "not
/// connected" and no reason. See [`crate`] consumers and route::diagnose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrCode {
    HandshakeTimeout,
    UdpBlocked,
    GrantRejected,
    GrantExpired,
    RouteInstallFailed,
    DnsInstallFailed,
    KillSwitchFailed,
    MtuTooLow,
}

/// One PoP's probe result, aggregated from ~10 probes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProbeSample {
    pub pop_id: u16,
    pub rtt_p50_ms: f32,
    pub rtt_p95_ms: f32,
    pub jitter_ms: f32,
    /// 0.0..=1.0.
    pub loss: f32,
    /// 0.0..=1.0, reported by the PoP.
    pub load: f32,
    pub replies: u8,
}

/// Current tunnel state, reported to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TunnelStatus {
    pub connected: bool,
    pub entry_pop: Option<u16>,
    pub exit_pop: Option<u16>,
    pub tunnel_ip: Option<Ipv4Addr>,
    /// Unix secs the tunnel came up; `None` when disconnected.
    pub since_unix: Option<u64>,
    /// Whether the pf kill switch is currently installed.
    pub kill_switch: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pop() -> Pop {
        Pop {
            id: 3,
            country: "KE".into(),
            city: "Nairobi".into(),
            endpoint: "203.0.113.10:51820".parse().unwrap(),
            probe: "203.0.113.10:51821".parse().unwrap(),
            quic: "203.0.113.10:443".parse().unwrap(),
            public_key: [7u8; 32],
            can_exit: true,
        }
    }

    /// Every wire type must serde round-trip byte-stable — the daemon, app, and
    /// control plane are separate processes that only agree via these bytes.
    #[test]
    fn wire_types_round_trip() {
        let reqs = vec![
            Request::Status,
            Request::Disconnect,
            Request::PurgeConfig,
            Request::Probe { pops: vec![pop()] },
            Request::Connect(Box::new(ConnectReq {
                grant_blob: vec![1, 2, 3],
                entry: pop(),
                exit_pop: 9,
                mtu: 1420,
                dns: vec![Ipv4Addr::new(10, 7, 1, 1)],
            })),
        ];
        for r in reqs {
            let j = serde_json::to_string(&r).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&j).unwrap(), r);
        }

        let sample = ProbeSample {
            pop_id: 3,
            rtt_p50_ms: 42.0,
            rtt_p95_ms: 58.0,
            jitter_ms: 16.0,
            loss: 0.02,
            load: 0.4,
            replies: 9,
        };
        let events = vec![
            Event::ProbeResult(sample),
            Event::Throughput {
                rx_bps: 1_000_000,
                tx_bps: 500_000,
                rtt_ms: 42.0,
            },
            Event::Error {
                code: ErrCode::UdpBlocked,
                detail: "nothing answered".into(),
            },
            Event::Status(TunnelStatus {
                connected: true,
                entry_pop: Some(3),
                exit_pop: Some(9),
                tunnel_ip: Some(Ipv4Addr::new(10, 7, 3, 5)),
                since_unix: Some(1_753_900_000),
                kill_switch: true,
            }),
        ];
        for e in events {
            let j = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<Event>(&j).unwrap(), e);
        }

        let grant = SessionGrant {
            client_pub: [1u8; 32],
            tunnel_ip: Ipv4Addr::new(10, 7, 3, 5),
            entry_pop: 3,
            exit_pop: 9,
            tier: 1,
            not_after: 1_753_900_600,
            nonce: [9u8; 16],
        };
        let j = serde_json::to_string(&grant).unwrap();
        assert_eq!(serde_json::from_str::<SessionGrant>(&j).unwrap(), grant);

        let link = MeshLink {
            from: 3,
            to: 9,
            rtt_ms: 120.0,
            loss: 0.01,
        };
        let j = serde_json::to_string(&link).unwrap();
        assert_eq!(serde_json::from_str::<MeshLink>(&j).unwrap(), link);
    }

    /// Default status is the disconnected state (no live tunnel).
    #[test]
    fn tunnel_status_default_is_disconnected() {
        let s = TunnelStatus::default();
        assert!(!s.connected);
        assert!(s.exit_pop.is_none());
        assert!(!s.kill_switch);
    }
}
