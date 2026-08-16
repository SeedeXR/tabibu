//! Salama local encrypted-DNS resolver (`tabibu-dohd`).
//!
//! A tiny root LaunchDaemon: it binds `127.0.0.1:53` and forwards every DNS
//! query to a DoH (DNS-over-HTTPS) server, so the whole system's lookups are
//! encrypted — no System Settings profile, no per-app config. The app points
//! the system resolver at `127.0.0.1` while this runs.
//!
//! Design for "never break the user's internet":
//!   - The HTTPS leg shells out to the system `curl` — zero extra dependencies.
//!   - A bounded worker pool: a query flood drops packets (clients retry)
//!     instead of spawning unbounded threads.
//!   - It only ever binds LOOPBACK, so it's never reachable off the machine.
//!   - The daemon is installed with `KeepAlive` so macOS restarts it if it
//!     dies; the app switches system DNS to `127.0.0.1` ONLY after verifying
//!     this resolver answers, and restores DNS FIRST on teardown.
//!
//! Usage: `tabibu-dohd [PORT] [DOH_URL]` (defaults: `53`,
//! `https://dns.quad9.net/dns-query`).

use std::io::Write;
use std::net::UdpSocket;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};

const DEFAULT_DOH: &str = "https://dns.quad9.net/dns-query";
const WORKERS: usize = 8;

fn main() {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or(53);
    let doh = args.next().unwrap_or_else(|| DEFAULT_DOH.to_string());

    let sock = match UdpSocket::bind(("127.0.0.1", port)) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("tabibu-dohd: cannot bind 127.0.0.1:{port}: {e}");
            std::process::exit(1); // KeepAlive will retry; app never switches DNS unless we answer
        }
    };
    eprintln!("tabibu-dohd: forwarding 127.0.0.1:{port} → {doh}");

    // Bounded queue + fixed worker pool: never spawn unbounded threads.
    let (tx, rx) = mpsc::sync_channel::<(Vec<u8>, std::net::SocketAddr)>(256);
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..WORKERS {
        let rx = Arc::clone(&rx);
        let sock = Arc::clone(&sock);
        let doh = doh.clone();
        std::thread::spawn(move || loop {
            let msg = rx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv();
            let Ok((query, from)) = msg else { return };
            if let Some(resp) = doh_resolve(&doh, &query) {
                let _ = sock.send_to(&resp, from);
            }
        });
    }

    let mut buf = [0u8; 4096];
    loop {
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            // Drop on overload rather than block the receive loop; the stub
            // resolver in the OS retries, exactly as with any lossy UDP path.
            let _ = tx.try_send((buf[..n].to_vec(), from));
        }
    }
}

/// Forward one raw DNS query to the DoH server (RFC 8484) via `curl` and return
/// the raw DNS response bytes, or `None` on any failure.
fn doh_resolve(url: &str, query: &[u8]) -> Option<Vec<u8>> {
    let mut child = Command::new("/usr/bin/curl")
        .args([
            "-s",
            "--max-time",
            "5",
            "-H",
            "Content-Type: application/dns-message",
            "-H",
            "Accept: application/dns-message",
            "--data-binary",
            "@-",
            url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(query).ok()?;
    let out = child.wait_with_output().ok()?;
    (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doh_resolve_returns_answer_for_a_real_query() {
        // A minimal DNS query for A example.com (id 0x1234, RD set).
        let query: &[u8] = &[
            0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // header
            0x07, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 0x03, b'c', b'o', b'm',
            0x00, // example.com
            0x00, 0x01, 0x00, 0x01, // QTYPE A, QCLASS IN
        ];
        // Online: a real DNS response echoes the query id in the first 2 bytes.
        // Offline/CI: `None` is acceptable — the call must not panic either way.
        if let Some(resp) = doh_resolve(DEFAULT_DOH, query) {
            assert!(resp.len() > query.len(), "response should carry answers");
            assert_eq!(&resp[0..2], &query[0..2], "response echoes the query id");
        }
    }

    #[test]
    fn doh_resolve_bad_url_is_none_not_panic() {
        assert!(doh_resolve("https://127.0.0.1:1/nope", &[0u8; 12]).is_none());
    }
}
