//! Library-only NZB URL policy validation and buffered fetch simulation.
//!
//! **Not a production fetcher or a runtime SSRF boundary.** No ingest route
//! calls this module; it has no real DNS/HTTPS implementation, environment
//! read or decision-log emission. Runtime wiring is tracked in
//! <https://github.com/TogetherWeOwn/nntunev2/issues/84>.
//!
//! [`validate_fetch_url`] checks HTTPS, credentials, port 443, an exact
//! case-insensitive host allowlist, and every address returned by the supplied
//! resolver. It rejects private, metadata, reserved and other forbidden address
//! ranges, even for allowlisted hosts; IP literals bypass DNS but not policy.
//! [`ValidatedUrl`] records the result, but does not bind a network connection.
//!
//! [`fetch_nzb`] models redirect revalidation, rejection codes, byte limits and
//! accumulated *reported* chunk delays with an injected, buffered transport.
//! It does not pass the vetted addresses to that transport. Its URL-only
//! [`FetchTransport`] interface cannot enforce IP pinning or original-host TLS
//! validation. [`RawResponse`] already contains every chunk before limits are
//! checked: no streaming memory bound, wall-clock timeout or cancellation is
//! enforced, and DNS/connect/header time is not measured. Do not implement a
//! live HTTPS client behind this simulator interface.
//!
//! The future runtime must carry the validated host/address set to a pinned
//! HTTPS transport, preserve original-host SNI/certificate checks and Host,
//! stream with a byte cap, and apply one cancellable deadline to DNS, connect,
//! TLS, headers, body and all redirects. Keep URL fetching unavailable until
//! those properties are demonstrated; exporting this module is not wiring it.
//!
//! ## Policy configuration (not read from the Worker environment)
//!
//! [`FetchConfig::allowlist_from_env`] parses a caller-supplied string; it does
//! not read `NZB_FETCH_ALLOWLIST`. That future variable is a comma-separated
//! list of exact hostnames (e.g. `nzb.example,cdn.example`); subdomains are not
//! implied. [`FetchConfig::default`] denies every host. The library returns
//! `HostNotAllowlisted`; no route currently converts it to an HTTP response.
//!
//! ## Model defaults (not enforced network limits)
//!
//! - [`FETCH_TIMEOUT_MS`]: 10_000 ms of reported chunk delays across hops.
//! - [`MAX_REDIRECTS`]: 3 redirect hops after the initial simulated response.
//! - Returned-body cap: [`MAX_NZB_BYTES`](crate::nzb_intake::MAX_NZB_BYTES)
//!   (2 MiB), checked after the transport has buffered its response.
//!
//! ## Stable codes (never reworded)
//!
//! | Rejection | code | HTTP status |
//! |---|---|---|
//! | `BadScheme` | `nzb_fetch_bad_scheme` | 400 |
//! | `MalformedUrl` | `nzb_fetch_malformed_url` | 400 |
//! | `EmbeddedCredentials` | `nzb_fetch_embedded_credentials` | 400 |
//! | `HostNotAllowlisted` | `nzb_fetch_host_not_allowlisted` | 403 |
//! | `ForbiddenAddress` | `nzb_fetch_forbidden_address` | 403 |
//! | `UnresolvableHost` | `nzb_fetch_unresolvable_host` | 400 |
//! | `TooManyRedirects` | `nzb_fetch_too_many_redirects` | 400 |
//! | `Timeout` | `nzb_fetch_timeout` | 504 |
//! | `TooLarge` | `nzb_fetch_too_large` | 413 |
//! | `UpstreamStatus(_)` | `nzb_fetch_upstream_status` | 502 |
//!
//! ## Future decision log and acceptance boundary
//!
//! [`url_host_for_log`] extracts a host without credentials, path or query;
//! it does not emit logs. Future wiring must log only that sanitized host
//! and the stable rejection code, never a full URL or response body.
//!
//! The fake-transport acceptance suite exercises this library model only.
//! Its original runtime-wiring prose describes the future end state, not
//! functionality delivered here. It does not prove pinned HTTPS, streaming
//! limits, actual deadlines, route/config wiring or block-log emission.

use std::net::{IpAddr, Ipv4Addr};

use crate::nzb_intake::MAX_NZB_BYTES;

/// Default simulated chunk-delay budget (ms), not a wall-clock deadline.
pub const FETCH_TIMEOUT_MS: u64 = 10_000;
/// Maximum redirect hops followed after the initial fetch.
pub const MAX_REDIRECTS: u32 = 3;
/// The only explicit port a fetch URL may name (https default when absent).
/// Any other explicit port is malformed: the fetch would otherwise become
/// a port-scan primitive.
const ALLOWED_FETCH_PORT: u16 = 443;

/// Injected address lookup for library policy checks; no runtime DNS adapter
/// is supplied. Return ALL records: any forbidden address in the set rejects.
pub trait DnsResolver {
    fn resolve(&self, host: &str) -> Vec<IpAddr>;
}

/// Tunables for the [`fetch_nzb`] policy simulator (deny-all allowlist,
/// reported-delay budget, redirect cap and returned-body cap).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchConfig {
    pub allowed_hosts: Vec<String>,
    pub timeout_ms: u64,
    pub max_redirects: u32,
    pub max_bytes: usize,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            allowed_hosts: Vec::new(),
            timeout_ms: FETCH_TIMEOUT_MS,
            max_redirects: MAX_REDIRECTS,
            max_bytes: MAX_NZB_BYTES,
        }
    }
}

impl FetchConfig {
    /// Parse a supplied allowlist string, not the Worker environment.
    /// Comma-separated exact hostnames; blanks are dropped and entries
    /// trimmed. Matching is case-insensitive.
    pub fn allowlist_from_env(raw: &str) -> Vec<String> {
        raw.split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .collect()
    }
}

/// Library rejection reasons with proposed HTTP mappings for future wiring.
/// No route currently emits these codes or statuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchRejection {
    BadScheme,
    MalformedUrl,
    EmbeddedCredentials,
    HostNotAllowlisted,
    ForbiddenAddress,
    UnresolvableHost,
    TooManyRedirects,
    Timeout,
    TooLarge,
    UpstreamStatus(u16),
}

impl FetchRejection {
    /// Stable machine code (never reworded).
    pub fn code(&self) -> &'static str {
        match self {
            FetchRejection::BadScheme => "nzb_fetch_bad_scheme",
            FetchRejection::MalformedUrl => "nzb_fetch_malformed_url",
            FetchRejection::EmbeddedCredentials => "nzb_fetch_embedded_credentials",
            FetchRejection::HostNotAllowlisted => "nzb_fetch_host_not_allowlisted",
            FetchRejection::ForbiddenAddress => "nzb_fetch_forbidden_address",
            FetchRejection::UnresolvableHost => "nzb_fetch_unresolvable_host",
            FetchRejection::TooManyRedirects => "nzb_fetch_too_many_redirects",
            FetchRejection::Timeout => "nzb_fetch_timeout",
            FetchRejection::TooLarge => "nzb_fetch_too_large",
            FetchRejection::UpstreamStatus(_) => "nzb_fetch_upstream_status",
        }
    }

    /// Stable HTTP status for a future route to map this rejection to.
    pub fn http_status(&self) -> u16 {
        match self {
            FetchRejection::BadScheme
            | FetchRejection::MalformedUrl
            | FetchRejection::EmbeddedCredentials
            | FetchRejection::UnresolvableHost
            | FetchRejection::TooManyRedirects => 400,
            FetchRejection::HostNotAllowlisted | FetchRejection::ForbiddenAddress => 403,
            FetchRejection::Timeout => 504,
            FetchRejection::TooLarge => 413,
            FetchRejection::UpstreamStatus(_) => 502,
        }
    }
}

/// URL and resolver answers that passed library policy, not a pinned socket.
/// A future transport must consume this value, dial only these addresses,
/// and use the host for TLS SNI/certificate checks and HTTP Host without
/// re-resolution. The current simulator does NOT pass this value to `get`.
/// Never cache it across requests; revalidate every hop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedUrl {
    /// The URL as validated.
    pub url: String,
    /// Lowercased hostname, no trailing dot or brackets, used for lookup.
    /// Future HTTPS wiring must preserve this name for TLS identity.
    pub host: String,
    /// Every vetted resolver answer, in order; no dialing happens here.
    pub addresses: Vec<IpAddr>,
}

/// Buffered single-hop simulator seam; implementations must not follow
/// redirects. This URL-only API receives neither vetted addresses nor a
/// remaining deadline and cannot enforce safe live HTTPS. Use fakes only;
/// the runtime follow-up must supply a different, pinned streaming contract.
pub trait FetchTransport {
    fn get(&mut self, url: &str) -> Result<RawResponse, FetchRejection>;
}

/// Fully buffered simulated hop, not a streaming response. Limits are
/// evaluated after this value has already been allocated and returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawResponse {
    pub status: u16,
    pub location: Option<String>,
    pub chunks: Vec<BodyChunk>,
}

/// Buffered bytes plus a supplied delay for simulation. Reported delays
/// are accumulated, not measured: no real I/O is timed or interrupted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyChunk {
    pub delay_ms: u64,
    pub bytes: Vec<u8>,
}

// --- Address guard (mirrors workers/api vault guard; keep in sync) --------

fn v4_is_forbidden(o: &[u8; 4]) -> bool {
    let v4 = Ipv4Addr::from(*o);
    v4.is_loopback()         // 127/8
        || v4.is_multicast() // 224.0.0.0/4
        || v4.is_link_local() // 169.254/16 (incl. the cloud metadata endpoint)
        || v4.is_private() // 10/8, 172.16/12, 192.168/16
        || v4.is_broadcast()
        || v4.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || o[0] == 0 // 0.0.0.0/8 ("this network")
        || (o[0] == 100 && (o[1] & 0xC0) == 64) // 100.64/10 carrier-grade NAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 (IETF protocol assignments)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // 198.18.0.0/15 benchmarking
        || o[0] >= 240 // 240.0.0.0/4 reserved + 255.255.255.255 broadcast
}

fn ip_is_forbidden(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_is_forbidden(&v4.octets()),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            // Only native global unicast (2000::/3). This excludes NAT64,
            // mapped/translated/compatible IPv4, site-local and discard.
            // Also reject transition and special-use space inside that range
            // rather than trusting a translator's embedded IPv4 destination.
            (s[0] & 0xE000) != 0x2000
                || s[0] == 0x2002 // 6to4 (2002::/16)
                || (s[0] == 0x2001 && s[1] < 0x0200) // IETF special-use, incl. Teredo/ORCHID
                || (s[0] == 0x2001 && s[1] == 0x0DB8) // documentation
                || (s[0] == 0x3FFF && (s[1] & 0xF000) == 0) // documentation (3fff::/20)
        }
    }
}

// --- URL parsing (hand-rolled like newznab::check_base_url) -----------------

/// Split `https://authority/rest` into the authority. Wrong schemes are
/// [`FetchRejection::BadScheme`]; a missing scheme or empty authority is
/// [`FetchRejection::MalformedUrl`].
fn split_authority(url: &str) -> Result<&str, FetchRejection> {
    if url.is_empty() {
        return Err(FetchRejection::MalformedUrl);
    }
    let Some(scheme_end) = url.find("://") else {
        return Err(FetchRejection::MalformedUrl);
    };
    if &url[..scheme_end] != "https" {
        return Err(FetchRejection::BadScheme);
    }
    let after = &url[scheme_end + "://".len()..];
    let authority = after.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err(FetchRejection::MalformedUrl);
    }
    Ok(authority)
}

/// Split an authority into the bare host, enforcing the port rule: no
/// explicit port (https default) or exactly 443. Any other explicit port
/// is [`FetchRejection::MalformedUrl`]; an unparseable port is malformed
/// too (there is no port variant in this contract — non-443 ports are
/// simply not NZB-fetch URLs).
fn split_host(authority: &str) -> Result<String, FetchRejection> {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let Some(bracket_end) = bracketed.find(']') else {
            return Err(FetchRejection::MalformedUrl);
        };
        let host = &bracketed[..bracket_end];
        let rest = &bracketed[bracket_end + 1..];
        if host.is_empty() {
            return Err(FetchRejection::MalformedUrl);
        }
        if let Some(digits) = rest.strip_prefix(':') {
            check_port(digits)?;
        } else if !rest.is_empty() {
            return Err(FetchRejection::MalformedUrl);
        }
        return Ok(host.to_string());
    }
    if authority.chars().filter(|&c| c == ':').count() > 1 {
        // Unbracketed colons can only be a bare IPv6 literal, which the
        // authority grammar forbids (RFC 3986: literals need brackets).
        return Err(FetchRejection::MalformedUrl);
    }
    match authority.split_once(':') {
        None => Ok(authority.to_string()),
        Some((host, digits)) => {
            if host.is_empty() {
                return Err(FetchRejection::MalformedUrl);
            }
            check_port(digits)?;
            Ok(host.to_string())
        }
    }
}

fn check_port(digits: &str) -> Result<(), FetchRejection> {
    if digits.is_empty() {
        return Err(FetchRejection::MalformedUrl);
    }
    match digits.parse::<u16>() {
        Ok(ALLOWED_FETCH_PORT) => Ok(()),
        // Numeric-but-impossible (65536+) or simply not 443: not an
        // NZB-fetch URL either way.
        Ok(_) => Err(FetchRejection::MalformedUrl),
        Err(_) => Err(FetchRejection::MalformedUrl),
    }
}

/// True when the normalized host is exactly one of the configured
/// entries (case-insensitive). Subdomains are never implied.
fn host_allowlisted(host: &str, config: &FetchConfig) -> bool {
    config
        .allowed_hosts
        .iter()
        .any(|entry| entry.to_ascii_lowercase() == host)
}

/// Pre-fetch guard only (no I/O beyond the resolver): https only, no
/// embedded credentials, host on the allowlist, and every resolved
/// address public. IP-literal hosts are checked directly, with no DNS
/// call. The hostname is lowercased and one trailing dot stripped before
/// the allowlist check and resolution, so `NZB.Example.` matches exactly
/// the vetted name.
pub fn validate_fetch_url(
    url: &str,
    config: &FetchConfig,
    dns: &dyn DnsResolver,
) -> Result<ValidatedUrl, FetchRejection> {
    if url.chars().any(|c| c.is_whitespace()) {
        return Err(FetchRejection::MalformedUrl);
    }
    let authority = split_authority(url)?;
    if authority.contains('@') {
        return Err(FetchRejection::EmbeddedCredentials);
    }
    let raw_host = split_host(authority)?;
    let lower = raw_host.to_ascii_lowercase();
    let host = lower.strip_suffix('.').unwrap_or(&lower);
    if host.is_empty() {
        return Err(FetchRejection::MalformedUrl);
    }
    if !host_allowlisted(host, config) {
        return Err(FetchRejection::HostNotAllowlisted);
    }
    let addresses = match host.parse::<IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => dns.resolve(host),
    };
    if addresses.is_empty() {
        // Literals always parse above, so an empty set is an unresolvable
        // name — refused, never fetched.
        return Err(FetchRejection::UnresolvableHost);
    }
    if addresses.iter().any(ip_is_forbidden) {
        // Resolve-ALL: one forbidden record poisons the whole answer, even
        // when the host was allowlisted by mistake.
        return Err(FetchRejection::ForbiddenAddress);
    }
    Ok(ValidatedUrl {
        url: url.to_string(),
        host: host.to_string(),
        addresses,
    })
}

// --- Redirects ---------------------------------------------------------------

/// Resolve a redirect `Location` against the current hop from scratch:
/// absolute URLs as given, protocol-relative (`//host/path`) on the same
/// https scheme, path-absolute (`/path`) on the current host. Anything
/// else is malformed — never silently joined.
fn resolve_location(current_host: &str, location: &str) -> Result<String, FetchRejection> {
    if location.is_empty() || location.chars().any(|c| c.is_whitespace()) {
        return Err(FetchRejection::MalformedUrl);
    }
    if let Some(rest) = location.strip_prefix("//") {
        if rest.is_empty() {
            return Err(FetchRejection::MalformedUrl);
        }
        return Ok(format!("https:{location}"));
    }
    if location.starts_with('/') {
        return Ok(format!(
            "https://{}{location}",
            bracket_ipv6_host(current_host)
        ));
    }
    if location.contains("://") {
        return Ok(location.to_string());
    }
    Err(FetchRejection::MalformedUrl)
}

fn bracket_ipv6_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Simulate URL-fetch policy over already-buffered responses, not live I/O.
///
/// Validates before each simulated hop, but passes only the URL string to
/// `get`: the validated addresses/host are NOT transport-pinned. A real client
/// that re-resolves the host would have a DNS-rebinding window.
///
/// Accumulates reported chunk delays across hops and rejects at or beyond
/// `config.timeout_ms`; no actual deadline bounds DNS or `get`. Checks the
/// final body's size only after all response chunks have been buffered by
/// the transport. Neither a stalled transport nor unbounded upstream memory
/// use can be stopped by this API. Errors never return a partial result.
///
/// Revalidates 301/302/303/307/308 locations and follows at most
/// `config.max_redirects` hops. A missing Location or other non-2xx status
/// becomes UpstreamStatus(status). See the module's runtime follow-up before
/// connecting this model to any caller-supplied URL-fetch route.
pub fn fetch_nzb(
    url: &str,
    config: &FetchConfig,
    dns: &dyn DnsResolver,
    transport: &mut dyn FetchTransport,
) -> Result<Vec<u8>, FetchRejection> {
    let mut current = url.to_string();
    let mut followed: u32 = 0;
    let mut elapsed: u64 = 0;
    loop {
        // The guard runs before EVERY transport call, including the first:
        // a blocked URL is rejected before any fetch happens.
        let target = validate_fetch_url(&current, config, dns)?;
        let response = transport.get(&current)?;
        for chunk in &response.chunks {
            elapsed = elapsed.saturating_add(chunk.delay_ms);
            if elapsed >= config.timeout_ms {
                return Err(FetchRejection::Timeout);
            }
        }
        match response.status {
            200..=299 => {
                let mut body = Vec::new();
                for chunk in &response.chunks {
                    body.extend_from_slice(&chunk.bytes);
                    if body.len() > config.max_bytes {
                        return Err(FetchRejection::TooLarge);
                    }
                }
                return Ok(body);
            }
            status if is_redirect(status) => {
                let Some(location) = response.location else {
                    return Err(FetchRejection::UpstreamStatus(status));
                };
                if followed >= config.max_redirects {
                    return Err(FetchRejection::TooManyRedirects);
                }
                followed += 1;
                current = resolve_location(&target.host, &location)?;
            }
            status => return Err(FetchRejection::UpstreamStatus(status)),
        }
    }
}

/// Host-only extraction for the decision log: the log records the blocked
/// URL's host and the rejection code, NEVER the full URL (it may embed
/// credentials or an apikey) and never the path or query. Unparseable
/// input yields the `invalid` placeholder, which contains no part of the
/// input.
pub fn url_host_for_log(url: &str) -> String {
    const PLACEHOLDER: &str = "invalid";
    parse_log_host(url).unwrap_or_else(|| PLACEHOLDER.to_string())
}

fn parse_log_host(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    // Strip credentials (everything through the last '@'); the userinfo
    // never reaches the log.
    let hostport = authority.rsplit('@').next()?;
    if hostport.is_empty() {
        return None;
    }
    if let Some(bracketed) = hostport.strip_prefix('[') {
        let (inside, _) = bracketed.split_once(']')?;
        if inside.is_empty() {
            return None;
        }
        return Some(inside.to_ascii_lowercase());
    }
    let host = if hostport.chars().filter(|&c| c == ':').count() == 1 {
        let (host, _) = hostport.split_once(':')?;
        if host.is_empty() {
            return None;
        }
        host.to_ascii_lowercase()
    } else {
        // No port, or an unbracketed multi-colon literal: log the host part
        // only (credentials already stripped, path/query never included).
        hostport.to_ascii_lowercase()
    };
    let host = host.strip_suffix('.').unwrap_or(&host);
    if host.is_empty() {
        return None;
    }
    Some(host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoDns;
    impl DnsResolver for NoDns {
        fn resolve(&self, _host: &str) -> Vec<IpAddr> {
            vec![]
        }
    }

    #[test]
    fn allowlist_parsing_drops_blanks() {
        assert!(FetchConfig::allowlist_from_env("").is_empty());
        assert_eq!(
            FetchConfig::allowlist_from_env(" nzb.example ,, cdn.example "),
            vec!["nzb.example".to_string(), "cdn.example".to_string()]
        );
    }

    #[test]
    fn explicit_non_443_port_is_malformed() {
        let cfg = FetchConfig {
            allowed_hosts: vec!["nzb.example".to_string()],
            ..FetchConfig::default()
        };
        // A fetch URL naming any other explicit port is not an NZB-fetch
        // URL: the fetch must never become a port-scan primitive.
        assert_eq!(
            validate_fetch_url("https://nzb.example:8080/x.nzb", &cfg, &NoDns),
            Err(FetchRejection::MalformedUrl)
        );
        assert_eq!(
            validate_fetch_url("https://nzb.example:65536/x.nzb", &cfg, &NoDns),
            Err(FetchRejection::MalformedUrl)
        );
    }

    struct PublicDns;
    impl DnsResolver for PublicDns {
        fn resolve(&self, _host: &str) -> Vec<IpAddr> {
            vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))]
        }
    }

    struct ScriptedTransport {
        responses: Vec<RawResponse>,
    }
    impl FetchTransport for ScriptedTransport {
        fn get(&mut self, _url: &str) -> Result<RawResponse, FetchRejection> {
            Ok(self.responses.remove(0))
        }
    }

    fn delayed(status: u16, location: Option<&str>, delay_ms: u64, bytes: &[u8]) -> RawResponse {
        RawResponse {
            status,
            location: location.map(str::to_string),
            chunks: vec![BodyChunk {
                delay_ms,
                bytes: bytes.to_vec(),
            }],
        }
    }

    #[test]
    fn total_timeout_spans_redirect_hops() {
        let cfg = FetchConfig {
            allowed_hosts: vec!["nzb.example".to_string()],
            timeout_ms: 1_000,
            ..FetchConfig::default()
        };
        let url = "https://nzb.example/a.nzb";

        let mut transport = ScriptedTransport {
            responses: vec![
                delayed(302, Some("https://nzb.example/final.nzb"), 900, b""),
                delayed(200, None, 900, b"<nzb/>"),
            ],
        };
        assert_eq!(
            fetch_nzb(url, &cfg, &PublicDns, &mut transport),
            Err(FetchRejection::Timeout)
        );

        let mut transport = ScriptedTransport {
            responses: vec![
                delayed(302, Some("https://nzb.example/final.nzb"), 400, b""),
                delayed(200, None, 400, b"<nzb/>"),
            ],
        };
        assert_eq!(
            fetch_nzb(url, &cfg, &PublicDns, &mut transport),
            Ok(b"<nzb/>".to_vec())
        );
    }

    #[test]
    fn path_absolute_location_with_url_in_query_stays_on_current_host() {
        assert_eq!(
            resolve_location("nzb.example", "/dl?src=https://cdn.example/x"),
            Ok("https://nzb.example/dl?src=https://cdn.example/x".to_string())
        );
    }

    #[test]
    fn path_absolute_location_rebrackets_ipv6_host() {
        assert_eq!(
            resolve_location("2606:4700::1111", "/x.nzb"),
            Ok("https://[2606:4700::1111]/x.nzb".to_string())
        );
    }
}
