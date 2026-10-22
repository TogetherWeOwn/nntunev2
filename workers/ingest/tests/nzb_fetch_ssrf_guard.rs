//! NZB fetch-from-URL SSRF guard acceptance: allowlist + timeout.
//!
//! Paired acceptance tests for the `ingest: NZB fetch-from-URL SSRF guard
//! with allowlist and timeout` build slice (server-side fetch of NZB URLs is
//! a fresh SSRF surface: scheme/host allowlist, private/metadata-IP
//! rejection, redirect cap and timeout, stable 4xx on block). The builder
//! builds on this branch and never edits this file: implement the missing
//! symbols to satisfy it. This is distinct from the BYOP vault
//! test-connection SSRF guard in `workers/api`.
//!
//! Acceptance mapping:
//!
//! - Metadata-IP and redirect-loop fixtures blocked with a stable 4xx:
//!   [`metadata_ip_literal_blocked_even_when_allowlisted`],
//!   [`allowlisted_host_resolving_to_forbidden_address_blocked_before_fetch`],
//!   [`redirect_loop_blocked_at_cap_with_stable_4xx`].
//! - Allowlisted fixture fetches; slow-drip fixture times out with a stable
//!   code: [`allowlisted_fixture_fetches_and_passes_intake`],
//!   [`slow_drip_fixture_times_out_with_stable_code`].
//!
//! Run only this file (the rest of the crate stays green until the build
//! lands):
//!
//! ```sh
//! cargo test --manifest-path workers/ingest/Cargo.toml --test nzb_fetch_ssrf_guard
//! ```
//!
//! Fixtures are synthetic and `std`-only: a fake DNS resolver and a fake
//! transport with per-chunk simulated delays. No real network, no DNS, no
//! Cloudflare bindings.
//!
//! # BUILDER CONTRACT (implement to make this file compile green)
//!
//! Add one module to `workers/ingest/src` and re-export it from `lib.rs`:
//!
//! ```text
//! nntune_ingest::nzb_fetch
//! ```
//!
//! Minimum shapes used below (you may extend, never narrow). No new external
//! crates are needed: `std` plus the crate's existing deps suffice.
//!
//! ```text
//! pub const FETCH_TIMEOUT_MS: u64 = 10_000;
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
//!     pub allowed_hosts: Vec<String>, // exact host match, case-insensitive
//!     pub timeout_ms: u64,
//!     pub max_redirects: u32,
//!     pub max_bytes: usize,
//! }
//! impl Default for FetchConfig
//! // allowed_hosts empty (deny-all), timeout_ms == FETCH_TIMEOUT_MS,
//! // max_redirects == MAX_REDIRECTS, max_bytes == MAX_NZB_BYTES (intake cap)
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq)]
//! pub enum FetchRejection {
//!     BadScheme, MalformedUrl, EmbeddedCredentials, HostNotAllowlisted,
//!     ForbiddenAddress, UnresolvableHost, TooManyRedirects, Timeout,
//!     TooLarge, UpstreamStatus(u16),
//! }
//! impl FetchRejection {
//!     /// Stable machine code, e.g. "nzb_fetch_forbidden_address".
//!     pub fn code(&self) -> &'static str;
//!     /// Stable HTTP status the route answers with this rejection.
//!     pub fn http_status(&self) -> u16;
//! }
//!
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! pub struct ValidatedUrl {
//!     pub url: String,
//!     pub host: String,
//!     pub addresses: Vec<std::net::IpAddr>,
//! }
//!
//! /// Pre-fetch guard: https only, no embedded credentials, host on the
//! /// allowlist, and every resolved address public (reject loopback,
//! /// link-local (including the cloud metadata endpoint), private, CGNAT, multicast,
//! /// reserved/documentation, IPv6 non-global). A forbidden address is
//! /// rejected even when the host was allowlisted by mistake. IP-literal
//! /// hosts are checked directly without DNS.
//! pub fn validate_fetch_url(
//!     url: &str,
//!     config: &FetchConfig,
//!     dns: &dyn DnsResolver,
//! ) -> Result<ValidatedUrl, FetchRejection>;
//!
//! /// One HTTP GET with NO redirect following (the fetcher follows manually
//! /// so every hop is re-guarded). The real Worker transport must pin the
//! /// dial to the validated addresses and keep the host for TLS SNI/Host.
//! pub trait FetchTransport {
//!     fn get(&mut self, url: &str) -> Result<RawResponse, FetchRejection>;
//! }
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! pub struct RawResponse {
//!     pub status: u16,
//!     pub location: Option<String>,
//!     pub chunks: Vec<BodyChunk>,
//! }
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! pub struct BodyChunk {
//!     /// Simulated/measured ms elapsed before this chunk arrived; the
//!     /// fetcher accumulates these against config.timeout_ms (slow-drip).
//!     pub delay_ms: u64,
//!     pub bytes: Vec<u8>,
//! }
//!
//! /// Guarded fetch: validate before ANY transport call; on 301/302/303/
//! /// 307/308 re-validate the Location from scratch and follow at most
//! /// config.max_redirects hops; accumulate chunk delays and abort with
//! /// Timeout past config.timeout_ms (never return partial bytes); abort
//! /// with TooLarge past config.max_bytes; non-2xx non-redirect upstream
//! /// statuses become UpstreamStatus(status).
//! pub fn fetch_nzb(
//!     url: &str,
//!     config: &FetchConfig,
//!     dns: &dyn DnsResolver,
//!     transport: &mut dyn FetchTransport,
//! ) -> Result<Vec<u8>, FetchRejection>;
//!
//! /// Host-only extraction for the decision log: the log records the
//! /// blocked URL's host and the rejection code, NEVER the full URL (it
//! /// may embed credentials or an apikey). Unparseable input yields a
//! /// placeholder containing no part of the input.
//! pub fn url_host_for_log(url: &str) -> String;
//! ```
//!
//! Wire the guarded fetch into the ingest fetch-from-URL path so a blocked
//! URL answers `FetchRejection::http_status()` with its stable
//! `FetchRejection::code()` before any fetch happens, and log each block as
//! host + code only. Document the allowlist config, the timeout/redirect
//! numbers and every stable code in `workers/ingest/README.md`.
//!
//! EXPECTED STATE: this file does NOT compile until the build lands
//! (unresolved `nzb_fetch` import). That red target is the acceptance proof;
//! `--lib` and `pipeline_e2e` stay green around it. Do not merge this branch
//! to main while it is red.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use nntune_ingest::fixtures::SMALL_NZB;
use nntune_ingest::nzb_fetch::{
    fetch_nzb, url_host_for_log, validate_fetch_url, BodyChunk, DnsResolver, FetchConfig,
    FetchRejection, FetchTransport, RawResponse, FETCH_TIMEOUT_MS, MAX_REDIRECTS,
};
use nntune_ingest::nzb_intake::{estimate_nzb, MAX_NZB_BYTES};
use nntune_ssrf_guard::{FORBIDDEN_VECTORS, PUBLIC_CONTROL_VECTORS};

const ALLOWED_HOST: &str = "nzb.example";
const CDN_HOST: &str = "cdn.example";
const ALLOWED_URL: &str = "https://nzb.example/get/abc123.nzb";
const METADATA_V4: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);
const PUBLIC_V4: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);

#[derive(Default)]
struct FakeDns {
    map: HashMap<String, Vec<IpAddr>>,
}

impl FakeDns {
    fn with(host: &str, addrs: &[IpAddr]) -> Self {
        let mut dns = Self::default();
        dns.map.insert(host.to_string(), addrs.to_vec());
        dns
    }
}

impl DnsResolver for FakeDns {
    fn resolve(&self, host: &str) -> Vec<IpAddr> {
        self.map.get(host).cloned().unwrap_or_default()
    }
}

#[derive(Default)]
struct FakeTransport {
    routes: HashMap<String, RawResponse>,
    calls: Vec<String>,
}

impl FakeTransport {
    fn serve(mut self, url: &str, response: RawResponse) -> Self {
        self.routes.insert(url.to_string(), response);
        self
    }
}

impl FetchTransport for FakeTransport {
    fn get(&mut self, url: &str) -> Result<RawResponse, FetchRejection> {
        self.calls.push(url.to_string());
        self.routes
            .get(url)
            .cloned()
            .ok_or(FetchRejection::UpstreamStatus(404))
    }
}

fn config(allowed: &[&str]) -> FetchConfig {
    FetchConfig {
        allowed_hosts: allowed.iter().map(|h| h.to_string()).collect(),
        ..FetchConfig::default()
    }
}

fn ok_response(body: &[u8]) -> RawResponse {
    RawResponse {
        status: 200,
        location: None,
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
        chunks: vec![],
    }
}

/// Pinned contract numbers + stable-code taxonomy (mirrored in the worker
/// README by the build). Every guard block is a stable 4xx; the timeout is
/// a stable 504; codes are distinct per rejection reason.
#[test]
fn contract_numbers_and_stable_codes_are_pinned() {
    assert_eq!(FETCH_TIMEOUT_MS, 10_000);
    assert_eq!(MAX_REDIRECTS, 3);
    let defaults = FetchConfig::default();
    assert!(defaults.allowed_hosts.is_empty(), "default must deny all");
    assert_eq!(defaults.timeout_ms, FETCH_TIMEOUT_MS);
    assert_eq!(defaults.max_redirects, MAX_REDIRECTS);
    assert_eq!(defaults.max_bytes, MAX_NZB_BYTES);

    let cases = [
        (FetchRejection::BadScheme, "nzb_fetch_bad_scheme", 400),
        (FetchRejection::MalformedUrl, "nzb_fetch_malformed_url", 400),
        (
            FetchRejection::EmbeddedCredentials,
            "nzb_fetch_embedded_credentials",
            400,
        ),
        (
            FetchRejection::HostNotAllowlisted,
            "nzb_fetch_host_not_allowlisted",
            403,
        ),
        (
            FetchRejection::ForbiddenAddress,
            "nzb_fetch_forbidden_address",
            403,
        ),
        (
            FetchRejection::UnresolvableHost,
            "nzb_fetch_unresolvable_host",
            400,
        ),
        (
            FetchRejection::TooManyRedirects,
            "nzb_fetch_too_many_redirects",
            400,
        ),
        (FetchRejection::Timeout, "nzb_fetch_timeout", 504),
        (FetchRejection::TooLarge, "nzb_fetch_too_large", 413),
        (
            FetchRejection::UpstreamStatus(503),
            "nzb_fetch_upstream_status",
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

/// The cloud metadata endpoint itself: blocked as a forbidden address even
/// if an operator mistakenly allowlists it, with a stable 4xx, and no
/// transport call is ever made.
#[test]
fn metadata_ip_literal_blocked_even_when_allowlisted() {
    let dns = FakeDns::default();
    let mut transport = FakeTransport::default();
    let metadata_url = "https://169.254.169.254/latest/meta-data/";
    let err = fetch_nzb(
        metadata_url,
        &config(&["169.254.169.254"]),
        &dns,
        &mut transport,
    )
    .expect_err("metadata IP must be blocked");
    assert_eq!(err, FetchRejection::ForbiddenAddress);
    assert_eq!(err.code(), "nzb_fetch_forbidden_address");
    assert!((400..500).contains(&err.http_status()));
    assert!(transport.calls.is_empty(), "blocked before any fetch");

    // Same guard at the validate-only seam, for every non-public family.
    for url in [
        "https://127.0.0.1/x.nzb",
        "https://10.0.0.5/x.nzb",
        "https://192.168.1.1/x.nzb",
        "https://172.16.0.9/x.nzb",
        "https://100.64.0.1/x.nzb",
        "https://[::1]/x.nzb",
        "https://[fe80::1]/x.nzb",
        "https://[fd00::1]/x.nzb",
    ] {
        let host = url_host_for_log(url);
        let err = validate_fetch_url(url, &config(&[host.as_str()]), &dns)
            .expect_err("non-public literal must be blocked");
        assert_eq!(err, FetchRejection::ForbiddenAddress, "url {url}");
    }
}

/// Shared transition/special vectors (single owner:
/// `nntune_ssrf_guard::FORBIDDEN_VECTORS`): mapped, 6to4, documentation,
/// Teredo and NAT64 literals are blocked even when allowlisted, via both
/// the literal and the DNS seams, with no transport call. No literal in
/// this test may be copied from the shared table: every address comes
/// from the import above.
#[test]
fn shared_transition_vectors_blocked_even_when_allowlisted() {
    for raw in FORBIDDEN_VECTORS {
        // Literal seam: the address as a URL host (bracketed v6).
        let url = format!("https://[{raw}]/x.nzb");
        let host = url_host_for_log(&url);
        assert_eq!(host, raw.to_ascii_lowercase(), "log host for {raw}");
        let dns = FakeDns::default();
        let err = validate_fetch_url(&url, &config(&[host.as_str()]), &dns)
            .expect_err("shared forbidden literal must be blocked");
        assert_eq!(err, FetchRejection::ForbiddenAddress, "vector {raw}");
        assert_eq!(err.code(), "nzb_fetch_forbidden_address");
        let mut transport = FakeTransport::default();
        let err = fetch_nzb(&url, &config(&[host.as_str()]), &dns, &mut transport)
            .expect_err("shared forbidden literal must be blocked before fetch");
        assert_eq!(err, FetchRejection::ForbiddenAddress, "vector {raw}");
        assert!(
            transport.calls.is_empty(),
            "blocked before any fetch: {raw}"
        );

        // DNS seam: an allowlisted name resolving to the vector is blocked.
        let ip: IpAddr = raw.parse().expect("shared vector must parse");
        let dns = FakeDns::with(ALLOWED_HOST, &[ip]);
        let mut transport = FakeTransport::default();
        let err = fetch_nzb(ALLOWED_URL, &config(&[ALLOWED_HOST]), &dns, &mut transport)
            .expect_err("shared forbidden DNS answer must be blocked");
        assert_eq!(err, FetchRejection::ForbiddenAddress, "vector {raw}");
        assert!(
            transport.calls.is_empty(),
            "blocked before any fetch: {raw}"
        );
    }
}

/// Shared public controls (single owner:
/// `nntune_ssrf_guard::PUBLIC_CONTROL_VECTORS`): public addresses must
/// NOT be blocked, via both the literal and DNS seams.
#[test]
fn shared_public_controls_are_not_blocked() {
    for raw in PUBLIC_CONTROL_VECTORS {
        let ip: IpAddr = raw.parse().expect("public control must parse");
        // Literal seam: a public literal validates with no DNS call.
        let url = if raw.contains(':') {
            format!("https://[{raw}]/x.nzb")
        } else {
            format!("https://{raw}/x.nzb")
        };
        let host = url_host_for_log(&url);
        let validated = validate_fetch_url(&url, &config(&[host.as_str()]), &FakeDns::default())
            .expect("public control literal must validate");
        assert_eq!(validated.addresses, vec![ip], "control {raw}");

        // DNS seam: an allowlisted name resolving only to public controls fetches.
        let dns = FakeDns::with(ALLOWED_HOST, &[ip]);
        let mut transport =
            FakeTransport::default().serve(ALLOWED_URL, ok_response(SMALL_NZB.as_bytes()));
        let bytes = fetch_nzb(ALLOWED_URL, &config(&[ALLOWED_HOST]), &dns, &mut transport)
            .expect("public control DNS answer must fetch");
        assert_eq!(bytes, SMALL_NZB.as_bytes(), "control {raw}");
    }
}

/// DNS-based SSRF: an allowlisted name that resolves (in whole or in part)
/// to a forbidden address is blocked before any fetch. Resolve-ALL: one
/// private record in the set poisons the whole answer.
#[test]
fn allowlisted_host_resolving_to_forbidden_address_blocked_before_fetch() {
    for addrs in [
        vec![IpAddr::V4(METADATA_V4)],
        vec![IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))],
        // Mixed answer: one public, one metadata — still blocked.
        vec![IpAddr::V4(PUBLIC_V4), IpAddr::V4(METADATA_V4)],
        vec![IpAddr::V6(Ipv6Addr::LOCALHOST)],
    ] {
        let dns = FakeDns::with(ALLOWED_HOST, &addrs);
        let mut transport = FakeTransport::default();
        let err = fetch_nzb(ALLOWED_URL, &config(&[ALLOWED_HOST]), &dns, &mut transport)
            .expect_err("forbidden resolved address must be blocked");
        assert_eq!(err, FetchRejection::ForbiddenAddress, "addrs {addrs:?}");
        assert!((400..500).contains(&err.http_status()));
        assert!(transport.calls.is_empty(), "blocked before any fetch");
    }

    // A host that resolves to nothing is refused, not fetched.
    let mut transport = FakeTransport::default();
    let err = fetch_nzb(
        ALLOWED_URL,
        &config(&[ALLOWED_HOST]),
        &FakeDns::default(),
        &mut transport,
    )
    .expect_err("unresolvable host must be refused");
    assert_eq!(err, FetchRejection::UnresolvableHost);
    assert!(transport.calls.is_empty());
}

/// Disallowed scheme, embedded credentials and non-allowlisted hosts are
/// all rejected before any fetch; the default config denies everything.
#[test]
fn bad_scheme_credentials_and_non_allowlisted_blocked_before_fetch() {
    let dns = FakeDns::with(ALLOWED_HOST, &[IpAddr::V4(PUBLIC_V4)]);
    let mut transport = FakeTransport::default();
    let cfg = config(&[ALLOWED_HOST]);

    for (url, want) in [
        ("http://nzb.example/x.nzb", FetchRejection::BadScheme),
        ("ftp://nzb.example/x.nzb", FetchRejection::BadScheme),
        ("file:///etc/passwd", FetchRejection::BadScheme),
        ("gopher://nzb.example/x", FetchRejection::BadScheme),
        (
            "https://user:pass@nzb.example/x.nzb",
            FetchRejection::EmbeddedCredentials,
        ),
        ("https://", FetchRejection::MalformedUrl),
        ("not a url", FetchRejection::MalformedUrl),
        (
            "https://evil.example/x.nzb",
            FetchRejection::HostNotAllowlisted,
        ),
        // Subdomains are not implied by an allowlist entry.
        (
            "https://sub.nzb.example/x.nzb",
            FetchRejection::HostNotAllowlisted,
        ),
        (
            "https://nzb.example.evil.example/x.nzb",
            FetchRejection::HostNotAllowlisted,
        ),
    ] {
        let err =
            fetch_nzb(url, &cfg, &dns, &mut transport).expect_err("hostile URL must be blocked");
        assert_eq!(err, want, "url {url}");
        assert!((400..500).contains(&err.http_status()), "url {url}");
    }
    assert!(
        transport.calls.is_empty(),
        "every block happens before any fetch"
    );

    // Deny-by-default: with no allowlist, even a clean public host is refused.
    let err = fetch_nzb(ALLOWED_URL, &FetchConfig::default(), &dns, &mut transport)
        .expect_err("empty allowlist must deny");
    assert_eq!(err, FetchRejection::HostNotAllowlisted);
    assert!(transport.calls.is_empty());
}

/// A redirect loop is cut off at the redirect cap with a stable 4xx.
#[test]
fn redirect_loop_blocked_at_cap_with_stable_4xx() {
    let dns = FakeDns::with(ALLOWED_HOST, &[IpAddr::V4(PUBLIC_V4)]);
    let mut cfg = config(&[ALLOWED_HOST]);
    cfg.max_redirects = 2;
    let mut transport = FakeTransport::default().serve(ALLOWED_URL, redirect_response(ALLOWED_URL));

    let err = fetch_nzb(ALLOWED_URL, &cfg, &dns, &mut transport)
        .expect_err("redirect loop must be blocked");
    assert_eq!(err, FetchRejection::TooManyRedirects);
    assert_eq!(err.code(), "nzb_fetch_too_many_redirects");
    assert!((400..500).contains(&err.http_status()));
    // Initial fetch + 2 followed redirects, then the loop is cut off.
    assert_eq!(transport.calls.len(), 3);
}

/// Every redirect hop is re-guarded from scratch: a hop to a forbidden
/// address or an http downgrade is blocked and never fetched.
#[test]
fn redirect_hops_are_reguarded() {
    // Allowlisted start redirects to an allowlisted name that resolves to
    // the metadata IP: the hop is blocked, the target never fetched.
    let mut dns = FakeDns::with(ALLOWED_HOST, &[IpAddr::V4(PUBLIC_V4)]);
    dns.map
        .insert(CDN_HOST.to_string(), vec![IpAddr::V4(METADATA_V4)]);
    let cdn_url = "https://cdn.example/x.nzb";
    let mut transport = FakeTransport::default().serve(ALLOWED_URL, redirect_response(cdn_url));
    let err = fetch_nzb(
        ALLOWED_URL,
        &config(&[ALLOWED_HOST, CDN_HOST]),
        &dns,
        &mut transport,
    )
    .expect_err("redirect to a forbidden address must be blocked");
    assert_eq!(err, FetchRejection::ForbiddenAddress);
    assert_eq!(transport.calls, vec![ALLOWED_URL.to_string()]);

    // Downgrade to http on the second hop: blocked as a bad scheme.
    let mut transport =
        FakeTransport::default().serve(ALLOWED_URL, redirect_response("http://nzb.example/x.nzb"));
    let err = fetch_nzb(ALLOWED_URL, &config(&[ALLOWED_HOST]), &dns, &mut transport)
        .expect_err("http downgrade must be blocked");
    assert_eq!(err, FetchRejection::BadScheme);
    assert_eq!(transport.calls, vec![ALLOWED_URL.to_string()]);

    // Hop to a host that is not allowlisted: blocked, never fetched.
    let mut transport = FakeTransport::default()
        .serve(ALLOWED_URL, redirect_response("https://evil.example/x.nzb"));
    let err = fetch_nzb(ALLOWED_URL, &config(&[ALLOWED_HOST]), &dns, &mut transport)
        .expect_err("redirect off the allowlist must be blocked");
    assert_eq!(err, FetchRejection::HostNotAllowlisted);
    assert_eq!(transport.calls, vec![ALLOWED_URL.to_string()]);
}

/// The allowlisted fixture fetches successfully inside the timeout, the
/// fetched bytes pass the existing NZB intake guard, and a single
/// allowlisted redirect hop is followed.
#[test]
fn allowlisted_fixture_fetches_and_passes_intake() {
    let dns = FakeDns::with(ALLOWED_HOST, &[IpAddr::V4(PUBLIC_V4)]);
    let mut transport =
        FakeTransport::default().serve(ALLOWED_URL, ok_response(SMALL_NZB.as_bytes()));
    let bytes = fetch_nzb(ALLOWED_URL, &config(&[ALLOWED_HOST]), &dns, &mut transport)
        .expect("allowlisted fixture must fetch");
    assert_eq!(bytes, SMALL_NZB.as_bytes());
    let est = estimate_nzb(std::str::from_utf8(&bytes).unwrap())
        .expect("fetched fixture must pass intake");
    assert_eq!(est.file_count, 1);
    assert_eq!(transport.calls, vec![ALLOWED_URL.to_string()]);

    // Host matching is case-insensitive, and the validated form keeps the
    // vetted addresses for dial pinning.
    let validated = validate_fetch_url("https://NZB.Example/x.nzb", &config(&[ALLOWED_HOST]), &dns)
        .expect("case-insensitive host match");
    assert_eq!(validated.addresses, vec![IpAddr::V4(PUBLIC_V4)]);

    // One absolute redirect to the same allowlisted host is followed.
    let final_url = "https://nzb.example/final.nzb";
    let mut transport = FakeTransport::default()
        .serve(ALLOWED_URL, redirect_response(final_url))
        .serve(final_url, ok_response(SMALL_NZB.as_bytes()));
    let bytes = fetch_nzb(ALLOWED_URL, &config(&[ALLOWED_HOST]), &dns, &mut transport)
        .expect("one allowlisted redirect must be followed");
    assert_eq!(bytes, SMALL_NZB.as_bytes());
    assert_eq!(
        transport.calls,
        vec![ALLOWED_URL.to_string(), final_url.to_string()]
    );
}

/// A slow-drip upstream that dribbles bytes past the total timeout is
/// aborted with the stable timeout code; partial bytes are never returned.
#[test]
fn slow_drip_fixture_times_out_with_stable_code() {
    let dns = FakeDns::with(ALLOWED_HOST, &[IpAddr::V4(PUBLIC_V4)]);
    let mut cfg = config(&[ALLOWED_HOST]);
    cfg.timeout_ms = 1_000;
    let drip = RawResponse {
        status: 200,
        location: None,
        chunks: vec![
            BodyChunk {
                delay_ms: 400,
                bytes: b"<?xml".to_vec(),
            },
            // Cumulative 1_100 ms > 1_000 ms budget: abort here.
            BodyChunk {
                delay_ms: 700,
                bytes: b" version?>".to_vec(),
            },
            BodyChunk {
                delay_ms: 10,
                bytes: b"<nzb></nzb>".to_vec(),
            },
        ],
    };
    let mut transport = FakeTransport::default().serve(ALLOWED_URL, drip);
    let err = fetch_nzb(ALLOWED_URL, &cfg, &dns, &mut transport)
        .expect_err("slow-drip fetch must time out, not hang or succeed");
    assert_eq!(err, FetchRejection::Timeout);
    assert_eq!(err.code(), "nzb_fetch_timeout");
    assert_eq!(err.http_status(), 504);
    assert_eq!(transport.calls, vec![ALLOWED_URL.to_string()]);

    // The same body inside the budget succeeds (the abort is the timeout,
    // not the content): cumulative delay under the limit fetches fully.
    let fast = RawResponse {
        status: 200,
        location: None,
        chunks: vec![
            BodyChunk {
                delay_ms: 400,
                bytes: SMALL_NZB.as_bytes()[..10].to_vec(),
            },
            BodyChunk {
                delay_ms: 400,
                bytes: SMALL_NZB.as_bytes()[10..].to_vec(),
            },
        ],
    };
    let mut transport = FakeTransport::default().serve(ALLOWED_URL, fast);
    let bytes =
        fetch_nzb(ALLOWED_URL, &cfg, &dns, &mut transport).expect("in-budget fetch must succeed");
    assert_eq!(bytes, SMALL_NZB.as_bytes());
}

/// Upstream error statuses and oversize bodies map to stable codes, never
/// to a successful (partial) fetch.
#[test]
fn upstream_status_and_oversize_map_to_stable_codes() {
    let dns = FakeDns::with(ALLOWED_HOST, &[IpAddr::V4(PUBLIC_V4)]);
    let mut transport = FakeTransport::default().serve(
        ALLOWED_URL,
        RawResponse {
            status: 500,
            location: None,
            chunks: vec![],
        },
    );
    let err = fetch_nzb(ALLOWED_URL, &config(&[ALLOWED_HOST]), &dns, &mut transport)
        .expect_err("upstream 500 must not be a successful fetch");
    assert_eq!(err, FetchRejection::UpstreamStatus(500));
    assert_eq!(err.code(), "nzb_fetch_upstream_status");

    let mut cfg = config(&[ALLOWED_HOST]);
    cfg.max_bytes = 16;
    let mut transport =
        FakeTransport::default().serve(ALLOWED_URL, ok_response(SMALL_NZB.as_bytes()));
    let err = fetch_nzb(ALLOWED_URL, &cfg, &dns, &mut transport)
        .expect_err("oversize body must be refused");
    assert_eq!(err, FetchRejection::TooLarge);
    assert_eq!(err.http_status(), 413);
}

/// The decision log records host + reason only: extracting the log host
/// can never leak embedded credentials, an apikey, or the URL path/query.
#[test]
fn decision_log_host_never_leaks_url_secrets() {
    let host =
        url_host_for_log("https://user:SECRET-PASS@nzb.example/get/abc.nzb?apikey=SECRET-KEY");
    assert_eq!(host, ALLOWED_HOST);
    assert!(!host.contains("SECRET"));

    assert_eq!(url_host_for_log(ALLOWED_URL), ALLOWED_HOST);
    assert_eq!(
        url_host_for_log("https://169.254.169.254/x"),
        "169.254.169.254"
    );
    for garbage in ["", "not a url", "https://", "://missing-scheme"] {
        let host = url_host_for_log(garbage);
        assert!(
            !host.contains("SECRET") && (garbage.is_empty() || !host.contains(garbage)),
            "unparseable input must not echo back: {host:?}"
        );
    }
}

/// The allowlist config, timeout/redirect numbers and stable codes are
/// documented in the worker README (the build documents them with the code).
#[test]
fn readme_documents_allowlist_and_stable_codes() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("ingest README must exist");
    let lower = readme.to_ascii_lowercase();
    assert!(
        lower.contains("allowlist"),
        "README must document the allowlist"
    );
    assert!(readme.contains("nzb_fetch_host_not_allowlisted"));
    assert!(readme.contains("nzb_fetch_forbidden_address"));
    assert!(readme.contains("nzb_fetch_too_many_redirects"));
    assert!(readme.contains("nzb_fetch_timeout"));
    assert!(
        readme.contains("10000") || readme.contains("10_000") || lower.contains("10 s"),
        "README must state the fetch timeout with the contract value"
    );
}
