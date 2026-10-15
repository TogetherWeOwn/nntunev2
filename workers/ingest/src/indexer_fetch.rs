//! Guarded outbound fetch for BYO Newznab indexer search URLs.
//!
//! The indexer URL is user-supplied and the fetch carries the vault-resolved
//! API key, so the fetch is hostile input shaped like the vault
//! `test-connection` and NZB fetch surfaces: syntactic URL checks alone are
//! bypassable through DNS (rebinding, mixed public/private answers) and
//! redirects (metadata endpoints, port-scan hops, http downgrades that would
//! leak the key in cleartext).
//!
//! Invariant (mirrors `workers/api` vault guard; keep the two in sync):
//! - https only, no embedded credentials, 443-only port allowlist;
//! - resolve ALL DNS records and reject loopback / link-local / private /
//!   unique-local / reserved (v4+v6, including transition space);
//! - dial ONLY the vetted addresses from the single validation resolve
//!   (DNS pin — the transport never re-resolves), retaining the hostname
//!   for TLS SNI / certificate checks and the HTTP Host header;
//! - every redirect hop is re-guarded from scratch (cap [`MAX_REDIRECTS`]);
//! - dial + total timeouts ([`DIAL_TIMEOUT_MS`], [`FETCH_TIMEOUT_MS`], the
//!   vault outbound numbers, reused not reinvented);
//! - every block maps to a stable [`FetchRejection::code`] +
//!   [`FetchRejection::http_status`] through the shared structured-error
//!   taxonomy envelope (this module supplies codes and statuses only — it
//!   does not define an envelope).
//!
//! `std`-only plus the crate's existing deps: the URL is parsed the same
//! hand-rolled way `newznab::check_base_url` already does (no `url` crate
//! in this worker). DNS is a trait so tests use a fake and the Worker uses
//! real DNS; the HTTP hop is a trait so tests use a fake transport while
//! the real Worker transport pins the TCP/TLS dial to
//! [`ValidatedTarget::addresses`] with [`ValidatedTarget::host`] for SNI.

use std::net::{IpAddr, Ipv4Addr};

/// The only port an indexer fetch may dial (https default). Any other
/// explicit port turns the fetch into a port-scan primitive.
pub const ALLOWED_INDEXER_PORT: u16 = 443;
/// Dial/handshake deadline per hop (ms): the vault dial number, reused.
pub const DIAL_TIMEOUT_MS: u64 = 5_000;
/// Total deadline for one fetch (ms): the vault total, reused.
pub const FETCH_TIMEOUT_MS: u64 = 15_000;
/// Maximum redirect hops followed after the initial fetch.
pub const MAX_REDIRECTS: u32 = 3;

/// DNS resolution as a trait so tests use a fake and the Worker uses real
/// DNS. Resolve ALL records: any forbidden address in the set rejects.
pub trait DnsResolver {
    fn resolve(&self, host: &str) -> Vec<IpAddr>;
}

/// Tunables for [`fetch_indexer`]; defaults are the pinned contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchConfig {
    pub dial_timeout_ms: u64,
    pub timeout_ms: u64,
    pub max_redirects: u32,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            dial_timeout_ms: DIAL_TIMEOUT_MS,
            timeout_ms: FETCH_TIMEOUT_MS,
            max_redirects: MAX_REDIRECTS,
        }
    }
}

/// Why a fetch was refused or failed. Guard blocks carry a stable 4xx;
/// timeouts a stable 504; upstream failures a stable 502.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchRejection {
    BadScheme,
    MalformedUrl,
    EmbeddedCredentials,
    PortNotAllowed,
    ForbiddenAddress,
    UnresolvableHost,
    TooManyRedirects,
    DialTimeout,
    Timeout,
    UpstreamStatus(u16),
}

impl FetchRejection {
    /// Stable machine code for the shared taxonomy (never reworded).
    pub fn code(&self) -> &'static str {
        match self {
            FetchRejection::BadScheme => "indexer_fetch_bad_scheme",
            FetchRejection::MalformedUrl => "indexer_fetch_malformed_url",
            FetchRejection::EmbeddedCredentials => "indexer_fetch_embedded_credentials",
            FetchRejection::PortNotAllowed => "indexer_fetch_port_not_allowed",
            FetchRejection::ForbiddenAddress => "indexer_fetch_forbidden_address",
            FetchRejection::UnresolvableHost => "indexer_fetch_unresolvable_host",
            FetchRejection::TooManyRedirects => "indexer_fetch_too_many_redirects",
            FetchRejection::DialTimeout => "indexer_fetch_dial_timeout",
            FetchRejection::Timeout => "indexer_fetch_timeout",
            FetchRejection::UpstreamStatus(_) => "indexer_fetch_upstream_status",
        }
    }

    /// Stable HTTP status the caller answers with.
    pub fn http_status(&self) -> u16 {
        match self {
            FetchRejection::BadScheme
            | FetchRejection::MalformedUrl
            | FetchRejection::EmbeddedCredentials
            | FetchRejection::UnresolvableHost
            | FetchRejection::TooManyRedirects => 400,
            FetchRejection::PortNotAllowed | FetchRejection::ForbiddenAddress => 403,
            FetchRejection::DialTimeout | FetchRejection::Timeout => 504,
            FetchRejection::UpstreamStatus(_) => 502,
        }
    }
}

/// One fetch target that passed the guard, pinned for immediate use.
/// Never store it across requests: revalidate at every fetch, dial ONLY
/// `addresses`, and keep `host` for TLS SNI/certificate checks and the
/// HTTP Host header. The transport never re-resolves `host`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedTarget {
    /// The URL as validated (may carry the api key).
    pub url: String,
    /// Lowercased hostname, no trailing dot or brackets: the exact name
    /// that was resolved, and what TLS SNI sees.
    pub host: String,
    /// Always [`ALLOWED_INDEXER_PORT`].
    pub port: u16,
    /// EVERY vetted address, in order: the only dial destinations.
    pub addresses: Vec<IpAddr>,
}

/// The one dial the transport may make for a validated target. This is
/// where the pin becomes observable: `addresses` is the vetted set from
/// the single validation resolve, `sni_host` is the original hostname for
/// TLS SNI/certificate checks and Host, and the real Worker transport
/// must connect to those addresses on `port` without any second DNS
/// lookup of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedRequest {
    pub url: String,
    pub host: String,
    pub sni_host: String,
    pub port: u16,
    pub addresses: Vec<IpAddr>,
    pub dial_timeout_ms: u64,
}

/// One HTTP GET with NO redirect following (the fetcher follows manually
/// so every hop is re-guarded). `connect_delay_ms` is the
/// simulated/measured dial+handshake time; each chunk carries the
/// simulated/measured ms elapsed before it arrived.
pub trait IndexerTransport {
    fn get(&mut self, request: &PinnedRequest) -> Result<RawResponse, FetchRejection>;
}

/// Single-hop transport answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawResponse {
    pub status: u16,
    pub location: Option<String>,
    pub connect_delay_ms: u64,
    pub chunks: Vec<BodyChunk>,
}

/// One body chunk with the ms elapsed before it arrived.
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

/// Split `https://authority/rest` into the authority. `None` when the URL
/// has no `https`-valid shape at the scheme level; the caller maps that to
/// [`FetchRejection::BadScheme`] (wrong scheme) or
/// [`FetchRejection::MalformedUrl`] (no scheme at all).
fn split_authority(url: &str) -> Result<Option<&str>, FetchRejection> {
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
    Ok(Some(authority))
}

/// Split an authority into (host, port): brackets for v6 literals, one
/// optional `:port` otherwise. Any other explicit port is
/// [`FetchRejection::PortNotAllowed`]; an unparseable port is
/// [`FetchRejection::MalformedUrl`].
fn split_host_port(authority: &str) -> Result<(String, u16), FetchRejection> {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let Some(bracket_end) = bracketed.find(']') else {
            return Err(FetchRejection::MalformedUrl);
        };
        let host = &bracketed[..bracket_end];
        let rest = &bracketed[bracket_end + 1..];
        if host.is_empty() {
            return Err(FetchRejection::MalformedUrl);
        }
        let port = if rest.is_empty() {
            ALLOWED_INDEXER_PORT
        } else if let Some(digits) = rest.strip_prefix(':') {
            parse_port(digits)?
        } else {
            return Err(FetchRejection::MalformedUrl);
        };
        return check_port(host, port);
    }
    if authority.chars().filter(|&c| c == ':').count() > 1 {
        // Unbracketed colons can only be a bare IPv6 literal, which the
        // authority grammar forbids (RFC 3986: literals need brackets).
        return Err(FetchRejection::MalformedUrl);
    }
    match authority.split_once(':') {
        None => check_port(authority, ALLOWED_INDEXER_PORT),
        Some((host, digits)) => {
            if host.is_empty() {
                return Err(FetchRejection::MalformedUrl);
            }
            check_port(host, parse_port(digits)?)
        }
    }
}

fn parse_port(digits: &str) -> Result<u16, FetchRejection> {
    if digits.is_empty() {
        return Err(FetchRejection::MalformedUrl);
    }
    digits.parse::<u16>().map_err(|_| {
        // A numeric-but-impossible port (65536+) is still "not 443", so it
        // stays a port block; non-numeric junk is malformed.
        if digits.bytes().all(|b| b.is_ascii_digit()) {
            FetchRejection::PortNotAllowed
        } else {
            FetchRejection::MalformedUrl
        }
    })
}

fn check_port(host: &str, port: u16) -> Result<(String, u16), FetchRejection> {
    if port != ALLOWED_INDEXER_PORT {
        return Err(FetchRejection::PortNotAllowed);
    }
    Ok((host.to_string(), port))
}

/// Pre-fetch guard only (no I/O beyond the resolver): https only, no
/// embedded credentials, explicit port must be 443 (default 443), and
/// every resolved address public. IP-literal hosts are checked directly,
/// with no DNS call. The hostname is lowercased and one trailing dot
/// stripped before resolution, so `Indexer.Example.` resolves exactly the
/// vetted name.
pub fn validate_indexer_url(
    url: &str,
    _config: &FetchConfig,
    dns: &dyn DnsResolver,
) -> Result<ValidatedTarget, FetchRejection> {
    let authority = split_authority(url)?.unwrap_or_default();
    if url.chars().any(|c| c.is_whitespace()) {
        return Err(FetchRejection::MalformedUrl);
    }
    if authority.contains('@') {
        return Err(FetchRejection::EmbeddedCredentials);
    }
    let (raw_host, port) = split_host_port(authority)?;
    let host = raw_host.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    if host.is_empty() {
        return Err(FetchRejection::MalformedUrl);
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
        // Resolve-ALL: one forbidden record poisons the whole answer.
        return Err(FetchRejection::ForbiddenAddress);
    }
    Ok(ValidatedTarget {
        url: url.to_string(),
        host: host.to_string(),
        port,
        addresses,
    })
}

// --- Redirects ---------------------------------------------------------------

/// Resolve a redirect `Location` against the current hop from scratch:
/// absolute URLs as given, protocol-relative (`//host/path`) on the same
/// https scheme, path-absolute (`/path`) on the current host. Anything
/// else is malformed — never silently joined. The result is re-validated
/// by the caller; the original URL's `apikey=` is never forwarded.
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
    if location.contains("://") {
        return Ok(location.to_string());
    }
    if let Some(_path) = location.strip_prefix('/') {
        return Ok(format!("https://{current_host}{location}"));
    }
    Err(FetchRejection::MalformedUrl)
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Guarded fetch for one Newznab search URL (typically built by
/// `newznab::build_search_url` + `inject_api_key`):
/// - validate before ANY transport call, resolving each hop's host exactly
///   once and dialing only that vetted set (a second, different DNS answer
///   is never consulted — no rebinding window);
/// - a dial reaching `config.dial_timeout_ms` aborts with DialTimeout (at
///   the deadline counts as expired, the vault `TestConnectionBudget`
///   semantics), and connect + chunk delays reaching `config.timeout_ms`
///   abort with Timeout; partial bytes are never returned;
/// - on 301/302/303/307/308 re-validate the Location from scratch and
///   follow at most `config.max_redirects` hops; a redirect status without
///   a Location is UpstreamStatus(status), as is any other non-2xx;
/// - the api key is never forwarded to a redirect target: hops are fetched
///   at the Location as given, without re-injecting the original URL's
///   `apikey=` value onto a new host.
pub fn fetch_indexer(
    url: &str,
    config: &FetchConfig,
    dns: &dyn DnsResolver,
    transport: &mut dyn IndexerTransport,
) -> Result<Vec<u8>, FetchRejection> {
    let mut current = url.to_string();
    let mut followed: u32 = 0;
    loop {
        let target = validate_indexer_url(&current, config, dns)?;
        let request = PinnedRequest {
            url: current.clone(),
            host: target.host.clone(),
            sni_host: target.host.clone(),
            port: target.port,
            addresses: target.addresses.clone(),
            dial_timeout_ms: config.dial_timeout_ms,
        };
        let response = transport.get(&request)?;
        if response.connect_delay_ms >= config.dial_timeout_ms {
            return Err(FetchRejection::DialTimeout);
        }
        let mut elapsed = response.connect_delay_ms;
        if elapsed >= config.timeout_ms {
            return Err(FetchRejection::Timeout);
        }
        match response.status {
            200..=299 => {
                let mut body = Vec::new();
                for chunk in &response.chunks {
                    elapsed = elapsed.saturating_add(chunk.delay_ms);
                    if elapsed >= config.timeout_ms {
                        return Err(FetchRejection::Timeout);
                    }
                    body.extend_from_slice(&chunk.bytes);
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
/// URL's host and the rejection code, NEVER the full URL (it embeds the
/// user's indexer api key). Unparseable input yields the `invalid`
/// placeholder, which contains no part of the input.
pub fn url_host_for_log(url: &str) -> String {
    const PLACEHOLDER: &str = "invalid";
    parse_log_host(url).unwrap_or_else(|| PLACEHOLDER.to_string())
}

fn parse_log_host(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    // Strip credentials (everything through the last '@'); the password
    // and any userinfo never reach the log.
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
    fn numeric_overflow_port_is_a_port_block() {
        let cfg = FetchConfig::default();
        assert_eq!(
            validate_indexer_url("https://indexer.example:65536/api", &cfg, &NoDns),
            Err(FetchRejection::PortNotAllowed)
        );
    }
}
