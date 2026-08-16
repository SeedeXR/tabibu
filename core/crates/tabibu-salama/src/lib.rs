//! Salama — LOCAL privacy engine (no server, no Developer ID).
//!
//! The full Salama VPN (hide your IP behind an exit) needs Tabibu-run servers
//! and is deferred (see `tabibu-route`/`tabibu-vpnproto` + ADR-0004). But two
//! privacy wins are entirely local and ship **live**:
//!
//! 1. **Exposure readout** — what your ISP / the network sees right now: your
//!    public IP, country, and network (ISP), read from a NEUTRAL echo
//!    (`ipinfo.io`). Salama depends on Cloudflare/WARP for nothing.
//! 2. **Encrypted DNS (DoH)** — a native macOS configuration profile that
//!    routes DNS over HTTPS to a resolver of the user's choice (Cloudflare,
//!    Quad9, Google, AdGuard, or a custom one), so the ISP, router, and Wi-Fi
//!    can no longer see *which sites you visit*. This is Salama's OWN
//!    encryption — it works with WARP off and runs no Tabibu process. macOS
//!    requires the user to
//!    approve the profile once (Apple won't let any app silently reroute DNS —
//!    that would be malware); after that it's system-wide and survives reboots,
//!    with no resident Tabibu process.
//!
//! Honest scope: encrypted DNS hides your DNS *lookups*, not your IP — sites and
//! the ISP still see the destination addresses you connect to. Hiding the IP is
//! the server-dependent VPN, which waits.
//!
//! The OS reads (`scutil`, `curl`) shell out; the parsers and the profile
//! generator are pure and unit-tested.

use serde::Serialize;
use std::process::Command;

/// What the network can see about you right now. Read from a NEUTRAL IP echo
/// (ipinfo.io) — Salama does not depend on Cloudflare/WARP for anything.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct Exposure {
    /// Public IP as seen from the internet (`None` if the probe failed/offline).
    pub ip: Option<String>,
    /// ISO country code (e.g. `"TZ"`).
    pub country: Option<String>,
    /// The network your traffic appears to come from — usually your ISP
    /// (e.g. `"Vodacom Tanzania Ltd"`), or a relay's name if one is active.
    pub org: Option<String>,
}

/// Current DNS resolver posture.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct DnsStatus {
    /// Resolver addresses macOS is using (resolver #1).
    pub resolvers: Vec<String>,
    /// Every resolver is loopback → a local resolver (WARP or a local proxy) is
    /// handling DNS rather than the plaintext LAN/ISP resolver.
    pub local_resolver: bool,
    /// Best-effort: `scutil` shows a DoH/encrypted marker for resolver #1 (a
    /// managed DoH profile uses *public* bootstrap IPs, so loopback alone can't
    /// detect it).
    pub encrypted: bool,
    /// Lookups likely go out in the clear: a non-loopback resolver AND no
    /// encryption marker. Deliberately conservative — never claims "exposed"
    /// when DoH might be on.
    pub exposed: bool,
}

/// Combined local privacy status for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct PrivacyStatus {
    pub exposure: Exposure,
    pub dns: DnsStatus,
}

/// Read the live exposure via the neutral `ipinfo.io` echo. Best-effort:
/// returns `Exposure::default()` (all `None`) if offline or curl is absent.
#[must_use]
pub fn exposure() -> Exposure {
    let out = Command::new("/usr/bin/curl")
        .args(["-s", "--max-time", "8", "https://ipinfo.io/json"])
        .output();
    match out {
        Ok(o) if o.status.success() => parse_ipinfo(&String::from_utf8_lossy(&o.stdout)),
        _ => Exposure::default(),
    }
}

/// Read the current DNS resolvers from `scutil --dns`.
#[must_use]
pub fn dns_status() -> DnsStatus {
    let out = Command::new("/usr/sbin/scutil").arg("--dns").output();
    match out {
        Ok(o) if o.status.success() => parse_scutil_dns(&String::from_utf8_lossy(&o.stdout)),
        _ => DnsStatus::default(),
    }
}

/// Full local privacy snapshot.
#[must_use]
pub fn status() -> PrivacyStatus {
    PrivacyStatus {
        exposure: exposure(),
        dns: dns_status(),
    }
}

// ---------------------------------------------------------------------------
// Encrypted DNS (DoH) configuration profile
// ---------------------------------------------------------------------------

/// The exact set of profile ids the app is allowed to install/remove. Removal
/// runs `profiles remove` with admin rights, so the id must NEVER come straight
/// from the frontend — it's validated against this list first.
pub const PROFILE_IDS: &[&str] = &["cloudflare", "quad9", "google", "adguard", "custom"];

/// Full profile identifier for a validated id (`ai.tabibu.salama.dns.<id>`).
/// Returns `None` for anything not in [`PROFILE_IDS`] — the injection guard.
#[must_use]
pub fn profile_identifier(id: &str) -> Option<String> {
    PROFILE_IDS
        .contains(&id)
        .then(|| format!("ai.tabibu.salama.dns.{id}"))
}

/// The DoH endpoint URL for a built-in provider id (defaults to Cloudflare for
/// an unknown id). Used by the local resolver daemon (`tabibu-dohd`). Always one
/// of a fixed set of trusted `https://…` URLs — never user input.
#[must_use]
pub fn provider_doh_url(id: &str) -> &'static str {
    Provider::from_id(id).doh_url()
}

/// A built-in encrypted-DNS provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Provider {
    /// Cloudflare `1.1.1.1` — fast, no query logging.
    Cloudflare,
    /// Quad9 `9.9.9.9` — Swiss, no logging, blocks known-malware domains.
    Quad9,
    /// Google Public DNS `8.8.8.8`.
    Google,
    /// AdGuard DNS — blocks ads & trackers at the DNS layer.
    AdGuard,
}

impl Provider {
    #[must_use]
    pub fn from_id(s: &str) -> Self {
        match s {
            "quad9" => Self::Quad9,
            "google" => Self::Google,
            "adguard" => Self::AdGuard,
            _ => Self::Cloudflare,
        }
    }
    fn suffix(self) -> &'static str {
        match self {
            Self::Cloudflare => "cloudflare",
            Self::Quad9 => "quad9",
            Self::Google => "google",
            Self::AdGuard => "adguard",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Cloudflare => "Cloudflare",
            Self::Quad9 => "Quad9",
            Self::Google => "Google",
            Self::AdGuard => "AdGuard",
        }
    }
    fn doh_url(self) -> &'static str {
        match self {
            Self::Cloudflare => "https://cloudflare-dns.com/dns-query",
            Self::Quad9 => "https://dns.quad9.net/dns-query",
            Self::Google => "https://dns.google/dns-query",
            Self::AdGuard => "https://dns.adguard-dns.com/dns-query",
        }
    }
    fn addresses(self) -> &'static [&'static str] {
        match self {
            Self::Cloudflare => &[
                "1.1.1.1",
                "1.0.0.1",
                "2606:4700:4700::1111",
                "2606:4700:4700::1001",
            ],
            Self::Quad9 => &["9.9.9.9", "149.112.112.112", "2620:fe::fe", "2620:fe::9"],
            Self::Google => &[
                "8.8.8.8",
                "8.8.4.4",
                "2001:4860:4860::8888",
                "2001:4860:4860::8844",
            ],
            Self::AdGuard => &[
                "94.140.14.14",
                "94.140.15.15",
                "2a10:50c0::ad1:ff",
                "2a10:50c0::ad2:ff",
            ],
        }
    }
    /// Stable UUIDs per provider — reinstalling replaces the same profile rather
    /// than stacking duplicates. (Fixed, not random: this crate has no clock/RNG
    /// and stability is the desired behaviour.)
    fn uuids(self) -> (&'static str, &'static str) {
        match self {
            Self::Cloudflare => (
                "5A1A3A00-C10D-4A00-9000-000000000001",
                "5A1A3A00-C10D-4A00-9000-000000000002",
            ),
            Self::Quad9 => (
                "5A1A3A00-9009-4A00-9000-000000000001",
                "5A1A3A00-9009-4A00-9000-000000000002",
            ),
            Self::Google => (
                "5A1A3A00-6006-4A00-9000-000000000001",
                "5A1A3A00-6006-4A00-9000-000000000002",
            ),
            Self::AdGuard => (
                "5A1A3A00-AD00-4A00-9000-000000000001",
                "5A1A3A00-AD00-4A00-9000-000000000002",
            ),
        }
    }
}

const CUSTOM_UUIDS: (&str, &str) = (
    "5A1A3A00-C057-4A00-9000-000000000001",
    "5A1A3A00-C057-4A00-9000-000000000002",
);

/// Escape a string for inclusion in XML character data — REQUIRED for the
/// user-supplied custom label/URL, or a `<`/`&` could break out of its element
/// and inject extra profile payloads into a file the user then installs.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Build a macOS `.mobileconfig` for one DoH resolver. `suffix` comes only from
/// our own code (never the frontend); `label`/`url`/`addresses` may be user
/// input and are XML-escaped. The app writes it to disk and `open`s it so the
/// user approves it once in System Settings; removable any time.
fn profile_xml(
    suffix: &str,
    label: &str,
    url: &str,
    addresses: &[String],
    cfg_uuid: &str,
    payload_uuid: &str,
) -> String {
    let label = xml_escape(label);
    let url = xml_escape(url);
    let addresses = addresses
        .iter()
        .map(|a| format!("        <string>{}</string>", xml_escape(a)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>PayloadType</key><string>com.apple.dnsSettings.managed</string>
      <key>PayloadVersion</key><integer>1</integer>
      <key>PayloadIdentifier</key><string>ai.tabibu.salama.dns.{suffix}</string>
      <key>PayloadUUID</key><string>{payload_uuid}</string>
      <key>PayloadDisplayName</key><string>Salama Encrypted DNS ({label})</string>
      <key>DNSSettings</key>
      <dict>
        <key>DNSProtocol</key><string>HTTPS</string>
        <key>ServerURL</key><string>{url}</string>
        <key>ServerAddresses</key>
        <array>
{addresses}
        </array>
      </dict>
    </dict>
  </array>
  <key>PayloadType</key><string>Configuration</string>
  <key>PayloadVersion</key><integer>1</integer>
  <key>PayloadIdentifier</key><string>ai.tabibu.salama.dns.{suffix}</string>
  <key>PayloadUUID</key><string>{cfg_uuid}</string>
  <key>PayloadDisplayName</key><string>Salama Encrypted DNS ({label})</string>
  <key>PayloadDescription</key><string>Routes this Mac's DNS over HTTPS ({label}), so your ISP, router, and Wi-Fi can no longer see which sites you visit. Installed by Tabibu (Salama). Remove any time in System Settings.</string>
  <key>PayloadOrganization</key><string>Tabibu</string>
  <key>PayloadRemovalDisallowed</key><false/>
</dict>
</plist>
"#
    )
}

/// Build the profile for a built-in provider.
#[must_use]
pub fn build_doh_profile(p: Provider) -> String {
    let (cfg, payload) = p.uuids();
    let addrs: Vec<String> = p.addresses().iter().map(|s| (*s).to_string()).collect();
    profile_xml(p.suffix(), p.label(), p.doh_url(), &addrs, cfg, payload)
}

/// Build a profile for a user-defined custom DoH resolver. `label`/`url`/
/// `addresses` are escaped; the identifier suffix is the fixed `custom` slot.
#[must_use]
pub fn build_custom_doh_profile(label: &str, url: &str, addresses: &[String]) -> String {
    profile_xml(
        "custom",
        label,
        url,
        addresses,
        CUSTOM_UUIDS.0,
        CUSTOM_UUIDS.1,
    )
}

// ---------------------------------------------------------------------------
// Pure parsers (unit-tested without the OS)
// ---------------------------------------------------------------------------

/// Parse an `ipinfo.io/json` body into [`Exposure`]. The `org` field carries an
/// `AS#####` prefix (e.g. `"AS36908 Vodacom Tanzania Ltd"`) which we strip for
/// display.
#[must_use]
pub fn parse_ipinfo(json: &str) -> Exposure {
    #[derive(serde::Deserialize)]
    struct IpInfo {
        ip: Option<String>,
        country: Option<String>,
        org: Option<String>,
    }
    match serde_json::from_str::<IpInfo>(json) {
        Ok(i) => Exposure {
            ip: i.ip,
            country: i.country,
            org: i.org.map(|o| strip_asn(&o)),
        },
        Err(_) => Exposure::default(),
    }
}

/// Drop a leading `AS<digits> ` from an ipinfo `org` string.
fn strip_asn(org: &str) -> String {
    org.strip_prefix("AS")
        .and_then(|rest| rest.split_once(' '))
        .map_or_else(|| org.to_string(), |(_, name)| name.to_string())
}

/// Is an address a loopback resolver (a local proxy is handling DNS)?
fn is_loopback(addr: &str) -> bool {
    let a = addr.trim();
    a.starts_with("127.") || a == "::1" || a.contains("127.0.")
}

/// Parse the resolver #1 nameservers out of `scutil --dns`.
#[must_use]
pub fn parse_scutil_dns(text: &str) -> DnsStatus {
    let mut resolvers = Vec::new();
    let mut in_first = false;
    let mut doh = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("resolver #") {
            if t == "resolver #1" && resolvers.is_empty() {
                in_first = true;
            } else if in_first {
                break; // reached the next resolver block; #1 is done
            }
            continue;
        }
        if in_first {
            if let Some(v) = t.strip_prefix("nameserver[") {
                if let Some(addr) = v.split_once(':').map(|(_, a)| a.trim()) {
                    if !addr.is_empty() {
                        resolvers.push(addr.to_string());
                    }
                }
            }
            // Best-effort DoH marker: a managed encrypted-DNS resolver reports
            // its `https://…` DoH URL / an "Encrypted" flag in this block.
            if t.contains("https://") || t.contains("Encrypted") {
                doh = true;
            }
        }
    }
    let local_resolver = !resolvers.is_empty() && resolvers.iter().all(|r| is_loopback(r));
    let encrypted = doh || local_resolver;
    // Never claim "exposed" when there's an encryption signal.
    let exposed = !encrypted && resolvers.iter().any(|r| !is_loopback(r));
    DnsStatus {
        resolvers,
        local_resolver,
        encrypted,
        exposed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ipinfo_reads_ip_country_org() {
        let body = r#"{"ip":"197.250.226.93","city":"Dar es Salaam","country":"TZ","org":"AS36908 Vodacom Tanzania Ltd","timezone":"Africa/Dar_es_Salaam"}"#;
        let e = parse_ipinfo(body);
        assert_eq!(e.ip.as_deref(), Some("197.250.226.93"));
        assert_eq!(e.country.as_deref(), Some("TZ"));
        // The AS##### prefix is stripped.
        assert_eq!(e.org.as_deref(), Some("Vodacom Tanzania Ltd"));

        // Missing fields → None, not a panic.
        let partial = parse_ipinfo(r#"{"ip":"1.2.3.4"}"#);
        assert_eq!(partial.ip.as_deref(), Some("1.2.3.4"));
        assert!(partial.country.is_none() && partial.org.is_none());

        // Garbage → default (offline/blocked), not a panic.
        assert_eq!(parse_ipinfo("not json"), Exposure::default());
    }

    #[test]
    fn parse_scutil_loopback_is_local_not_exposed() {
        // Real WARP-style output: resolver #1 with loopback nameservers.
        let text = "\
DNS configuration

resolver #1
  nameserver[0] : 127.0.2.2
  nameserver[1] : 127.0.2.3
  nameserver[2] : ::ffff:127.0.2.2
  flags    : Request A records
  reach    : 0x00030002

resolver #2
  nameserver[0] : 8.8.8.8
";
        let d = parse_scutil_dns(text);
        assert_eq!(d.resolvers.len(), 3, "only resolver #1's nameservers");
        assert!(d.local_resolver, "all loopback → local resolver");
        assert!(d.encrypted, "local resolver counts as encrypted");
        assert!(!d.exposed, "loopback resolvers are not visible to the ISP");
    }

    #[test]
    fn parse_scutil_public_resolver_is_exposed() {
        let text = "\
resolver #1
  nameserver[0] : 192.168.1.1
  nameserver[1] : 8.8.8.8
";
        let d = parse_scutil_dns(text);
        assert_eq!(d.resolvers, vec!["192.168.1.1", "8.8.8.8"]);
        assert!(!d.local_resolver);
        assert!(!d.encrypted);
        assert!(d.exposed, "router/public resolvers see your lookups");
    }

    #[test]
    fn parse_scutil_doh_marker_is_encrypted_not_exposed() {
        // A DoH profile uses PUBLIC bootstrap IPs, but a DoH URL / Encrypted flag
        // in resolver #1 must be read as encrypted, never "exposed".
        let text = "\
resolver #1
  nameserver[0] : 1.1.1.1
  flags    : Encrypted Request A records
  dns_over_https : https://cloudflare-dns.com/dns-query
";
        let d = parse_scutil_dns(text);
        assert!(d.encrypted, "DoH marker → encrypted");
        assert!(
            !d.exposed,
            "encrypted DoH is not exposed despite a public IP"
        );
    }

    #[test]
    fn doh_profile_is_wellformed_per_provider() {
        for (p, url, needle) in [
            (
                Provider::Cloudflare,
                "https://cloudflare-dns.com/dns-query",
                "1.1.1.1",
            ),
            (
                Provider::Quad9,
                "https://dns.quad9.net/dns-query",
                "9.9.9.9",
            ),
            (Provider::Google, "https://dns.google/dns-query", "8.8.8.8"),
            (
                Provider::AdGuard,
                "https://dns.adguard-dns.com/dns-query",
                "94.140.14.14",
            ),
        ] {
            let x = build_doh_profile(p);
            assert!(x.starts_with("<?xml"));
            assert!(
                x.contains("com.apple.dnsSettings.managed"),
                "DoH payload type"
            );
            assert!(x.contains("<key>DNSProtocol</key><string>HTTPS</string>"));
            assert!(x.contains(url), "provider DoH URL present");
            assert!(x.contains(needle), "provider address present");
            assert!(x.contains("PayloadRemovalDisallowed"));
            assert!(x.contains("</plist>"));
        }
    }

    #[test]
    fn provider_from_id_defaults_to_cloudflare() {
        assert_eq!(Provider::from_id("quad9"), Provider::Quad9);
        assert_eq!(Provider::from_id("google"), Provider::Google);
        assert_eq!(Provider::from_id("adguard"), Provider::AdGuard);
        assert_eq!(Provider::from_id("nonsense"), Provider::Cloudflare);
    }

    #[test]
    fn custom_profile_escapes_injection() {
        // A hostile label tries to close its element and inject another payload.
        let evil = r#"Evil</string><key>PayloadRemovalDisallowed</key><true/><string>"#;
        let x =
            build_custom_doh_profile(evil, "https://doh.example/dns-query", &["9.9.9.9".into()]);
        // The raw injection must NOT appear verbatim (it's escaped)…
        assert!(!x.contains("</string><key>PayloadRemovalDisallowed</key><true/>"));
        assert!(x.contains("&lt;/string&gt;"), "angle brackets escaped");
        // …so removal stays allowed (the injected <true/> never took effect).
        assert!(x.contains("<key>PayloadRemovalDisallowed</key><false/>"));
        assert!(x.contains("https://doh.example/dns-query"));
    }

    #[test]
    fn profile_identifier_allowlists() {
        assert_eq!(
            profile_identifier("cloudflare").as_deref(),
            Some("ai.tabibu.salama.dns.cloudflare")
        );
        assert_eq!(
            profile_identifier("custom").as_deref(),
            Some("ai.tabibu.salama.dns.custom")
        );
        // Anything not in the allowlist (incl. injection attempts) → None.
        assert_eq!(profile_identifier("cloudflare; rm -rf /"), None);
        assert_eq!(profile_identifier(".."), None);
        assert_eq!(profile_identifier(""), None);
    }
}
