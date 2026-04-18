//! SSRF guard for WebFetch / WebSearch.
//!
//! A prefix check (`http:` / `https:`) is not enough: a prompt-injection
//! payload in any fetched content can request
//! `http://169.254.169.254/latest/meta-data/iam/security-credentials/` (the
//! AWS/GCP/Azure instance metadata endpoint) and get short-lived IAM
//! credentials, which the model will then quote back into a tool_result.
//! Redis / Ollama / internal admin UIs on `127.*` and corporate networks on
//! `10/8` / `192.168/16` are equally reachable.
//!
//! This module resolves the hostname **before** the HTTP request fires and
//! rejects any address that sits in private / loopback / link-local space.
//! The opt-out env var `CC_WEBFETCH_ALLOW_PRIVATE=1` re-enables them for
//! local development (local MCP servers etc.).
//!
//! DNS rebinding is mitigated by returning the resolved IP so callers can
//! pin the outbound request to that specific IP.
use cc_core::CcError;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use url::{Host, Url};

const OPT_OUT_ENV: &str = "CC_WEBFETCH_ALLOW_PRIVATE";

/// Result of the SSRF guard. When `allowed`, the resolved socket address is
/// returned so the caller can pin the reqwest client to that IP and defeat
/// DNS-rebinding.
#[derive(Debug, Clone)]
pub struct GuardOk {
    pub resolved: SocketAddr,
}

/// Resolve `url`'s host and reject private / internal addresses. Returns
/// `Ok` with the resolved socket address on success; callers should pass
/// that address to `reqwest::Client::builder().resolve(host, addr)` to pin
/// the outbound request.
pub async fn guard_url(url: &Url) -> Result<GuardOk, CcError> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(CcError::tool(
            "tool",
            format!("WebFetch refused: unsupported URL scheme {scheme}"),
        ));
    }
    let host = url
        .host()
        .ok_or_else(|| CcError::tool("tool", "WebFetch refused: URL has no host"))?;
    let port = url.port_or_known_default().unwrap_or(match scheme {
        "https" => 443,
        _ => 80,
    });

    // Use the typed Host enum so IPv6 literals don't fall through to DNS.
    // url::Url::host_str() returns "[::1]" (with brackets) for IPv6 which
    // doesn't parse as IpAddr, and then tokio::net::lookup_host would
    // helpfully resolve "[::1]" as a DNS name on some systems (macOS can
    // return a 198.18.* shim address). Host::Ipv6 avoids that entirely.
    let literal_ip: Option<IpAddr> = match &host {
        Host::Ipv4(v) => Some(IpAddr::V4(*v)),
        Host::Ipv6(v) => Some(IpAddr::V6(*v)),
        Host::Domain(_) => None,
    };
    let host_display: String = match &host {
        Host::Ipv4(v) => v.to_string(),
        Host::Ipv6(v) => format!("[{v}]"),
        Host::Domain(d) => (*d).to_string(),
    };

    let opted_out =
        matches!(std::env::var(OPT_OUT_ENV), Ok(v) if v == "1" || v.eq_ignore_ascii_case("true"));

    if let Some(ip) = literal_ip {
        let is_private = is_private_address(&ip);
        if is_private {
            if !opted_out {
                return Err(reject(&host_display));
            }
            tracing::warn!(
                host = %host_display,
                ip = %ip,
                "CC_WEBFETCH_ALLOW_PRIVATE=1 — SSRF guard bypassed for host={host_display} ip={ip}"
            );
        }
        return Ok(GuardOk {
            resolved: SocketAddr::new(ip, port),
        });
    }

    // DNS: resolve all addresses and reject if ANY is private (an attacker
    // controlling DNS could serve one public and one private record — we
    // treat the presence of any private record as a refusal rather than
    // trying to pick the "safe" one).
    let domain = match &host {
        Host::Domain(d) => *d,
        _ => unreachable!("IP literals handled above"),
    };
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((domain, port))
        .await
        .map_err(|e| {
            CcError::tool(
                "tool",
                format!("WebFetch refused: DNS lookup for {domain} failed: {e}"),
            )
        })?
        .collect();

    if addrs.is_empty() {
        return Err(CcError::tool(
            "tool",
            format!("WebFetch refused: DNS returned no addresses for {domain}"),
        ));
    }

    let private_hit = addrs.iter().find(|a| is_private_address(&a.ip()));
    if let Some(private_addr) = private_hit {
        if !opted_out {
            return Err(reject(domain));
        }
        let ip = private_addr.ip();
        tracing::warn!(
            host = %domain,
            ip = %ip,
            "CC_WEBFETCH_ALLOW_PRIVATE=1 — SSRF guard bypassed for host={domain} ip={ip}"
        );
    }

    // Pin to the first returned address. reqwest's `resolve` API lets us
    // force the outbound request to use this socket even if DNS later
    // rebinds.
    Ok(GuardOk { resolved: addrs[0] })
}

fn reject(host: &str) -> CcError {
    CcError::tool(
        "tool",
        format!(
            "WebFetch refused to fetch private/internal address {host}; \
             set {OPT_OUT_ENV}=1 to override for local development."
        ),
    )
}

/// Return true for IPs that must never be reached from a tool-loop driven
/// fetch unless the user has explicitly opted in.
pub fn is_private_address(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => is_private_v6(v6),
    }
}

fn is_private_v4(ip: &Ipv4Addr) -> bool {
    // Cover:
    //   127.0.0.0/8   loopback
    //   10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16  RFC1918
    //   169.254.0.0/16  link-local (includes 169.254.169.254 cloud metadata)
    //   100.64.0.0/10  CGNAT
    //   0.0.0.0/8  unspecified / "this network"
    //   broadcast / multicast / documentation ranges
    if ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified() {
        return true;
    }
    let octets = ip.octets();
    // CGNAT: 100.64.0.0/10  (100.64.* through 100.127.*)
    if octets[0] == 100 && (octets[1] & 0b1100_0000) == 0b0100_0000 {
        return true;
    }
    // Broadcast.
    if ip.is_broadcast() {
        return true;
    }
    // Multicast + reserved — not useful for fetching, deny.
    if ip.is_multicast() {
        return true;
    }
    // Documentation ranges: 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24.
    // These are never real endpoints and generally indicate tests / examples.
    matches!(
        (octets[0], octets[1], octets[2]),
        (192, 0, 2) | (198, 51, 100) | (203, 0, 113)
    )
}

fn is_private_v6(ip: &Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    let segments = ip.segments();
    // Link-local: fe80::/10
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // Unique-local: fc00::/7
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // IPv4-mapped: ::ffff:a.b.c.d — check the embedded v4.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_private_v4(&v4);
    }
    // Documentation: 2001:db8::/32
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return true;
    }
    false
}

/// Shared lock for all tests that mutate `CC_WEBFETCH_ALLOW_PRIVATE`. Env
/// vars are process-global, so parallel cargo test threads will race on
/// them without a lock. Exposed as `pub(crate)` so the web_fetch parent
/// module's tests can share the same lock. `tokio::sync::Mutex` so the
/// guard can be held safely across await points.
#[cfg(test)]
pub(crate) static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[tokio::test]
    async fn literal_loopback_rejected() {
        let _lock = ENV_LOCK.lock().await;
        std::env::remove_var(OPT_OUT_ENV);
        let err = guard_url(&url("http://127.0.0.1/")).await.unwrap_err();
        assert!(err.to_string().contains("refused"));
    }

    #[tokio::test]
    async fn literal_aws_metadata_rejected() {
        let _lock = ENV_LOCK.lock().await;
        std::env::remove_var(OPT_OUT_ENV);
        let err = guard_url(&url("http://169.254.169.254/latest/meta-data/"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("refused"));
    }

    #[tokio::test]
    async fn literal_rfc1918_rejected() {
        let _lock = ENV_LOCK.lock().await;
        std::env::remove_var(OPT_OUT_ENV);
        for addr in &[
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
        ] {
            let err = guard_url(&url(addr)).await.unwrap_err();
            assert!(err.to_string().contains("refused"), "addr={addr}");
        }
    }

    #[tokio::test]
    async fn literal_ipv6_loopback_rejected() {
        let _lock = ENV_LOCK.lock().await;
        std::env::remove_var(OPT_OUT_ENV);
        let err = guard_url(&url("http://[::1]/")).await.unwrap_err();
        assert!(err.to_string().contains("refused"));
    }

    #[tokio::test]
    async fn literal_ipv6_link_local_rejected() {
        let _lock = ENV_LOCK.lock().await;
        std::env::remove_var(OPT_OUT_ENV);
        let err = guard_url(&url("http://[fe80::1]/")).await.unwrap_err();
        assert!(err.to_string().contains("refused"));
    }

    #[tokio::test]
    async fn literal_ipv6_unique_local_rejected() {
        let _lock = ENV_LOCK.lock().await;
        std::env::remove_var(OPT_OUT_ENV);
        let err = guard_url(&url("http://[fc00::1]/")).await.unwrap_err();
        assert!(err.to_string().contains("refused"));
    }

    #[tokio::test]
    async fn literal_cgnat_rejected() {
        let _lock = ENV_LOCK.lock().await;
        std::env::remove_var(OPT_OUT_ENV);
        let err = guard_url(&url("http://100.64.0.1/")).await.unwrap_err();
        assert!(err.to_string().contains("refused"));
    }

    #[tokio::test]
    async fn literal_unspecified_rejected() {
        let _lock = ENV_LOCK.lock().await;
        std::env::remove_var(OPT_OUT_ENV);
        let err = guard_url(&url("http://0.0.0.0/")).await.unwrap_err();
        assert!(err.to_string().contains("refused"));
    }

    #[tokio::test]
    async fn file_scheme_rejected() {
        // Doesn't read env var, so no lock needed.
        let err = guard_url(&url("file:///etc/passwd")).await.unwrap_err();
        assert!(err.to_string().contains("scheme"));
    }

    #[tokio::test]
    async fn opt_out_env_bypasses_guard() {
        // SAFETY: env var set/remove in a test. The shared ENV_LOCK mutex
        // serializes with other tests that read/write OPT_OUT_ENV.
        let _lock = ENV_LOCK.lock().await;
        std::env::set_var(OPT_OUT_ENV, "1");
        let ok = guard_url(&url("http://127.0.0.1/")).await;
        std::env::remove_var(OPT_OUT_ENV);
        assert!(ok.is_ok(), "opt-out should allow loopback");
        assert_eq!(ok.unwrap().resolved.ip().to_string(), "127.0.0.1");
    }

    // Dedicated traced test: confirms the warn! fires ONLY on the opt-out
    // bypass path. Spec Scenario "Developer opt-out" requires the log so
    // the bypass is observable in process logs.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn opt_out_emits_warn_log() {
        let _lock = ENV_LOCK.lock().await;
        std::env::set_var(OPT_OUT_ENV, "1");
        let ok = guard_url(&url("http://127.0.0.1/")).await;
        std::env::remove_var(OPT_OUT_ENV);
        assert!(ok.is_ok(), "opt-out should allow loopback");
        assert!(
            logs_contain("SSRF guard bypassed"),
            "expected warn! log when CC_WEBFETCH_ALLOW_PRIVATE=1 bypasses a private address"
        );
        assert!(
            logs_contain("127.0.0.1"),
            "warn! log must name the bypassed host/ip"
        );
    }

    // Public-host path must NOT log the bypass warning (we only warn when we
    // actually had a private address to bypass).
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn public_host_does_not_emit_bypass_warn() {
        let _lock = ENV_LOCK.lock().await;
        std::env::set_var(OPT_OUT_ENV, "1");
        let _ = guard_url(&url("http://8.8.8.8/")).await;
        std::env::remove_var(OPT_OUT_ENV);
        assert!(
            !logs_contain("SSRF guard bypassed"),
            "public address must not trigger the bypass warn"
        );
    }

    #[test]
    fn private_v4_covers_all_ranges() {
        for s in &[
            "127.0.0.1",
            "10.1.2.3",
            "172.16.5.5",
            "192.168.0.1",
            "169.254.169.254",
            "100.100.0.1",
            "0.0.0.0",
            "224.0.0.1",       // multicast
            "255.255.255.255", // broadcast
            "192.0.2.1",       // doc
        ] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(is_private_address(&ip), "{s}");
        }
    }

    #[test]
    fn public_v4_allowed() {
        for s in &["8.8.8.8", "1.1.1.1", "142.250.80.46"] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(!is_private_address(&ip), "{s} should be public");
        }
    }

    #[test]
    fn javascript_url_wont_parse_as_http() {
        // javascript: is caught in the scheme check; the test is mostly
        // documentation — url::Url parses it fine, our guard rejects it.
        let parsed = Url::parse("javascript:alert(1)").unwrap();
        assert_ne!(parsed.scheme(), "http");
        assert_ne!(parsed.scheme(), "https");
    }
}
