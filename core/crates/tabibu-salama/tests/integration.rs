//! Integration tests that exercise the real OS paths a generated profile and
//! the status reads depend on. Structural/robust so a CI runner with no network
//! still passes.

use std::io::Write;
use tabibu_salama::{build_doh_profile, dns_status, exposure, Provider};

/// The generated profile must be a valid property list — otherwise macOS
/// rejects it at install with an opaque error. Validate with the real `plutil`.
#[test]
fn generated_profile_passes_plutil_lint() {
    for p in [Provider::Cloudflare, Provider::Quad9] {
        let xml = build_doh_profile(p);
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(xml.as_bytes()).unwrap();
        let out = std::process::Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(f.path())
            .output()
            .expect("plutil should exist on macOS");
        assert!(
            out.status.success(),
            "plutil rejected the {p:?} profile: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn status_reads_never_panic() {
    // Exercises curl (ipinfo) + scutil; assertions are structural so offline CI
    // passes. An IP, when present, is non-empty.
    let e = exposure();
    if let Some(ip) = &e.ip {
        assert!(!ip.is_empty());
    }
    let d = dns_status();
    // When there are no resolvers (offline/failed), nothing is claimed.
    if d.resolvers.is_empty() {
        assert!(!d.local_resolver && !d.exposed && !d.encrypted);
    }
    // "encrypted" and "exposed" are mutually exclusive by construction.
    assert!(!(d.encrypted && d.exposed));
}
