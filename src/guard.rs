//! Deciding whether a *stranger* may ask this process to dial a given address.
//!
//! This exists only for the hosted HTTP wrapper. Handing the public internet an
//! endpoint field that becomes an outbound connection is server-side request
//! forgery: without a check, anyone could point the service at `127.0.0.1`, at
//! a cloud metadata endpoint like `169.254.169.254`, or at another host inside
//! the provider's network, and read back whatever came out.
//!
//! **The CLI does not use any of this, and must not.** Probing `127.0.0.1:9944`
//! is the CLI's default and the entire point of running it against a local dev
//! node. The difference is who is asking: your own shell, or an anonymous POST.

use std::net::IpAddr;
use url::Url;

/// Why an endpoint was refused. The message is shown to the caller, so it says
/// what is allowed rather than describing the internal network.
pub type Rejection = String;

/// Checks that `endpoint` is a WebSocket URL pointing somewhere on the public
/// internet, resolving the host to be sure.
///
/// # Arguments
///
/// * `endpoint` - The address the caller asked to probe.
///
/// # Returns
///
/// `Ok(())` if every address the host resolves to is public.
///
/// # Limits
///
/// Resolution here and the connection made later are separate lookups, so a
/// name whose record changes in between could still slip through (DNS
/// rebinding). Closing that needs the connection pinned to the address checked,
/// which the WebSocket client does not expose. This blocks the direct attack,
/// not a determined one — which is why the service also runs with no secrets,
/// no metadata credentials worth reaching, and short timeouts.
pub async fn public_websocket_endpoint(endpoint: &str) -> Result<(), Rejection> {
    let url = Url::parse(endpoint).map_err(|e| format!("not a valid URL: {e}"))?;

    match url.scheme() {
        "ws" | "wss" => {}
        other => return Err(format!("scheme must be ws:// or wss://, got {other}://")),
    }

    let port = url.port_or_known_default().unwrap_or(443);

    // Matched on the typed host rather than the string: `host_str()` hands back
    // an IPv6 literal still wrapped in brackets (`[::1]`), which parses as
    // neither an address nor a name, so a string-first version falls through to
    // DNS and refuses `::1` only because the lookup happens to fail. That is
    // the right answer for the wrong reason, and not a thing to rely on.
    let host = match url.host() {
        Some(url::Host::Ipv4(v4)) => {
            let ip = IpAddr::V4(v4);
            return if is_public(ip) {
                Ok(())
            } else {
                Err(reject(&ip.to_string()))
            };
        }
        Some(url::Host::Ipv6(v6)) => {
            let ip = IpAddr::V6(v6);
            return if is_public(ip) {
                Ok(())
            } else {
                Err(reject(&ip.to_string()))
            };
        }
        Some(url::Host::Domain(d)) => d.to_string(),
        None => return Err("URL has no host".to_string()),
    };

    let resolved = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| format!("could not resolve {host}: {e}"))?;

    let mut any = false;
    for addr in resolved {
        any = true;
        if !is_public(addr.ip()) {
            return Err(reject(&host));
        }
    }

    if any {
        Ok(())
    } else {
        Err(format!("{host} resolved to no addresses"))
    }
}

fn reject(host: &str) -> Rejection {
    format!(
        "{host} is not a public address. This hosted service only probes nodes reachable on the \
         public internet; to probe a private or local node, run the CLI, which has no such limit."
    )
}

/// Whether an address is on the public internet.
///
/// Written as an allowlist of "not one of these" rather than a range list so
/// that adding a case is a one-line change; the categories are the ones that
/// let an SSRF reach something interesting.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()          // 10/8, 172.16/12, 192.168/16
                || v4.is_loopback()        // 127/8
                || v4.is_link_local()      // 169.254/16 — cloud metadata lives here
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()     // 0.0.0.0
                || a == 100 && (64..128).contains(&b)  // 100.64/10 carrier-grade NAT
                || a >= 224) // multicast and reserved
        }
        IpAddr::V6(v6) => {
            // An IPv4-mapped address is an IPv4 address wearing a hat; judge the
            // address underneath or ::ffff:127.0.0.1 walks straight through.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || (seg[0] & 0xfe00) == 0xfc00   // fc00::/7 unique local
                || (seg[0] & 0xffc0) == 0xfe80   // fe80::/10 link local
                || (seg[0] & 0xff00) == 0xff00) // ff00::/8 multicast
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn public_addresses_are_allowed() {
        for s in ["1.1.1.1", "8.8.8.8", "104.18.32.7", "2606:4700::1111"] {
            assert!(is_public(ip(s)), "{s} should be public");
        }
    }

    /// Each of these is a real place an SSRF wants to reach.
    #[test]
    fn private_and_special_addresses_are_refused() {
        for s in [
            "127.0.0.1",              // loopback
            "0.0.0.0",                // unspecified
            "10.0.0.5",               // private
            "172.16.4.1",             // private
            "192.168.1.1",            // private
            "169.254.169.254",        // cloud metadata
            "100.64.0.1",             // carrier-grade NAT
            "224.0.0.1",              // multicast
            "::1",                    // v6 loopback
            "fd00::1",                // v6 unique local
            "fe80::1",                // v6 link local
            "::ffff:127.0.0.1",       // v4-mapped loopback — the sneaky one
            "::ffff:169.254.169.254", // v4-mapped metadata
        ] {
            assert!(!is_public(ip(s)), "{s} should be refused");
        }
    }

    /// 100.64/10 has public neighbours on both sides; check the edges rather
    /// than assuming the range arithmetic is right.
    #[test]
    fn carrier_grade_nat_boundaries() {
        assert!(is_public(ip("100.63.255.255")), "just below the range");
        assert!(!is_public(ip("100.64.0.0")), "first in range");
        assert!(!is_public(ip("100.127.255.255")), "last in range");
        assert!(is_public(ip("100.128.0.0")), "just above the range");
    }

    #[tokio::test]
    async fn non_websocket_schemes_are_refused() {
        for url in [
            "http://example.com",
            "https://example.com",
            "file:///etc/passwd",
            "gopher://example.com",
        ] {
            assert!(
                public_websocket_endpoint(url).await.is_err(),
                "{url} should be refused"
            );
        }
    }

    #[tokio::test]
    async fn loopback_literals_are_refused_without_dns() {
        for url in [
            "ws://127.0.0.1:9944",
            "ws://[::1]:9944",
            "wss://169.254.169.254",
            "ws://10.1.2.3:9944",
        ] {
            let err = public_websocket_endpoint(url)
                .await
                .expect_err("{url} should be refused");
            assert!(err.contains("not a public address"), "unhelpful: {err}");
        }
    }

    #[tokio::test]
    async fn garbage_is_refused_rather_than_dialled() {
        assert!(public_websocket_endpoint("not a url").await.is_err());
        assert!(public_websocket_endpoint("ws://").await.is_err());
    }
}
