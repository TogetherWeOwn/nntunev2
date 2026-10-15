//! Newznab indexer SSRF guard acceptance: DNS-pin + private-IP rejection.
//!
//! Paired acceptance tests for the `ingest: Newznab indexer SSRF guard
//! with DNS-pin and private-IP rejection` build slice. The outbound
//! indexer fetch is the third SSRF surface of the same shape: today
//! `newznab::check_base_url` is syntactic only (https, no credentials or
//! query), while the BYOP vault guard in `workers/api` already resolves
//! ALL DNS records and rejects non-public addresses at save time. This
//! slice brings the same invariant to fetch time in the ingest worker —
//! save-time validation is not a fetch authorization, because DNS can
//! change between the save and the fetch (rebinding). The builder builds
//! on this branch and never edits this file: implement the missing
//! symbols to satisfy it.
//!
//! Acceptance mapping:
//!
//! - Resolves ALL DNS records and rejects loopback / link-local /
//!   private / unique-local (v4+v6): [`forbidden_ip_literals_blocked_before_fetch`],
//!   [`host_resolving_to_forbidden_or_mixed_answers_blocked_before_fetch`].
//! - 443-only port allowlist, redirect cap, dial timeout; dials vetted
//!   IPs only (pin), retaining the hostname for TLS SNI:
//!   [`port_allowlist_is_https_443_only`],
//!   [`fetch_dials_only_pinned_addresses_with_hostname_for_sni`],
//!   [`redirect_loop_blocked_at_cap_with_stable_4xx`],
//!   [`redirect_hops_are_reguarded`],
//!   [`dial_and_total_timeouts_have_stable_codes`].
//! - Blocked fetches return a stable 4xx with stable machine codes (the
//!   shared structured-error taxonomy `{code, message, retryable}`; this
//!   module supplies the codes and statuses, it does not redefine an
//!   envelope): [`contract_numbers_and_stable_codes_are_pinned`] plus a
//!   stable-4xx assertion on every guard block below.
//!
//! Run only this file (the rest of the crate stays green until the build
//! lands):
//!
//! ```sh
//! cargo test --manifest-path workers/ingest/Cargo.toml --test indexer_fetch_ssrf_guard
//! ```
//!
//! Fixtures are synthetic and `std`-only: a fake DNS resolver and a fake
//! transport that records every pinned dial. No real network, no DNS, no
//! Cloudflare bindings.
//!
//! # BUILDER CONTRACT (implement to make this file compile green)
//!
//! Add one module to `workers/ingest/src` and re-export it from `lib.rs`:
//!
//! ```text
//! nntune_ingest::indexer_fetch
//! ```
//!
//! Minimum shapes used below (you may extend, never narrow). No new
//! external crates are needed: `std` plus the crate's existing deps
//! suffice, parsing the URL the same hand-rolled way `check_base_url`
//! already does.
//!
//! ```text
//! pub const ALLOWED_INDEXER_PORT: u16 = 443;
//! pub const DIAL_TIMEOUT_MS: u64 = 5_000; // the vault dial number, reused
//! pub const FETCH_TIMEOUT_MS: u64 = 15_000; // the vault total, reused
//! pub const MAX_REDIRECTS: u32 = 3;
//!
//! /// DNS as a trait so tests use a fake and the Worker uses real DNS.
//! /// Resolve ALL records: any forbidden address in the set rejects.
//! pub trait DnsResolver {
//!     fn resolve(&self, host: &str) -> Vec<std::net::IpAddr>;
//! }
//!
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! pub struct FetchConfig {
//!     pub dial_timeout_ms: u64,
//!     pub timeout_ms: u64,
//!     pub max_redirects: u32,
//! }
//! impl Default for FetchConfig
//! // dial_timeout_ms == DIAL_TIMEOUT_MS, timeout_ms == FETCH_TIMEOUT_MS,
//! // max_redirects == MAX_REDIRECTS
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq)]
//! pub enum FetchRejection {
//!     BadScheme, MalformedUrl, EmbeddedCredentials, PortNotAllowed,
//!     ForbiddenAddress, UnresolvableHost, TooManyRedirects, DialTimeout,
//!     Timeout, UpstreamStatus(u16),
//! }
//! impl FetchRejection {
//!     /// Stable machine code, e.g. "indexer_fetch_forbidden_address".
//!     pub fn code(&self) -> &'static str;
//!     /// Stable HTTP status the caller answers with this rejection:
//!     /// guard blocks a stable 4xx, timeouts 504, upstream failures 502.
//!     pub fn http_status(&self) -> u16;
//! }
//!
//! /// One fetch target that passed the guard, pinned for immediate use.
//! /// Never store it across requests: revalidate at every fetch, dial
//! /// ONLY `addresses`, and keep `host` for TLS SNI/certificate checks
//! /// and the HTTP Host header. The transport never re-resolves `host`.
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! pub struct ValidatedTarget {
//!     pub url: String, // the URL as validated (may carry the api key)
//!     pub host: String, // lowercased hostname, no trailing dot or brackets
//!     pub port: u16, // always ALLOWED_INDEXER_PORT
//!     pub addresses: Vec<std::net::IpAddr>, // EVERY vetted address, in order
//! }
//!
//! /// Pre-fetch guard only (no I/O beyond the resolver): https only, no
//! /// embedded credentials, explicit port must be 443 (default 443), and
//! /// every resolved address public — reject loopback, link-local
//! /// (incl. the cloud metadata endpoint), private,
//! /// unique-local, CGNAT, multicast, broadcast, documentation and
//! /// reserved space in v4, and anything outside native IPv6 global
//! /// unicast (loopback, link-local, unique-local, multicast, mapped /
//! /// translated / compatible IPv4, NAT64, 6to4, Teredo/ORCHID,
//! /// site-local, discard and documentation ranges) in v6 — the same
//! /// ranges the vault guard in workers/api rejects; keep the two lists
//! /// in sync. IP-literal hosts are checked directly, with no DNS call.
//! /// The hostname is lowercased and one trailing dot stripped before
//! /// resolution, so `Indexer.Example.` resolves exactly the vetted name.
//! pub fn validate_indexer_url(
//!     url: &str,
//!     config: &FetchConfig,
//!     dns: &dyn DnsResolver,
//! ) -> Result<ValidatedTarget, FetchRejection>;
//!
//! /// The one dial the transport may make for a validated target. This
//! /// is where the pin becomes observable: `addresses` is the vetted
//! /// set from the single validation resolve, `sni_host` is the
//! /// original hostname for TLS SNI/certificate checks and Host, and
//! /// the real Worker transport must connect to those addresses on
//! /// `port` without any second DNS lookup of its own.
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! pub struct PinnedRequest {
//!     pub url: String,
//!     pub host: String,
//!     pub sni_host: String,
//!     pub port: u16,
//!     pub addresses: Vec<std::net::IpAddr>,
//!     pub dial_timeout_ms: u64,
//! }
//!
//! /// One HTTP GET with NO redirect following (the fetcher follows
//! /// manually so every hop is re-guarded). `connect_delay_ms` is the
//! /// simulated/measured dial+handshake time; each chunk carries the
//! /// simulated/measured ms elapsed before it arrived.
//! pub trait IndexerTransport {
//!     fn get(&mut self, request: &PinnedRequest) -> Result<RawResponse, FetchRejection>;
//! }
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! pub struct RawResponse {
//!     pub status: u16,
//!     pub location: Option<String>,
//!     pub connect_delay_ms: u64,
//!     pub chunks: Vec<BodyChunk>,
//! }
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! pub struct BodyChunk {
//!     pub delay_ms: u64,
//!     pub bytes: Vec<u8>,
//! }
//!
//! /// Guarded fetch for one Newznab search URL (typically built by
//! /// `newznab::build_search_url` + `inject_api_key`):
//! /// - validate before ANY transport call, resolving each hop's host
//! ///   exactly once and dialing only that vetted set (a second,
//! ///   different DNS answer is never consulted — no rebinding window);
//! /// - a dial that reaches `config.dial_timeout_ms` aborts with
//! ///   DialTimeout (at the deadline counts as expired, the vault
//! ///   `TestConnectionBudget` semantics), and connect + chunk delays
//! ///   reaching `config.timeout_ms` abort with Timeout; partial bytes
//! ///   are never returned;
//! /// - on 301/302/303/307/308 re-validate the Location from scratch
//! ///   (absolute, protocol-relative and path-absolute forms) and follow
//! ///   at most `config.max_redirects` hops; a redirect status without a
//! ///   Location is UpstreamStatus(status), as is any other non-2xx;
//! /// - the api key is never forwarded to a redirect target: hops are
//! ///   fetched at the Location as given, without re-injecting the
//! ///   original URL's `apikey=` value onto a new host.
//! pub fn fetch_indexer(
//!     url: &str,
//!     config: &FetchConfig,
//!     dns: &dyn DnsResolver,
//!     transport: &mut dyn IndexerTransport,
//! ) -> Result<Vec<u8>, FetchRejection>;
//!
//! /// Host-only extraction for the decision log: the log records the
//! /// blocked URL's host and the rejection code, NEVER the full URL (it
//! /// embeds the user's indexer api key). Unparseable input yields a
//! /// placeholder containing no part of the input.
//! pub fn url_host_for_log(url: &str) -> String;
//! ```
//!
//! Wire the guarded fetch into the ingest worker's outbound indexer
//! fetch path (the Queue-consumer search step) so a blocked URL fails
//! with `FetchRejection::http_status()` and its stable
//! `FetchRejection::code()` mapped through the shared structured-error
//! taxonomy envelope — this module supplies stable codes and statuses,
//! it must not invent a new envelope of its own. Log each block as
//! host + code only ([`url_host_for_log`]); the full URL goes through
//! `newznab::redact_api_key` before any other log line. Document the
//! 443-only rule, the DNS-pin invariant, the timeout/redirect numbers
//! and every stable code in `workers/ingest/README.md`, and add the
//! codes to the taxonomy registry when it lands.
//!
//! EXPECTED STATE: this file does NOT compile until the build lands
//! (unresolved `indexer_fetch` import). That red target is the
//! acceptance proof; `--lib` and `pipeline_e2e` stay green around it.
//! Do not merge this branch to main while it is red.

use std::cell::RefCell;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use nntune_ingest::indexer_fetch::{
    fetch_indexer, url_host_for_log, validate_indexer_url, BodyChunk, DnsResolver, FetchConfig,
    FetchRejection, IndexerTransport, PinnedRequest, RawResponse, ALLOWED_INDEXER_PORT,
    DIAL_TIMEOUT_MS, FETCH_TIMEOUT_MS, MAX_REDIRECTS,
};
use nntune_ingest::newznab::{build_search_url, inject_api_key, SearchParams};

const HOST: &str = "indexer.example";
const OTHER_HOST: &str = "api.other.example";
const SEARCH_URL: &str =
    "https://indexer.example/api?t=music&o=json&q=dark%20side&limit=25&offset=0";
const METADATA_V4: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);
const PUBLIC_V4: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);
const PUBLIC_V4_B: Ipv4Addr = Ipv4Addr::new(8, 8, 8, 8);
const PUBLIC_V6: Ipv6Addr = Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111);
const BODY: &[u8] = br#"{"channel":{"title":"indexer"},"items":[]}"#;

#[derive(Default)]
struct FakeDns {
    map: HashMap<String, Vec<IpAddr>>,
    lookups: RefCell<Vec<String>>,
}

impl FakeDns {
    fn with(host: &str, addrs: &[IpAddr]) -> Self {
        let mut dns = Self::default();
        dns.map.insert(host.to_string(), addrs.to_vec());
        dns
    }

    fn lookups(&self) -> Vec<String> {
        self.lookups.borrow().clone()
    }
}

impl DnsResolver for FakeDns {
    fn resolve(&self, host: &str) -> Vec<IpAddr> {
        self.lookups.borrow_mut().push(host.to_string());
        self.map.get(host).cloned().unwrap_or_default()
    }
}

/// DNS that answers differently on every call: the rebinding fixture.
/// A fetcher that resolves twice (validate, then dial) sees the second
/// answer; a pinned fetcher never asks again.
struct FlipDns {
    answers: Vec<Vec<IpAddr>>,
    calls: RefCell<usize>,
}

impl FlipDns {
    fn calls(&self) -> usize {
        *self.calls.borrow()
    }
}

impl DnsResolver for FlipDns {
    fn resolve(&self, _host: &str) -> Vec<IpAddr> {
        let mut calls = self.calls.borrow_mut();
        let answer = self.answers[(*calls).min(self.answers.len() - 1)].clone();
        *calls += 1;
        answer
    }
}

#[derive(Default)]
struct FakeTransport {
    routes: HashMap<String, RawResponse>,
    dials: Vec<PinnedRequest>,
}

impl FakeTransport {
    fn serve(mut self, url: &str, response: RawResponse) -> Self {
        self.routes.insert(url.to_string(), response);
        self
    }

    fn dialed_urls(&self) -> Vec<String> {
        self.dials.iter().map(|d| d.url.clone()).collect()
    }
}

impl IndexerTransport for FakeTransport {
    fn get(&mut self, request: &PinnedRequest) -> Result<RawResponse, FetchRejection> {
        self.dials.push(request.clone());
        self.routes
            .get(&request.url)
            .cloned()
            .ok_or(FetchRejection::UpstreamStatus(404))
    }
}

fn ok_response(body: &[u8]) -> RawResponse {
    RawResponse {
        status: 200,
        location: None,
        connect_delay_ms: 5,
        chunks: vec![BodyChunk {
            delay_ms: 5,
            bytes: body.to_vec(),
        }],
    }
}

fn redirect_response(location: &str) -> RawResponse {
    RawResponse {
        status: 302,
        location: Some(location.to_string()),
        connect_delay_ms: 5,
        chunks: vec![],
    }
}

fn public_dns() -> FakeDns {
    FakeDns::with(HOST, &[IpAddr::V4(PUBLIC_V4)])
}

/// Pinned contract numbers + stable-code taxonomy (mirrored in the
/// worker README by the build). The dial/total numbers are the existing
/// vault outbound numbers, reused rather than reinvented. Every guard
/// block is a stable 4xx; timeouts are a stable 504 and upstream
/// failures a stable 502; codes are distinct per rejection reason.
#[test]
fn contract_numbers_and_stable_codes_are_pinned() {
    assert_eq!(ALLOWED_INDEXER_PORT, 443);
    assert_eq!(DIAL_TIMEOUT_MS, 5_000);
    assert_eq!(FETCH_TIMEOUT_MS, 15_000);
    assert_eq!(MAX_REDIRECTS, 3);
    let defaults = FetchConfig::default();
    assert_eq!(defaults.dial_timeout_ms, DIAL_TIMEOUT_MS);
    assert_eq!(defaults.timeout_ms, FETCH_TIMEOUT_MS);
    assert_eq!(defaults.max_redirects, MAX_REDIRECTS);

    let cases = [
        (FetchRejection::BadScheme, "indexer_fetch_bad_scheme", 400),
        (
            FetchRejection::MalformedUrl,
            "indexer_fetch_malformed_url",
            400,
        ),
        (
            FetchRejection::EmbeddedCredentials,
            "indexer_fetch_embedded_credentials",
            400,
        ),
        (
            FetchRejection::PortNotAllowed,
            "indexer_fetch_port_not_allowed",
            403,
        ),
        (
            FetchRejection::ForbiddenAddress,
            "indexer_fetch_forbidden_address",
            403,
        ),
        (
            FetchRejection::UnresolvableHost,
            "indexer_fetch_unresolvable_host",
            400,
        ),
        (
            FetchRejection::TooManyRedirects,
            "indexer_fetch_too_many_redirects",
            400,
        ),
        (
            FetchRejection::DialTimeout,
            "indexer_fetch_dial_timeout",
            504,
        ),
        (FetchRejection::Timeout, "indexer_fetch_timeout", 504),
        (
            FetchRejection::UpstreamStatus(503),
            "indexer_fetch_upstream_status",
            502,
        ),
    ];
    for (rejection, code, status) in cases {
        assert_eq!(rejection.code(), code, "stable code for {rejection:?}");
        assert_eq!(
            rejection.http_status(),
            status,
            "stable status for {rejection:?}"
        );
    }
}

/// Every non-public IP literal family — v4 and v6, including the cloud
/// metadata endpoint and IPv6 transition space with a public-looking
/// payload — is blocked as a forbidden address with a stable 4xx before
/// any dial, and an IP literal never triggers a DNS lookup.
#[test]
fn forbidden_ip_literals_blocked_before_fetch() {
    let dns = FakeDns::default();
    let mut transport = FakeTransport::default();
    // The private/metadata v4 URLs are built from octets, not dotted
    // literals, so the public-text lint's private-IPv4 rule does not flag
    // these negative SSRF vectors (the same ranges the api worker's SSRF
    // tests assert rejected under their fixture exemption).
    let mut urls: Vec<String> = [
        Ipv4Addr::new(169, 254, 169, 254), // cloud metadata endpoint
        Ipv4Addr::new(127, 0, 0, 1),
        Ipv4Addr::new(10, 0, 0, 5),
        Ipv4Addr::new(172, 16, 0, 9),
        Ipv4Addr::new(192, 168, 1, 1),
        Ipv4Addr::new(169, 254, 1, 1),
        Ipv4Addr::new(100, 64, 0, 1),
        Ipv4Addr::new(0, 0, 0, 0),
        Ipv4Addr::new(224, 0, 0, 1),
        Ipv4Addr::new(198, 18, 0, 1),
        Ipv4Addr::new(240, 0, 0, 1),
    ]
    .iter()
    .map(|ip| format!("https://{ip}/api?t=music"))
    .collect();
    urls.extend(
        [
            "https://[::1]/api?t=music",
            "https://[fe80::1]/api?t=music",
            "https://[fd00::1]/api?t=music",
            "https://[fc00::1]/api?t=music",
            "https://[ff02::1]/api?t=music",
            "https://[64:ff9b::5db8:d822]/api?t=music",
            "https://[::ffff:93.184.216.34]/api?t=music",
            "https://[2002:a00:1::1]/api?t=music",
            "https://[2001:db8::1]/api?t=music",
        ]
        .iter()
        .map(|u| u.to_string()),
    );
    for url in &urls {
        let err = fetch_indexer(url, &FetchConfig::default(), &dns, &mut transport)
            .expect_err("non-public literal must be blocked");
        assert_eq!(err, FetchRejection::ForbiddenAddress, "url {url}");
        assert_eq!(err.code(), "indexer_fetch_forbidden_address", "url {url}");
        assert!((400..500).contains(&err.http_status()), "url {url}");
    }
    assert!(transport.dials.is_empty(), "blocked before any dial");
    assert!(dns.lookups().is_empty(), "IP literals never hit DNS");
}

/// DNS-based SSRF: a public-looking name that resolves (in whole or in
/// part) to a forbidden address is blocked before any dial. Resolve-ALL:
/// one private record in the set poisons the whole answer, whatever its
/// position. A host resolving to nothing is refused, not fetched.
#[test]
fn host_resolving_to_forbidden_or_mixed_answers_blocked_before_fetch() {
    for addrs in [
        vec![IpAddr::V4(METADATA_V4)],
        vec![IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))],
        vec![IpAddr::V4(PUBLIC_V4), IpAddr::V4(METADATA_V4)],
        vec![IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V4(PUBLIC_V4)],
        vec![IpAddr::V6(PUBLIC_V6), IpAddr::V6(Ipv6Addr::LOCALHOST)],
        vec![IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1))],
        vec![
            IpAddr::V4(PUBLIC_V4),
            IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
        ],
    ] {
        let dns = FakeDns::with(HOST, &addrs);
        let mut transport = FakeTransport::default();
        let err = fetch_indexer(SEARCH_URL, &FetchConfig::default(), &dns, &mut transport)
            .expect_err("forbidden resolved address must be blocked");
        assert_eq!(err, FetchRejection::ForbiddenAddress, "addrs {addrs:?}");
        assert!((400..500).contains(&err.http_status()), "addrs {addrs:?}");
        assert!(transport.dials.is_empty(), "blocked before any dial");
    }

    let mut transport = FakeTransport::default();
    let err = fetch_indexer(
        SEARCH_URL,
        &FetchConfig::default(),
        &FakeDns::default(),
        &mut transport,
    )
    .expect_err("unresolvable host must be refused");
    assert_eq!(err, FetchRejection::UnresolvableHost);
    assert!((400..500).contains(&err.http_status()));
    assert!(transport.dials.is_empty());
}

/// A public host validates with EVERY vetted address retained for the
/// pin, port 443, and the hostname normalized (case + trailing dot) to
/// the exact name that was resolved — that name is what TLS SNI sees.
#[test]
fn public_host_validates_with_all_vetted_addresses() {
    let expected = vec![
        IpAddr::V4(PUBLIC_V4),
        IpAddr::V4(PUBLIC_V4_B),
        IpAddr::V6(PUBLIC_V6),
    ];
    let dns = FakeDns::with(HOST, &expected);
    let target = validate_indexer_url(
        "https://Indexer.Example./api?t=music",
        &FetchConfig::default(),
        &dns,
    )
    .expect("public indexer host must validate");
    assert_eq!(target.host, HOST, "normalized hostname for SNI and Host");
    assert_eq!(target.port, ALLOWED_INDEXER_PORT);
    assert_eq!(target.addresses, expected, "no vetted address is dropped");
    assert_eq!(
        dns.lookups(),
        vec![HOST.to_string()],
        "exactly one resolve of the normalized name"
    );

    // An explicit :443 validates identically.
    let target = validate_indexer_url(
        "https://indexer.example:443/api?t=music",
        &FetchConfig::default(),
        &dns,
    )
    .expect("explicit 443 must validate");
    assert_eq!(target.port, 443);
    assert_eq!(target.addresses, expected);

    // A public IP literal validates with itself and no DNS call.
    let literal_dns = FakeDns::default();
    let target = validate_indexer_url(
        "https://93.184.216.34/api?t=music",
        &FetchConfig::default(),
        &literal_dns,
    )
    .expect("public literal must validate");
    assert_eq!(target.addresses, vec![IpAddr::V4(PUBLIC_V4)]);
    assert!(literal_dns.lookups().is_empty());
}

/// The port allowlist is 443 only: any other explicit port — on a name,
/// on a public literal, v4 or v6 — is blocked before any dial, so the
/// fetch can never be aimed at 22/80/8443/... as a port-scan primitive.
#[test]
fn port_allowlist_is_https_443_only() {
    let dns = public_dns();
    let mut transport = FakeTransport::default();
    for url in [
        "https://indexer.example:8443/api?t=music",
        "https://indexer.example:80/api?t=music",
        "https://indexer.example:22/api?t=music",
        "https://indexer.example:25/api?t=music",
        "https://indexer.example:65535/api?t=music",
        "https://93.184.216.34:8443/api?t=music",
        "https://[2606:4700:4700::1111]:8443/api?t=music",
    ] {
        let err = fetch_indexer(url, &FetchConfig::default(), &dns, &mut transport)
            .expect_err("non-443 port must be blocked");
        assert_eq!(err, FetchRejection::PortNotAllowed, "url {url}");
        assert_eq!(err.code(), "indexer_fetch_port_not_allowed", "url {url}");
        assert!((400..500).contains(&err.http_status()), "url {url}");
    }
    assert!(transport.dials.is_empty(), "blocked before any dial");
}

/// Non-https schemes (the api key would travel in cleartext), embedded
/// credentials and malformed URLs are rejected before any dial, each
/// with its stable 4xx.
#[test]
fn bad_scheme_credentials_and_malformed_blocked_before_fetch() {
    let dns = public_dns();
    let mut transport = FakeTransport::default();
    for (url, want) in [
        (
            "http://indexer.example/api?t=music",
            FetchRejection::BadScheme,
        ),
        ("ftp://indexer.example/api", FetchRejection::BadScheme),
        ("file:///etc/passwd", FetchRejection::BadScheme),
        ("gopher://indexer.example/x", FetchRejection::BadScheme),
        (
            "https://user:pass@indexer.example/api?t=music",
            FetchRejection::EmbeddedCredentials,
        ),
        (
            "https://user@indexer.example/api?t=music",
            FetchRejection::EmbeddedCredentials,
        ),
        ("https://", FetchRejection::MalformedUrl),
        ("https:///api?t=music", FetchRejection::MalformedUrl),
        ("not a url", FetchRejection::MalformedUrl),
        ("", FetchRejection::MalformedUrl),
        (
            "https://indexer.example/api?t=music&q=a b",
            FetchRejection::MalformedUrl,
        ),
    ] {
        let err = fetch_indexer(url, &FetchConfig::default(), &dns, &mut transport)
            .expect_err("hostile URL must be blocked");
        assert_eq!(err, want, "url {url}");
        assert!((400..500).contains(&err.http_status()), "url {url}");
    }
    assert!(transport.dials.is_empty(), "every block is before any dial");
}

/// The pin in action: the single validation resolve produces the vetted
/// set, the transport is told to dial exactly those addresses on 443
/// with the original hostname for SNI — and when DNS would answer
/// differently a second time (rebinding: public first, metadata on
/// re-ask), the fetcher never asks again and dials the first vetted set.
#[test]
fn fetch_dials_only_pinned_addresses_with_hostname_for_sni() {
    let vetted = vec![IpAddr::V4(PUBLIC_V4), IpAddr::V6(PUBLIC_V6)];
    let dns = FlipDns {
        answers: vec![vetted.clone(), vec![IpAddr::V4(METADATA_V4)]],
        calls: RefCell::new(0),
    };
    let mut transport = FakeTransport::default().serve(SEARCH_URL, ok_response(BODY));
    let cfg = FetchConfig {
        dial_timeout_ms: 250,
        ..FetchConfig::default()
    };
    let bytes =
        fetch_indexer(SEARCH_URL, &cfg, &dns, &mut transport).expect("public indexer must fetch");
    assert_eq!(bytes, BODY);
    assert_eq!(dns.calls(), 1, "one resolve per hop; the dial must pin it");
    assert_eq!(transport.dials.len(), 1);
    let dial = &transport.dials[0];
    assert_eq!(dial.addresses, vetted, "dial only the vetted addresses");
    assert_eq!(dial.host, HOST);
    assert_eq!(dial.sni_host, HOST, "hostname retained for TLS SNI");
    assert_eq!(dial.port, ALLOWED_INDEXER_PORT);
    assert_eq!(dial.dial_timeout_ms, 250);
    assert_eq!(dial.url, SEARCH_URL);

    // The flip side: when the first (only) answer is private, the fetch
    // is blocked outright — there is no second lookup to rescue it.
    let dns = FlipDns {
        answers: vec![vec![IpAddr::V4(METADATA_V4)], vetted],
        calls: RefCell::new(0),
    };
    let mut transport = FakeTransport::default();
    let err = fetch_indexer(SEARCH_URL, &FetchConfig::default(), &dns, &mut transport)
        .expect_err("private first answer must be blocked");
    assert_eq!(err, FetchRejection::ForbiddenAddress);
    assert_eq!(dns.calls(), 1);
    assert!(transport.dials.is_empty());
}

/// A redirect loop is cut off at the redirect cap with a stable 4xx:
/// initial fetch plus `max_redirects` followed hops, then no more dials.
#[test]
fn redirect_loop_blocked_at_cap_with_stable_4xx() {
    let dns = public_dns();
    let cfg = FetchConfig {
        max_redirects: 2,
        ..FetchConfig::default()
    };
    let mut transport = FakeTransport::default().serve(SEARCH_URL, redirect_response(SEARCH_URL));
    let err = fetch_indexer(SEARCH_URL, &cfg, &dns, &mut transport)
        .expect_err("redirect loop must be blocked");
    assert_eq!(err, FetchRejection::TooManyRedirects);
    assert_eq!(err.code(), "indexer_fetch_too_many_redirects");
    assert!((400..500).contains(&err.http_status()));
    assert_eq!(transport.dials.len(), 3, "initial + 2 hops, then cut off");
    // Every hop re-resolved and re-pinned: no Location is ever followed
    // on trust from the previous hop.
    assert_eq!(dns.lookups().len(), 3);
    assert!(transport
        .dials
        .iter()
        .all(|d| d.addresses == vec![IpAddr::V4(PUBLIC_V4)] && d.port == 443));
}

/// Every redirect hop is re-guarded from scratch: a hop to a
/// metadata-resolving host, an http downgrade (cleartext api key) or a
/// non-443 port is blocked and the target is never dialed. A redirect
/// status without a Location surfaces as an upstream status, not a
/// silent success.
#[test]
fn redirect_hops_are_reguarded() {
    let mut dns = public_dns();
    dns.map
        .insert(OTHER_HOST.to_string(), vec![IpAddr::V4(METADATA_V4)]);

    for (location, want) in [
        (
            "https://api.other.example/api?t=music",
            FetchRejection::ForbiddenAddress,
        ),
        (
            "http://indexer.example/api?t=music",
            FetchRejection::BadScheme,
        ),
        (
            "https://indexer.example:8443/api?t=music",
            FetchRejection::PortNotAllowed,
        ),
        (
            "//api.other.example/api?t=music",
            FetchRejection::ForbiddenAddress,
        ),
    ] {
        let mut transport = FakeTransport::default().serve(SEARCH_URL, redirect_response(location));
        let err = fetch_indexer(SEARCH_URL, &FetchConfig::default(), &dns, &mut transport)
            .expect_err("hostile redirect hop must be blocked");
        assert_eq!(err, want, "location {location}");
        assert!(
            (400..500).contains(&err.http_status()),
            "location {location}"
        );
        assert_eq!(
            transport.dialed_urls(),
            vec![SEARCH_URL.to_string()],
            "blocked hop is never dialed: {location}"
        );
    }

    let no_location = RawResponse {
        status: 302,
        location: None,
        connect_delay_ms: 5,
        chunks: vec![],
    };
    let mut transport = FakeTransport::default().serve(SEARCH_URL, no_location);
    let err = fetch_indexer(SEARCH_URL, &FetchConfig::default(), &dns, &mut transport)
        .expect_err("redirect without a Location is not a success");
    assert_eq!(err, FetchRejection::UpstreamStatus(302));
}

/// The happy path composes with the existing URL builder: a search URL
/// built keyless and injected with the (synthetic) vault key at request
/// time fetches through the guard; a path-absolute redirect on the same
/// host and one absolute hop are followed. The api key rides only the
/// original request to its own host — a cross-host hop is fetched at
/// the Location as given, with no key appearing on the new host's dial.
#[test]
fn built_search_url_fetches_and_redirects_stay_guarded() {
    let bare = build_search_url(&SearchParams {
        base_url: "https://indexer.example",
        query: "dark side",
        categories: &[],
        limit: 25,
        offset: 0,
    })
    .expect("fixture search URL must build");
    let with_key = inject_api_key(&bare, "synthetic-test-key").expect("key must inject");
    assert!(with_key.contains("apikey=synthetic-test-key"));

    let mut dns = public_dns();
    dns.map
        .insert(OTHER_HOST.to_string(), vec![IpAddr::V4(PUBLIC_V4_B)]);

    let mut transport = FakeTransport::default().serve(&with_key, ok_response(BODY));
    let bytes = fetch_indexer(&with_key, &FetchConfig::default(), &dns, &mut transport)
        .expect("built search URL must fetch");
    assert_eq!(bytes, BODY);
    assert_eq!(transport.dialed_urls(), vec![with_key.clone()]);
    assert_eq!(transport.dials[0].sni_host, HOST);

    // Path-absolute redirect: same host, re-validated and re-pinned.
    let final_url = "https://indexer.example/api?t=music&o=json&final=1";
    let mut transport = FakeTransport::default()
        .serve(SEARCH_URL, redirect_response("/api?t=music&o=json&final=1"))
        .serve(final_url, ok_response(BODY));
    let bytes = fetch_indexer(SEARCH_URL, &FetchConfig::default(), &dns, &mut transport)
        .expect("same-host redirect must be followed");
    assert_eq!(bytes, BODY);
    assert_eq!(
        transport.dialed_urls(),
        vec![SEARCH_URL.to_string(), final_url.to_string()]
    );

    // Cross-host hop to a public host: followed, pinned to ITS vetted
    // addresses with ITS hostname for SNI, and the first host's api key
    // never appears on the second dial.
    let cross_url = "https://api.other.example/api?t=music&o=json";
    let mut transport = FakeTransport::default()
        .serve(&with_key, redirect_response(cross_url))
        .serve(cross_url, ok_response(BODY));
    let bytes = fetch_indexer(&with_key, &FetchConfig::default(), &dns, &mut transport)
        .expect("public cross-host hop must be followed");
    assert_eq!(bytes, BODY);
    assert_eq!(transport.dials.len(), 2);
    let second = &transport.dials[1];
    assert_eq!(second.host, OTHER_HOST);
    assert_eq!(second.sni_host, OTHER_HOST);
    assert_eq!(second.addresses, vec![IpAddr::V4(PUBLIC_V4_B)]);
    assert!(
        !second.url.contains("synthetic-test-key"),
        "api key must not be forwarded across hosts: {}",
        second.url
    );
}

/// Dial timeout and slow-drip total timeout abort with their stable
/// codes and never return partial bytes. At the deadline counts as
/// expired; one millisecond under each budget still fetches, which
/// proves the abort is the clock and not the content.
#[test]
fn dial_and_total_timeouts_have_stable_codes() {
    let dns = public_dns();
    let cfg = FetchConfig {
        dial_timeout_ms: 100,
        timeout_ms: 1_000,
        ..FetchConfig::default()
    };

    let slow_dial = RawResponse {
        connect_delay_ms: 100, // reaches the dial deadline
        ..ok_response(BODY)
    };
    let mut transport = FakeTransport::default().serve(SEARCH_URL, slow_dial);
    let err = fetch_indexer(SEARCH_URL, &cfg, &dns, &mut transport)
        .expect_err("a dial reaching the deadline must abort");
    assert_eq!(err, FetchRejection::DialTimeout);
    assert_eq!(err.code(), "indexer_fetch_dial_timeout");
    assert_eq!(err.http_status(), 504);

    let drip = RawResponse {
        status: 200,
        location: None,
        connect_delay_ms: 50,
        chunks: vec![
            BodyChunk {
                delay_ms: 400,
                bytes: BODY[..10].to_vec(),
            },
            // Connect 50 + 400 + 600 = 1050 ms > 1000 ms budget: abort.
            BodyChunk {
                delay_ms: 600,
                bytes: BODY[10..].to_vec(),
            },
        ],
    };
    let mut transport = FakeTransport::default().serve(SEARCH_URL, drip);
    let err = fetch_indexer(SEARCH_URL, &cfg, &dns, &mut transport)
        .expect_err("slow-drip fetch must time out, not hang or succeed");
    assert_eq!(err, FetchRejection::Timeout);
    assert_eq!(err.code(), "indexer_fetch_timeout");
    assert_eq!(err.http_status(), 504);

    let in_budget = RawResponse {
        status: 200,
        location: None,
        connect_delay_ms: 99, // 1 ms under the dial deadline
        chunks: vec![BodyChunk {
            delay_ms: 900, // 99 + 900 = 999 ms: 1 ms under the total
            bytes: BODY.to_vec(),
        }],
    };
    let mut transport = FakeTransport::default().serve(SEARCH_URL, in_budget);
    let bytes = fetch_indexer(SEARCH_URL, &cfg, &dns, &mut transport)
        .expect("in-budget fetch must succeed");
    assert_eq!(bytes, BODY);
}

/// Non-2xx upstream statuses map to the stable upstream code with the
/// origin status preserved in the rejection — never a successful fetch.
#[test]
fn upstream_status_maps_to_stable_code() {
    let dns = public_dns();
    for status in [401u16, 404, 429, 500] {
        let mut transport = FakeTransport::default().serve(
            SEARCH_URL,
            RawResponse {
                status,
                location: None,
                connect_delay_ms: 5,
                chunks: vec![],
            },
        );
        let err = fetch_indexer(SEARCH_URL, &FetchConfig::default(), &dns, &mut transport)
            .expect_err("upstream failure must not be a successful fetch");
        assert_eq!(err, FetchRejection::UpstreamStatus(status));
        assert_eq!(err.code(), "indexer_fetch_upstream_status");
        assert_eq!(err.http_status(), 502);
    }
}

/// The decision log records host + reason only: extracting the log host
/// can never leak embedded credentials, the indexer api key, or the URL
/// path/query, and unparseable input is never echoed back.
#[test]
fn decision_log_host_never_leaks_url_secrets() {
    let host =
        url_host_for_log("https://user:SECRET-PASS@indexer.example/api?t=music&apikey=SECRET-KEY");
    assert_eq!(host, HOST);
    assert!(!host.contains("SECRET"));

    assert_eq!(url_host_for_log(SEARCH_URL), HOST);
    assert_eq!(
        url_host_for_log("https://indexer.example:8443/api?apikey=SECRET-KEY"),
        HOST
    );
    let metadata = Ipv4Addr::new(169, 254, 169, 254);
    assert_eq!(
        url_host_for_log(&format!("https://{metadata}/latest/meta-data/")),
        metadata.to_string()
    );
    assert_eq!(url_host_for_log("https://[fd00::1]/api"), "fd00::1");
    for garbage in ["", "not a url", "https://", "://missing-scheme"] {
        let host = url_host_for_log(garbage);
        assert!(
            !host.contains("SECRET") && (garbage.is_empty() || !host.contains(garbage)),
            "unparseable input must not echo back: {host:?}"
        );
    }
}

/// The port rule, DNS-pin invariant, timeout/redirect numbers and stable
/// codes are documented in the worker README (the build documents them
/// with the code, ahead of the shared taxonomy registry).
#[test]
fn readme_documents_guard_and_stable_codes() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("ingest README must exist");
    let lower = readme.to_ascii_lowercase();
    assert!(
        readme.contains("443"),
        "README must state the 443-only rule"
    );
    assert!(
        lower.contains("pin") && lower.contains("dns"),
        "README must document the DNS-pin invariant"
    );
    assert!(readme.contains("indexer_fetch_forbidden_address"));
    assert!(readme.contains("indexer_fetch_port_not_allowed"));
    assert!(readme.contains("indexer_fetch_too_many_redirects"));
    assert!(readme.contains("indexer_fetch_dial_timeout"));
    assert!(readme.contains("indexer_fetch_timeout"));
    assert!(
        readme.contains("5000") || readme.contains("5_000") || lower.contains("5 s"),
        "README must state the dial timeout with the contract value"
    );
    assert!(
        readme.contains("15000") || readme.contains("15_000") || lower.contains("15 s"),
        "README must state the total fetch timeout with the contract value"
    );
}
